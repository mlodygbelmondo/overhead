//! Explaining a finished search, as the reference video does after its
//! search: per-fly-by and combined score maps (its QGIS heat maps), the
//! scores of every candidate per fly-by grouped by flight assignment (its
//! scatter plot of event 1 against event 2), the modelled Doppler curve of
//! any candidate on the spectrogram, the flights a place would have heard
//! but that are not in the recording (its silence filter), and links to
//! look at the place (its Street View check).
//!
//! [`crate::locate::locate_session`] returns a [`Session`] next to the
//! normal output; it keeps the detections, their spectrogram
//! neighbourhoods, the candidate flights, the terrain and every refined
//! hypothesis, so each question below is answered without searching again.
//! Nothing here changes the search's result.

use std::collections::HashMap;

use anyhow::{Result, bail};
use overhead_core::detect::Detection;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::dem::Dem;
use crate::flight::Flight;
use crate::geo::{self, BBox, M_PER_DEG_LAT, ecef, enu_axes};
use crate::links::{self, MapLinks};
use crate::matcher::{self, Assign, Candidate, CoarseCell, Cuts, MIN_CURVE, MatchConfig, Refined, Search};
use crate::score::Observed;
use crate::types::AircraftClass;

/// What a finished search keeps for explaining its result (see the module docs).
pub struct Session {
    pub dets: Vec<Detection>,
    pub observed: Vec<Observed>,
    pub flights: Vec<Flight>,
    pub dem: Dem,
    pub duration: f64,
    pub start_windows: Vec<(f64, f64)>,
    pub region: BBox,
    pub cfg: MatchConfig,
    pub cuts: Cuts,
    /// The reported candidates (as in the output).
    pub candidates: Vec<Candidate>,
    /// Every refined hypothesis, best first, and which one each candidate is.
    pub refined: Vec<Refined>,
    pub candidate_refined: Vec<usize>,
    /// The matcher's coarse per-cell scores, on cells of `coarse_cell_m`.
    pub coarse: Vec<CoarseCell>,
    pub coarse_cell_m: f64,
    /// The recording's background over time (silence), when there was audio.
    pub background: Option<crate::silence::Background>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("detections", &self.dets.len())
            .field("flights", &self.flights.len())
            .field("candidates", &self.candidates.len())
            .field("refined", &self.refined.len())
            .field("coarse_cells", &self.coarse.len())
            .finish()
    }
}

/// Coarse score map cell size for a search box: 1/150 of its larger side, 200 m–5 km.
pub fn coarse_cell_for(b: &BBox) -> f64 {
    let lat_mid = 0.5 * (b.lat_min + b.lat_max);
    let ns = (b.lat_max - b.lat_min) * M_PER_DEG_LAT;
    let ew = (b.lon_max - b.lon_min) * M_PER_DEG_LAT * lat_mid.to_radians().cos().abs();
    (ns.max(ew) / 150.0).clamp(200.0, 5000.0)
}

/// An aircraft as shown to people.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FlightTag {
    pub hex: String,
    pub reg: Option<String>,
    pub type_code: Option<String>,
    pub class: AircraftClass,
    pub callsign: Option<String>,
}

impl FlightTag {
    /// "PH-ABC C172" (registration or hex, then type).
    pub fn label(&self) -> String {
        let id = self.reg.clone().unwrap_or_else(|| self.hex.clone());
        match &self.type_code {
            Some(t) => format!("{id} {t}"),
            None => id,
        }
    }
}

/// A hypothesis to explain: a place, the recording start (per part of the
/// recording with cuts) and which flight pass explains which detection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hypothesis {
    pub lat: f64,
    pub lon: f64,
    /// Start (unix s) of each part of the recording (one without cuts);
    /// `None` for a part without a matched detection.
    pub starts: Vec<Option<f64>>,
    /// Ground speed of sound (m/s).
    pub c0: f64,
    pub flights: Vec<FlightChoice>,
}

/// One detection explained by one flight.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlightChoice {
    pub detection: usize,
    /// ICAO hex or registration.
    pub flight: String,
    /// Emission time of the closest approach (unix s); `None` = the pass
    /// heard nearest the detection's `tc` at this place and start.
    #[serde(default)]
    pub t_cpa_unix: Option<f64>,
}

