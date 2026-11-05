//! Automatic detection of aircraft fly-bys in a whole recording.
//!
//! 1. Whitened overview spectrogram of the whole file, as a z-score map.
//! 2. Partial tracking: per-column spectral peaks linked into tracks whose
//!    frequency may only fall (or stay level) from column to column.
//! 3. Keep tracks that last long enough and fall by a Doppler-like amount;
//!    stationary hum and short speech glides drop out here.
//! 4. Fit the retarded-time model in a box around each track, seeded by the
//!    track's own points (in parallel).
//! 5. Accept fits that stand out from frequency-shifted copies of themselves
//!    and are physically plausible for an aircraft.
//! 6. Group lines sharing speed, distance and CPA time into one fly-by
//!    (harmonics of one aircraft), keeping the strongest line as primary.
//! 7. Per fly-by, scan for every other line that follows the primary's
//!    Doppler shape (`comb`), express the lines as integer multiples of their
//!    fundamental (which may lie below the strongest line and need not be
//!    heard), refit the fly-by on all lines together, and drop fly-bys that
//!    turn out to be lines of another.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

use crate::audio::Audio;
use crate::comb;
use crate::fit::{self, FitConfig, Ranges, Region, RidgePoint, ScoreMap, TransitionQuality};
use crate::model::{DEFAULT_C, Flyby};
use crate::spectrogram::{self, SpecRequest, Spectrogram};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetectConfig {
    /// Frequency band searched for Doppler lines (Hz).
    pub f_min: f64,
    pub f_max: f64,
    /// Speed of sound (m/s).
    pub c: f64,
    /// Slowest source accepted as an aircraft (m/s).
    pub v_min: f64,
    /// Minimum contrast (fit score vs. frequency-shifted copies, in sigmas).
    pub min_contrast: f64,
}

impl Default for DetectConfig {
    fn default() -> Self {
        Self { f_min: 40.0, f_max: 1000.0, c: DEFAULT_C, v_min: 25.0, min_contrast: 6.0 }
    }
}

/// One detected fly-by (all harmonics of one aircraft grouped together).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Detection {
    /// The fly-by; `f0` is the fundamental whose multiples `harmonics` are
    /// the lines heard. It need not be heard itself (a propeller whose 3rd,
    /// 4th and 7th multiples are), and may lie below the strongest line.
    pub flyby: Flyby,
    /// Multipliers of `flyby.f0` for every line of this fly-by, rising.
    /// Integers, except lines that are no multiple of the fundamental of the
    /// others (kept as measured ratios, possibly < 1). Older saved
    /// detections hold 1.0 for the strongest line and measured ratios for
    /// the others.
    pub harmonics: Vec<f64>,
    pub ranges: Ranges,
    /// Mean z-score under the fitted curve.
    pub score: f64,
    /// How far the curve stands out from frequency-shifted copies (sigmas).
    pub contrast: f64,
    /// Fit quality on the steep part of the curve.
    pub quality: TransitionQuality,
    /// Slow and close, or measured too loosely to rule that out: possibly a
    /// ground vehicle (or machine, bird), or just an uncertain fit. Not
    /// positive evidence of a ground source; kept so a later stage (ADS-B
    /// matching) can decide.
    pub likely_ground_vehicle: bool,
    /// No parameter range ran off the edge of its scan. Unconfident
    /// detections are still real candidates for the "silence" logic later,
    /// but their measurements constrain little.
    pub confident: bool,
    /// Rest frequencies of the lines other than the strongest (Hz): the
    /// other `harmonics` times `flyby.f0`.
    pub other_lines: Vec<f64>,
    /// Box the strongest line was found in.
    pub region: Region,
    /// Points `[t s, f Hz]` traced by hand along the line at `flyby.f0`.
    /// When present, the ADS-B matcher scores candidates by how close their
    /// modelled curve passes to these points instead of by spectrogram
    /// brightness (a traced line is usually too faint for that).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<Vec<[f64; 2]>>,
    /// Found by the detector, moved by the user, or traced by the user.
    #[serde(default, skip_serializing_if = "Origin::is_detected")]
    pub origin: Origin,
}

/// Where a detection came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    #[default]
    Detected,
    /// A detected curve the user moved; refitted near the new placement.
    Edited,
    /// Traced by hand (see `Detection::trace`).
    Traced,
}

impl Origin {
    pub fn is_detected(&self) -> bool {
        *self == Origin::Detected
    }
}

/// Counts from each pipeline stage, for diagnostics.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DetectStats {
    pub tracks: usize,
    pub candidates: usize,
    pub accepted_lines: usize,
    pub flybys: usize,
}

/// Overview spectrogram resolution.
/// Overview spectrograms as (FFT length at 8 kHz, column step s).
const OVERVIEWS: [(usize, f64); 2] = [(8192, 0.5), (2048, 0.25)];
/// Peaks below this z-score are ignored by the tracker.
const PEAK_Z: f32 = 1.8;
const MAX_PEAKS_PER_COL: usize = 40;
/// Columns a track may skip before it ends.
const MAX_GAP: usize = 4;
/// When 8+ bright peaks lie near the steep part of the curve they must follow
/// it this closely (bins). Stair-stepping machine tones fail this.
const MAX_RMS_BINS: f64 = 0.9;
/// Slower AND closer than this: more likely a road vehicle, machine or bird
/// than an aircraft. Flagged, not dropped: ADS-B matching settles it.
/// Road traffic rarely exceeds `GROUND_V_MAX` (130 km/h). Between that and
/// `GROUND_V_SLOW_MAX` the benchmark's ground sources are mis-measured cars
/// and machines that pass within ~200 m, while aircraft that slow
/// (helicopters, light aircraft in slow flight or in a turn, whose radial
/// speed stays below their ground speed) pass 220-430 m away; real ones:
/// a C152 at 39 m/s, 391 m (ADS-B: 600 m slant) and a helicopter right
/// overhead at 39 m/s, 286 m. So in that band only very close passes are
/// flagged.
const GROUND_V_MAX: f64 = 36.0;
const GROUND_D_MAX: f64 = 450.0;
const GROUND_V_SLOW_MAX: f64 = 45.0;
const GROUND_D_SLOW_MAX: f64 = 200.0;
/// A fit whose plausible ranges span more than these factors (max/min of v,
/// or of d) is too loose to tell an aircraft from a ground source; it is
/// flagged when the low ends reach slow and close. Mostly mis-fits that
/// joined pieces of different sources; tightly measured low, slow aircraft
/// (helicopters at 35-50 m/s, 450-750 m) span factors of 1.1-1.4 and 1.7-3.1.
/// Distance is never a reason on its own: aircraft near airfields pass
/// closer than 200 m (an earlier "d < 200 m" rule flagged them).
const GROUND_LOOSE_V: f64 = 2.0;
const GROUND_LOOSE_D: f64 = 4.0;
/// A d range counts as loose only if it is also this wide (m): close passes
/// span large factors in d with little absolute spread (a helicopter at
/// 245 m [77..680] is measured well enough). Benchmark mis-fits that are
/// loose in d only span 840 m or more.
const GROUND_LOOSE_D_SPAN: f64 = 700.0;
/// Minimum contrast on each side of CPA (see `fit::side_contrast`).
const MIN_SIDE_CONTRAST: f64 = 3.0;
/// Highest harmonic multiple grouped by curve shape.
const MAX_HARMONIC: f64 = 16.0;
/// A line is a multiple of a group's curve when the scaled curve's rms
/// distance (bins) to the line's points is within
/// `EXPLAINED_RMS_FACTOR * own + EXPLAINED_RMS_BINS` (`own`: the line's fit).
const EXPLAINED_RMS_FACTOR: f64 = 1.5;
const EXPLAINED_RMS_BINS: f64 = 0.5;
/// Points used for that comparison: z-score and minimum count per CPA side.
const EXPLAINED_MIN_Z: f64 = 2.5;
/// Or: this (z-weighted) fraction of the points lies on integer multiples.
const EXPLAINED_HARMONIC_FRACTION: f64 = 0.7;
const EXPLAINED_MIN_SIDE_POINTS: usize = 4;
/// CPA times further apart than this are never compared by curve (s).
const EXPLAINED_TC_TOL: f64 = 30.0;
/// A curve runs along a group line (`claimed_by`) where they are this close:
/// max(bins, fraction of the frequency).
const CLAIM_TOL_BINS: f64 = 1.5;
const CLAIM_TOL_REL: f64 = 0.006;
/// CPA times of harmonic lines of one fly-by agree within this (s).
const HARMONIC_TC_TOL: f64 = 8.0;
/// Fits whose f0 differ by less than this fraction are the same line.
const SAME_LINE_REL: f64 = 0.05;
/// Lines of a group that were fitted on their own, and lie outside the
/// comb scan's map, are snapped to the multiple of the fundamental within
/// this fraction (their fits drift).
const SNAP_REL: f64 = 0.015;
/// ... when that window spans at most this fraction of a multiple either
/// side (up to the 6th): further up, nearly any line would lie near some
/// multiple of a low fundamental, and snapping would only move it.
const SNAP_MAX_SPAN: f64 = 0.1;
/// A joint refit that moves the speed or the distance of the primary's own
/// fit by more than these fractions does not clear the primary's ground
/// verdict (see `finish_group`). Real passes whose refit cleared it:
/// Vlissingen at 796 s, v and d within 2 %; a tractor in the benchmark
/// mis-fitted far and fast: v +15 %, d +41 %.
const GROUND_REFIT_V_REL: f64 = 0.1;
const GROUND_REFIT_D_REL: f64 = 0.25;
/// At most this many lines per fly-by are fitted and scored jointly (the
/// strongest); the matcher's map spans them all.
const MAX_LINES: usize = 8;
/// The joint fit's map leaves room for a pass up to this many times faster
/// than the primary's fit, and starts the fit at these multiples of its
/// speed (with `d` scaled by their squares, keeping the steepness).
const JOINT_V_RANGE: f64 = 2.2;
const JOINT_V_STARTS: [f64; 3] = [1.0, 1.4, 2.0];
/// The joint fit's box spans at least this far either side of CPA (s).
const JOINT_MIN_HALF_S: f64 = 60.0;
/// The joint fit may move the CPA time this far (s) and the rest frequency
/// by this fraction, and must keep this fraction of the primary line's
/// contrast on the primary's own map (and the detector's minimum). A line's
/// own fit places its rest frequency well even when its CPA time is off by
/// many seconds (Amer, PH-NSC: 15 s early, rest frequency within the
/// matcher's ±1.2 % sweep), and a joint fit that moves it further has
/// settled on the wrong CPA time (there: 6.5 % and 12 s).
const JOINT_TC_TOL: f64 = 10.0;
const JOINT_F0_REL: f64 = 0.01;
const JOINT_KEEP_CONTRAST: f64 = 0.75;
/// ... and this fraction of the lines' mean contrast under the primary's geometry.
const JOINT_KEEP_LINES: f64 = 0.9;
/// A line of one fly-by lies on a line of another when its rest frequency
/// under the other's geometry is that line's within this fraction.
const COMB_SAME_REL: f64 = 0.015;
/// ... and keeps this fraction of its own contrast there (and the minimum).
const SHARED_CONTRAST: f64 = 0.5;
/// Of two such fly-bys the weaker one's geometry replaces the stronger's
/// only when it explains both primaries by this factor better.
const DUPLICATE_SWAP: f64 = 1.1;
/// Aircraft tones are not audible beyond this slant distance (m).
const MAX_AUDIBLE_D: f64 = 15_000.0;
/// Minimum number of points (overview columns) in a candidate track.
const MIN_TRACK_POINTS: usize = 16;
/// Steepest drop the overview tracker follows (Hz/s); a 400 Hz line passing
/// at 200 m/s and 1 km falls at ~47 Hz/s.
const TRACK_MAX_SLOPE_HZ_S: f64 = 50.0;
/// Shorter fragments are dropped before joining.
const MIN_FRAGMENT_POINTS: usize = 6;
/// Largest gap between fragments of one line (s).
const JOIN_GAP_S: f64 = 4.0;

