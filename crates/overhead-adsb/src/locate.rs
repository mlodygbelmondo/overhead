//! The whole stage-2 pipeline: inputs -> archives -> candidate flights ->
//! matching -> ranked locations.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use overhead_core::audio::Audio;
use overhead_core::detect::Detection;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ambiguity::{self, Ambiguity, AmbiguityConfig};
use crate::cache::{self, Day};
use crate::dem::Dem;
use crate::flight::{self, Flight, FlightOptions};
use crate::geo::BBox;
use crate::matcher::{self, Candidate, MatchConfig, Search, SearchStats};
use crate::region::{Region, TimeSpec};
use crate::score::Observed;

#[derive(Clone)]
pub struct LocateInput<'a> {
    pub detections: Vec<Detection>,
    /// The recording, for spectrogram scoring (else the detections' own curves are used).
    pub audio: Option<&'a Audio>,
    pub duration: f64,
    pub time: TimeSpec,
    pub region: Option<Region>,
    pub cfg: MatchConfig,
    pub cache_root: PathBuf,
    pub streams: usize,
    /// Fixed terrain height instead of SRTM (tests/offline).
    pub flat_ground: Option<f64>,
    /// Validation: true (lat, lon, start).
    pub truth: Option<(f64, f64, f64)>,
    /// Cuts in the recording (empty = none, the default behaviour).
    pub cuts: Vec<Cut>,
    /// Most material that can have been removed at one cut (s); `None` =
    /// anything up to the end of the possible day(s).
    pub cut_gap_max_s: Option<f64>,
}

/// A cut in the recording: material was removed at `at`, so detections
/// after it get their own recording start (see [`matcher::Cuts`]).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Cut {
    /// Recording time of the cut (s).
    pub at: f64,
    /// When the part after the cut started, if known: the absolute time
    /// (unix s) of its recording time 0, i.e. `start of the part − at`, so
    /// that it has the same meaning as the recording start. `None` = free
    /// (from the earliest possible start to the end of the possible day(s),
    /// or `cut_gap_max_s` per cut).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_unix: Option<f64>,
    /// Tolerance of `start_unix` (s, default 120).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_tol_s: Option<f64>,
}

/// A detections file: either a plain list (`overhead-cli detect`, the app's
/// saved edits) or `{"detections": [...], "cuts": [...]}` where a cut is a
/// number (seconds) or a [`Cut`] object.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct DetectionsFile {
    pub detections: Vec<Detection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cuts: Vec<CutSpec>,
}

/// A cut in a file: `1234.5` or `{"at": 1234.5, "start_unix": ..., "start_tol_s": ...}`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum CutSpec {
    At(f64),
    Full(Cut),
}

impl From<CutSpec> for Cut {
    fn from(c: CutSpec) -> Self {
        match c {
            CutSpec::At(at) => Cut { at, start_unix: None, start_tol_s: None },
            CutSpec::Full(c) => c,
        }
    }
}

/// Read a detections file in either form.
pub fn read_detections(bytes: &[u8]) -> Result<(Vec<Detection>, Vec<Cut>)> {
    if bytes.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'[') {
        return Ok((serde_json::from_slice(bytes)?, Vec::new()));
    }
    let f: DetectionsFile = serde_json::from_slice(bytes)?;
    Ok((f.detections, f.cuts.into_iter().map(Cut::from).collect()))
}

/// A cut as searched: where, and the start window of the part after it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CutUsed {
    pub at: f64,
    pub start_window_utc: (String, String),
}