// ---------------------------------------------------------------- score maps

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScoreMapOptions {
    /// Candidate (1-based rank) whose flights the fine maps are drawn for.
    pub candidate: usize,
    /// Fine maps: half width (m) and spacing (m) of the grid around the candidate.
    pub fine_radius_m: f64,
    pub fine_step_m: f64,
    /// Curve sampling stride for the fine maps (1 = every spectrogram column).
    pub stride: usize,
}

impl Default for ScoreMapOptions {
    fn default() -> Self {
        Self { candidate: 1, fine_radius_m: 2500.0, fine_step_m: 100.0, stride: 2 }
    }
}

/// Per-fly-by and combined score maps.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScoreMaps {
    /// Over the whole search: the matcher's own coarse cells.
    pub coarse: CoarseMap,
    /// Around one candidate with its flights: the full Doppler-curve score.
    pub fine: Option<FineMap>,
}

/// The matcher's coarse stage on a grid: in every cell, the best hypothesis
/// any of its 200 m cells produced. Scores are the coarse match quality
/// (speed, slant range and timing agreement, 0..1 per fly-by, times the
/// fly-by's weight; the combined one minus 0.75 per silence violation).
/// Cells where no flight could explain any fly-by are absent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoarseMap {
    pub cell_m: f64,
    pub cells: Vec<CoarseMapCell>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoarseMapCell {
    pub lat: f64,
    pub lon: f64,
    pub combined: f64,
    /// Fly-bys matched by the best combined hypothesis, and its flights (per matched detection).
    pub matched: usize,
    pub flights: Vec<(usize, String)>,
    /// Per detection: best coarse score of the cell and the flight giving it.
    pub per_detection: Vec<Option<(f64, String)>>,
}

/// Full curve scores on a regular grid around a candidate, for its flights.
/// `values` are row-major from the south-west corner (`rows` × `cols`),
/// NaN where the flight's pass is not known there (written as `null` in JSON).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FineMap {
    pub candidate: usize,
    pub lat0: f64,
    pub lon0: f64,
    pub dlat: f64,
    pub dlon: f64,
    pub rows: usize,
    pub cols: usize,
    pub step_m: f64,
    /// One layer per matched detection: its curve score alone (≈1 = as
    /// good as the stage-1 fit), with its own start at each point (the
    /// start that puts the flight's heard closest approach at the
    /// detection's `tc`), like scoring one event in the video.
    pub per_detection: Vec<FineLayer>,
    /// All matched detections together with one start (locked timing; the
    /// start their passes imply at each point): the matcher's objective.
    pub combined: FineLayer,
    pub seconds: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FineLayer {
    /// Detection index (`None` for the combined layer).
    pub detection: Option<usize>,
    pub flights: Vec<String>,
    pub values: Vec<f32>,
    pub max: f64,
    /// Where the maximum is.
    pub best_lat: f64,
    pub best_lon: f64,
}

// ---------------------------------------------------------------- per-event scores