/// Find every fly-by in `audio`, sorted by heard-CPA time.
pub fn detect(audio: &Audio, cfg: &DetectConfig) -> Vec<Detection> {
    detect_with_stats(audio, cfg).0
}

pub fn detect_with_stats(audio: &Audio, cfg: &DetectConfig) -> (Vec<Detection>, DetectStats) {
    let mut stats = DetectStats::default();
    let duration = audio.duration();
    // the decimator's transition band lets aliases in above ~75% of Nyquist
    let f_max = cfg.f_max.min(audio.sample_rate / 2.0 * 0.75);
    if duration < 10.0 || f_max <= cfg.f_min {
        return (Vec::new(), stats);
    }
    let fit_cfg = FitConfig { c: cfg.c, ..FitConfig::default() };
    // Track lines at two time/frequency resolutions: the long window keeps
    // faint, slowly changing lines; the short one keeps steep lines of fast or
    // close passes, which smear over many bins within a long window.
    let mut tracks = Vec::new();
    for (n_fft, dt) in OVERVIEWS {
        let spec = spectrogram::compute(
            audio,
            &SpecRequest {
                t_start: 0.0,
                t_end: duration,
                f_min: cfg.f_min,
                f_max,
                columns: (duration / dt).round().max(1.0) as usize,
                n_fft,
                whiten: true,
            },
        );
        let overview = ScoreMap::new(spec);
        tracks.extend(track_partials(&overview.spec, TRACK_MAX_SLOPE_HZ_S));
    }
    stats.tracks = tracks.len();
    let candidates: Vec<&Track> = tracks.iter().filter(|t| is_doppler_like(t)).collect();
    stats.candidates = candidates.len();

    // Fit each candidate in parallel.
    let next = AtomicUsize::new(0);
    let lines = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(track) = candidates.get(i) else { break };
                    if let Some(line) = fit_track(audio, track, cfg, &fit_cfg, f_max) {
                        lines.lock().unwrap().push(line);
                    }
                }
            });
        }
    });
    let mut lines = lines.into_inner().unwrap();
    stats.accepted_lines = lines.len();

    // Strongest first, so each group's primary is its clearest line.
    lines.sort_by(|a, b| b.contrast.total_cmp(&a.contrast));
    let mut groups: Vec<Group> = Vec::new();
    let debug = std::env::var_os("OVERHEAD_DEBUG").is_some();
    for line in lines {
        // How the line belongs to a group, if it does
        let belongs = |g: &Line, others: &[f64]| -> Option<Belongs> {
            let by_params = same_flyby(&g.flyby, &line.flyby)
                || std::iter::once(g.flyby.f0)
                    .chain(others.iter().copied())
                    .any(|f| harmonic_pair(f, line.flyby.f0) && loosely_same_geometry(&g.flyby, &line.flyby));
            if by_params {
                return Some(Belongs::Join(line.flyby.f0));
            }
            if let Some(f) = explained_by(&line, &g.flyby, cfg.c) {
                return Some(Belongs::Join(f));
            }
            if let Some(f) = explained_by(g, &line.flyby, cfg.c) {
                // The primary's points lie on the line's curve; the primary
                // may be the mis-fit (e.g. a curve hopping between harmonics).
                return Some(if better_geometry(&line, g, Some(f), None, cfg.c) {
                    Belongs::Swap(f)
                } else {
                    Belongs::Join(line.flyby.f0)
                });
            }
            let (f, free) = claimed_by(&line, &g.flyby, others, cfg.c)?;
            // Mutual: one side of each runs along the other (a fit that
            // joined one side of a line to unrelated pieces is claimed back
            // by it). The one with more evidence of its own off the other's
            // lines is the real pass.
            if let Some((f_old, g_free)) = claimed_by(g, &line.flyby, &[], cfg.c)
                && free > g_free
            {
                return Some(Belongs::Swap(f_old));
            }
            Some(Belongs::Join(f))
        };
        match groups.iter_mut().find_map(|(g, others)| belongs(g, others).map(|b| (g, others, b))) {
            Some((primary, others, Belongs::Swap(f_old))) => {
                if debug {
                    eprintln!("swap primary {:?} for {:?}", primary.flyby, line.flyby);
                }
                let line_f0 = line.flyby.f0;
                *primary = line;
                // the old primary's line, now as a multiple of the new curve;
                // drop lines that coincide with the new primary or each other
                let mut kept: Vec<f64> = Vec::new();
                for o in std::iter::once(f_old).chain(others.drain(..)) {
                    let close = |a: f64, b: f64| (a - b).abs() < SAME_LINE_REL * a.max(b);
                    if !close(o, line_f0) && !kept.iter().any(|k| close(o, *k)) {
                        kept.push(o);
                    }
                }
                *others = kept;
            }
            Some((primary, others, Belongs::Join(f0))) => {
                // Two fits of the same line (e.g. the halves of a track split
                // at CPA) differ by a few percent in f0; that is not a harmonic.
                let close = |o: f64| (f0 - o).abs() < SAME_LINE_REL * f0.max(o);
                let duplicate = close(primary.flyby.f0) || others.iter().any(|o| close(*o));
                if !duplicate {
                    others.push(f0);
                }
            }
            None => groups.push((line, Vec::new())),
        }
    }
    // After a swap a group can carry the geometry of a group formed earlier
    // around one of its other lines: merge groups whose primaries now agree.
    let mut i = 0;
    while i < groups.len() {
        let mut j = i + 1;
        while j < groups.len() {
            let (a, b) = (&groups[i].0.flyby, &groups[j].0.flyby);
            if same_flyby(a, b) || (harmonic_pair(a.f0, b.f0) && loosely_same_geometry(a, b)) {
                let (mut line, mut more) = groups.remove(j);
                let (primary, others) = &mut groups[i];
                let swapped = line.contrast > primary.contrast;
                if swapped {
                    std::mem::swap(primary, &mut line);
                    std::mem::swap(others, &mut more);
                }
                if debug {
                    eprintln!("merge {:?} into {:?}", line.flyby, primary.flyby);
                }
                let close = |a: f64, b: f64| (a - b).abs() < SAME_LINE_REL * a.max(b);
                for o in std::iter::once(line.flyby.f0).chain(more) {
                    if !close(o, primary.flyby.f0) && !others.iter().any(|k| close(o, *k)) {
                        others.push(o);
                    }
                }
                // a new primary must be compared again with the groups
                // already passed over
                if swapped {
                    j = i + 1;
                }
                continue;
            }
            j += 1;
        }
        i += 1;
    }

    // Per group (in parallel): every line sharing the primary's Doppler
    // shape, their fundamental, and one fit on all of them.
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<Group>>> = groups.into_iter().map(|g| Mutex::new(Some(g))).collect();
    let done = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(slot) = slots.get(i) else { break };
                    let (line, others) = slot.lock().unwrap().take().expect("each group taken once");
                    let fin = finish_group(audio, line, others, cfg, f_max);
                    done.lock().unwrap().push((i, fin));
                }
            });
        }
    });
    let mut finished = done.into_inner().unwrap();
    finished.sort_by_key(|(i, _)| *i);
    let finished = merge_comb_duplicates(finished.into_iter().map(|(_, f)| f).collect(), cfg);
    let mut detections: Vec<Detection> = finished.into_iter().map(|f| f.detection).collect();
    detections.sort_by(|a, b| a.flyby.tc.total_cmp(&b.flyby.tc));
    stats.flybys = detections.len();
    (detections, stats)
}