/// The matcher's [`matcher::Cuts`] for a recording: cuts inside the
/// recording, sorted, each with the start window of the part after it.
pub fn cut_windows(cuts: &[Cut], time: &TimeSpec, duration: f64, gap_max: Option<f64>) -> matcher::Cuts {
    let mut cuts: Vec<Cut> = cuts.iter().filter(|c| c.at > 0.0 && c.at < duration).cloned().collect();
    cuts.sort_by(|a, b| a.at.total_cmp(&b.at));
    cuts.dedup_by(|a, b| (a.at - b.at).abs() < 1e-6);
    let first = time.start_windows();
    let (lo, hi0) = (first.first().map_or(0.0, |w| w.0), first.last().map_or(0.0, |w| w.1));
    // latest start without the start and hours hints: the end of the possible day(s)
    let day_hi = TimeSpec { start: None, hours: None, ..time.clone() }.start_hull().1.max(hi0);
    let windows = cuts
        .iter()
        .enumerate()
        .map(|(k, c)| match c.start_unix {
            Some(s) => {
                let tol = c.start_tol_s.unwrap_or(120.0);
                vec![(s - tol, s + tol)]
            }
            None => {
                // k + 1 cuts before this part, each removing at most `g`
                let hi = match gap_max {
                    Some(g) => hi0 + g.max(0.0) * (k + 1) as f64,
                    None => day_hi,
                };
                vec![(lo, hi)]
            }
        })
        .collect();
    matcher::Cuts { at: cuts.iter().map(|c| c.at).collect(), windows }
}

/// One gap between consecutive fly-bys tried as the place of a cut (see [`find_cuts`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CutTest {
    /// The cut tried (s): halfway between the two detections.
    pub at: f64,
    /// The detections (indices into the input) on either side of it.
    pub after_detection: usize,
    pub before_detection: usize,
    /// Best candidate with this cut added, and its gain over the search without it.
    pub score: f64,
    pub matched: usize,
    pub lat: f64,
    pub lon: f64,
    pub score_gain: f64,
    pub matched_gain: i64,
    /// Round of the greedy search (0 = first) and whether this cut was taken.
    pub round: usize,
    pub accepted: bool,
}

/// What [`find_cuts`] tried and the cuts it suggests.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CutSearch {
    /// Best score and matched fly-bys without any tested cut (round 0).
    pub baseline_score: f64,
    pub baseline_matched: usize,
    pub tests: Vec<CutTest>,
    /// Accepted cuts, in the order found.
    pub suggested: Vec<FoundCut>,
}

/// A cut found from the fly-bys.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FoundCut {
    /// Where it was searched (s): in the first gap of `range`.
    pub at: f64,
    /// The cut can be anywhere from `range.0` to `range.1` s: every gap in
    /// it gives the same answer (the fly-bys in between fit nothing either way).
    pub range: (f64, f64),
    pub score_gain: f64,
    pub matched_gain: i64,
    /// With the recording: the strongest waveform evidence of a cut inside
    /// `range` ([`overhead_core::cuts::suggest`], sensitive settings), as
    /// (time s, strength). A hint for the exact place, not proof.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_hint: Option<(f64, f64)>,
}

/// A cut is taken when it makes at least one more fly-by fit and raises the
/// best score by this much (one well-fitting fly-by adds about 0.8–1.1).
/// Freeing a start always helps a little: on the unedited Oosterschelde
/// recording every gap gave at most +0.05 and no extra fly-by, and with the
/// real cut taken, a second, wrong cut let a ground vehicle fit some flight
/// for +0.36 with one more fly-by.
pub const CUT_SCORE_GAIN_MIN: f64 = 0.6;

/// Finds cuts from the fly-bys themselves: a cut only matters for the search
/// through which fly-bys end up on either side of it, so every gap between
/// two consecutive detections is tried as a cut (with that part's start
/// free), and the gap that makes the most fly-bys fit together is taken if
/// it beats the search without it clearly ([`CUT_SCORE_GAIN_MIN`]); then the
/// remaining gaps are tried again, up to `max_cuts`. The exact place inside
/// the gap does not matter for the search; a cut found from the audio
/// ([`overhead_core::cuts::suggest`]) or by the user gives it. One search per
/// gap and round: slower than one search, but the waveform evidence of a
/// splice inside one steady background is weak (a hard splice is about as
/// sharp as a natural click), while a cut that breaks the timing between
/// fly-bys is exactly what this sees.
pub fn find_cuts(inp: &LocateInput, max_cuts: usize) -> Result<CutSearch> {
    find_cuts_with(&inp.detections, &inp.cuts, inp.audio, max_cuts, |cuts| {
        let mut i = inp.clone();
        i.cuts = cuts.to_vec();
        let out = locate(i)?;
        Ok(out.candidates.first().map_or((0.0, 0, f64::NAN, f64::NAN), |c| (c.score, c.matched, c.lat, c.lon)))
    })
}