/// Every refined hypothesis scored per detection: the video's scatter plot
/// (event 1 score against event 2 score), and the best point per flight assignment.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventScores {
    pub detections: Vec<EventDetection>,
    pub points: Vec<EventPoint>,
    /// The best point of every distinct assignment of flights to detections
    /// (only matches above the threshold define the assignment).
    pub best_per_assignment: Vec<EventPoint>,
    /// A per-detection score must reach this to count as a match.
    pub min_score: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventDetection {
    pub index: usize,
    pub tc: f64,
    pub f0: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventPoint {
    pub lat: f64,
    pub lon: f64,
    /// Ranking score (curve score minus silence penalties).
    pub total: f64,
    pub starts: Vec<Option<f64>>,
    pub c0: f64,
    /// Per detection (by index): the assigned flight and its curve score.
    pub per_detection: Vec<Option<EventScore>>,
    /// "det 0: PH-ABC C172 · det 2: D-EFGH" (matched flights only).
    pub assignment: String,
    /// Rank of the reported candidate this point is, if any.
    pub candidate_rank: Option<usize>,
    pub silence_violations: usize,
    /// How many refined hypotheses converged on this point (same place
    /// within 1 m, same assignment and total): they are listed once.
    pub hypotheses: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventScore {
    pub flight: FlightTag,
    pub score: f64,
    pub f0_scale: f64,
}

// ---------------------------------------------------------------- curves

/// The modelled Doppler curve of one flight for one detection: what that
/// flight would have sounded like at the place and start.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelCurve {
    pub detection: usize,
    pub flight: Option<FlightTag>,
    /// Curve score against the recording (≈1 = as good as the stage-1 fit,
    /// 0 = nothing), timing included.
    pub score: f64,
    /// Rest frequency used: the detection's `f0` times the best sweep factor.
    pub f0: f64,
    pub f0_scale: f64,
    /// Recording time of the modelled heard closest approach nearest the detection.
    pub heard_cpa_rec: Option<f64>,
    /// One polyline per line of the detection (`harmonic` × `f0`), each in
    /// pieces where the track is known: `[t_rec s, f Hz]` every 0.25 s.
    pub lines: Vec<CurveLine>,
    /// Why there is no curve, when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurveLine {
    pub harmonic: f64,
    pub pieces: Vec<Vec<[f64; 2]>>,
}

// ---------------------------------------------------------------- silence

/// What a place would have heard: every pass of a candidate flight within
/// `radius_m` while the recording ran, and whether something in the
/// recording explains it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Silence {
    pub radius_m: f64,
    /// Slant range within which the matcher counts unheard passes against
    /// the place (the soft silence model's range, or the flat silence distance).
    pub quiet_d_m: f64,
    pub passes: Vec<ExpectedPass>,
    /// Distinct flights that count against the place.
    pub violations: usize,
    /// What they cost (score units; each flight its most expensive pass).
    #[serde(default)]
    pub penalty: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExpectedPass {
    pub flight: FlightTag,
    pub part: usize,
    pub heard_rec: f64,
    pub heard_utc: String,
    pub cpa_utc: String,
    pub slant_m: f64,
    /// "matched" (the hypothesis assigns it to `detection`), "near_detection"
    /// (a detection is heard within 30 s), "silent" (nothing in the recording
    /// explains it) or "edge" (in the first or last 45 s of the recording or
    /// of a part, where a fly-by can be cut off).
    pub status: String,
    pub detection: Option<usize>,
    /// Silent and costing something: a violation.
    pub counts_against: bool,
    /// Probability that stage 1 detects such a pass (soft silence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_detect: Option<f64>,
    /// What it costs the place when silent (score units).
    #[serde(default)]
    pub cost: f64,
}

// ---------------------------------------------------------------- bundle

/// Everything above for a search (`locate --explain`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Explain {
    pub score_maps: ScoreMaps,
    pub event_scores: EventScores,
    pub candidates: Vec<CandidateExplain>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidateExplain {
    pub rank: usize,
    pub hypothesis: Hypothesis,
    pub curves: Vec<ModelCurve>,
    pub silence: Silence,
    pub links: MapLinks,
}

impl Session {
    fn search(&self) -> Search<'_> {
        Search {
            dets: &self.dets,
            observed: &self.observed,
            flights: &self.flights,
            dem: &self.dem,
            duration: self.duration,
            start_windows: self.start_windows.clone(),
            region: self.region,
            cfg: self.cfg.clone(),
            truth: None,
            cuts: self.cuts.clone(),
            collect: Default::default(),
            background: self.background.as_ref(),
        }
    }

    pub fn tag(&self, fi: u32, t: f64) -> FlightTag {
        let f = &self.flights[fi as usize];
        FlightTag {
            hex: f.hex.clone(),
            reg: f.reg.clone(),
            type_code: f.type_code.clone(),
            class: f.class,
            callsign: f.callsign_at(t).map(str::to_string),
        }
    }

    fn flight_index(&self, id: &str) -> Option<u32> {
        let id = id.trim();
        self.flights
            .iter()
            .position(|f| f.hex.eq_ignore_ascii_case(id) || f.reg.as_deref().is_some_and(|r| r.eq_ignore_ascii_case(id)))
            .map(|i| i as u32)
    }

    fn n_parts(&self) -> usize {
        self.cuts.at.len() + 1
    }

    /// The refined hypothesis of candidate `rank` (1-based).
    fn candidate_hyp(&self, rank: usize) -> Option<&Refined> {
        let i = *self.candidate_refined.get(rank.checked_sub(1)?)?;
        self.refined.get(i)
    }

    /// Candidate `rank` (1-based) as a [`Hypothesis`] (only its matched flights).
    pub fn candidate_hypothesis(&self, rank: usize) -> Option<Hypothesis> {
        let r = self.candidate_hyp(rank)?;
        let c = self.candidates.get(rank - 1)?;
        let flights = r
            .assign
            .iter()
            .filter(|a| c.flights.iter().any(|m| m.detection == a.det && m.hex == self.flights[a.flight as usize].hex))
            .map(|a| FlightChoice { detection: a.det, flight: self.flights[a.flight as usize].hex.clone(), t_cpa_unix: Some(a.t_cpa) })
            .collect();
        Some(Hypothesis {
            lat: r.lat,
            lon: r.lon,
            starts: r.starts.iter().map(|s| s.is_finite().then_some(*s)).collect(),
            c0: r.c0,
            flights,
        })
    }

    fn starts_of(&self, h: &Hypothesis) -> Vec<f64> {
        let mut s: Vec<f64> = h.starts.iter().map(|x| x.unwrap_or(f64::NAN)).collect();
        s.resize(self.n_parts(), f64::NAN);
        s
    }

    /// Resolve a hypothesis' flights into the matcher's assignments (each
    /// pass found where needed); unknown flights or parts without a start
    /// give an error message instead.
    fn resolve(&self, h: &Hypothesis) -> Vec<(usize, Result<Assign, String>)> {
        let search = self.search();
        let starts = self.starts_of(h);
        h.flights
            .iter()
            .map(|fc| {
                let det = fc.detection;
                if det >= self.dets.len() {
                    return (det, Err(format!("no detection {det}")));
                }
                let Some(fi) = self.flight_index(&fc.flight) else {
                    return (det, Err(format!("{} is not among the {} candidate flights", fc.flight, self.flights.len())));
                };
                if let Some(t) = fc.t_cpa_unix {
                    return (det, Ok(Assign { det, flight: fi, t_cpa: t }));
                }
                let s = starts[self.cuts.part_of(self.dets[det].flyby.tc)];
                if !s.is_finite() {
                    return (det, Err("no start for this part of the recording".into()));
                }
                let tc = self.dets[det].flyby.tc;
                let best = search
                    .approaches_at(h.lat, h.lon, s, h.c0, self.cfg.d_cap)
                    .into_iter()
                    .filter(|a| a.0 == fi)
                    .min_by(|a, b| (a.3 - tc).abs().total_cmp(&(b.3 - tc).abs()));
                match best {
                    Some((_, t_cpa, _, _)) => (det, Ok(Assign { det, flight: fi, t_cpa })),
                    None => (det, Err(format!("{} does not pass within {:.0} km of the place during the recording", fc.flight, self.cfg.d_cap / 1000.0))),
                }
            })
            .collect()
    }

    // ------------------------------------------------------------ score maps

    /// Coarse map over the search and fine maps around one candidate.
    pub fn score_maps(&self, opt: &ScoreMapOptions) -> ScoreMaps {
        ScoreMaps { coarse: self.coarse_map(), fine: self.fine_map(opt) }
    }

    /// The matcher's coarse cells as a map (see [`CoarseMap`]).
    pub fn coarse_map(&self) -> CoarseMap {
        let label = |a: &Assign| self.tag(a.flight, a.t_cpa).label();
        let cells = self
            .coarse
            .iter()
            .map(|c| CoarseMapCell {
                lat: c.lat,
                lon: c.lon,
                combined: c.combined,
                matched: c.assign.len(),
                flights: c.assign.iter().map(|a| (a.det, label(a))).collect(),
                per_detection: c.per_det.iter().map(|p| p.as_ref().map(|(q, a, _)| (*q, label(a)))).collect(),
            })
            .collect();
        CoarseMap { cell_m: self.coarse_cell_m, cells }
    }

    /// Fine per-detection and combined curve-score maps around candidate
    /// `opt.candidate` with its matched flights (`None` without that candidate).
    pub fn fine_map(&self, opt: &ScoreMapOptions) -> Option<FineMap> {
        let h = self.candidate_hypothesis(opt.candidate)?;
        self.fine_map_for(&h, opt).map(|mut m| {
            m.candidate = opt.candidate;
            m
        })
    }

    /// [`Session::fine_map`] for any hypothesis.
    pub fn fine_map_for(&self, h: &Hypothesis, opt: &ScoreMapOptions) -> Option<FineMap> {
        let started = std::time::Instant::now();
        let assign: Vec<Assign> = self.resolve(h).into_iter().filter_map(|(_, a)| a.ok()).collect();
        if assign.is_empty() {
            return None;
        }
        let search = self.search();
        let step = opt.fine_step_m.max(10.0);
        let n = (opt.fine_radius_m / step).round().max(1.0) as i64;
        let side = (2 * n + 1) as usize;
        let m_lon = M_PER_DEG_LAT * h.lat.to_radians().cos();
        let (dlat, dlon) = (step / M_PER_DEG_LAT, step / m_lon);
        let (lat0, lon0) = (h.lat - n as f64 * dlat, h.lon - n as f64 * dlon);
        let stride = opt.stride.max(1);
        let c0 = h.c0;
        let k = assign.len();
        // per point: k single-detection scores and the combined one
        let vals: Vec<Vec<f32>> = (0..side * side)
            .into_par_iter()
            .map(|idx| {
                let (i, j) = (idx / side, idx % side);
                let (la, lo) = (lat0 + i as f64 * dlat, lon0 + j as f64 * dlon);
                let mut v = Vec::with_capacity(k + 1);
                for a in &assign {
                    let one = std::slice::from_ref(a);
                    let r = match search.implied_starts(la, lo, c0, one) {
                        Some(s) => search.curve_scores(la, lo, &s, c0, one, stride).1[0].0,
                        None => f64::NAN,
                    };
                    v.push(r as f32);
                }
                let comb = match search.implied_starts(la, lo, c0, &assign) {
                    Some(s) => search.curve_scores_plain(la, lo, &s, c0, &assign, stride),
                    None => f64::NAN,
                };
                v.push(comb as f32);
                v
            })
            .collect();
        let layer = |li: usize, detection: Option<usize>, flights: Vec<String>| -> FineLayer {
            let values: Vec<f32> = vals.iter().map(|v| v[li]).collect();
            let (mut best, mut bi) = (f64::NEG_INFINITY, 0usize);
            for (i, &x) in values.iter().enumerate() {
                if x.is_finite() && (x as f64) > best {
                    (best, bi) = (x as f64, i);
                }
            }
            let (r, c) = (bi / side, bi % side);
            FineLayer { detection, flights, values, max: best, best_lat: lat0 + r as f64 * dlat, best_lon: lon0 + c as f64 * dlon }
        };
        let per_detection =
            assign.iter().enumerate().map(|(li, a)| layer(li, Some(a.det), vec![self.tag(a.flight, a.t_cpa).label()])).collect();
        let combined = layer(k, None, assign.iter().map(|a| self.tag(a.flight, a.t_cpa).label()).collect());
        Some(FineMap {
            candidate: 0,
            lat0,
            lon0,
            dlat,
            dlon,
            rows: side,
            cols: side,
            step_m: step,
            per_detection,
            combined,
            seconds: started.elapsed().as_secs_f64(),
        })
    }

    // ------------------------------------------------------------ per-event scores

    /// Per-detection scores of every refined hypothesis (see [`EventScores`]).
    pub fn event_scores(&self) -> EventScores {
        let mut points: Vec<EventPoint> = Vec::new();
        for (ri, r) in self.refined.iter().enumerate() {
            if r.parts.len() != r.assign.len() {
                continue;
            }
            let mut per: Vec<Option<EventScore>> = vec![None; self.dets.len()];
            let mut keys: Vec<String> = Vec::new();
            for (a, &(score, m)) in r.assign.iter().zip(&r.parts) {
                let tag = self.tag(a.flight, a.t_cpa);
                if score >= MIN_CURVE {
                    keys.push(format!("det {}: {}", a.det, tag.label()));
                }
                if a.det < per.len() {
                    per[a.det] = Some(EventScore { flight: tag, score, f0_scale: m });
                }
            }
            let candidate_rank = self.candidate_refined.iter().position(|&c| c == ri).map(|p| p + 1);
            let assignment = keys.join(" · ");
            // several refinements often converge on one point: list it once
            if let Some(p) = points.iter_mut().find(|p| {
                p.assignment == assignment && (p.total - r.total).abs() < 1e-6 && crate::geo::ground_distance(p.lat, p.lon, r.lat, r.lon) < 1.0
            }) {
                p.hypotheses += 1;
                p.candidate_rank = match (p.candidate_rank, candidate_rank) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                continue;
            }
            points.push(EventPoint {
                lat: r.lat,
                lon: r.lon,
                total: r.total,
                starts: r.starts.iter().map(|s| s.is_finite().then_some(*s)).collect(),
                c0: r.c0,
                per_detection: per,
                assignment,
                candidate_rank,
                silence_violations: r.violations.len(),
                hypotheses: 1,
            });
        }
        // best point per assignment (points are already ranked best first)
        let mut seen: HashMap<String, ()> = HashMap::new();
        let best_per_assignment: Vec<EventPoint> =
            points.iter().filter(|p| !p.assignment.is_empty() && seen.insert(p.assignment.clone(), ()).is_none()).cloned().collect();
        EventScores {
            detections: self.dets.iter().enumerate().map(|(i, d)| EventDetection { index: i, tc: d.flyby.tc, f0: d.flyby.f0 }).collect(),
            points,
            best_per_assignment,
            min_score: MIN_CURVE,
        }
    }

    // ------------------------------------------------------------ curves

    /// The modelled curves of a hypothesis, one per flight choice.
    pub fn model_curves(&self, h: &Hypothesis) -> Vec<ModelCurve> {
        let search = self.search();
        let starts = self.starts_of(h);
        let gh = self.dem.height(h.lat, h.lon);
        let o = ecef(h.lat, h.lon, gh + self.cfg.mic_height);
        let (_, _, up) = enu_axes(h.lat, h.lon);
        let sound = search.sound(h.c0);
        self.resolve(h)
            .into_iter()
            .map(|(det, a)| {
                let d = &self.dets[det.min(self.dets.len().saturating_sub(1))];
                let empty = |note: String| ModelCurve {
                    detection: det,
                    flight: None,
                    score: 0.0,
                    f0: d.flyby.f0,
                    f0_scale: 1.0,
                    heard_cpa_rec: None,
                    lines: Vec::new(),
                    note: Some(note),
                };
                let a = match a {
                    Ok(a) => a,
                    Err(e) => return empty(e),
                };
                let Some(seg) = self.flights[a.flight as usize].segment_at(a.t_cpa) else {
                    return empty("the flight has no track at that time".into());
                };
                let part = self.cuts.part_of(d.flyby.tc);
                let s = starts[part];
                let (r, m, cross) = search.curve_scores(h.lat, h.lon, &starts, h.c0, std::slice::from_ref(&a), 1).1[0];
                // the detection's neighbourhood (as scored), inside its part of the recording
                let (p0, p1) = search.part_span(part);
                let t0 = (d.region.t_start - 30.0).max(p0).max(0.0);
                let t1 = (d.region.t_end + 30.0).min(p1);
                let dt = 0.25;
                let n = ((t1 - t0) / dt).ceil().max(1.0) as usize;
                let times: Vec<f64> = (0..=n).map(|i| (t0 + i as f64 * dt).min(t1)).collect();
                let (k, _) = Search::heard_factors(seg, o, up, &sound, s, &times);
                let f0 = d.flyby.f0 * m;
                let harmonics: Vec<f64> = if d.harmonics.is_empty() { vec![1.0] } else { d.harmonics.clone() };
                let lines = harmonics
                    .iter()
                    .map(|&hm| {
                        let mut pieces: Vec<Vec<[f64; 2]>> = Vec::new();
                        let mut cur: Vec<[f64; 2]> = Vec::new();
                        for (t, kk) in times.iter().zip(&k) {
                            if kk.is_finite() {
                                cur.push([*t, f0 * hm * kk]);
                            } else if !cur.is_empty() {
                                pieces.push(std::mem::take(&mut cur));
                            }
                        }
                        if !cur.is_empty() {
                            pieces.push(cur);
                        }
                        CurveLine { harmonic: hm, pieces }
                    })
                    .collect();
                ModelCurve {
                    detection: det,
                    flight: Some(self.tag(a.flight, a.t_cpa)),
                    score: r,
                    f0,
                    f0_scale: m,
                    heard_cpa_rec: cross,
                    lines,
                    note: None,
                }
            })
            .collect()
    }

    /// The modelled curves of candidate `rank` (1-based) for its matched flights.
    pub fn candidate_curves(&self, rank: usize) -> Vec<ModelCurve> {
        self.candidate_hypothesis(rank).map(|h| self.model_curves(&h)).unwrap_or_default()
    }

    // ------------------------------------------------------------ silence

    /// Slant range within which unheard passes count against a place (m).
    pub fn quiet_d(&self) -> f64 {
        self.search().silence_radius(matcher::quiet_distance(&self.dets, &self.cfg))
    }

    /// What the place of a hypothesis would have heard (see [`Silence`]);
    /// `radius_m` defaults to the silence distance.
    pub fn silence(&self, h: &Hypothesis, radius_m: Option<f64>) -> Silence {
        let search = self.search();
        let quiet_d = self.quiet_d();
        let radius = radius_m.unwrap_or(quiet_d).max(quiet_d);
        let starts = self.starts_of(h);
        let assign: Vec<Assign> = self.resolve(h).into_iter().filter_map(|(_, a)| a.ok()).collect();
        let mut passes: Vec<ExpectedPass> = search
            .loud_passes_at(h.lat, h.lon, &starts, &assign, radius)
            .into_iter()
            .map(|p| {
                let (p_detect, cost) = search.loud_pass_cost(&p, &assign);
                let (status, detection) = match (p.assigned, p.near_det, p.inside) {
                    (Some(d), _, _) => ("matched", Some(d)),
                    (None, Some(d), _) => ("near_detection", Some(d)),
                    (None, None, false) => ("edge", None),
                    (None, None, true) => ("silent", None),
                };
                let s = starts[p.part];
                ExpectedPass {
                    flight: self.tag(p.flight, p.t_cpa),
                    part: p.part,
                    heard_rec: p.heard_rec,
                    heard_utc: matcher::utc(s + p.heard_rec),
                    cpa_utc: matcher::utc(p.t_cpa),
                    slant_m: p.slant_m,
                    status: status.into(),
                    detection,
                    counts_against: status == "silent" && p.slant_m < quiet_d && cost > 0.0,
                    p_detect,
                    cost: if status == "silent" && p.slant_m < quiet_d { cost } else { 0.0 },
                }
            })
            .collect();
        passes.sort_by(|a, b| a.heard_rec.total_cmp(&b.heard_rec));
        passes.dedup_by(|a, b| a.flight.hex == b.flight.hex && (a.heard_rec - b.heard_rec).abs() < 60.0 && a.status == b.status);
        let mut bad: Vec<(&str, f64)> = Vec::new();
        for p in passes.iter().filter(|p| p.counts_against) {
            match bad.iter_mut().find(|b| b.0 == p.flight.hex) {
                Some(b) => b.1 = b.1.max(p.cost),
                None => bad.push((&p.flight.hex, p.cost)),
            }
        }
        let penalty = bad.iter().map(|b| b.1).sum();
        Silence { radius_m: radius, quiet_d_m: quiet_d, violations: bad.len(), penalty, passes }
    }

    /// [`Session::silence`] for candidate `rank` (1-based).
    pub fn candidate_silence(&self, rank: usize, radius_m: Option<f64>) -> Option<Silence> {
        self.candidate_hypothesis(rank).map(|h| self.silence(&h, radius_m))
    }

    // ------------------------------------------------------------ bundle

    /// Score maps for candidate 1, per-event scores, and curves, silence and
    /// links for the best `top` candidates.
    pub fn explain(&self, opt: &ScoreMapOptions, top: usize) -> Explain {
        let candidates = (1..=self.candidates.len().min(top))
            .filter_map(|rank| {
                let h = self.candidate_hypothesis(rank)?;
                Some(CandidateExplain {
                    rank,
                    curves: self.model_curves(&h),
                    silence: self.silence(&h, Some(if self.cfg.soft_silence() { self.quiet_d() } else { 2.0 * self.quiet_d() })),
                    links: links::for_place(h.lat, h.lon),
                    hypothesis: h,
                })
            })
            .collect();
        Explain { score_maps: self.score_maps(opt), event_scores: self.event_scores(), candidates }
    }

    /// Check a hypothesis given by hand (e.g. from the app): its curve
    /// scores, silence and links; errors when no flight could be resolved.
    pub fn check(&self, h: &Hypothesis) -> Result<CandidateExplain> {
        let curves = self.model_curves(h);
        if curves.iter().all(|c| c.flight.is_none()) {
            bail!("none of the flights could be placed: {}", curves.iter().filter_map(|c| c.note.clone()).collect::<Vec<_>>().join("; "));
        }
        Ok(CandidateExplain { rank: 0, curves, silence: self.silence(h, None), links: links::for_place(h.lat, h.lon), hypothesis: h.clone() })
    }
}

/// Distance (m) between two places, for callers that compare hypotheses.
pub fn distance_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    geo::ground_distance(a.0, a.1, b.0, b.1)
}