/// A group of lines: its primary and the rest frequencies of the others.
type Group = (Line, Vec<f64>);

/// How a line joins an existing group (see the grouping loop).
enum Belongs {
    /// As another line, at this rest frequency.
    Join(f64),
    /// As the new primary; the old primary's line sits at this frequency.
    Swap(f64),
}

/// A tracked spectral line in the overview: (time s, frequency Hz, z) points.
#[derive(Clone, Debug)]
struct Track {
    points: Vec<RidgePoint>,
    last_col: usize,
    /// Smoothed frequency change per column (Hz).
    slope: f64,
}

/// One accepted Doppler line with the region map it was fitted on.
struct Line {
    flyby: Flyby,
    score: f64,
    contrast: f64,
    quality: TransitionQuality,
    region: Region,
    map: ScoreMap,
    /// Spectrogram settings the line was fitted with.
    analysis: FitConfig,
}

/// Link per-column peaks into tracks whose frequency never rises by more than
/// one bin per column and never falls faster than `max_slope_hz_s`.
fn track_partials(spec: &Spectrogram, max_slope_hz_s: f64) -> Vec<Track> {
    let max_drop = max_slope_hz_s * spec.dt;
    let mut active: Vec<Track> = Vec::new();
    let mut done: Vec<Track> = Vec::new();
    let mut peaks: Vec<(f64, f32)> = Vec::new();
    for col in 0..spec.cols {
        let column = spec.column(col);
        peaks.clear();
        for r in 1..spec.rows.saturating_sub(1) {
            let v = column[r];
            if v >= PEAK_Z && v > column[r - 1] && v >= column[r + 1] {
                // parabolic interpolation of the peak position
                let (a, b, c) = (column[r - 1] as f64, v as f64, column[r + 1] as f64);
                let denom = a - 2.0 * b + c;
                let offset = if denom.abs() > 1e-9 { (0.5 * (a - c) / denom).clamp(-0.5, 0.5) } else { 0.0 };
                peaks.push((spec.f_min + (r as f64 + offset) * spec.df, v));
            }
        }
        if peaks.len() > MAX_PEAKS_PER_COL {
            peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
            peaks.truncate(MAX_PEAKS_PER_COL);
        }
        let t = spec.col_time(col);

        // candidate (track, peak, cost) pairs, assigned greedily by cost
        let mut pairs: Vec<(usize, usize, f64)> = Vec::new();
        for (ti, track) in active.iter().enumerate() {
            let gap = (col - track.last_col) as f64;
            let last = track.points.last().expect("non-empty track").f;
            let predicted = last + track.slope * gap;
            for (pi, &(f, _)) in peaks.iter().enumerate() {
                let rise = f - last;
                if rise > spec.df * gap || rise < -max_drop * gap {
                    continue;
                }
                let err = (f - predicted).abs();
                if err <= 1.5 * spec.df + 0.5 * max_drop.min(4.0 * spec.df) {
                    pairs.push((ti, pi, err + 0.5 * spec.df * (gap - 1.0)));
                }
            }
        }
        pairs.sort_by(|a, b| a.2.total_cmp(&b.2));
        let mut track_used = vec![false; active.len()];
        let mut peak_used = vec![false; peaks.len()];
        for (ti, pi, _) in pairs {
            if track_used[ti] || peak_used[pi] {
                continue;
            }
            track_used[ti] = true;
            peak_used[pi] = true;
            let track = &mut active[ti];
            let gap = (col - track.last_col) as f64;
            let last = track.points.last().unwrap().f;
            let (f, z) = peaks[pi];
            let observed = ((f - last) / gap).clamp(-max_drop, 0.0);
            track.slope = 0.6 * track.slope + 0.4 * observed;
            track.points.push(RidgePoint { t, f, weight: z as f64 });
            track.last_col = col;
        }
        for (pi, &(f, z)) in peaks.iter().enumerate() {
            if !peak_used[pi] {
                active.push(Track { points: vec![RidgePoint { t, f, weight: z as f64 }], last_col: col, slope: 0.0 });
            }
        }
        let (keep, ended): (Vec<Track>, Vec<Track>) =
            active.into_iter().partition(|tr| col - tr.last_col <= MAX_GAP);
        active = keep;
        done.extend(ended.into_iter().filter(|tr| tr.points.len() >= MIN_FRAGMENT_POINTS));
    }
    done.extend(active.into_iter().filter(|tr| tr.points.len() >= MIN_FRAGMENT_POINTS));
    join_fragments(done, max_slope_hz_s, spec.df)
        .into_iter()
        .filter(|tr| tr.points.len() >= MIN_TRACK_POINTS)
        .collect()
}

/// Join fragments of one line. Near CPA a fast, close pass sweeps through
/// several bins within one FFT window, its peak drops below the threshold and
/// the track splits; the halves are linked when the second starts within
/// `JOIN_GAP_S` of the first's end at a frequency it could have fallen to.
fn join_fragments(mut tracks: Vec<Track>, max_slope_hz_s: f64, df: f64) -> Vec<Track> {
    tracks.sort_by(|a, b| a.points[0].t.total_cmp(&b.points[0].t));
    let starts: Vec<f64> = tracks.iter().map(|t| t.points[0].t).collect();
    let mut used = vec![false; tracks.len()];
    let mut out = Vec::new();
    for i in 0..tracks.len() {
        if used[i] {
            continue;
        }
        used[i] = true;
        let mut cur = tracks[i].clone();
        loop {
            let end = *cur.points.last().expect("non-empty track");
            let first = starts.partition_point(|s| *s <= end.t);
            let last = starts.partition_point(|s| *s <= end.t + JOIN_GAP_S);
            let next = (first..last)
                .filter(|&j| !used[j])
                .filter(|&j| {
                    let start = tracks[j].points[0];
                    let gap = start.t - end.t;
                    start.f <= end.f + 2.0 * df && start.f >= end.f - max_slope_hz_s * (gap + 1.0)
                })
                .max_by_key(|&j| tracks[j].points.len());
            let Some(j) = next else { break };
            used[j] = true;
            cur.points.extend_from_slice(&tracks[j].points);
            cur.last_col = tracks[j].last_col;
            cur.slope = tracks[j].slope;
        }
        out.push(cur);
    }
    out
}

/// A track is worth fitting if it is long enough and falls by a Doppler-like
/// amount; level lines (hum, engines at rest) are rejected.
fn is_doppler_like(track: &Track) -> bool {
    let pts = &track.points;
    if pts.len() < MIN_TRACK_POINTS {
        return false;
    }
    let k = (pts.len() / 6).max(3);
    let head = pts[..k].iter().map(|p| p.f).sum::<f64>() / k as f64;
    let tail = pts[pts.len() - k..].iter().map(|p| p.f).sum::<f64>() / k as f64;
    let drop = head - tail;
    // v ≥ ~10 m/s gives a total drop of ≥ ~6% between asymptotes; a partial
    // track sees less, so require 1.5% and at least 2 Hz.
    drop >= 2.0_f64.max(0.015 * head)
}