/// [`find_cuts`] with any search: `best(cuts)` returns the best candidate's
/// (score, matched, lat, lon) with those cuts.
pub fn find_cuts_with(
    detections: &[Detection],
    given: &[Cut],
    audio: Option<&Audio>,
    max_cuts: usize,
    mut best: impl FnMut(&[Cut]) -> Result<(f64, usize, f64, f64)>,
) -> Result<CutSearch> {
    let mut order: Vec<usize> = (0..detections.len()).collect();
    order.sort_by(|a, b| detections[*a].flyby.tc.total_cmp(&detections[*b].flyby.tc));
    let gaps: Vec<(f64, usize, usize)> = order
        .windows(2)
        .filter_map(|w| {
            let (a, b) = (&detections[w[0]], &detections[w[1]]);
            let (end, start) = (a.region.t_end.max(a.flyby.tc), b.region.t_start.min(b.flyby.tc));
            let at = if end < start { 0.5 * (end + start) } else { 0.5 * (a.flyby.tc + b.flyby.tc) };
            (b.flyby.tc - a.flyby.tc > 1.0).then_some((at, w[0], w[1]))
        })
        .collect();
    let (mut base_score, mut base_matched, _, _) = best(given)?;
    let mut res = CutSearch { baseline_score: base_score, baseline_matched: base_matched, ..Default::default() };
    let mut cuts = given.to_vec();
    let mut audio_found: Option<Vec<overhead_core::cuts::CutSuggestion>> = None;
    for round in 0..max_cuts {
        let first = res.tests.len();
        for &(at, a, b) in &gaps {
            let (ta, tb) = (detections[a].flyby.tc, detections[b].flyby.tc);
            // a cut already separates these two
            if cuts.iter().any(|c| c.at > ta && c.at < tb) {
                continue;
            }
            let mut with = cuts.clone();
            with.push(Cut { at, start_unix: None, start_tol_s: None });
            let (score, matched, lat, lon) = best(&with)?;
            eprintln!(
                "cut test {round}: between det {a} and {b} at {at:.1} s: score {score:.2} ({:+.2}), {matched} matched ({:+})",
                score - base_score,
                matched as i64 - base_matched as i64
            );
            res.tests.push(CutTest {
                at,
                after_detection: a,
                before_detection: b,
                score,
                matched,
                lat,
                lon,
                score_gain: score - base_score,
                matched_gain: matched as i64 - base_matched as i64,
                round,
                accepted: false,
            });
        }
        let pick = res.tests[first..]
            .iter()
            .enumerate()
            .filter(|(_, t)| t.matched_gain >= 1 && t.score_gain >= CUT_SCORE_GAIN_MIN)
            .max_by(|x, y| x.1.score.total_cmp(&y.1.score))
            .map(|(k, _)| first + k);
        let Some(k) = pick else { break };
        // neighbouring gaps that do as well: the cut can be in any of them
        let same = |t: &CutTest| (t.score - res.tests[k].score).abs() < 0.02 && t.matched == res.tests[k].matched;
        let (mut a, mut b) = (k, k);
        while a > first && same(&res.tests[a - 1]) {
            a -= 1;
        }
        while b + 1 < res.tests.len() && same(&res.tests[b + 1]) {
            b += 1;
        }
        let k = a;
        res.tests[k].accepted = true;
        let (lo_det, hi_det) = (&detections[res.tests[a].after_detection], &detections[res.tests[b].before_detection]);
        let range = (lo_det.region.t_end.max(lo_det.flyby.tc), hi_det.region.t_start.min(hi_det.flyby.tc));
        let t = &res.tests[k];
        cuts.push(Cut { at: t.at, start_unix: None, start_tol_s: None });
        if audio_found.is_none()
            && let Some(a) = audio
        {
            let cfg = overhead_core::cuts::CutConfig { max: 1000, ..overhead_core::cuts::CutConfig::sensitive() };
            audio_found = Some(overhead_core::cuts::suggest(a, &cfg));
        }
        let audio_hint = audio_found.as_ref().and_then(|v| {
            v.iter()
                .filter(|c| c.at > range.0 && c.at < range.1)
                .filter(|c| !detections.iter().any(|d| c.at > d.region.t_start && c.at < d.region.t_end))
                .max_by(|x, y| x.strength.total_cmp(&y.strength))
                .map(|c| (c.at, c.strength))
        });
        res.suggested.push(FoundCut { at: t.at, range, score_gain: t.score_gain, matched_gain: t.matched_gain, audio_hint });
        (base_score, base_matched) = (t.score, t.matched);
    }
    Ok(res)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocateOutput {
    pub region: Option<Region>,
    pub search_box: BBox,
    /// Earliest and latest possible start.
    pub start_window_utc: (String, String),
    /// The start windows searched (one per possible UTC offset and day unless they overlap).
    pub start_windows_utc: Vec<(String, String)>,
    pub utc_offsets_h: Vec<f64>,
    pub utc_offset_source: String,
    pub warnings: Vec<String>,
    pub days: Vec<String>,
    pub detections_used: usize,
    pub candidate_flights: usize,
    pub stats: SearchStats,
    pub candidates: Vec<Candidate>,
    /// How sure the answer is: distinct places that fit, their probabilities,
    /// the plausible area and a confident/ambiguous verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambiguity: Option<Ambiguity>,
    /// Cuts in the recording that were searched (absent without cuts).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cuts: Vec<CutUsed>,
    /// The gaps between fly-bys tried as cuts (only with `locate --find-cuts`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut_search: Option<CutSearch>,
    /// Score maps, per-fly-by scores, modelled curves, silence and map links
    /// (only with `locate --explain`, see [`crate::explain`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explain: Option<crate::explain::Explain>,
}

pub fn dem_for(root: &Path, flat: Option<f64>) -> Dem {
    match flat {
        Some(h) => Dem::flat(h),
        None => Dem::new(root.parent().unwrap_or(root).join("dem")),
    }
}

/// Candidate flights in a box and time span from the cached days (fetching them if needed).
pub fn load_flights(root: &Path, days: &[chrono::NaiveDate], bbox: &BBox, t0: f64, t1: f64, streams: usize) -> Result<Vec<Flight>> {
    let mut tracks = Vec::new();
    for d in days {
        let day = Day::ensure(root, *d, streams)?;
        tracks.extend(day.tracks(bbox, t0, t1, 9000.0)?);
    }
    let tracks = cache::merge_same_hex(tracks);
    // a regional pressure-altitude correction only makes sense for a regional box
    let regional = bbox.lat_max - bbox.lat_min < 6.0 && bbox.lon_max - bbox.lon_min < 8.0;
    let baro_offset = if regional { flight::regional_baro_offset(&tracks) } else { None };
    let opt = FlightOptions { baro_offset, ..FlightOptions::default() };
    Ok(tracks.par_iter().filter_map(|t| Flight::from_track(t, &opt)).collect())
}

/// Detections are tracked in 64-bit masks.
pub const MAX_DETECTIONS: usize = 64;

/// Up to `n` detections, keeping the most useful ones (confident aircraft
/// first, then by contrast), in their original order.
pub fn select_detections(dets: &[Detection], n: usize) -> Vec<Detection> {
    if dets.len() <= n {
        return dets.to_vec();
    }
    let mut idx: Vec<usize> = (0..dets.len()).collect();
    let key = |d: &Detection| (d.confident && !d.likely_ground_vehicle) as u8;
    idx.sort_by(|a, b| key(&dets[*b]).cmp(&key(&dets[*a])).then(dets[*b].contrast.total_cmp(&dets[*a].contrast)));
    idx.truncate(n);
    idx.sort_unstable();
    idx.into_iter().map(|i| dets[i].clone()).collect()
}

pub fn locate(inp: LocateInput) -> Result<LocateOutput> {
    Ok(run(inp, false)?.0)
}

/// [`locate`], keeping what is needed to explain the result afterwards
/// (score maps, per-fly-by scores, modelled curves, silence): see
/// [`crate::explain::Session`]. The output is the same as `locate`'s.
pub fn locate_session(inp: LocateInput) -> Result<(LocateOutput, crate::explain::Session)> {
    let (out, s) = run(inp, true)?;
    Ok((out, s.expect("session kept")))
}

fn run(inp: LocateInput, keep: bool) -> Result<(LocateOutput, Option<crate::explain::Session>)> {
    let mut warnings: Vec<String> = Vec::new();
    let dets = select_detections(&inp.detections, MAX_DETECTIONS);
    if dets.is_empty() {
        bail!("no detections to match");
    }
    if inp.detections.len() > dets.len() {
        let w = format!(
            "{} detections given; only the {} strongest (confident first, then by contrast) are matched",
            inp.detections.len(),
            dets.len()
        );
        eprintln!("WARNING: {w}");
        warnings.push(w);
    }
    let windows = inp.time.start_windows();
    let cuts = cut_windows(&inp.cuts, &inp.time, inp.duration, inp.cut_gap_max_s);
    let (s_lo, mut s_hi) = inp.time.start_hull();
    let mut days = inp.time.utc_days(inp.duration);
    if !cuts.at.is_empty() {
        // the later parts may start later than the first one
        s_hi = cuts.windows.iter().flatten().map(|w| w.1).fold(s_hi, f64::max);
        let mut d = chrono::DateTime::from_timestamp((s_lo - 900.0) as i64, 0).unwrap_or_default().date_naive();
        let last = chrono::DateTime::from_timestamp((s_hi + inp.duration + 900.0) as i64, 0).unwrap_or_default().date_naive();
        while d <= last {
            if !days.contains(&d) {
                days.push(d);
            }
            d += chrono::Duration::days(1);
        }
        days.sort();
    }
    let search_box = inp.region.as_ref().map_or(BBox::WORLD, |r| r.bbox);
    if inp.region.is_none() {
        eprintln!("no region given: searching every candidate flight of the day(s); this is slow");
    }
    let t_setup = std::time::Instant::now();
    let flights = load_flights(
        &inp.cache_root,
        &days,
        &search_box.expand(15_000.0),
        s_lo - 1800.0,
        s_hi + inp.duration + 1800.0,
        inp.streams,
    )?;
    eprintln!(
        "{} candidate flights, start window {} .. {} ({} start window(s))",
        flights.len(),
        matcher::utc(s_lo),
        matcher::utc(s_hi),
        windows.len()
    );
    // traced detections are scored against their points, the others as before
    let t_flights = t_setup.elapsed().as_secs_f64();
    let observed: Vec<Observed> = dets.par_iter().map(|d| Observed::for_detection(inp.audio, d)).collect();
    let t_observed = t_setup.elapsed().as_secs_f64() - t_flights;
    let dem = dem_for(&inp.cache_root, inp.flat_ground);
    let background = inp.audio.map(crate::silence::Background::measure);
    if std::env::var_os("OVERHEAD_TIMING").is_some() {
        eprintln!(
            "[timing] flights {t_flights:.2}s, observed maps {t_observed:.2}s, background {:.2}s",
            t_setup.elapsed().as_secs_f64() - t_flights - t_observed
        );
    }
    let search = Search {
        dets: &dets,
        observed: &observed,
        flights: &flights,
        dem: &dem,
        duration: inp.duration,
        start_windows: windows.clone(),
        region: search_box,
        cfg: inp.cfg.clone(),
        truth: inp.truth,
        cuts: cuts.clone(),
        collect: matcher::Collect { coarse_cell_m: keep.then(|| crate::explain::coarse_cell_for(&search_box)) },
        background: background.as_ref(),
    };
    // diagnostics: OVERHEAD_PROBE="lat,lon,start_unix,hex_or_reg"
    if let Ok(p) = std::env::var("OVERHEAD_PROBE") {
        let v: Vec<&str> = p.split(',').collect();
        if v.len() == 4
            && let (Ok(la), Ok(lo), Ok(s)) = (v[0].parse::<f64>(), v[1].parse::<f64>(), v[2].parse::<f64>())
        {
            search.probe(la, lo, s, v[3]);
        }
    }
    let detailed = search.run_detailed();
    let mut candidates = detailed.candidates;
    let (stats, alternatives) = (detailed.stats, detailed.alternatives);
    let amb = assess_ambiguity(&alternatives, &mut candidates, &search_box, &windows, dets.len(), &ambiguity_config());
    for t in dem.failed_tiles() {
        warnings.push(format!("terrain tile {t} could not be downloaded; heights above ground there assume 0 m"));
    }
    drop(search);
    let cuts_used: Vec<CutUsed> = cuts
        .at
        .iter()
        .zip(&cuts.windows)
        .map(|(at, w)| CutUsed {
            at: *at,
            start_window_utc: (matcher::utc(w.first().map_or(0.0, |x| x.0)), matcher::utc(w.last().map_or(0.0, |x| x.1))),
        })
        .collect();
    let out = LocateOutput {
        region: inp.region,
        search_box,
        start_window_utc: (matcher::utc(s_lo), matcher::utc(s_hi)),
        start_windows_utc: windows.iter().map(|w| (matcher::utc(w.0), matcher::utc(w.1))).collect(),
        utc_offsets_h: inp.time.offsets.iter().map(|o| *o as f64 / 3600.0).collect(),
        utc_offset_source: inp.time.offset_source.clone(),
        warnings,
        days: days.iter().map(|d| d.to_string()).collect(),
        detections_used: dets.len(),
        candidate_flights: flights.len(),
        stats,
        candidates,
        ambiguity: amb,
        cuts: cuts_used,
        cut_search: None,
        explain: None,
    };
    let session = keep.then(|| crate::explain::Session {
        dets,
        observed,
        flights,
        dem,
        duration: inp.duration,
        start_windows: windows,
        region: search_box,
        cfg: inp.cfg,
        cuts,
        candidates: out.candidates.clone(),
        refined: detailed.refined,
        candidate_refined: detailed.candidate_refined,
        coarse: detailed.coarse,
        coarse_cell_m: detailed.coarse_cell_m,
        background,
    });
    Ok((out, session))
}

/// The ambiguity settings, with `OVERHEAD_AMBIG_T` / `OVERHEAD_AMBIG_ELSE`
/// (a score, or `none`) overriding the temperature and the elsewhere score
/// (for calibration).
pub fn ambiguity_config() -> AmbiguityConfig {
    let mut c = AmbiguityConfig::default();
    if let Some(t) = std::env::var("OVERHEAD_AMBIG_T").ok().and_then(|v| v.parse::<f64>().ok()) {
        c.temperature = t;
    }
    if let Ok(v) = std::env::var("OVERHEAD_AMBIG_ELSE") {
        c.elsewhere_score = v.parse::<f64>().ok();
    }
    if let Some(n) = std::env::var("OVERHEAD_AMBIG_PLACES").ok().and_then(|v| v.parse::<usize>().ok()) {
        c.max_places = n;
    }
    c
}

/// Ambiguity assessment of a search, and each candidate's place probability.
pub fn assess_ambiguity(
    alternatives: &[ambiguity::Alternative],
    candidates: &mut [Candidate],
    search_box: &BBox,
    windows: &[(f64, f64)],
    detections: usize,
    cfg: &AmbiguityConfig,
) -> Option<Ambiguity> {
    let mut amb = ambiguity::assess(alternatives, candidates, search_box, cfg)?;
    let mut seen: Vec<(usize, usize)> = Vec::new(); // (place index, candidate rank)
    for c in candidates.iter_mut() {
        // places beyond the listed ones still have a probability, but a
        // candidate is always among the best places, so it is listed
        match ambiguity::place_of(&amb.places, c.lat, c.lon, cfg.distinct_m) {
            Some(i) => match seen.iter().find(|s| s.0 == i) {
                Some(s) => c.same_place_as = Some(s.1),
                None => {
                    c.probability = Some(amb.places[i].probability);
                    seen.push((i, c.rank));
                }
            },
            None => c.probability = Some(0.0),
        }
    }
    if !amb.confident {
        let span: f64 = windows.iter().map(|w| w.1 - w.0).sum();
        let matched = candidates.first().map_or(0, |c| c.matched);
        amb.hints = ambiguity::hints(span, search_box, matched, detections);
    }
    Some(amb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::StartTime;
    use chrono::{NaiveDate, TimeZone, Utc};

    fn time(start: Option<f64>) -> TimeSpec {
        let day = NaiveDate::from_ymd_opt(2024, 8, 18).unwrap();
        TimeSpec {
            first_day: day,
            last_day: day,
            offsets: vec![7200],
            offset_source: "test".into(),
            start: start.map(|s| (StartTime::Utc(Utc.timestamp_opt(s as i64, 0).unwrap()), 180.0)),
            hours: None,
            hours_slack_s: 1800.0,
        }
    }

    #[test]
    fn detections_file_in_both_forms() {
        let (d, c) = read_detections(b"  []").unwrap();
        assert!(d.is_empty() && c.is_empty());
        let (d, c) =
            read_detections(br#"{"detections": [], "cuts": [600.5, {"at": 1200, "start_unix": 1723965000, "start_tol_s": 30}]}"#).unwrap();
        assert!(d.is_empty());
        assert_eq!(
            c,
            vec![
                Cut { at: 600.5, start_unix: None, start_tol_s: None },
                Cut { at: 1200.0, start_unix: Some(1_723_965_000.0), start_tol_s: Some(30.0) }
            ]
        );
        let (_, c) = read_detections(br#"{"detections": []}"#).unwrap();
        assert!(c.is_empty());
        assert!(read_detections(b"{\"cuts\": []}").is_err());
    }

    #[test]
    fn cut_windows_with_and_without_a_known_start() {
        let s = 1_723_965_780.0; // 2024-08-18T07:23:00Z
        let t = time(Some(s));
        let first = t.start_windows();
        assert_eq!(first.len(), 1);
        let (lo, hi) = first[0];
        let cuts = [
            Cut { at: 3000.0, start_unix: Some(s + 900.0), start_tol_s: None },
            Cut { at: 1000.0, start_unix: None, start_tol_s: None },
            Cut { at: 9000.0, start_unix: None, start_tol_s: None }, // past the end: ignored
            Cut { at: 1000.0, start_unix: None, start_tol_s: None }, // duplicate
        ];
        let c = cut_windows(&cuts, &t, 7000.0, None);
        assert_eq!(c.at, vec![1000.0, 3000.0]);
        // unknown start: from the earliest first-part start to the end of the
        // day (local 24:00 = 22:00Z)
        assert_eq!(c.windows[0].len(), 1);
        assert_eq!(c.windows[0][0].0, lo);
        let end_of_day = Utc.with_ymd_and_hms(2024, 8, 18, 22, 0, 0).unwrap().timestamp() as f64;
        assert!(c.windows[0][0].1 >= end_of_day - 3600.0, "{:?} vs {end_of_day}", c.windows[0]);
        // known start: ± the default tolerance
        assert_eq!(c.windows[1], vec![(s + 900.0 - 120.0, s + 900.0 + 120.0)]);
        // with a largest gap per cut
        let g = cut_windows(&cuts[1..2], &t, 7000.0, Some(600.0));
        assert_eq!(g.windows[0], vec![(lo, hi + 600.0)]);
        // no cuts: the default, one part
        let none = cut_windows(&[], &t, 7000.0, None);
        assert!(none.at.is_empty() && none.windows.is_empty());
    }
}