fn fit_track(audio: &Audio, track: &Track, cfg: &DetectConfig, fit_cfg: &FitConfig, f_max: f64) -> Option<Line> {
    let pts = &track.points;
    let (t0, t1) = (pts[0].t, pts[pts.len() - 1].t);
    let dur = t1 - t0;
    let f_lo = pts.iter().map(|p| p.f).fold(f64::INFINITY, f64::min);
    let f_hi = pts.iter().map(|p| p.f).fold(f64::NEG_INFINITY, f64::max);
    // Pass 1: a box around the track, wide enough for the contrast copies.
    let region = Region {
        t_start: (t0 - dur.max(30.0)).max(0.0),
        t_end: (t1 + dur.max(30.0)).min(audio.duration()),
        f_min: (f_lo * 0.85 - 25.0).max(1.0),
        f_max: (f_hi * 1.15 + 25.0).min(f_max),
    };
    let map = ScoreMap::new(fit::region_spectrogram(audio, &region, fit_cfg));
    let (first, _) = fit::fit_seeded(&map, pts, fit_cfg)?;
    if !first.v.is_finite() || first.v < cfg.v_min * 0.5 || first.d > 2.0 * MAX_AUDIBLE_D {
        return None;
    }
    // cheap early exit: pass 2 rarely more than doubles the contrast
    if !(fit::contrast(&map, &first, fit_cfg) >= 0.5 * cfg.min_contrast) {
        return None;
    }

    // Pass 2: the track may cover only part of the curve (a fast close pass
    // smears near CPA and splits the track), so rebuild the box from the
    // fitted asymptotes and transition time, with an FFT window matched to
    // the steepest slope, and refit there.
    let c = cfg.c;
    let transition = (4.0 * first.d / first.v).clamp(40.0, 600.0);
    let analysis = matched_analysis(&first, audio.sample_rate, fit_cfg);
    // room for the ±20-bin contrast copies at this FFT size
    let pad = copy_margin(&analysis, audio.sample_rate);
    let region = Region {
        t_start: (first.tc - transition).max(0.0),
        t_end: (first.tc + transition).min(audio.duration()),
        f_min: (first.f_recede(c) * 0.85 - pad).max(1.0),
        f_max: (first.f_approach(c) * 1.15 + pad).min(f_max),
    };
    let map = ScoreMap::new(fit::region_spectrogram(audio, &region, &analysis));
    let guided = fit::guided_points(&map, &first, c);
    let (flyby, score) = fit::fit_from(&map, &guided, first, &analysis)
        .map(|(p, s)| {
            // keep whichever of the two fits is brighter on the refined map
            let s1 = map.score(&first, c, &analysis.harmonics);
            if s >= s1 { (p, s) } else { (first, s1) }
        })
        .unwrap_or((first, map.score(&first, c, &analysis.harmonics)));

    // the fitted CPA must sit near the tracked line, not wander off
    if flyby.tc < t0 - dur.max(10.0) - transition || flyby.tc > t1 + dur.max(10.0) + transition {
        return None;
    }
    if !(flyby.v >= cfg.v_min) || !(flyby.d <= MAX_AUDIBLE_D) {
        return None;
    }
    // The region must show most of the S-shaped drop; a curve that is almost
    // level across it is a slowly drifting tone, not a fly-by.
    let seen = flyby.freq_at(region.t_start, c) - flyby.freq_at(region.t_end, c);
    if !(seen >= 0.5 * (flyby.f_approach(c) - flyby.f_recede(c))) {
        return None;
    }
    let contrast = fit::contrast(&map, &flyby, &analysis);
    let quality = fit::transition_quality(&map, &flyby, c);
    let sides = fit::side_contrast(&map, &flyby, &analysis);
    // a steep line smears across bins within one FFT window; allow for it
    let rms_limit = MAX_RMS_BINS + 0.1 * quality.smear_bins;
    if std::env::var_os("OVERHEAD_DEBUG").is_some() {
        eprintln!(
            "cand t {:.0}..{:.0} -> tc {:.1} v {:.1} d {:.0} f0 {:.1} contrast {:.1} sides {:.1}/{:.1} cov {:.2} rms {:.2}/{:.2} cols {} nfft {}",
            t0, t1, flyby.tc, flyby.v, flyby.d, flyby.f0, contrast, sides.0, sides.1, quality.coverage, quality.rms_bins, rms_limit,
            quality.columns, analysis.n_fft
        );
    }
    if !(contrast >= cfg.min_contrast)
        || !(sides.0.min(sides.1) >= MIN_SIDE_CONTRAST)
        // the shape test needs enough peaks; faint lines are judged on contrast
        || (quality.coverage * quality.columns as f64 >= 8.0 && !(quality.rms_bins <= rms_limit))
    {
        return None;
    }
    Some(Line { flyby, score, contrast, quality, region, map, analysis })
}

/// One-line summary of a group's primary line, for `OVERHEAD_DEBUG`.
fn line_report(line: &Line, n_lines: usize) -> String {
    let p = &line.flyby;
    let sides = fit::side_contrast(&line.map, p, &line.analysis);
    let q = &line.quality;
    format!(
        "line f0 {:.1} v {:.1} d {:.0} contrast {:.1} sides {:.1}/{:.1} cov {:.2}/{:.2} rms {:.2} smear {:.2} cols {} box t {:.0}..{:.0} lines {n_lines}",
        p.f0,
        p.v,
        p.d,
        line.contrast,
        sides.0,
        sides.1,
        q.coverage_sides.0,
        q.coverage_sides.1,
        q.rms_bins,
        q.smear_bins,
        q.columns,
        line.region.t_start,
        line.region.t_end
    )
}

/// Frequency margin (Hz) for the frequency-shifted copies `fit::contrast` uses
/// (20 bins), plus a little slack.
fn copy_margin(analysis: &FitConfig, sample_rate: f64) -> f64 {
    21.0 * sample_rate / analysis.n_fft as f64 + 5.0
}

/// FFT window matched to the curve's steepest slope: a chirp of slope `s`
/// Hz/s is sharpest with a window of about `1/sqrt(s)` seconds.
fn matched_analysis(p: &Flyby, sample_rate: f64, base: &FitConfig) -> FitConfig {
    let slope = p.slope_at_cpa(base.c).abs().max(1e-3);
    let window = (1.0 / slope.sqrt()).clamp(0.25, 2.0);
    let n_fft = ((window * sample_rate).log2().round().exp2() as usize).clamp(2048, 16384);
    let dt = (0.5 * n_fft as f64 / sample_rate).min(base.dt);
    FitConfig { n_fft, dt, ..base.clone() }
}

/// A finished group: its primary line and the detection made of it.
struct Finished {
    line: Line,
    detection: Detection,
}

/// Turn a group into a detection: find every line along the primary's
/// Doppler shape (`comb::scan_lines`), express the lines as multiples of
/// their fundamental (`comb::fundamental`, which may lie below the strongest
/// line and need not be heard itself) and refit the fly-by on all of them.
/// A group whose primary has no other line stays a one-line fit.
fn finish_group(audio: &Audio, line: Line, mut others: Vec<f64>, cfg: &DetectConfig, f_max: f64) -> Finished {
    let c = cfg.c;
    let debug = std::env::var_os("OVERHEAD_DEBUG").is_some();
    others.sort_by(f64::total_cmp);
    let dbg = debug.then(|| line_report(&line, others.len() + 1));
    let p = line.flyby;
    let comb = comb::scan_lines(audio, &p, ((cfg.f_min * 0.8 - 25.0).max(10.0), f_max), c);
    let df = comb.as_ref().map_or(line.map.spec.df, |cb| cb.df);
    let same = |a: f64, b: f64| (a - b).abs() <= (comb::PEAK_REL * a.max(b)).max(comb::PEAK_BINS * df);
    // The precisely placed lines, the primary first: the lines the scan
    // found, then those grouped by their own fits, measured again along the
    // primary's shape on the scan map (their own rest frequencies are less
    // precise). The primary as the scan measures it: ratios between lines measured on
    // one map are precise, while the primary's own fit may place its line a
    // fraction of a percent off the scan's (Vlissingen at 3411 s: 124.0 Hz,
    // the scan 123.6 Hz and 61.6 Hz, one half of it). Rest frequencies are
    // converted back to the primary's fit with `to_fit`.
    // (Only when the scan sees it as a line there: on a close pass the
    // scan's curve may smear it, Vlissingen at 2408 s: 151.1 Hz against the
    // fit's 150.2 Hz and the scan's 75.1 Hz.)
    let primary_f = comb
        .as_ref()
        .and_then(|cb| cb.measure(p.f0))
        .filter(|m| m.is_line() && same(m.f, p.f0))
        .map_or(p.f0, |m| m.f);
    let to_fit = p.f0 / primary_f;
    // (frequency, weight, also found by its own fit)
    let mut lines: Vec<(f64, f64, bool)> = vec![(primary_f, line.contrast, true)];
    for l in comb.iter().flat_map(|cb| &cb.lines) {
        if !lines.iter().any(|q| same(q.0, l.f)) {
            lines.push((l.f, l.contrast, false));
        }
    }
    // Grouped lines that the scan map does not confirm (outside it, or not
    // standing out along the primary's shape on its steep part): kept at
    // their own fits' rest frequencies, but only in the slots the lines
    // placed on the scan map leave free. Their own fits share the pass
    // (Amer at 2566 s: two lines of a PC-7 whose own fits agree, too faint
    // on the steep part to stand out, and needed to match it), but one that
    // the scan places on a line already found is that line (Oosterschelde
    // at 4938 s: 398.6 Hz is the scan's 405.0 Hz).
    let mut loose: Vec<f64> = Vec::new();
    for &o in &others {
        if let Some(q) = lines.iter_mut().find(|q| same(q.0, o)) {
            q.2 = true;
            continue;
        }
        match comb.as_ref().map(|cb| cb.measure(o)) {
            Some(Some(m)) => {
                if debug {
                    eprintln!("grouped tc {:.1}: {o:.1} Hz measured {m:?}", p.tc);
                }
                if let Some(q) = lines.iter_mut().find(|q| same(q.0, m.f)) {
                    // a line already found
                    q.2 = true;
                } else if m.contrast >= comb::MIN_CONTRAST && m.level < comb::MAX_STATIONARY {
                    lines.push((m.f, m.contrast, true));
                } else {
                    loose.push(o);
                }
            }
            _ => loose.push(o),
        }
    }
    let tol = |f: f64| (comb::TOL_REL * f).max(comb::TOL_BINS * df);
    let fw: Vec<(f64, f64)> = lines.iter().map(|l| (l.0, l.1)).collect();
    let (f1, mult) = comb::fundamental(&fw, 0, tol);
    // Lines at no multiple of the fundamental still share the pass when
    // their own fits found them too (a turboprop's turbine tone) or they
    // form a comb of their own (a twin's other propeller): kept at their
    // measured ratios. A lone line only the scan found, at no multiple of
    // anything heard, is left out: the scan follows the primary's fitted
    // shape, which may be far from the pass's (Amer at 2566 s: a PC-7 fitted
    // at 40 m/s and 470 m, where a lone 82 Hz line along that shape spoils
    // the match with its real pass at 2.4 km).
    let mut keep: Vec<bool> = lines.iter().zip(&mult).map(|(l, k)| k.is_some() || l.2).collect();
    keep[0] = true;
    loop {
        let pool: Vec<usize> = (0..lines.len()).filter(|i| !keep[*i]).collect();
        if pool.len() < 2 {
            break;
        }
        let sub: Vec<(f64, f64)> = pool.iter().map(|&i| fw[i]).collect();
        let strongest = (0..sub.len()).fold(0, |a, b| if sub[b].1 > sub[a].1 { b } else { a });
        let (_, m) = comb::fundamental(&sub, strongest, tol);
        if m.iter().flatten().count() < 2 {
            break;
        }
        for (&i, k) in pool.iter().zip(&m) {
            keep[i] |= k.is_some();
        }
    }
    let f1 = f1 * to_fit;
    // Multiples of the fundamental, or measured ratios. Weights: contrast;
    // loose lines below every line placed on the scan map.
    let mut picked: Vec<(f64, f64)> = lines
        .iter()
        .zip(&mult)
        .zip(&keep)
        .filter(|(_, kept)| **kept)
        .map(|((l, k), _)| (k.map_or(l.0 * to_fit / f1, f64::from), l.1))
        .collect();
    let primary_k = picked[0].0;
    for &o in &loose {
        let r = o / f1;
        let k = r.round();
        let h = if k >= 1.0 && SNAP_REL * k <= SNAP_MAX_SPAN && (r - k).abs() <= SNAP_REL * k { k } else { r };
        picked.push((h, f64::NEG_INFINITY));
    }
    // the strongest lines, the primary always among them
    let strongest_k = picked.iter().fold(picked[0], |a, b| if b.1 > a.1 { *b } else { a }).0;
    let mut rest: Vec<(f64, f64)> = picked[1..].to_vec();
    rest.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut harmonics: Vec<f64> = vec![primary_k];
    for (k, _) in &rest {
        if harmonics.len() < MAX_LINES && !harmonics.iter().any(|h| (h / k - 1.0).abs() < 1e-6) {
            harmonics.push(*k);
        }
    }
    harmonics.sort_by(f64::total_cmp);

    let joint = if harmonics.len() >= 2 { joint_fit(audio, &line, f1, &harmonics, primary_k, strongest_k, cfg, f_max) } else { None };
    let refitted = joint.is_some();
    // the primary's own fit and ranges
    let one = FitConfig { harmonics: vec![1.0], ..line.analysis.clone() };
    let own_ranges = || fit::profile_ranges(&line.map, &p, line.score, &one);
    let (flyby, score, ranges) = match joint {
        Some((q, s, map, joint_cfg)) => (q, s, fit::profile_ranges(&map, &q, s, &joint_cfg)),
        // its lines now multiples of `f1`
        None => (Flyby { f0: f1, ..p }, line.score, own_ranges()),
    };
    // Any harmonic source (an engine on the road, a voice) has lines that
    // share its fundamental's shape, so the extra lines tell nothing about
    // what made them. A refit that keeps the primary's speed and distance
    // and narrows their ranges may clear the primary's ground verdict; one
    // that slides along the `v^2 / d` ridge to another geometry is not
    // confirmed by the primary, which keeps its verdict.
    let moved = refitted
        && ((flyby.v / p.v).ln().abs() > GROUND_REFIT_V_REL.ln_1p() || (flyby.d / p.d).ln().abs() > GROUND_REFIT_D_REL.ln_1p());
    let likely_ground_vehicle = likely_ground(&flyby, &ranges) || (moved && likely_ground(&p, &own_ranges()));
    if let Some(dbg) = dbg {
        eprintln!(
            "final tc {:.1} f0 {:.1} x {harmonics:?} v {:.1} d {:.0}{} (line {:.1} Hz v {:.1} d {:.0}, lines (f, weight, own fit) {lines:?} kept {keep:?}, grouped {others:?}, unconfirmed {loose:?}) {dbg}",
            flyby.tc,
            flyby.f0,
            flyby.v,
            flyby.d,
            if moved { " moved" } else { "" },
            p.f0,
            p.v,
            p.d
        );
    }
    let quality = if refitted {
        fit::transition_quality(&line.map, &Flyby { f0: primary_k * flyby.f0, ..flyby }, c)
    } else {
        line.quality
    };
    let mut other_lines: Vec<f64> =
        harmonics.iter().filter(|h| (**h - primary_k).abs() > 1e-9).map(|h| h * flyby.f0).collect();
    other_lines.sort_by(f64::total_cmp);
    let detection = Detection {
        flyby,
        ranges,
        harmonics,
        score,
        contrast: line.contrast,
        quality,
        likely_ground_vehicle,
        confident: !ranges.clipped.iter().any(|c| *c),
        other_lines,
        region: line.region,
        trace: None,
        origin: Origin::Detected,
    };
    Finished { line, detection }
}

/// Refit all lines of one fly-by together: they share `v`, `d` and `tc`, so
/// every extra line adds evidence, and the faint tails of several lines pin
/// the speed and distance that the steep middle of one line leaves open
/// (it fixes little more than `v^2 / d`). Lines are `harmonics` multiples of
/// `f1` (rest frequency under the primary's geometry); the primary is
/// multiple `primary_k`, the strongest line `strongest_k`. The fit must be
/// brighter on all lines, explain them as lines about as well as the
/// primary's geometry, and keep the primary a clear line near its CPA time
/// and rest frequency; `None` otherwise. Returns the fly-by (`f0`: the
/// fundamental), its score, the map and the settings it was fitted with.
fn joint_fit(
    audio: &Audio,
    line: &Line,
    f1: f64,
    harmonics: &[f64],
    primary_k: f64,
    strongest_k: f64,
    cfg: &DetectConfig,
    f_max: f64,
) -> Option<(Flyby, f64, ScoreMap, FitConfig)> {
    let c = cfg.c;
    let start = Flyby { f0: f1, ..line.flyby };
    let (lo, hi) = harmonics.iter().fold((f64::INFINITY, 0.0f64), |(a, b), h| (a.min(*h), b.max(*h)));
    // room for a faster pass than the primary's fit (its speed is often low)
    let v_room = (JOINT_V_RANGE * start.v).min(0.6 * c);
    // FFT window matched to the strongest line (the primary's window may be
    // far too short for a strong low line)
    let analysis = if (strongest_k - primary_k).abs() < 1e-9 {
        line.analysis.clone()
    } else {
        matched_analysis(&Flyby { f0: f1 * strongest_k, ..start }, audio.sample_rate, &FitConfig { c, ..FitConfig::default() })
    };
    let margin = copy_margin(&analysis, audio.sample_rate);
    let region = Region {
        t_start: line.region.t_start.min(start.tc - JOINT_MIN_HALF_S).max(0.0),
        t_end: line.region.t_end.max(start.tc + JOINT_MIN_HALF_S).min(audio.duration()),
        f_min: (f1 * lo * c / (c + v_room) * 0.95 - margin).max(1.0),
        // copies shifted by up to 20 bins at the lowest line, hi / lo times that at the highest
        f_max: (f1 * hi * c / (c - v_room) * 1.05 + hi / lo * margin).min(f_max),
    };
    let joint_cfg = FitConfig { harmonics: harmonics.to_vec(), ..analysis.clone() };
    let map = ScoreMap::new(fit::region_spectrogram(audio, &region, &joint_cfg));
    let start_score = map.score(&start, c, harmonics);
    // seed: the bright points along every line, in units of the fundamental
    let mut seed: Vec<RidgePoint> = harmonics
        .iter()
        .flat_map(|&h| {
            fit::guided_points(&map, &Flyby { f0: h * f1, ..start }, c)
                .into_iter()
                .filter(|q| q.weight >= EXPLAINED_MIN_Z)
                .map(move |q| RidgePoint { f: q.f / h, ..q })
        })
        .collect();
    seed.sort_by(|a, b| a.t.total_cmp(&b.t));
    // starts along the v^2/d ridge the primary's fit leaves open
    let mut best: Option<(Flyby, f64)> = None;
    for m in JOINT_V_STARTS {
        let guess = Flyby { v: (start.v * m).min(0.6 * c), d: start.d * m * m, ..start };
        if let Some((q, s)) = fit::fit_from(&map, &seed, guess, &joint_cfg)
            && best.is_none_or(|b| s > b.1)
        {
            best = Some((q, s));
        }
    }
    let debug = std::env::var_os("OVERHEAD_DEBUG").is_some();
    if debug && best.is_none() {
        eprintln!("joint tc {:.1} x {harmonics:?}: no fit (region {region:?}, start score {start_score:.3}, {} seed points)", start.tc, seed.len());
    }
    if let Some((q, s)) = best {
        // each line's contrast on the joint map: the fit must explain the
        // lines as lines about as well as the primary's geometry does (it
        // maximises their brightness, which may cost a little contrast)
        let one = FitConfig { harmonics: vec![1.0], ..analysis.clone() };
        let per_line = |g: &Flyby| -> Vec<f64> {
            harmonics.iter().map(|h| fit::contrast(&map, &Flyby { f0: h * g.f0, ..*g }, &one)).collect()
        };
        let mean = |x: &[f64]| x.iter().map(|v| v.max(0.0)).sum::<f64>() / x.len() as f64;
        let (lines_q, lines_start) = (per_line(&q), per_line(&start));
        let kept = fit::contrast(&line.map, &Flyby { f0: primary_k * q.f0, ..q }, &line.analysis);
        let ok = s >= start_score
            && mean(&lines_q) >= JOINT_KEEP_LINES * mean(&lines_start)
            && (q.tc - start.tc).abs() <= JOINT_TC_TOL
            && (q.f0 / f1 - 1.0).abs() <= JOINT_F0_REL
            && kept >= cfg.min_contrast.max(JOINT_KEEP_CONTRAST * line.contrast);
        if debug {
            let r = |x: &[f64]| x.iter().map(|v| (v * 10.0).round() / 10.0).collect::<Vec<_>>();
            eprintln!(
                "joint {q:?} score {s:.3} (start {start_score:.3}) primary contrast {kept:.1} (was {:.1}) lines {:?} (start {:?}){}",
                line.contrast,
                r(&lines_q),
                r(&lines_start),
                if ok { "" } else { " rejected" }
            );
        }
        if ok {
            return Some((q, s, map, joint_cfg));
        }
    }
    None
}

/// Fly-bys whose primary line turned out to be a line of another fly-by
/// once each was scanned for all its lines: one aircraft whose lines were
/// too far apart in rest frequency, or too differently mis-fitted, to be
/// grouped by their own fits, showing up twice. The geometry that explains
/// both primaries better stays (the stronger primary's when about equal).
fn merge_comb_duplicates(mut list: Vec<Finished>, cfg: &DetectConfig) -> Vec<Finished> {
    let c = cfg.c;
    // strongest first
    let mut order: Vec<usize> = (0..list.len()).collect();
    order.sort_by(|a, b| list[*b].line.contrast.total_cmp(&list[*a].line.contrast));
    let mut drop = vec![false; list.len()];
    let debug = std::env::var_os("OVERHEAD_DEBUG").is_some();
    // `line`'s best contrast under the shape of `det`, if it lies on one of `det`'s lines
    let shares_line = |line: &Line, det: &Detection| -> bool {
        let (x, f) = cross_contrast_at(line, &det.flyby, None, c);
        let r = f / det.flyby.f0;
        if debug {
            eprintln!("shares {:?} under {:?} {:?}: contrast {x:.1} at x{r:.3}", line.flyby, det.flyby, det.harmonics);
        }
        det.harmonics.iter().any(|h| (r / h - 1.0).abs() <= COMB_SAME_REL)
            && x >= cfg.min_contrast.max(SHARED_CONTRAST * line.contrast)
    };
    for ai in 0..order.len() {
        let a = order[ai];
        if drop[a] {
            continue;
        }
        for &b in &order[ai + 1..] {
            if drop[b] {
                continue;
            }
            let (da, db) = (&list[a].detection, &list[b].detection);
            if (da.harmonics.len() < 2 && db.harmonics.len() < 2) || (da.flyby.tc - db.flyby.tc).abs() > EXPLAINED_TC_TOL {
                continue;
            }
            let (la, lb) = (&list[a].line, &list[b].line);
            if !shares_line(lb, da) && !shares_line(la, db) {
                continue;
            }
            let total = |g: &Flyby| cross_contrast_at(la, g, None, c).0 + cross_contrast_at(lb, g, None, c).0;
            let (ta, tb) = (total(&da.flyby), total(&db.flyby));
            // the stronger primary's geometry stays unless clearly worse
            let b_better = tb > DUPLICATE_SWAP * ta;
            if debug {
                eprintln!(
                    "comb duplicate {:?} {:?} ({ta:.1}) and {:?} {:?} ({tb:.1}): keep the {}",
                    da.flyby,
                    da.harmonics,
                    db.flyby,
                    db.harmonics,
                    if b_better { "second" } else { "first" }
                );
            }
            if b_better {
                drop[a] = true;
                break;
            }
            drop[b] = true;
        }
    }
    let mut keep = drop.into_iter();
    list.retain(|_| !keep.next().unwrap());
    list
}

/// Slow and close, or measured so loosely that slow and close cannot be
/// ruled out: see `GROUND_V_MAX`, `GROUND_LOOSE_V`, `GROUND_LOOSE_D_SPAN`.
pub fn likely_ground(p: &Flyby, ranges: &Ranges) -> bool {
    let slow_close =
        |v: f64, d: f64| (v < GROUND_V_MAX && d < GROUND_D_MAX) || (v < GROUND_V_SLOW_MAX && d < GROUND_D_SLOW_MAX);
    let loose = ranges.v.1 > GROUND_LOOSE_V * ranges.v.0
        || (ranges.d.1 > GROUND_LOOSE_D * ranges.d.0 && ranges.d.1 - ranges.d.0 > GROUND_LOOSE_D_SPAN);
    // The band between the two speed limits spares a pass that was measured
    // to be slow at a few hundred metres; the low ends of a loose fit say
    // nothing that precise, so they keep the plain limits.
    let low_ends_slow_close = ranges.v.0 < GROUND_V_SLOW_MAX && ranges.d.0 < GROUND_D_MAX;
    slow_close(p.v, p.d) || (loose && low_ends_slow_close)
}

/// Whether one frequency is a 2nd–8th multiple of the other (within 3%).
/// Harmonics of a maneuvering aircraft fit to different `v`/`d` (the error
/// scales with the harmonic), so this plus a common CPA time groups them.
fn harmonic_pair(a: f64, b: f64) -> bool {
    let r = a.max(b) / a.min(b);
    let k = r.round();
    (2.0..=8.0).contains(&k) && (r - k).abs() < 0.03 * k
}

/// Harmonics of one maneuvering aircraft: CPA times agree within
/// `HARMONIC_TC_TOL`, and speed and distance roughly (two different aircraft
/// passing seconds apart rarely also match in both).
fn loosely_same_geometry(a: &Flyby, b: &Flyby) -> bool {
    (a.tc - b.tc).abs() <= HARMONIC_TC_TOL
        && (a.v - b.v).abs() <= 0.3 * a.v.max(b.v)
        && (a.d / b.d).ln().abs() <= 2.5_f64.ln()
}

/// Whether `line` belongs to the fly-by `geometry`, judged on the line's own
/// data points on the steep part of the pass. Fits of a weak or high harmonic
/// can land on quite different `v`/`d` (the parameters trade off against each
/// other, and a fit may join pieces of neighbouring harmonics) yet trace the
/// same data; another aircraft's curve has a different slope or CPA time and
/// misses the points. Returns the line's frequency under `geometry`.
fn explained_by(line: &Line, geometry: &Flyby, c: f64) -> Option<f64> {
    if (line.flyby.tc - geometry.tc).abs() > EXPLAINED_TC_TOL {
        return None;
    }
    let p = &line.flyby;
    let df = line.map.spec.df;
    let max_slope = p.slope_at_cpa(c).abs();
    let h = 0.5 * line.map.spec.dt;
    let steep = |t: f64| ((p.freq_at(t + h, c) - p.freq_at(t - h, c)) / (2.0 * h)).abs() >= 0.25 * max_slope;
    let points: Vec<RidgePoint> = fit::guided_points(&line.map, p, c)
        .into_iter()
        .filter(|q| q.weight >= EXPLAINED_MIN_Z && steep(q.t))
        .collect();
    let sides = (points.iter().filter(|q| q.t < p.tc).count(), points.iter().filter(|q| q.t >= p.tc).count());
    if sides.0 < EXPLAINED_MIN_SIDE_POINTS || sides.1 < EXPLAINED_MIN_SIDE_POINTS {
        return None;
    }
    // the geometry's curve per unit rest frequency
    let unit = |t: f64| geometry.freq_at(t, c) / geometry.f0;
    let debug = std::env::var_os("OVERHEAD_DEBUG").is_some();

    // (a) one line at any ratio (e.g. the second propeller of a twin), scaled
    // by the median ratio, about as close to the points as the line's fit
    let mut ratios: Vec<f64> = points.iter().map(|q| q.f / unit(q.t)).collect();
    ratios.sort_by(f64::total_cmp);
    let f0 = ratios[ratios.len() / 2];
    let ratio = f0 / geometry.f0;
    let mut own: Vec<f64> = points.iter().map(|q| (q.f - p.freq_at(q.t, c)) / df).collect();
    let mut scaled: Vec<f64> = points.iter().map(|q| (q.f - f0 * unit(q.t)) / df).collect();
    let own = fit::trimmed_rms(&mut own, 0.15);
    let scaled = fit::trimmed_rms(&mut scaled, 0.15);
    if (1.0 / MAX_HARMONIC..=MAX_HARMONIC).contains(&ratio) && scaled <= EXPLAINED_RMS_FACTOR * own + EXPLAINED_RMS_BINS {
        if debug {
            eprintln!("explain {p:?} by {geometry:?}: scaled rms {scaled:.2} vs own {own:.2} over {sides:?} points");
        }
        return Some(f0);
    }

    // (b) most of the points lie on integer multiples of the geometry's curve
    let mut counts = [0.0f64; MAX_HARMONIC as usize + 1];
    let mut total = 0.0;
    for q in &points {
        total += q.weight;
        let base = geometry.f0 * unit(q.t);
        let k = (q.f / base).round();
        if (1.0..=MAX_HARMONIC).contains(&k) && (q.f - k * base).abs() <= (EXPLAINED_RMS_BINS * 3.0 * df).max(0.006 * q.f) {
            counts[k as usize] += q.weight;
        }
    }
    let hit = counts.iter().sum::<f64>() / total;
    if debug {
        eprintln!("explain {p:?} by {geometry:?}: scaled rms {scaled:.2} vs own {own:.2}, on harmonics {hit:.2}, over {sides:?} points");
    }
    if hit < EXPLAINED_HARMONIC_FRACTION {
        return None;
    }
    let k = (1..counts.len()).max_by(|a, b| counts[*a].total_cmp(&counts[*b]))?;
    Some(k as f64 * geometry.f0)
}

/// Whether `line` stops being a two-sided fly-by once the columns where its
/// curve runs along a line of the fly-by `group` are left out. A weak or high
/// harmonic whose own track covered only one side of CPA is often fitted to
/// a wrong `v`/`d`: its curve follows the real harmonic (or the half-rate
/// engine line) on one side and crosses darkness or another source on the
/// other. Too few strong points for `explained_by`, but its evidence on one
/// side is entirely pixels the group already owns. A second aircraft crosses
/// the group's lines only briefly and keeps its contrast on both sides.
/// Returns the group line the curve ran along most, and the larger of the
/// two unclaimed side contrasts (the evidence the line has of its own).
fn claimed_by(line: &Line, group: &Flyby, others: &[f64], c: f64) -> Option<(f64, f64)> {
    if (line.flyby.tc - group.tc).abs() > EXPLAINED_TC_TOL {
        return None;
    }
    let p = &line.flyby;
    let df = line.map.spec.df;
    let unit = |t: f64| group.freq_at(t, c) / group.f0;
    let max_slope = p.slope_at_cpa(c).abs();
    let h = 0.5 * line.map.spec.dt;
    let steep = |t: f64| ((p.freq_at(t + h, c) - p.freq_at(t - h, c)) / (2.0 * h)).abs() >= 0.25 * max_slope;
    // Only the group's own lines and their integer and half multiples: a
    // freely scaled copy of the group's curve also fits the flat tail of an
    // unrelated pass.
    let bases: Vec<f64> = std::iter::once(group.f0).chain(others.iter().copied()).collect();
    // rest frequency of the group line the curve lies on at time t, if any
    let claimed = |t: f64| -> Option<f64> {
        let f = p.freq_at(t, c);
        let u = unit(t);
        let tol = (CLAIM_TOL_BINS * df).max(CLAIM_TOL_REL * f);
        bases.iter().find_map(|&b| {
            let k = (2.0 * f / (b * u)).round() / 2.0;
            ((0.5..=MAX_HARMONIC).contains(&k) && (f - k * b * u).abs() <= tol).then_some(k * b)
        })
    };
    let side = |after: bool| {
        fit::contrast_on(&line.map, p, &line.analysis, |t| (t >= p.tc) == after && steep(t) && claimed(t).is_none())
            .map_or(0.0, |s| s.contrast)
    };
    let free = (side(false), side(true));
    let debug = std::env::var_os("OVERHEAD_DEBUG").is_some();
    if debug {
        let spec = &line.map.spec;
        let mut n = [[0usize; 2]; 2];
        for col in 0..spec.cols {
            let t = spec.col_time(col);
            if steep(t) {
                n[usize::from(t >= p.tc)][usize::from(claimed(t).is_some())] += 1;
            }
        }
        eprintln!(
            "claim {p:?} by {group:?} (+{others:?}): unclaimed side contrast {:.1}/{:.1}, steep columns free/claimed {:?}",
            free.0, free.1, n
        );
    }
    if free.0.min(free.1) >= MIN_SIDE_CONTRAST {
        return None;
    }
    // the group line most of the steep part runs along
    let mut votes: Vec<(f64, usize)> = Vec::new();
    for col in 0..line.map.spec.cols {
        let t = line.map.spec.col_time(col);
        if let Some(f) = claimed(t).filter(|_| steep(t)) {
            match votes.iter_mut().find(|v| (v.0 - f).abs() < 1e-6 * f) {
                Some(v) => v.1 += 1,
                None => votes.push((f, 1)),
            }
        }
    }
    let best = votes.into_iter().max_by_key(|v| v.1)?;
    Some((best.0, free.0.max(free.1)))
}

/// Whether `a`'s geometry (v, d, tc) describes both lines better than `b`'s:
/// each geometry gets its own line's contrast plus the best contrast of the
/// other line's data under it. `b_under_a`: rest frequency of `b`'s line
/// under `a`'s geometry (and vice versa), scanned ±2%; `None` guesses it from
/// the frequency at CPA.
fn better_geometry(a: &Line, b: &Line, b_under_a: Option<f64>, a_under_b: Option<f64>, c: f64) -> bool {
    let total = |own: &Line, other: &Line, guess: Option<f64>| own.contrast + cross_contrast(other, &own.flyby, guess, c);
    let (ta, tb) = (total(a, b, b_under_a), total(b, a, a_under_b));
    if std::env::var_os("OVERHEAD_DEBUG").is_some() {
        eprintln!("geometry {:?} {ta:.1} vs {:?} {tb:.1}", a.flyby, b.flyby);
    }
    ta > tb
}

/// Highest contrast of `line`'s map under the curve shape of `geometry`.
fn cross_contrast(line: &Line, geometry: &Flyby, guess: Option<f64>, c: f64) -> f64 {
    cross_contrast_at(line, geometry, guess, c).0
}

/// `cross_contrast` and the rest frequency it is reached at.
fn cross_contrast_at(line: &Line, geometry: &Flyby, guess: Option<f64>, c: f64) -> (f64, f64) {
    let guess = guess.unwrap_or_else(|| line.flyby.f0 * geometry.f0 / geometry.freq_at(line.flyby.tc, c));
    let step = 0.25 * line.map.spec.df / guess;
    let n = (0.02 / step).ceil() as i64;
    (-n..=n)
        .map(|k| {
            let f = guess * (1.0 + k as f64 * step);
            let p = Flyby { f0: f, ..*geometry };
            (fit::contrast(&line.map, &p, &line.analysis), f)
        })
        .fold((f64::NEG_INFINITY, guess), |a, b| if b.0 > a.0 { b } else { a })
}

/// Lines of one aircraft share speed, CPA distance and CPA time. Distant,
/// slow passes have a long, flat transition and so a less certain `tc`.
fn same_flyby(a: &Flyby, b: &Flyby) -> bool {
    let tc_tol = (0.1 * a.d.max(b.d) / a.v.min(b.v)).max(3.0);
    (a.tc - b.tc).abs() <= tc_tol && (a.v - b.v).abs() <= 0.15 * a.v.max(b.v) && (a.d / b.d).ln().abs() <= 2.0_f64.ln()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{SynthFlyby, synthesize};

    /// A `Line` for curve `p` fitted on a matched map of `audio`.
    fn line_for(audio: &crate::audio::Audio, p: Flyby) -> Line {
        let base = FitConfig::default();
        let analysis = matched_analysis(&p, audio.sample_rate, &base);
        let region = Region { t_start: p.tc - 120.0, t_end: p.tc + 120.0, f_min: 40.0, f_max: 1000.0 };
        let map = ScoreMap::new(fit::region_spectrogram(audio, &region, &analysis));
        let quality = fit::transition_quality(&map, &p, DEFAULT_C);
        Line { flyby: p, score: 0.0, contrast: 0.0, quality, region, map, analysis }
    }

    #[test]
    fn misfitted_harmonic_is_claimed_but_second_aircraft_is_not() {
        // easy-1-000 of the benchmark: a turboprop with lines at 1-4 x 101.8 Hz,
        // and a fit of only the receding half of its 3rd harmonic that ran
        // off to v = 83 m/s, d = 1590 m on the approach side
        let g = Flyby { f0: 101.8, v: 100.5, d: 2064.0, tc: 300.0 };
        let misfit = Flyby { f0: 294.2, v: 83.0, d: 1590.0, tc: 303.2 };
        // a second aircraft crossing the 3rd harmonic 5 s later
        let other = Flyby { f0: 302.0, v: 140.0, d: 3000.0, tc: 305.0 };
        let audio = synthesize(
            8000.0,
            600.0,
            &[
                SynthFlyby { flyby: g, harmonics: vec![1.0, 0.6, 0.4, 0.3], amplitude: 0.1 },
                SynthFlyby { flyby: other, harmonics: vec![1.0], amplitude: 0.1 },
            ],
            0.3,
            &[],
            3,
        );
        let claimed = claimed_by(&line_for(&audio, misfit), &g, &[203.6, 305.4], DEFAULT_C);
        assert!(claimed.is_some_and(|(f, _)| (f - 305.4).abs() < 1.0), "{claimed:?}");
        let claimed = claimed_by(&line_for(&audio, other), &g, &[203.6, 305.4], DEFAULT_C);
        assert!(claimed.is_none(), "{claimed:?}");
    }

    fn ranges(v: (f64, f64), d: (f64, f64)) -> Ranges {
        Ranges { v, d, tc: (0.0, 0.0), clipped: [false; 3] }
    }

    #[test]
    fn ground_flag_rules() {
        let p = |v, d| Flyby { f0: 100.0, v, d, tc: 0.0 };
        // low aircraft (approach or climb-out near an airfield): not flagged
        // for distance alone, even when d spans a large factor
        assert!(!likely_ground(&p(70.0, 173.0), &ranges((66.0, 74.0), (150.0, 200.0))));
        assert!(!likely_ground(&p(67.3, 158.0), &ranges((59.0, 82.0), (67.0, 376.0))));
        assert!(!likely_ground(&p(73.3, 91.0), &ranges((63.0, 90.0), (34.0, 230.0))));
        // helicopters: tightly measured, or d loose by factor but not by much
        assert!(!likely_ground(&p(41.2, 966.0), &ranges((37.0, 46.0), (622.0, 1406.0))));
        assert!(!likely_ground(&p(46.5, 245.0), &ranges((33.0, 61.0), (77.0, 680.0))));
        // slow and close
        assert!(likely_ground(&p(30.0, 120.0), &ranges((28.0, 32.0), (100.0, 140.0))));
        assert!(likely_ground(&p(42.8, 104.0), &ranges((40.0, 46.0), (80.0, 130.0))));
        // slow aircraft a few hundred metres away: Berlin C152 (ADS-B 600 m
        // slant) and the Mechlin helicopter passing overhead
        assert!(!likely_ground(&p(38.6, 391.0), &ranges((35.0, 45.0), (268.0, 535.0))));
        assert!(!likely_ground(&p(39.3, 286.0), &ranges((33.0, 45.0), (129.0, 506.0))));
        // loose (v range over 2x) with low ends in the 36-45 m/s band, a few
        // hundred metres away: benchmark machinery
        assert!(likely_ground(&p(59.6, 1371.0), &ranges((37.0, 76.0), (326.0, 5000.0))));
        // loose mis-fits of benchmark clutter whose low ends are slow and close
        assert!(likely_ground(&p(56.3, 129.0), &ranges((19.0, 116.0), (32.0, 2058.0))));
        assert!(likely_ground(&p(30.3, 510.0), &ranges((24.0, 37.0), (170.0, 1009.0))));
        // loose but fast at the low ends: not flagged
        assert!(!likely_ground(&p(103.2, 3179.0), &ranges((59.0, 206.0), (199.0, 29210.0))));
    }

    #[test]
    fn low_tight_pass_is_not_flagged_as_ground() {
        // aircraft at 170 m CPA (approach or climb-out near an airfield)
        let truth = Flyby { f0: 120.0, v: 70.0, d: 170.0, tc: 200.0 };
        let audio = synthesize(
            8000.0,
            400.0,
            &[SynthFlyby { flyby: truth, harmonics: vec![1.0, 0.5], amplitude: 0.2 }],
            0.3,
            &[],
            5,
        );
        let found = detect(&audio, &DetectConfig::default());
        assert_eq!(found.len(), 1, "{found:#?}");
        let d = &found[0];
        assert!((d.flyby.tc - truth.tc).abs() < 1.0 && (d.flyby.d / truth.d - 1.0).abs() < 0.25, "{d:#?}");
        assert!(!d.likely_ground_vehicle, "{d:#?}");
    }

    #[test]
    fn true_geometry_beats_misfit_of_its_harmonic() {
        // easy-1-000 again: the mis-fit of the 3rd harmonic must lose a
        // primary swap against the real curve of that harmonic
        let g = Flyby { f0: 101.8, v: 100.5, d: 2064.0, tc: 300.0 };
        let audio = synthesize(
            8000.0,
            600.0,
            &[SynthFlyby { flyby: g, harmonics: vec![1.0, 0.6, 0.4, 0.3], amplitude: 0.1 }],
            0.3,
            &[],
            3,
        );
        let with_contrast = |p: Flyby| {
            let mut l = line_for(&audio, p);
            l.contrast = fit::contrast(&l.map, &p, &l.analysis);
            l
        };
        let real = with_contrast(Flyby { f0: 3.0 * g.f0, ..g });
        let misfit = with_contrast(Flyby { f0: 294.2, v: 83.0, d: 1590.0, tc: 303.2 });
        assert!(better_geometry(&real, &misfit, None, None, DEFAULT_C));
        assert!(!better_geometry(&misfit, &real, None, None, DEFAULT_C));
    }

    /// Lines `(multiple, amplitude)` of a fly-by with fundamental `truth.f0`.
    fn comb_audio(truth: Flyby, lines: &[(usize, f64)], hums: &[(f64, f64)], seed: u64) -> crate::audio::Audio {
        let mut amps = vec![0.0; lines.iter().map(|l| l.0).max().unwrap_or(1)];
        for &(k, a) in lines {
            amps[k - 1] = a;
        }
        synthesize(8000.0, 400.0, &[SynthFlyby { flyby: truth, harmonics: amps, amplitude: 0.05 }], 0.3, hums, seed)
    }

    #[test]
    fn fundamental_below_the_strongest_line() {
        // Vlissingen det 1: the 7th multiple of an unheard 87.5 Hz is the
        // strongest line, the 3rd and 4th are weaker
        let truth = Flyby { f0: 87.5, v: 55.0, d: 900.0, tc: 200.0 };
        let audio = comb_audio(truth, &[(3, 0.3), (4, 0.3), (7, 0.6)], &[], 21);
        let found = detect(&audio, &DetectConfig::default());
        assert_eq!(found.len(), 1, "{found:#?}");
        let d = &found[0];
        assert!((d.flyby.f0 / truth.f0 - 1.0).abs() < 0.01, "{d:#?}");
        assert_eq!(d.harmonics, vec![3.0, 4.0, 7.0], "{d:#?}");
        assert!((d.flyby.tc - truth.tc).abs() < 1.5 && (d.flyby.v / truth.v - 1.0).abs() < 0.1, "{d:#?}");
        assert!(d.confident && !d.likely_ground_vehicle, "{d:#?}");
    }

    #[test]
    fn full_comb_is_one_flyby_on_its_fundamental() {
        let truth = Flyby { f0: 77.8, v: 60.0, d: 1000.0, tc: 200.0 };
        let audio = comb_audio(truth, &[(1, 0.6), (2, 1.0), (3, 0.7), (4, 0.5), (5, 0.4)], &[], 22);
        let found = detect(&audio, &DetectConfig::default());
        assert_eq!(found.len(), 1, "{found:#?}");
        let d = &found[0];
        assert!((d.flyby.f0 / truth.f0 - 1.0).abs() < 0.01, "{d:#?}");
        assert_eq!(d.harmonics, vec![1.0, 2.0, 3.0, 4.0, 5.0], "{d:#?}");
        assert!((d.flyby.tc - truth.tc).abs() < 1.5 && (d.flyby.v / truth.v - 1.0).abs() < 0.1, "{d:#?}");
        assert!((d.flyby.d / truth.d - 1.0).abs() < 0.25, "{d:#?}");
    }

    /// A propeller comb and an independent turbine line (or a twin's other
    /// propeller): the line at no multiple of the comb's fundamental is kept
    /// at its measured ratio, not dropped.
    #[test]
    fn line_of_a_second_comb_is_kept() {
        let truth = Flyby { f0: 120.0, v: 60.0, d: 1000.0, tc: 200.0 };
        let turbine = 173.0;
        let audio = synthesize(
            8000.0,
            400.0,
            &[
                SynthFlyby { flyby: truth, harmonics: vec![1.0, 0.7], amplitude: 0.05 },
                SynthFlyby { flyby: Flyby { f0: turbine, ..truth }, harmonics: vec![0.6], amplitude: 0.05 },
            ],
            0.3,
            &[],
            25,
        );
        let found = detect(&audio, &DetectConfig::default());
        assert_eq!(found.len(), 1, "{found:#?}");
        let d = &found[0];
        let lines: Vec<f64> = d.harmonics.iter().map(|h| h * d.flyby.f0).collect();
        for f in [truth.f0, 2.0 * truth.f0, turbine] {
            assert!(lines.iter().any(|l| (l / f - 1.0).abs() < 0.005), "{f}: {lines:?} {d:#?}");
        }
        assert_eq!(d.harmonics.len(), 3, "{d:#?}");
    }

    #[test]
    fn single_line_and_hum_add_no_lines() {
        let truth = Flyby { f0: 112.0, v: 65.0, d: 900.0, tc: 200.0 };
        // one line alone
        let audio = comb_audio(truth, &[(1, 1.0)], &[], 23);
        let found = detect(&audio, &DetectConfig::default());
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].harmonics, vec![1.0], "{found:#?}");
        assert!((found[0].flyby.f0 - truth.f0).abs() < 1.0, "{found:#?}");
        // the same line among stationary mains hum and its harmonics: the hum
        // crosses the scanned curves but does not follow them
        let hums = [(50.0, 0.1), (100.0, 0.08), (150.0, 0.06), (200.0, 0.04)];
        let audio = comb_audio(truth, &[(1, 1.0)], &hums, 24);
        let found = detect(&audio, &DetectConfig::default());
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].harmonics, vec![1.0], "{found:#?}");
        assert!(found[0].other_lines.is_empty(), "{found:#?}");
        // and a scan along the fly-by's own curve finds only its line
        let comb = comb::scan_lines(&audio, &truth, (10.0, 1000.0), DEFAULT_C).expect("scan");
        let fs: Vec<f64> = comb.lines.iter().map(|l| l.f).collect();
        assert!(fs.len() == 1 && (fs[0] - truth.f0).abs() < 1.0, "{fs:?}");
    }

    #[test]
    fn finds_two_synthetic_flybys_and_ignores_hum() {
        let truth = [
            Flyby { f0: 112.0, v: 72.0, d: 1100.0, tc: 200.0 },
            Flyby { f0: 131.0, v: 100.0, d: 2400.0, tc: 560.0 },
        ];
        let audio = synthesize(
            8000.0,
            720.0,
            &[
                SynthFlyby { flyby: truth[0], harmonics: vec![1.0, 0.5, 0.3], amplitude: 0.03 },
                SynthFlyby { flyby: truth[1], harmonics: vec![1.0, 0.6, 0.25], amplitude: 0.05 },
            ],
            0.3,
            &[(50.0, 0.05), (100.0, 0.03), (150.0, 0.02)],
            11,
        );
        let (found, stats) = detect_with_stats(&audio, &DetectConfig::default());
        eprintln!("{stats:?}");
        for d in &found {
            eprintln!("{:?} contrast {:.1} others {:?}", d.flyby, d.contrast, d.other_lines);
        }
        assert_eq!(found.len(), 2, "{found:#?}");
        for (d, t) in found.iter().zip(&truth) {
            assert!((d.flyby.tc - t.tc).abs() < 1.5, "tc {} vs {}", d.flyby.tc, t.tc);
            assert!((d.flyby.v - t.v).abs() < 0.08 * t.v, "v {} vs {}", d.flyby.v, t.v);
            assert!((d.flyby.d / t.d - 1.0).abs() < 0.25, "d {} vs {}", d.flyby.d, t.d);
            // primary line may be any harmonic; it must be an integer multiple of f0
            let k = (d.flyby.f0 / t.f0).round();
            assert!(k >= 1.0 && (d.flyby.f0 - k * t.f0).abs() < 1.0, "f0 {}", d.flyby.f0);
        }
    }
}
