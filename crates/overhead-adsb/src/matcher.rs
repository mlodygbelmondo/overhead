//! Matching detections to ADS-B flights and ground points.
//!
//! 1. **Passes.** For every candidate flight and every ground cell (default
//!    200 m) near its path, find each closest approach (a flight circling an
//!    airfield passes a point several times): emission time, slant range to
//!    the cell (terrain height from the DEM, microphone 1.5 m up), speed.
//!    The heard CPA time is emission time + range / c.
//! 2. **Per-detection filter.** A pass can explain detection *i* when its
//!    speed and slant range fall in the detection's (widened) ranges.
//! 3. **Combination.** The recording's absolute start `S` is unknown (or
//!    known only roughly), but the detections' relative `tc` offsets are
//!    exact. At each cell every compatible pass implies a start time
//!    `S = heard − tc_i`; we look for the `S` that the most detections agree
//!    on, each detection using its own pass (and flight pass) once.
//! 4. **Silence.** A prop aircraft passing the cell while the recording
//!    runs, with no detection anywhere near that time, counts against the
//!    hypothesis by how likely stage 1 was to detect it (slant range, class,
//!    background: [`crate::silence`]).
//! 5. **Curve sieve.** Hypotheses are grouped by flight assignment; the best
//!    few thousand groups get a cheap curve score at their best cell, with
//!    the start their passes imply there, and only the best of those go on.
//! 6. **Curve scoring.** The best hypotheses are refined continuously
//!    (observer east/north, `S`, ground speed of sound `c0`): for each matched
//!    detection the full heard Doppler curve of the real flight geometry
//!    (retarded time, speed of sound falling with height) is scored against
//!    the spectrogram, sweeping `f0`; `S` is shared, so every detection after
//!    the first is locked to the flight geometry.
//! 7. **Wide scan.** The best distinct assignments are also scanned over
//!    ±5 km (250 m steps, start implied per point) and refined from the best
//!    points found: the cell a group was seeded from can be kilometres from
//!    where its curves fit best.
//! 8. Rank, de-duplicate and report position, uncertainty, matched flights
//!    (with the pass that was scored). A further pass of a flight already
//!    matched that repeats its geometry (a circuit lap, out and back along
//!    one line) counts less ([`crate::repeat`]), in the ranking and the
//!    margin, never raising a margin.

use std::collections::HashMap;

use overhead_core::detect::Detection;
use overhead_core::fit::nelder_mead;
use overhead_core::model::DEFAULT_C;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub use crate::ambiguity::Alternative;
use crate::dem::Dem;
use crate::flight::{self, Flight, Sound};
use crate::geo::{self, BBox, Cell, Grid, M_PER_DEG_LAT, V3, dot, ecef, enu_axes, norm, sub};
pub use crate::repeat::REPEAT_WEIGHT;
use crate::repeat::{self, PassGeom, Repeat};
use crate::score::Observed;

/// Curve score (relative to the stage-1 fit) a match must exceed to count.
/// Chance alignments of a wrong flight's curve typically reach 0.1-0.5.
pub const MIN_CURVE: f64 = 0.45;
/// Cap on one detection's curve score in the total (scores are relative to the
/// stage-1 fit, which can be poor, making the ratio huge).
pub const MAX_CURVE: f64 = 3.0;
/// How far (s of emission time) a flight's track may be extrapolated beyond
/// its first and last ADS-B report when it is scored: about one report
/// interval of a thinned light-aircraft trace (5-10 s), within which the
/// straight-line continuation is as good as the interpolation between
/// reports. Beyond it the closest approach and the Doppler samples would be
/// made up (bug 1 of docs/evidence.md: 9 of 42 real runs had such a pass as #1).
pub const EXTRAP_S: f64 = 10.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchConfig {
    /// Coarse ground grid spacing (m).
    pub grid_m: f64,
    /// Resampling step of tracks for pass search (s).
    pub dt: f64,
    /// Upper bound on slant range considered (m).
    pub d_cap: f64,
    /// Speed tolerance: relative (speed of sound uncertainty) and absolute
    /// (wind: ADS-B gives ground speed, Doppler measures speed through air).
    pub v_rel_tol: f64,
    pub v_abs_tol: f64,
    /// Slant-range tolerance factors and absolute slack (m).
    pub d_lo_factor: f64,
    pub d_hi_factor: f64,
    pub d_abs_tol: f64,
    /// Extra heard-time tolerance (s) on top of each detection's tc range.
    pub t_tol: f64,
    /// The flat silence term instead of [`MatchConfig::soft_silence`]: slant
    /// range under which an undetected prop aircraft "should have been
    /// heard" (m), each such flight costing `quiet_penalty`. With
    /// `soft_silence` off and `None`: from the detections (half the median
    /// distance, clamped to 700..2000 m).
    pub quiet_d: Option<f64>,
    pub quiet_penalty: f64,
    /// Silence from detection probability ([`crate::silence::Model`]): an
    /// unheard pass costs `-weight·ln(1 - p_detect)`. Ignored when `quiet_d`
    /// is given.
    #[serde(default = "yes")]
    pub soft_silence: bool,
    #[serde(default)]
    pub silence: crate::silence::Model,
    /// With `soft_silence`: the coarse search only looks for unheard passes
    /// within this slant range (m); the ranking uses the model's whole range.
    #[serde(default = "silence_coarse_m")]
    pub silence_coarse_m: f64,
    /// Ignore silence violations this close to the start/end (s).
    pub quiet_margin: f64,
    /// Microphone height above ground (m).
    pub mic_height: f64,
    /// Speed of sound decreasing with source height.
    pub lapse: bool,
    pub c_bounds: (f64, f64),
    /// Assignment groups (best coarse first) that get the cheap curve sieve.
    pub sieve: usize,
    /// Of those, the groups (2 cells each) that get the start/c0 curve check.
    pub refine: usize,
    /// Of those, how many get the full continuous refinement (after a quick curve check).
    pub refine_full: usize,
    /// Distinct assignments that get a wide position scan, and its radius (m).
    pub widen: usize,
    pub widen_radius: f64,
    /// Uncertainty: score temperature per matched detection, and an RMS floor (m).
    pub unc_temp: f64,
    pub unc_floor_m: f64,
    /// Candidates reported.
    pub top: usize,
    /// Tile size for the search (degrees).
    pub tile_deg: f64,
    /// Soft speed/distance evidence ([`vd_evidence`]): in the coarse pick of
    /// each detection's pass, the re-assignment and the ranking (never in
    /// what decides which hypotheses are refined, the position, or alone in
    /// the margin, see [`margin`]).
    #[serde(default = "yes")]
    pub soft_vd: bool,
    /// Repeated passes of one flight: a further pass of a flight already
    /// matched counts `1 - repeat_weight · dependence` of its share
    /// ([`repeat_dependence`]: 1 for the same geometry again, as a circuit
    /// gives, 0 for an independent pass), in the ranking and the margin. 0 = off.
    #[serde(default = "repeat_weight")]
    pub repeat_weight: f64,
    /// Above this many search tiles the pass search uses coarse terrain.
    pub coarse_dem_tiles: usize,
}

impl Default for MatchConfig {
    fn default() -> Self {
        Self {
            grid_m: 200.0,
            dt: 3.0,
            d_cap: 12_000.0,
            v_rel_tol: 0.07,
            v_abs_tol: 12.0,
            d_lo_factor: 0.6,
            d_hi_factor: 1.6,
            d_abs_tol: 150.0,
            t_tol: 4.0,
            quiet_d: None,
            quiet_penalty: 0.75,
            quiet_margin: 45.0,
            mic_height: 1.5,
            lapse: true,
            c_bounds: (322.0, 356.0),
            sieve: 2500,
            refine: 300,
            refine_full: 80,
            widen: 16,
            widen_radius: 5000.0,
            unc_temp: 0.02,
            unc_floor_m: 120.0,
            top: 10,
            tile_deg: 1.0,
            coarse_dem_tiles: 40,
            soft_vd: true,
            repeat_weight: REPEAT_WEIGHT,
            soft_silence: true,
            silence: crate::silence::Model::default(),
            silence_coarse_m: silence_coarse_m(),
        }
    }
}

impl MatchConfig {
    /// Silence from detection probability (else the flat `quiet_d` term).
    pub fn soft_silence(&self) -> bool {
        self.soft_silence && self.quiet_d.is_none()
    }
}

fn yes() -> bool {
    true
}

fn silence_coarse_m() -> f64 {
    1000.0
}

fn repeat_weight() -> f64 {
    REPEAT_WEIGHT
}

/// Soft silence: a detection heard within `MASK_NEAR_S` of a pass explains
/// it; up to `MASK_FAR_S` it masks it in part ([`Search::silence_cost`]).
const MASK_NEAR_S: f64 = 30.0;
const MASK_FAR_S: f64 = 90.0;

/// A refined hypothesis: the hypothesis, its point, starts, `c0` and curve
/// score (without the soft speed/distance evidence).
type RefinedHyp = (Hyp, f64, f64, Vec<f64>, f64, f64);

/// Search window for one detection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Window {
    pub tc: f64,
    pub v: f64,
    pub d: f64,
    pub v_lo: f64,
    pub v_hi: f64,
    pub d_lo: f64,
    pub d_hi: f64,
    /// Heard-time tolerance (s).
    pub tau: f64,
    pub sigma_v: f64,
    pub sigma_d: f64,
    pub weight: f64,
}

pub fn windows(dets: &[Detection], cfg: &MatchConfig) -> Vec<Window> {
    dets.iter()
        .map(|d| {
            let r = &d.ranges;
            let fb = d.flyby;
            let (mut v_lo, mut v_hi) = (r.v.0.min(fb.v), r.v.1.max(fb.v));
            let (mut d_lo, mut d_hi) = (r.d.0.min(fb.d), r.d.1.max(fb.d));
            let mut tau = 0.5 * (r.tc.1 - r.tc.0).abs() + cfg.t_tol + fb.d * 0.07 / DEFAULT_C;
            if r.clipped[0] {
                v_lo /= 1.4;
                v_hi *= 1.4;
            }
            if r.clipped[1] {
                d_lo /= 2.0;
                d_hi *= 2.0;
            }
            if r.clipped[2] {
                tau += 10.0;
            }
            if d.likely_ground_vehicle {
                // slow-and-close flag: often a mis-measured aircraft; distance is least reliable
                d_hi *= 2.0;
            }
            let sigma_v = (0.5 * (v_hi - v_lo)).max(3.0) + 0.04 * fb.v + 0.5 * cfg.v_abs_tol;
            let sigma_d = (0.5 * (d_hi - d_lo)).max(50.0) + 0.08 * fb.d;
            let mut weight = if d.confident { 1.0 } else { 0.6 };
            if d.likely_ground_vehicle {
                weight *= 0.5;
            }
            Window {
                tc: fb.tc,
                v: fb.v,
                d: fb.d,
                v_lo: (v_lo * (1.0 - cfg.v_rel_tol) - cfg.v_abs_tol).max(5.0),
                v_hi: v_hi * (1.0 + cfg.v_rel_tol) + cfg.v_abs_tol,
                d_lo: (d_lo * cfg.d_lo_factor - cfg.d_abs_tol).max(20.0),
                d_hi: (d_hi * cfg.d_hi_factor + cfg.d_abs_tol).min(cfg.d_cap),
                tau,
                sigma_v,
                sigma_d,
                weight,
            }
        })
        .collect()
}

/// Soft speed/distance evidence: a pass slower than the stage-1 estimate by
/// more than [`VD_V_CORE`] half-widths of the range below it (half a
/// half-width beyond the range) costs as a Gaussian in log speed of
/// [`VD_V_SIGMA`] half-widths. A half-width is never taken below
/// [`VD_V_FLOOR`] (log units: speed of sound and wind). A faster pass costs
/// nothing: a true flight is as often faster than the range as a wrong one
/// (more often after a slow-and-close fit, see `likely_ground_vehicle`).
pub const VD_V_FLOOR: f64 = 0.10;
pub const VD_V_CORE: f64 = 1.5;
pub const VD_V_SIGMA: f64 = 0.5;
/// The `v^2 / d` ridge: a pass whose `v^2 / d` (central slope of the heard
/// curve) is off the stage-1 one by more than these log ratios costs as a
/// Gaussian of width [`VD_RIDGE_SIGMA`] beyond them. Stage 1 on a single
/// line pins little more than `v^2 / d`, and when it is wrong it is mostly
/// too steep (the pass farther than measured), so the core reaches further
/// below than above: [`VD_RIDGE_LO_CONFIDENT`] for a detection with closed
/// ranges, [`VD_RIDGE_LO`] for an unconfident one (see [`vd_evidence`]).
pub const VD_RIDGE_LO: f64 = -1.5;
pub const VD_RIDGE_LO_CONFIDENT: f64 = -2.5;
pub const VD_RIDGE_HI: f64 = 0.7;
pub const VD_RIDGE_SIGMA: f64 = 0.75;
/// Least factor the evidence leaves on a match: about the likelihood ratio
/// measured far in the tails, where a few true passes still lie.
pub const VD_FLOOR: f64 = 0.3;

/// Soft evidence (0.3..1, a factor on the match's share of the score) of how
/// well a pass with speed `v` (m/s) and slant range `d` (m) at its closest
/// approach fits a detection's stage-1 estimate and ranges.
///
/// Calibrated as a likelihood ratio on the synthetic end-to-end detections:
/// every true detection's stage-1 estimate against its true pass (157, all
/// of them, not only those the matcher found: the outer gate and the old
/// shape kept true passes far off the ridge out of the matched set), against
/// the flights matched at the wrong places among the reported candidates
/// (544), fitted by the logistic loss of "true" given the evidence. Inside
/// the stage-1 ranges, and anywhere along the `v^2/d` ridge, a true flight
/// is no more likely than a wrong one (stage 1's speed and distance slide
/// along the ridge, and its distance is often far too short), so all of that
/// costs nothing, and so does being faster than the speed range. What is
/// rare for a true flight is being much slower than the speed range, or far
/// off the ridge for an unconfident detection; on a confident one (closed
/// ranges) the true passes reach 2.5 below the ridge as often as wrong
/// ones, so only beyond that it costs. The fit's loss: 1.386 without the
/// evidence, 1.346 with the first shape (ridge core from −1.5 for every
/// detection, floor 0.1, which made it worse than nothing on confident
/// detections), 1.327 with this one; true passes below 0.5: 10 → 5 of 157.
pub fn vd_evidence(det: &Detection, v: f64, d: f64) -> f64 {
    if !(v > 0.0 && d > 0.0) {
        return 1.0;
    }
    let fb = det.flyby;
    let r = &det.ranges;
    let mut v_lo = r.v.0.min(fb.v);
    if r.clipped[0] {
        v_lo /= 1.4;
    }
    let x = (v / fb.v).ln();
    let s = (fb.v / v_lo.max(1e-3)).ln().max(VD_V_FLOOR);
    let chi_v = ((-x / s - VD_V_CORE).max(0.0) / VD_V_SIGMA).powi(2);
    let ridge = 2.0 * x - (d / fb.d.max(1.0)).ln();
    let lo = if det.confident { VD_RIDGE_LO_CONFIDENT } else { VD_RIDGE_LO };
    let chi_r = (((lo - ridge).max(0.0) + (ridge - VD_RIDGE_HI).max(0.0)) / VD_RIDGE_SIGMA).powi(2);
    (-0.5 * (chi_v + chi_r)).exp().max(VD_FLOOR)
}

/// A match's share of the score from its curve score (before the
/// detection's weight): nothing up to [`MIN_CURVE`], 1 at the stage-1 fit's
/// level, capped at [`MAX_CURVE`].
fn match_share(r: f64) -> f64 {
    (r.min(MAX_CURVE) - MIN_CURVE).max(0.0) / (1.0 - MIN_CURVE)
}

/// Speed (m/s) and slant range (m) of a segment at its closest approach to
/// `o` near emission time `tau` (±4 s); `range` stands in when the closest
/// approach lies beyond the segment's reports (within [`EXTRAP_S`]).
fn cpa_speed_range(seg: &flight::Segment, o: V3, tau: f64, range: f64) -> (f64, f64) {
    let (a, b) = ((tau - 4.0).max(seg.t0()), (tau + 4.0).min(seg.t1()));
    if b <= a {
        return (norm(seg.state(tau).1), range);
    }
    let dist = |t: f64| norm(sub(seg.state(t).0, o));
    let (mut bt, mut br) = (a, f64::INFINITY);
    let n = (b - a).ceil().max(1.0) as usize;
    for i in 0..=n {
        let t = (a + i as f64).min(b);
        let r = dist(t);
        if r < br {
            (bt, br) = (t, r);
        }
    }
    let (mut lo, mut hi) = ((bt - 1.0).max(a), (bt + 1.0).min(b));
    let g = 0.5 * (5f64.sqrt() - 1.0);
    for _ in 0..20 {
        let (c, e) = (hi - g * (hi - lo), lo + g * (hi - lo));
        if dist(c) < dist(e) { hi = e } else { lo = c }
    }
    let t = 0.5 * (lo + hi);
    (norm(seg.state(t).1), dist(t))
}

/// Slant range within which the coarse search looks for unheard passes:
/// `silence_coarse_m` with the soft silence, else the flat silence distance
/// (given, or from the detections).
pub fn quiet_distance(dets: &[Detection], cfg: &MatchConfig) -> f64 {
    if cfg.soft_silence() {
        return cfg.silence_coarse_m;
    }
    if let Some(q) = cfg.quiet_d {
        return q;
    }
    let mut d: Vec<f64> = dets.iter().filter(|d| d.confident && !d.likely_ground_vehicle).map(|d| d.flyby.d).collect();
    if d.is_empty() {
        d = dets.iter().map(|d| d.flyby.d).collect();
    }
    if d.is_empty() {
        return 1000.0;
    }
    d.sort_by(f64::total_cmp);
    (0.5 * d[d.len() / 2]).clamp(700.0, 2000.0)
}

#[derive(Clone, Copy, Debug)]
struct PassRec {
    cell: Cell,
    flight: u32,
    /// Emission time of the closest approach (unix s).
    t_cpa: f64,
    d: f32,
    v: f32,
    dets: u64,
    loud: bool,
    loudness: crate::silence::Loudness,
}

/// Height above ground (m) and speed (m/s) under which a pass is taken as
/// on or next to the ground (taxiing, landing roll, idle on the approach,
/// shielded by the terrain): with the soft silence it costs nothing unheard.
pub const LOW_AGL_M: f64 = 30.0;
pub const LOW_SPEED_MS: f64 = 20.0;

/// One matched detection inside a hypothesis.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Assign {
    pub det: usize,
    pub flight: u32,
    pub t_cpa: f64,
}

#[derive(Clone, Debug)]
struct Hyp {
    lat: f64,
    lon: f64,
    ground: f64,
    /// Recording start per part of the recording ([`Cuts`]; one value without
    /// cuts), NaN for a part without a matched detection.
    starts: Vec<f64>,
    score: f64,
    assign: Vec<Assign>,
    /// Coarse match quality of each assignment (only when collecting the coarse map).
    q: Vec<f64>,
}

/// Cuts in the recording: material was removed at these points, so the time
/// offsets between detections in different parts are unknown. Every part gets
/// its own start (the absolute time of recording time 0 if nothing had been
/// removed before it); within a part the timing stays locked to the flight
/// geometry as without cuts. Default: no cuts (one part, the behaviour
/// without this feature).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Cuts {
    /// Cut points (recording s), sorted, strictly inside the recording.
    pub at: Vec<f64>,
    /// Possible starts (unix s, sorted, disjoint windows) of the part after
    /// each cut (`at.len()` lists); the part before the first cut uses
    /// [`Search::start_windows`].
    pub windows: Vec<Vec<(f64, f64)>>,
}

impl Cuts {
    /// Part of the recording (0 = before the first cut) a recording time falls in.
    pub fn part_of(&self, t: f64) -> usize {
        self.at.partition_point(|c| *c <= t)
    }
}

/// Settings for what a search keeps besides its result (none by default).
#[derive(Clone, Debug, Default)]
pub struct Collect {
    /// Keep the coarse per-cell scores on a grid of this size (m) for score maps.
    pub coarse_cell_m: Option<f64>,
}

pub struct Search<'a> {
    pub dets: &'a [Detection],
    pub observed: &'a [Observed],
    pub flights: &'a [Flight],
    pub dem: &'a Dem,
    /// Recording length (s).
    pub duration: f64,
    /// Possible recording start times (unix s): sorted, disjoint windows.
    pub start_windows: Vec<(f64, f64)>,
    pub region: BBox,
    pub cfg: MatchConfig,
    /// Known answer (lat, lon, start) for validation: each candidate's
    /// assignment is also scored there.
    pub truth: Option<(f64, f64, f64)>,
    /// Cuts in the recording (default none).
    pub cuts: Cuts,
    /// Extra data kept for score maps (default none).
    pub collect: Collect,
    /// The recording's background over time, for the silence term (`None`:
    /// no audio, an average background is assumed).
    pub background: Option<&'a crate::silence::Background>,
}

/// One refined hypothesis of a search: where, when, which flights, and how
/// well each matched detection's curve fits.
#[derive(Clone, Debug, Serialize)]
pub struct Refined {
    pub lat: f64,
    pub lon: f64,
    /// Start per part of the recording (one without cuts); NaN (null in JSON)
    /// for a part with no matched detection.
    pub starts: Vec<f64>,
    pub c0: f64,
    /// Curve score (the refinement's objective).
    pub curve: f64,
    /// Curve score minus silence penalties: what candidates are ranked by.
    pub total: f64,
    pub coarse: f64,
    pub assign: Vec<Assign>,
    /// Per assignment (same order): curve score at full resolution and f0 multiplier
    /// (only for hypotheses that are alternatives, else empty).
    pub parts: Vec<(f64, f64)>,
    pub violations: Vec<Violation>,
}

/// Best coarse hypothesis in one cell of the coarse score map.
#[derive(Clone, Debug)]
pub struct CoarseCell {
    /// Centre of the output cell.
    pub lat: f64,
    pub lon: f64,
    /// Best combined coarse score (match qualities minus silence) of any
    /// matcher cell inside, with its assignment and starts.
    pub combined: f64,
    pub assign: Vec<Assign>,
    pub starts: Vec<f64>,
    /// Per detection: best coarse match quality (0..weight) and its assignment and start.
    pub per_det: Vec<Option<(f64, Assign, f64)>>,
}

/// Everything [`Search::run_detailed`] returns.
pub struct Detailed {
    pub candidates: Vec<Candidate>,
    pub stats: SearchStats,
    pub alternatives: Vec<Alternative>,
    /// Every refined hypothesis, ranked by `total` (best first).
    pub refined: Vec<Refined>,
    /// Index into `refined` of each candidate.
    pub candidate_refined: Vec<usize>,
    /// Coarse score map cells (when [`Collect::coarse_cell_m`] was set) and their size (m).
    pub coarse: Vec<CoarseCell>,
    pub coarse_cell_m: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchedFlight {
    pub detection: usize,
    pub tc: f64,
    pub hex: String,
    pub reg: Option<String>,
    pub type_code: Option<String>,
    pub class: crate::types::AircraftClass,
    pub callsign: Option<String>,
    /// Closest approach (emission) and when it is heard, UTC.
    pub cpa_utc: String,
    pub heard_utc: String,
    /// Recording time at which the model puts the heard CPA (s).
    pub heard_rec: f64,
    pub slant_m: f64,
    pub speed_ms: f64,
    pub height_agl_m: f64,
    /// Curve score (≈1 = as good as the stage-1 fit).
    pub curve_score: f64,
    pub f0_scale: f64,
    /// Soft speed/distance evidence of the scored pass ([`vd_evidence`]): the
    /// factor on this match's share of the score.
    #[serde(default = "one")]
    pub vd_evidence: f64,
    /// A further pass of a flight matched more strongly at another
    /// detection ([`crate::repeat`]): what it repeats and the factor on its share.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<Repeat>,
}

fn one() -> f64 {
    1.0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Violation {
    pub hex: String,
    pub type_code: Option<String>,
    pub heard_rec: f64,
    pub slant_m: f64,
    /// Probability that stage 1 would have detected the pass (soft silence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_detect: Option<f64>,
    /// What it costs the candidate (score units; a flight passing several
    /// times costs its most expensive pass).
    #[serde(default)]
    pub cost: f64,
}

/// A pass of a candidate flight close to a point while the recording ran
/// ([`Search::loud_passes_at`]).
#[derive(Clone, Copy, Debug)]
pub struct LoudPass {
    /// Index into the search's flights.
    pub flight: u32,
    /// Part of the recording (0 without cuts).
    pub part: usize,
    /// Closest approach (emission, unix s) and slant range there (m).
    pub t_cpa: f64,
    pub slant_m: f64,
    /// Recording time at which that closest approach is heard (s, at 343 m/s).
    pub heard_rec: f64,
    /// Detection this flight is matched to in the hypothesis.
    pub assigned: Option<usize>,
    /// A detection heard within 30 s.
    pub near_det: Option<usize>,
    /// Not in the first or last `quiet_margin` s of its part.
    pub inside: bool,
    /// Height above the point's ground and speed at the closest approach.
    pub agl_m: f64,
    pub speed_ms: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Uncertainty {
    /// RMS radius of the high-scoring area (m).
    pub radius_m: f64,
    pub major_m: f64,
    pub minor_m: f64,
    /// Azimuth of the major axis (deg from north).
    pub azimuth_deg: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub rank: usize,
    pub lat: f64,
    pub lon: f64,
    pub ground_m: f64,
    pub score: f64,
    /// Score minus the best score of any other refined hypothesis more than
    /// 2 km away (0 if there is none): a small margin means the answer is ambiguous.
    pub margin: f64,
    pub coarse_score: f64,
    pub matched: usize,
    pub start_utc: String,
    pub start_unix: f64,
    pub c0: f64,
    pub uncertainty: Uncertainty,
    pub flights: Vec<MatchedFlight>,
    pub violations: Vec<Violation>,
    /// Validation only: curve score of this assignment at the true point and start (best c0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truth_score: Option<f64>,
    /// Probability that the recording was made at this place (within 2 km),
    /// from the ambiguity assessment; `None` when an earlier candidate is
    /// already in the same place (see `same_place_as`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probability: Option<f64>,
    /// Rank of the better candidate within 2 km whose place this one belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_place_as: Option<usize>,
    /// With cuts: the start of every part of the recording (unix s; `None`
    /// for a part with no matched detection). `start_unix` is the first known one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_starts_unix: Option<Vec<Option<f64>>>,
    /// With cuts: the part whose start `start_unix` is (0 = the recording's
    /// own start). When it is not 0, no fly-by before the cut matched, so the
    /// recording's own start is unknown and `start_unix` is the time zero of
    /// a later part (which may lie outside the start window given).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_part: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SearchStats {
    pub flights: usize,
    pub tiles: usize,
    pub passes: usize,
    pub cells: usize,
    pub groups: usize,
    pub refined: usize,
    /// Extra hypotheses from the wide position scans.
    #[serde(default)]
    pub widened: usize,
    /// The pass search used coarse terrain (very large search area).
    #[serde(default)]
    pub coarse_terrain: bool,
    pub quiet_d: f64,
    pub seconds: f64,
    /// Validation only: best coarse hypothesis at the true cell, refined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truth_hypothesis: Option<TruthHyp>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TruthHyp {
    /// Rank of the truth cell's assignment group by coarse score (1 = best), and
    /// whether it got the full refinement.
    #[serde(default)]
    pub group_rank: Option<usize>,
    #[serde(default)]
    pub refined_in_search: bool,
    #[serde(default)]
    pub sieve_rank: Option<usize>,
    pub coarse: f64,
    pub refined: f64,
    pub dist_m: f64,
    pub assign: Vec<(usize, String, f64)>,
}

pub fn utc(t: f64) -> String {
    let tenths = (t * 10.0).round() as i64;
    chrono::DateTime::from_timestamp(tenths.div_euclid(10), 0)
        .map(|d| format!("{}.{}Z", d.format("%Y-%m-%dT%H:%M:%S"), tenths.rem_euclid(10)))
        .unwrap_or_default()
}

struct Lattice {
    lat: f64,
    lon: f64,
    m_lon: f64,
}

impl Lattice {
    fn offset(&self, e: f64, n: f64) -> (f64, f64) {
        (self.lat + n / M_PER_DEG_LAT, self.lon + e / self.m_lon)
    }
}

impl Search<'_> {
    /// Parts of the recording (cuts + 1).
    pub fn n_parts(&self) -> usize {
        self.cuts.at.len() + 1
    }

    /// Part of the recording detection `i` belongs to (by its `tc`).
    pub fn part_of_det(&self, i: usize) -> usize {
        self.cuts.part_of(self.dets[i].flyby.tc)
    }

    /// Recording time span of part `k`.
    pub fn part_span(&self, k: usize) -> (f64, f64) {
        let lo = if k == 0 { 0.0 } else { self.cuts.at[k - 1] };
        let hi = self.cuts.at.get(k).copied().unwrap_or(self.duration);
        (lo, hi)
    }

    /// Start windows of part `k`.
    pub fn part_windows(&self, k: usize) -> &[(f64, f64)] {
        if k == 0 { &self.start_windows } else { &self.cuts.windows[k - 1] }
    }

    /// Is `s` within `pad` of a start window of part `k`?
    fn start_ok_in(&self, k: usize, s: f64, pad: f64) -> bool {
        self.part_windows(k).iter().any(|w| s >= w.0 - pad && s <= w.1 + pad)
    }

    /// `s` moved into the nearest start window of part `k`.
    fn clamp_start_in(&self, k: usize, s: f64) -> f64 {
        let mut best = (f64::INFINITY, s);
        for w in self.part_windows(k) {
            let c = s.clamp(w.0, w.1);
            if (c - s).abs() < best.0 {
                best = ((c - s).abs(), c);
            }
        }
        best.1
    }

    /// How far (m) a point lies outside the search region (0 inside).
    fn outside_region_m(&self, lat: f64, lon: f64) -> f64 {
        let r = &self.region;
        let dn = (r.lat_min - lat).max(lat - r.lat_max).max(0.0) * M_PER_DEG_LAT;
        let de = (r.lon_min - lon).max(lon - r.lon_max).max(0.0) * M_PER_DEG_LAT * lat.to_radians().cos();
        (dn * dn + de * de).sqrt()
    }

    pub(crate) fn sound(&self, c0: f64) -> Sound {
        Sound { c0, lapse: self.cfg.lapse }
    }

    /// All passes of `fi` over cells inside `tile`.
    #[allow(clippy::too_many_arguments)]
    fn passes(&self, dem: &Dem, fi: usize, tile: &BBox, wins: &[Window], quiet_d: f64, grid: &Grid, sites: &CellSites, out: &mut Vec<PassRec>) {
        let f = &self.flights[fi];
        let loudness = crate::silence::Loudness::of(f.class, f.type_code.as_deref());
        let soft = self.cfg.soft_silence();
        let d_max = wins.iter().map(|w| w.d_hi).fold(quiet_d, f64::max);
        // emission times that can be heard during the recording, for every start window
        let mut raw: Vec<(f64, f64)> = Vec::new();
        for k in 0..self.n_parts() {
            let (p0, p1) = self.part_span(k);
            for w in self.part_windows(k) {
                raw.push(if self.cuts.at.is_empty() {
                    (w.0 - 120.0 - d_max / 300.0, w.1 + self.duration + 60.0)
                } else {
                    (w.0 + p0 - 120.0 - d_max / 300.0, w.1 + p1 + 60.0)
                });
            }
        }
        // (already sorted without cuts: the stable sort keeps that order)
        raw.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut spans: Vec<(f64, f64)> = Vec::new();
        for (a, b) in raw {
            match spans.last_mut() {
                Some(l) if a <= l.1 => l.1 = l.1.max(b),
                _ => spans.push((a, b)),
            }
        }
        let g = grid.spacing;
        let mic = self.cfg.mic_height;
        let mut cells: Vec<Cell> = Vec::new();
        let mut cell_pos: HashMap<Cell, (V3, f64), FxBuild> = HashMap::default();
        let mut uniq = CellSet::default();
        let (mut lat_a, mut lat_s0, mut lat_s1): (Vec<(f64, f64)>, Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new(), Vec::new());
        let reach = tile.expand(d_max);
        let wide = reach.expand(d_max);
        let mut rows = geo::GridRows::new(*grid, wide.lat_min, wide.lat_max);
        let mut ivs: Vec<(f64, f64)> = Vec::new();
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for samples in spans.iter().flat_map(|&(t0, t1)| f.resample(self.cfg.dt, t0, t1)) {
            for k in 0..samples.len().saturating_sub(1) {
                let (ta, pa, va, _) = samples[k];
                let (tb, pb, vb, _) = samples[k + 1];
                let (lat, lon, h) = geo::geodetic(pa);
                if !reach.contains(lat, lon) {
                    continue;
                }
                let ground = dem.height(lat, lon);
                let agl = h - ground;
                let speed = 0.5 * (norm(va) + norm(vb));
                // lateral ranges (m) of interest on either side of the path
                ivs.clear();
                let (agl_lo, agl_hi) = ((agl - 300.0).max(0.0), agl + 300.0);
                for w in wins {
                    if speed < w.v_lo - 5.0 || speed > w.v_hi + 5.0 || agl_lo > w.d_hi {
                        continue;
                    }
                    let lo = (w.d_lo * w.d_lo - agl_hi * agl_hi).max(0.0).sqrt();
                    let hi = (w.d_hi * w.d_hi - agl_lo * agl_lo).max(0.0).sqrt();
                    ivs.push((lo, hi));
                }
                if agl_lo < quiet_d {
                    ivs.push((0.0, (quiet_d * quiet_d - agl_lo * agl_lo).sqrt()));
                }
                if ivs.is_empty() {
                    continue;
                }
                ivs.sort_by(|a, b| a.0.total_cmp(&b.0));
                merged.clear();
                for &iv in &ivs {
                    match merged.last_mut() {
                        Some(m) if iv.0 <= m.1 + g => m.1 = m.1.max(iv.1),
                        _ => merged.push(iv),
                    }
                }
                // horizontal direction of travel
                let (e, n, _) = enu_axes(lat, lon);
                let ve = [dot(va, e), dot(va, n)];
                let hs = (ve[0] * ve[0] + ve[1] * ve[1]).sqrt();
                if hs < 1.0 {
                    continue;
                }
                let along = [ve[0] / hs, ve[1] / hs];
                let perp = [-along[1], along[0]];
                let seg_len = norm(sub(pb, pa));
                let lat_ctx = Lattice { lat, lon, m_lon: M_PER_DEG_LAT * lat.to_radians().cos() };
                cells.clear();
                let half = 0.5 * g;
                let na = ((seg_len + g) / half).ceil() as i32;
                // the lattice: steps along the path (a) times lateral offsets
                // (s, either side), east and north parts of each
                lat_a.clear();
                for ia in 0..=na {
                    let a = -half + ia as f64 * half;
                    lat_a.push((a * along[0], a * along[1]));
                }
                lat_s0.clear();
                lat_s1.clear();
                for &(lo, hi) in &merged {
                    let ns = ((hi - lo) / half).ceil() as i32;
                    for is in 0..=ns {
                        let s = (lo + is as f64 * half).min(hi);
                        for side in [-1.0, 1.0] {
                            if s == 0.0 && side > 0.0 {
                                continue;
                            }
                            lat_s0.push(side * s * perp[0]);
                            lat_s1.push(side * s * perp[1]);
                        }
                    }
                }
                lattice_cells(&mut rows, &lat_ctx, &lat_a, &mut lat_s0, &mut lat_s1, &mut cells);
                uniq.sort_dedup(&mut cells);
                for &c in &cells {
                    let (clat, clon) = rows.centre(c);
                    if !tile.contains(clat, clon) || !self.region.contains(clat, clon) {
                        continue;
                    }
                    let site = || {
                        let gh = dem.height(clat, clon);
                        (ecef(clat, clon, gh + mic), gh)
                    };
                    let (o, _) = match sites.slot(c) {
                        Some(slot) => *slot.get_or_init(site),
                        None => *cell_pos.entry(c).or_insert_with(site),
                    };
                    let fa = dot(sub(pa, o), va);
                    let fb = dot(sub(pb, o), vb);
                    if !(fa < 0.0 && fb >= 0.0) {
                        continue;
                    }
                    let dp = sub(pb, pa);
                    let u = (-dot(sub(pa, o), dp) / dot(dp, dp).max(1e-9)).clamp(0.0, 1.0);
                    let pc = [pa[0] + u * dp[0], pa[1] + u * dp[1], pa[2] + u * dp[2]];
                    let d = norm(sub(pc, o));
                    let v = norm([va[0] + u * (vb[0] - va[0]), va[1] + u * (vb[1] - va[1]), va[2] + u * (vb[2] - va[2])]);
                    let t_cpa = ta + u * (tb - ta);
                    let mut bits = 0u64;
                    for (i, w) in wins.iter().enumerate().take(64) {
                        if v >= w.v_lo && v <= w.v_hi && d >= w.d_lo && d <= w.d_hi {
                            bits |= 1 << i;
                        }
                    }
                    let low = soft && (agl < LOW_AGL_M || speed < LOW_SPEED_MS);
                    let loud = d < quiet_d && !low;
                    if bits == 0 && !loud {
                        continue;
                    }
                    out.push(PassRec {
                        cell: c,
                        flight: fi as u32,
                        t_cpa,
                        d: d as f32,
                        v: v as f32,
                        dets: bits,
                        loud,
                        loudness,
                    });
                }
            }
        }
    }

    /// Best hypothesis at one cell. With cuts every part of the recording is
    /// combined on its own (its own start) and the parts' scores add up.
    fn evaluate_cell(&self, recs: &[PassRec], wins: &[Window], grid: &Grid) -> Option<Hyp> {
        if self.cuts.at.is_empty() {
            return self.evaluate_part(recs, wins, grid, 0, u64::MAX);
        }
        let n = self.n_parts();
        let mut out: Option<Hyp> = None;
        for k in 0..n {
            let mut mask = 0u64;
            for i in 0..wins.len().min(64) {
                if self.part_of_det(i) == k {
                    mask |= 1 << i;
                }
            }
            if mask == 0 {
                continue;
            }
            let Some(h) = self.evaluate_part(recs, wins, grid, k, mask) else { continue };
            match &mut out {
                None => {
                    let mut starts = vec![f64::NAN; n];
                    starts[k] = h.starts[k];
                    out = Some(Hyp { starts, ..h });
                }
                // one pass of a flight cannot be heard in two parts (they do not overlap in time)
                Some(o) if h.assign.iter().any(|a| o.assign.iter().any(|b| b.flight == a.flight && (b.t_cpa - a.t_cpa).abs() < 60.0)) => {}
                Some(o) => {
                    o.starts[k] = h.starts[k];
                    o.score += h.score;
                    o.assign.extend(h.assign);
                    o.q.extend(h.q);
                }
            }
        }
        out
    }

    /// Best hypothesis at one cell for the detections in `mask` (all of
    /// part `k`), with part `k`'s start windows.
    fn evaluate_part(&self, recs: &[PassRec], wins: &[Window], grid: &Grid, k_part: usize, mask: u64) -> Option<Hyp> {
        let c = DEFAULT_C;
        let collect_q = self.collect.coarse_cell_m.is_some();
        let (p0, p1) = self.part_span(k_part);
        // heard CPA of every pass, and per detection its compatible passes
        // sorted by heard time, so that each start candidate only looks at
        // the passes within its time tolerance (same picks, same order of
        // evaluation as a scan over all passes)
        let heard: Vec<f64> = recs.iter().map(|r| r.t_cpa + r.d as f64 / c).collect();
        let mut cands: Vec<f64> = Vec::new();
        // (detection, heard, pass, soft speed/distance evidence)
        let mut by_det: Vec<(usize, f64, usize, f64)> = Vec::new();
        for (j, r) in recs.iter().enumerate() {
            if r.dets == 0 {
                continue;
            }
            let heard = heard[j];
            for (i, w) in wins.iter().enumerate() {
                if r.dets & mask & (1 << i) != 0 {
                    let g = if self.cfg.soft_vd { vd_evidence(&self.dets[i], r.v as f64, r.d as f64) } else { 1.0 };
                    by_det.push((i, heard, j, g));
                    let s = heard - w.tc;
                    if self.start_ok_in(k_part, s, w.tau) {
                        cands.push(s);
                    }
                }
            }
        }
        if cands.is_empty() {
            return None;
        }
        by_det.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
        let mut offs = vec![0usize; wins.len() + 1];
        for e in &by_det {
            offs[e.0 + 1] += 1;
        }
        for i in 0..wins.len() {
            offs[i + 1] += offs[i];
        }
        cands.sort_by(f64::total_cmp);
        cands.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        let mut order: Vec<usize> = (0..wins.len()).filter(|i| mask & (1 << i) != 0).collect();
        order.sort_by(|a, b| wins[*b].weight.total_cmp(&wins[*a].weight));
        let mut best: Option<Hyp> = None;
        let mut used = vec![false; recs.len()];
        // the passes silence may count, in their order
        let loud: Vec<usize> = (0..recs.len()).filter(|&j| recs[j].loud).collect();
        // per detection (in `order`): range of `by_det` heard within its tolerance
        let mut ranges: Vec<(usize, usize)> = vec![(0, 0); wins.len()];
        let mut near: Vec<(usize, f64)> = Vec::new();
        // slack (s) on the binary-search bounds; the exact test is applied per pass
        const SLACK: f64 = 1e-3;
        for &s in &cands {
            let mut ub = 0.0;
            for (k, &i) in order.iter().enumerate() {
                let w = &wins[i];
                let list = &by_det[offs[i]..offs[i + 1]];
                let lo = w.tc + s - w.tau - SLACK;
                let hi = w.tc + s + w.tau + SLACK;
                let a = list.partition_point(|e| e.1 < lo);
                let b = a + list[a..].partition_point(|e| e.1 <= hi);
                ranges[k] = (offs[i] + a, offs[i] + b);
                if b > a {
                    ub += w.weight;
                }
            }
            // every match scores at most its weight and silence only subtracts:
            // a start that cannot beat the best one so far is skipped
            if self.cfg.quiet_penalty >= 0.0
                && let Some(b) = &best
                && ub <= b.score
            {
                continue;
            }
            used.iter_mut().for_each(|u| *u = false);
            let mut assign = Vec::new();
            let mut qs = Vec::new();
            let mut q = 0.0;
            let mut matched_s = Vec::new();
            for (k, &i) in order.iter().enumerate() {
                let w = &wins[i];
                let mut pick: Option<(usize, f64)> = None;
                near.clear();
                near.extend(by_det[ranges[k].0..ranges[k].1].iter().map(|e| (e.2, e.3)));
                near.sort_unstable_by_key(|e| e.0);
                for &(j, g) in &near {
                    let r = &recs[j];
                    if used[j] {
                        continue;
                    }
                    let dt = heard[j] - w.tc - s;
                    if dt.abs() > w.tau {
                        continue;
                    }
                    let chi = (((r.v as f64 - w.v) / w.sigma_v).powi(2)
                        + ((r.d as f64 - w.d) / w.sigma_d).powi(2)
                        + (dt / (0.5 * w.tau)).powi(2))
                        / 3.0;
                    let qi = w.weight * (-0.5 * chi).exp() * g;
                    if pick.is_none_or(|p| qi > p.1) {
                        pick = Some((j, qi));
                    }
                }
                if let Some((j, qi)) = pick {
                    used[j] = true;
                    q += qi;
                    assign.push(Assign { det: i, flight: recs[j].flight, t_cpa: recs[j].t_cpa });
                    if collect_q {
                        qs.push(qi);
                    }
                    matched_s.push(recs[j].t_cpa + recs[j].d as f64 / c - w.tc);
                }
            }
            if assign.is_empty() {
                continue;
            }
            let s_mean = matched_s.iter().sum::<f64>() / matched_s.len() as f64;
            let s_use = self.clamp_start_in(k_part, s_mean);
            // silence: each unheard flight costs its most expensive pass
            let soft = self.cfg.soft_silence();
            let mut bad: Vec<(u32, f64)> = Vec::new();
            for &j in &loud {
                let r = &recs[j];
                if used[j] {
                    continue;
                }
                let t_rec = r.t_cpa + r.d as f64 / c - s_use;
                if t_rec < p0 + self.cfg.quiet_margin || t_rec > p1 - self.cfg.quiet_margin {
                    continue;
                }
                if wins.iter().any(|w| (w.tc - t_rec).abs() < 30.0) {
                    continue;
                }
                // (soft silence: a flight heard here is not held against the
                // place for its other passes, e.g. circuits)
                if assign.iter().any(|a| a.flight == r.flight && (soft || (a.t_cpa - r.t_cpa).abs() < 60.0)) {
                    continue;
                }
                let cost = self.silence_cost(r.d as f64, r.loudness, false, t_rec).1;
                if cost <= 0.0 {
                    continue;
                }
                match bad.iter_mut().find(|b| b.0 == r.flight) {
                    Some(b) => b.1 = b.1.max(cost),
                    None => bad.push((r.flight, cost)),
                }
            }
            let score = q - bad.iter().map(|b| b.1).sum::<f64>();
            if best.as_ref().is_none_or(|b| score > b.score) {
                let (lat, lon) = grid.centre(recs[0].cell);
                let mut starts = vec![f64::NAN; self.n_parts()];
                starts[k_part] = s_use;
                best = Some(Hyp { lat, lon, ground: 0.0, starts, score, assign, q: qs });
            }
        }
        best
    }

    /// The modelled heard Doppler factor `f_heard / f_rest` of one segment at
    /// recording times `times` for an observer at `o` and recording start `s`
    /// (NaN where the track is unknown, beyond [`EXTRAP_S`] from its reports),
    /// and the recording times of the heard closest approaches (radial
    /// velocity turning from approach to recession).
    pub fn heard_factors(seg: &flight::Segment, o: V3, up: V3, sound: &Sound, s: f64, times: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let mut h = HeardSweep::new(seg, s, times.len());
        for t in times {
            h.step(*t, o, up, sound);
        }
        (h.k, h.crossings)
    }

    /// [`Self::heard_factors`] of several passes (segment, start, times) at
    /// once. Each pass is computed exactly as on its own (every recording time
    /// starts from the previous one's emission time); they only advance
    /// together, so the processor can work on one pass's iteration while
    /// another waits on its divisions and square roots.
    pub fn heard_factors_many(o: V3, up: V3, sound: &Sound, jobs: &[(&flight::Segment, f64, &[f64])]) -> Vec<(Vec<f64>, Vec<f64>)> {
        let mut sw: Vec<HeardSweep> = jobs.iter().map(|(seg, s, times)| HeardSweep::new(seg, *s, times.len())).collect();
        let n = jobs.iter().map(|j| j.2.len()).max().unwrap_or(0);
        // (job, recording time) and Newton lane of every pass at this time
        let mut at: Vec<(usize, f64)> = Vec::with_capacity(jobs.len());
        let mut lanes: Vec<flight::Lane> = Vec::with_capacity(jobs.len());
        let mut heard: Vec<flight::Heard> = Vec::with_capacity(jobs.len());
        for i in 0..n {
            at.clear();
            lanes.clear();
            for (j, (h, (_, _, times))) in sw.iter_mut().zip(jobs).enumerate() {
                let Some(&t) = times.get(i) else { continue };
                let t_abs = h.s + t;
                // the first guess as `heard` makes it
                let tau = h.guess.unwrap_or_else(|| {
                    let (p, _, _) = h.seg.state_hint(t_abs, &mut h.hint);
                    t_abs - norm(sub(p, o)) / sound.c0
                });
                at.push((j, t));
                lanes.push(flight::Lane { seg: h.seg, hint: h.hint, t: t_abs, tau, at: h.last });
            }
            flight::heard_lanes(&mut lanes, o, up, sound, &mut heard);
            for ((&(j, t), l), &hd) in at.iter().zip(&lanes).zip(&heard) {
                sw[j].hint = l.hint;
                sw[j].finish(t, hd, sound);
            }
        }
        sw.into_iter().map(|h| (h.k, h.crossings)).collect()
    }

    /// Timing factor of a match (1 when the modelled heard CPA nearest the
    /// detection's `tc` is within its tolerance, fading beyond), and that CPA.
    pub fn timing(det: &Detection, crossings: &[f64]) -> (f64, Option<f64>) {
        let tol = 0.5 * (det.ranges.tc.1 - det.ranges.tc.0).abs() + 10.0 + if det.ranges.clipped[2] { 10.0 } else { 0.0 };
        let nearest = crossings.iter().copied().min_by(|a, b| (a - det.flyby.tc).abs().total_cmp(&(b - det.flyby.tc).abs()));
        let miss = nearest.map_or(f64::INFINITY, |c| (c - det.flyby.tc).abs());
        let timing = if miss <= tol { 1.0 } else if miss.is_finite() { (-((miss - tol) / 5.0).powi(2)).exp() } else { 0.0 };
        (timing, nearest)
    }

    /// Fine objective for a hypothesis at `(lat, lon)`, starts `s` (one per
    /// part of the recording), `c0`.
    /// Returns total and per-assignment `(score, f0 scale, heard CPA)`, the
    /// heard CPA being the recording time of the modelled pass nearest the
    /// detection's `tc` (the one that was scored).
    pub fn curve_scores(&self, lat: f64, lon: f64, s: &[f64], c0: f64, assign: &[Assign], stride: usize) -> (f64, Vec<(f64, f64, Option<f64>)>) {
        let (total, parts) = self.curve_parts(lat, lon, s, c0, assign, stride, self.cfg.soft_vd);
        (total, parts.into_iter().map(|p| (p.0, p.1, p.2)).collect())
    }

    /// [`Self::curve_scores`] without the soft speed/distance evidence: what
    /// the position is optimised and scanned on, and what the sieve, the
    /// quick check and the wide scan's picks rank by. The evidence weighs an
    /// assignment, it is no prior on the position: stage 1's distance is
    /// often far too short, and pulling the point towards it would move a
    /// right answer off. Nor may it decide which hypotheses are refined: a
    /// competitor it dropped there would be missing from the margin's
    /// alternatives, and the margin of whatever is #1 would rise with it.
    pub fn curve_scores_plain(&self, lat: f64, lon: f64, s: &[f64], c0: f64, assign: &[Assign], stride: usize) -> f64 {
        self.curve_parts_with(lat, lon, s, c0, assign, stride, false, true).0
    }

    /// [`Self::curve_scores`] with each assignment's soft speed/distance
    /// evidence ([`vd_evidence`] of the scored pass; 1 when none was scored
    /// or `soft` is off) as a fourth value. A match's share of the total is
    /// its capped curve score above [`MIN_CURVE`], times the detection's
    /// weight, times that evidence.
    #[allow(clippy::too_many_arguments)]
    pub fn curve_parts(
        &self,
        lat: f64,
        lon: f64,
        s: &[f64],
        c0: f64,
        assign: &[Assign],
        stride: usize,
        soft: bool,
    ) -> (f64, Vec<(f64, f64, Option<f64>, f64)>) {
        self.curve_parts_with(lat, lon, s, c0, assign, stride, soft, false)
    }

    /// [`Self::curve_parts`]; with `prune` a match whose timing alone keeps
    /// its curve score below [`MIN_CURVE`] whatever the spectrogram holds
    /// (the timing factor times [`Observed::score_bound`]) is not laid on the
    /// spectrogram: its share of the total is 0 either way, so the total is
    /// the same to the bit, but its part reports a curve score of 0 and an
    /// f0 scale of 1 (only known to be below [`MIN_CURVE`]).
    #[allow(clippy::too_many_arguments)]
    fn curve_parts_with(
        &self,
        lat: f64,
        lon: f64,
        s: &[f64],
        c0: f64,
        assign: &[Assign],
        stride: usize,
        soft: bool,
        prune: bool,
    ) -> (f64, Vec<(f64, f64, Option<f64>, f64)>) {
        let gh = self.dem.height(lat, lon);
        let o = ecef(lat, lon, gh + self.cfg.mic_height);
        let (_, _, up) = enu_axes(lat, lon);
        let sound = self.sound(c0);
        let mut total = 0.0;
        let mut parts = Vec::with_capacity(assign.len());
        // the modelled curves of every match first (together, see heard_factors_many)
        let segs: Vec<Option<(&flight::Segment, f64)>> = assign
            .iter()
            .map(|a| {
                let s = s[self.part_of_det(a.det)];
                self.flights[a.flight as usize].segment_at(a.t_cpa).filter(|_| s.is_finite()).map(|seg| (seg, s))
            })
            .collect();
        let times: Vec<Vec<f64>> =
            assign.iter().zip(&segs).map(|(a, g)| if g.is_some() { self.observed[a.det].times(stride) } else { Vec::new() }).collect();
        let jobs: Vec<(&flight::Segment, f64, &[f64])> =
            segs.iter().zip(&times).filter_map(|(g, t)| g.map(|(seg, s)| (seg, s, t.as_slice()))).collect();
        let mut curves = Self::heard_factors_many(o, up, &sound, &jobs).into_iter();
        for (a, g) in assign.iter().zip(&segs) {
            let Some((seg, s)) = *g else {
                parts.push((0.0, 1.0, None, 1.0));
                continue;
            };
            let obs = &self.observed[a.det];
            let (k, crossings) = curves.next().expect("one curve per match with a segment");
            // the flight's heard CPA must fall where the detection puts it ("locked" timing)
            let det = &self.dets[a.det];
            let (timing, nearest) = Self::timing(det, &crossings);
            // |score| <= bound, so score·timing <= bound·timing (rounding is monotonic)
            if prune && obs.score_bound() * timing < MIN_CURVE {
                parts.push((0.0, 1.0, nearest, 1.0));
                continue;
            }
            let (r, m) = obs.score(det.flyby.f0, &k, stride);
            let r = r * timing;
            // a match only helps once its curve is clearly better than a chance alignment
            // capped: a detection whose stage-1 fit was poor has a tiny reference,
            // and one such detection must not outweigh several good ones
            let share = match_share(r);
            // how well the speed and distance of the scored pass fit stage 1's
            let g = match nearest {
                Some(hr) if share > 0.0 && soft => {
                    let mut hint = 0;
                    let h = flight::heard(seg, o, up, &sound, s + hr, None, &mut hint);
                    let (v, d) = cpa_speed_range(seg, o, h.tau, h.range);
                    vd_evidence(det, v, d)
                }
                _ => 1.0,
            };
            total += det.weight_hint() * share * g;
            parts.push((r, m, nearest, g));
        }
        (total, parts)
    }

    /// Prior on the ground speed of sound: c(T) for T ≈ 15 ± 15 °C (σ ≈ 9 m/s),
    /// hard walls at `c_bounds`.
    /// What the soft speed/distance evidence took off a total scored with
    /// [`Self::curve_parts`] (`parts` of `assign`): add it back for the
    /// score without the evidence.
    fn vd_loss(&self, assign: &[Assign], parts: &[(f64, f64, Option<f64>, f64)]) -> f64 {
        assign.iter().zip(parts).map(|(a, p)| self.dets[a.det].weight_hint() * match_share(p.0) * (1.0 - p.3)).sum()
    }

    /// Geometry of the pass of `a` heard at recording time `hr` (start `s`)
    /// at its closest approach to the observer `o` (axes east, north, up).
    fn pass_geom(&self, o: V3, axes: (V3, V3, V3), sound: &Sound, s: f64, a: &Assign, hr: f64) -> Option<PassGeom> {
        let seg = self.flights[a.flight as usize].segment_at(a.t_cpa)?;
        let (e, n, up) = axes;
        let mut hint = 0;
        let h = flight::heard(seg, o, up, sound, s + hr, None, &mut hint);
        let t = match flight::closest_approach(seg, o, h.tau - 4.0, h.tau + 4.0) {
            (_, d) if !d.is_finite() => h.tau,
            (t, _) => t,
        };
        let (p, v, _) = seg.state(t);
        let r = sub(p, o);
        let d = norm(r);
        if !(d > 0.0) {
            return None;
        }
        let (ve, vn) = (dot(v, e), dot(v, n));
        let hv = ve.hypot(vn);
        Some(PassGeom {
            t,
            slant_m: d,
            dir: [dot(r, e) / d, dot(r, n) / d, dot(r, up) / d],
            track: if hv > 1.0 { [ve / hv, vn / hv] } else { [0.0; 2] },
        })
    }

    /// Which matches of a hypothesis repeat a stronger match of the same
    /// flight ([`repeat::discounts`]), from `parts` of [`Self::curve_parts`].
    fn repeats_at(&self, lat: f64, lon: f64, s: &[f64], c0: f64, assign: &[Assign], parts: &[(f64, f64, Option<f64>, f64)]) -> Vec<Option<Repeat>> {
        let w = self.cfg.repeat_weight;
        let shares: Vec<f64> = assign.iter().zip(parts).map(|(a, p)| self.dets[a.det].weight_hint() * match_share(p.0) * p.3).collect();
        let mut seen: Vec<u32> = Vec::new();
        let repeated: Vec<u32> = assign
            .iter()
            .zip(&shares)
            .filter(|(_, sh)| **sh > 0.0)
            .filter_map(|(a, _)| if seen.contains(&a.flight) { Some(a.flight) } else { seen.push(a.flight); None })
            .collect();
        if w <= 0.0 || repeated.is_empty() {
            return vec![None; assign.len()];
        }
        let gh = self.dem.height(lat, lon);
        let o = ecef(lat, lon, gh + self.cfg.mic_height);
        let axes = enu_axes(lat, lon);
        let sound = self.sound(c0);
        let m: Vec<repeat::Match> = assign
            .iter()
            .zip(parts)
            .zip(&shares)
            .map(|((a, p), &share)| {
                let geom = match p.2 {
                    Some(hr) if share > 0.0 && repeated.contains(&a.flight) => {
                        let s = s[self.part_of_det(a.det)];
                        s.is_finite().then(|| self.pass_geom(o, axes, &sound, s, a, hr)).flatten()
                    }
                    _ => None,
                };
                repeat::Match { det: a.det, flight: a.flight, share, geom }
            })
            .collect();
        repeat::discounts(&m, w)
    }

    /// What the repeat discount takes off a total scored with
    /// [`Self::curve_parts`]: (with the soft speed/distance evidence, without it).
    fn repeat_loss(&self, assign: &[Assign], parts: &[(f64, f64, Option<f64>, f64)], reps: &[Option<Repeat>]) -> (f64, f64) {
        let mut out = (0.0, 0.0);
        for ((a, p), r) in assign.iter().zip(parts).zip(reps) {
            if let Some(r) = r {
                let x = self.dets[a.det].weight_hint() * match_share(p.0) * (1.0 - r.factor);
                out.0 += x * p.3;
                out.1 += x;
            }
        }
        out
    }

    /// [`Self::vd_loss`] of a refined hypothesis at its point (0 without the evidence).
    fn vd_loss_at(&self, r: &RefinedHyp) -> f64 {
        if !self.cfg.soft_vd {
            return 0.0;
        }
        let (h, lat, lon, s, c0, _) = r;
        let (_, parts) = self.curve_parts(*lat, *lon, s, *c0, &h.assign, 1, true);
        self.vd_loss(&h.assign, &parts)
    }

    fn c_penalty(&self, c0: f64) -> f64 {
        let (lo, hi) = self.cfg.c_bounds;
        let over = (lo - c0).max(0.0) + (c0 - hi).max(0.0);
        over * 0.5 + 0.15 * ((c0 - 340.0) / 9.0).powi(2)
    }

    /// Refine a hypothesis: maximise the curve score over east/north offset,
    /// start(s) and c0. The vector is `[east, north, start shift, c0]`, with
    /// cuts followed by the start shifts of the other parts that have a start.
    /// The score returned is without the soft speed/distance evidence.
    fn refine(&self, h: &Hyp) -> (f64, f64, Vec<f64>, f64, f64) {
        let m_lon = M_PER_DEG_LAT * h.lat.to_radians().cos();
        let active: Vec<usize> = (0..h.starts.len()).filter(|&k| h.starts[k].is_finite()).collect();
        let starts_at = |x: &[f64]| -> Vec<f64> {
            let mut s = h.starts.clone();
            for (j, &k) in active.iter().enumerate() {
                s[k] += if j == 0 { x[2] } else { x[3 + j] };
            }
            s
        };
        let obj = |x: &[f64], stride: usize| -> f64 {
            let (lat, lon) = (h.lat + x[1] / M_PER_DEG_LAT, h.lon + x[0] / m_lon);
            let c0 = x[3];
            let s = starts_at(x);
            let tot = self.curve_scores_plain(lat, lon, &s, c0, &h.assign, stride);
            let r = (x[0] * x[0] + x[1] * x[1]).sqrt();
            // the start must stay in a start window (soft wall)
            let out: f64 = active
                .iter()
                .map(|&k| {
                    let st = s[k];
                    let out_s = (self.clamp_start_in(k, st) - st).abs();
                    0.05 * (out_s - 5.0).max(0.0)
                })
                .sum();
            tot - self.c_penalty(c0) - if r > 600.0 { (r - 600.0) * 0.01 } else { 0.0 } - out - 0.01 * self.outside_region_m(lat, lon)
        };
        let extra = active.len().saturating_sub(1);
        let with = |base: [f64; 4], e: f64| -> Vec<f64> { base.iter().copied().chain(std::iter::repeat_n(e, extra)).collect() };
        let mut best = (with([0.0, 0.0, 0.0, DEFAULT_C - 6.0], 0.0), f64::NEG_INFINITY);
        for c_start in [DEFAULT_C - 10.0, DEFAULT_C + 2.0] {
            let (x, v) = nelder_mead(|x| -obj(x, 4), &with([0.0, 0.0, 0.0, c_start], 0.0), &with([120.0, 120.0, 2.0, 8.0], 2.0), 160);
            if -v > best.1 {
                best = (x, -v);
            }
        }
        let (x, _) = nelder_mead(|x| -obj(x, 2), &best.0, &with([30.0, 30.0, 0.5, 2.0], 0.5), 120);
        let score = obj(&x, 1);
        (h.lat + x[1] / M_PER_DEG_LAT, h.lon + x[0] / m_lon, starts_at(&x), x[3], score)
    }

    /// Closest approaches (flight, emission time, slant range, heard recording time) of every
    /// candidate flight to a point during the recording.
    pub(crate) fn approaches_at(&self, lat: f64, lon: f64, s: f64, c0: f64, d_max: f64) -> Vec<(u32, f64, f64, f64)> {
        let gh = self.dem.height(lat, lon);
        let o = ecef(lat, lon, gh + self.cfg.mic_height);
        let (_, _, up) = enu_axes(lat, lon);
        let sound = self.sound(c0);
        let mut out = Vec::new();
        for (fi, f) in self.flights.iter().enumerate() {
            for seg in &f.segments {
                let (a, b) = (seg.t0().max(s - 120.0), seg.t1().min(s + self.duration + 30.0));
                // (the knots' box first, with 1 m to spare for rounding)
                if b <= a
                    || seg.knot_distance_bound(o) > d_max + 15_001.0
                    || !seg.knots.iter().any(|k| norm(sub(k.p, o)) < d_max + 15_000.0)
                {
                    continue;
                }
                let r = |t: f64| norm(sub(seg.state(t).0, o));
                let step = 2.0;
                let mut t = a;
                let (mut r2, mut r1) = (f64::INFINITY, r(a));
                while t + step <= b {
                    let r0 = r(t + step);
                    if r1 < r0 && r1 <= r2 && r1 < d_max {
                        let (tc, d) = flight::closest_approach(seg, o, t - step, t + step);
                        let dz = dot(sub(seg.state(tc).0, o), up);
                        out.push((fi as u32, tc, d, tc + d / sound.ray_c(dz) - s));
                    }
                    r2 = r1;
                    r1 = r0;
                    t += step;
                }
            }
        }
        out
    }

    /// Best flight pass for every detection at a fixed point, start and c0, chosen by
    /// curve score among passes heard within the detection's time tolerance.
    fn reassign(&self, lat: f64, lon: f64, s: &[f64], c0: f64, wins: &[Window]) -> Vec<Assign> {
        let mut cands: Vec<(f64, Assign)> = Vec::new();
        // each part of the recording with its own start (one part without cuts)
        for (k, &sk) in s.iter().enumerate() {
            if !sk.is_finite() {
                continue;
            }
            let apps = self.approaches_at(lat, lon, sk, c0, self.cfg.d_cap);
            for (i, w) in wins.iter().enumerate() {
                if self.part_of_det(i) != k {
                    continue;
                }
                for &(fi, tc, d, heard) in &apps {
                    if (heard - w.tc).abs() > w.tau + 3.0 || d > 2.5 * w.d_hi {
                        continue;
                    }
                    let a = Assign { det: i, flight: fi, t_cpa: tc };
                    // (pruned: below MIN_CURVE either way)
                    let (_, parts) = self.curve_parts_with(lat, lon, s, c0, std::slice::from_ref(&a), 2, self.cfg.soft_vd, true);
                    let (r, g) = (parts[0].0, parts[0].3);
                    if r >= MIN_CURVE {
                        cands.push((r * self.dets[i].weight_hint() * g, a));
                    }
                }
            }
        }
        cands.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut out: Vec<Assign> = Vec::new();
        for (_, a) in cands {
            let taken = out.iter().any(|x| x.det == a.det || (x.flight == a.flight && (x.t_cpa - a.t_cpa).abs() < 20.0));
            if !taken {
                out.push(a);
            }
        }
        out.sort_by_key(|a| a.det);
        out
    }

    /// Recording start implied at a point by an assignment: weighted median over
    /// matched detections of (heard CPA of the assigned pass − detection tc).
    /// [`Self::implied_start`] for every part of the recording (NaN for a part
    /// without a usable assignment); `None` when no part has one.
    pub fn implied_starts(&self, lat: f64, lon: f64, c0: f64, assign: &[Assign]) -> Option<Vec<f64>> {
        if self.cuts.at.is_empty() {
            return self.implied_start(lat, lon, c0, assign).map(|s| vec![s]);
        }
        let out: Vec<f64> = (0..self.n_parts())
            .map(|k| {
                let mine: Vec<Assign> = assign.iter().copied().filter(|a| self.part_of_det(a.det) == k).collect();
                self.implied_start(lat, lon, c0, &mine).unwrap_or(f64::NAN)
            })
            .collect();
        out.iter().any(|s| s.is_finite()).then_some(out)
    }

    fn implied_start(&self, lat: f64, lon: f64, c0: f64, assign: &[Assign]) -> Option<f64> {
        if assign.is_empty() {
            return None;
        }
        let gh = self.dem.height(lat, lon);
        let o = ecef(lat, lon, gh + self.cfg.mic_height);
        let (_, _, up) = enu_axes(lat, lon);
        let sound = self.sound(c0);
        let mut v: Vec<(f64, f64)> = Vec::new();
        for a in assign {
            let Some(seg) = self.flights[a.flight as usize].segment_at(a.t_cpa) else { continue };
            let (tc, d) = flight::closest_approach(seg, o, a.t_cpa - 90.0, a.t_cpa + 90.0);
            if !d.is_finite() || d > 2.0 * self.cfg.d_cap {
                continue;
            }
            let dz = dot(sub(seg.state(tc).0, o), up);
            let det = &self.dets[a.det];
            v.push((tc + d / sound.ray_c(dz) - det.flyby.tc, det.weight_hint()));
        }
        if v.is_empty() {
            return None;
        }
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        let half = 0.5 * v.iter().map(|x| x.1).sum::<f64>();
        let mut acc = 0.0;
        for (s, w) in &v {
            acc += w;
            if acc >= half {
                return Some(*s);
            }
        }
        v.last().map(|x| x.0)
    }

    /// Wide scan of the observer position for a fixed assignment: the
    /// continuous refinement only moves a few hundred metres, but the coarse
    /// cell a group was seeded from can be kilometres from where its flights'
    /// curves fit best. Returns up to `keep` local optima `(score, lat, lon,
    /// start)` at least 400 m from each other.
    fn widen(&self, lat: f64, lon: f64, assign: &[Assign], radius: f64, step: f64, keep: usize) -> Vec<(f64, f64, f64, Vec<f64>)> {
        let m_lon = M_PER_DEG_LAT * lat.to_radians().cos();
        let n = (radius / step).round() as i32;
        let pts: Vec<(f64, f64)> = (-n..=n)
            .flat_map(|i| (-n..=n).map(move |j| (i as f64 * step, j as f64 * step)))
            .filter(|(e, nn)| e * e + nn * nn <= radius * radius)
            .map(|(e, nn)| (lat + nn / M_PER_DEG_LAT, lon + e / m_lon))
            .filter(|(la, lo)| self.region.contains(*la, *lo))
            .collect();
        let c0 = 340.0;
        let mut vals: Vec<(f64, f64, f64, Vec<f64>)> = pts
            .par_iter()
            .filter_map(|&(la, lo)| {
                let s = self.implied_starts(la, lo, c0, assign)?;
                if s.iter().enumerate().any(|(k, sk)| sk.is_finite() && !self.start_ok_in(k, *sk, 60.0)) {
                    return None;
                }
                Some((self.curve_scores_plain(la, lo, &s, c0, assign, 4), la, lo, s))
            })
            .collect();
        vals.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut out: Vec<(f64, f64, f64, Vec<f64>)> = Vec::new();
        for v in vals {
            if out.len() >= keep || v.0 <= 0.0 {
                break;
            }
            if out.iter().all(|o| geo::ground_distance(o.1, o.2, v.1, v.2) > 400.0) {
                out.push(v);
            }
        }
        out
    }

    /// Refine, re-pick the flights at the refined point, refine again; keep the better.
    /// [`Self::refine`], then [`Self::reassign`] at the refined point and
    /// refine again if that changed the assignment. Keeps the better of the
    /// two, or both when the curve score (`.5`, without the soft
    /// speed/distance evidence) and the score with the evidence disagree on
    /// which is better: the ranking uses the evidence, the margin also the
    /// curves alone (see [`margin`]), and neither may lose the hypothesis the
    /// other one prefers.
    fn refine_full(&self, h: &Hyp, wins: &[Window]) -> Vec<RefinedHyp> {
        let (lat, lon, s, c0, score) = self.refine(h);
        let a2 = self.reassign(lat, lon, &s, c0, wins);
        let first = (h.clone(), lat, lon, s.clone(), c0, score);
        let same = a2.len() == h.assign.len()
            && a2.iter().all(|a| h.assign.iter().any(|b| b.det == a.det && b.flight == a.flight && (b.t_cpa - a.t_cpa).abs() < 20.0));
        if a2.is_empty() || same {
            return vec![first];
        }
        // a part left without a matched detection has no start any more
        let starts: Vec<f64> =
            s.iter().enumerate().map(|(k, &sk)| if a2.iter().any(|a| self.part_of_det(a.det) == k) { sk } else { f64::NAN }).collect();
        let h2 = Hyp { lat, lon, ground: h.ground, starts, score: h.score, assign: a2, q: Vec::new() };
        let (lat2, lon2, s2, c2, score2) = self.refine(&h2);
        let second = (h2, lat2, lon2, s2, c2, score2);
        let curves = score2 > score;
        if !self.cfg.soft_vd {
            return vec![if curves { second } else { first }];
        }
        let soft = |r: &RefinedHyp| r.5 - self.vd_loss_at(r);
        match (curves, soft(&second) > soft(&first)) {
            (true, true) => vec![second],
            (false, false) => vec![first],
            _ => vec![first, second],
        }
    }

    /// Every pass of a candidate flight closer than `radius` (m) to a point
    /// while the recording ran (per part of the recording with a known
    /// start), in scan order, with what explains it: a matched assignment, a
    /// detection heard within 30 s, or nothing (silence evidence). Passes in
    /// the first and last `quiet_margin` s of a part are flagged as such.
    pub fn loud_passes_at(&self, lat: f64, lon: f64, s: &[f64], assign: &[Assign], radius: f64) -> Vec<LoudPass> {
        let gh = self.dem.height(lat, lon);
        let o = ecef(lat, lon, gh + self.cfg.mic_height);
        let near3 = BBox::around(lat, lon, radius + 2000.0).expand(3000.0);
        let mut out = Vec::new();
        for (part, &sk) in s.iter().enumerate() {
            if !sk.is_finite() {
                continue;
            }
            let (p0, p1) = self.part_span(part);
            let (lo, hi) = (sk + p0 - 60.0, sk + p1);
            for (fi, f) in self.flights.iter().enumerate() {
                for seg in &f.segments {
                    if seg.t1() < lo || seg.t0() > hi {
                        continue;
                    }
                    // the knots' box first: no knot can be near if the box is not
                    if !seg.geo.intersects(&near3)
                        || !seg.knots.iter().any(|k| {
                            let (la, lo, _) = geo::geodetic(k.p);
                            near3.contains(la, lo)
                        })
                    {
                        continue;
                    }
                    // scan for local minima of range
                    let (a, b) = (seg.t0().max(lo), seg.t1().min(hi));
                    let mut t = a;
                    let mut prev: Option<(f64, f64)> = None;
                    let mut prev2: Option<(f64, f64)> = None;
                    while t <= b {
                        let r = norm(sub(seg.state(t).0, o));
                        if let (Some(p1v), Some(p2v)) = (prev, prev2)
                            && p1v.1 < r
                            && p1v.1 <= p2v.1
                            && p1v.1 < radius
                        {
                            let (tc, d) = flight::closest_approach(seg, o, p1v.0 - 3.0, p1v.0 + 3.0);
                            let t_rec = tc + d / DEFAULT_C - sk;
                            let assigned = assign.iter().find(|x| x.flight == fi as u32 && (x.t_cpa - tc).abs() < 60.0).map(|x| x.det);
                            let near_det = self.dets.iter().position(|dd| (dd.flyby.tc - t_rec).abs() < 30.0);
                            let inside = t_rec > p0 + self.cfg.quiet_margin && t_rec < p1 - self.cfg.quiet_margin;
                            let (pos, vel, _) = seg.state(tc);
                            let agl_m = geo::geodetic(pos).2 - gh;
                            out.push(LoudPass {
                                flight: fi as u32,
                                part,
                                t_cpa: tc,
                                slant_m: d,
                                heard_rec: t_rec,
                                assigned,
                                near_det,
                                inside,
                                agl_m,
                                speed_ms: norm(vel),
                            });
                        }
                        prev2 = prev;
                        prev = Some((t, r));
                        t += 2.0;
                    }
                }
            }
        }
        out
    }

    /// Detection probability (soft silence only) and cost of an unheard pass
    /// of a `loud` source at slant range `slant_m`, heard at recording time
    /// `t_rec`; `low`: on or next to the ground ([`LOW_AGL_M`]), costs nothing.
    /// A detection heard 30–90 s from the pass masks it in part (stage 1
    /// does not separate two fly-bys heard that close together reliably):
    /// `p` falls linearly to 0 at 30 s.
    pub fn silence_cost(&self, slant_m: f64, loud: crate::silence::Loudness, low: bool, t_rec: f64) -> (Option<f64>, f64) {
        if !self.cfg.soft_silence() {
            return (None, self.cfg.quiet_penalty);
        }
        if low {
            return (Some(0.0), 0.0);
        }
        let m = &self.cfg.silence;
        let gap = self.dets.iter().map(|d| (d.flyby.tc - t_rec).abs()).fold(f64::INFINITY, f64::min);
        let unmasked = ((gap - MASK_NEAR_S) / (MASK_FAR_S - MASK_NEAR_S)).clamp(0.0, 1.0);
        let p = unmasked * m.p_detect(slant_m, loud, self.background.and_then(|b| b.at(t_rec)));
        (Some(p), m.cost(p))
    }

    /// [`Search::silence_cost`] of an unexplained pass from
    /// [`Search::loud_passes_at`] at a hypothesis with assignment `assign`
    /// (soft silence: a flight the hypothesis matches costs nothing for its
    /// other passes).
    pub fn loud_pass_cost(&self, p: &LoudPass, assign: &[Assign]) -> (Option<f64>, f64) {
        let f = &self.flights[p.flight as usize];
        if self.cfg.soft_silence() && assign.iter().any(|a| a.flight == p.flight) {
            return (Some(0.0), 0.0);
        }
        let low = self.cfg.soft_silence() && (p.agl_m < LOW_AGL_M || p.speed_ms < LOW_SPEED_MS);
        self.silence_cost(p.slant_m, crate::silence::Loudness::of(f.class, f.type_code.as_deref()), low, p.heard_rec)
    }

    /// Slant range within which unheard passes count at a refined point: the
    /// soft silence model's range, else the flat silence distance.
    pub fn silence_radius(&self, quiet_d: f64) -> f64 {
        if self.cfg.soft_silence() { self.cfg.silence.radius_m() } else { quiet_d }
    }

    /// Passes of candidate flights at a point during the recording that no
    /// detection explains and that cost something (with the soft silence:
    /// every pass with a detection probability worth counting).
    fn violations_at(&self, lat: f64, lon: f64, s: &[f64], assign: &[Assign], quiet_d: f64) -> Vec<Violation> {
        let mut out: Vec<Violation> = self
            .loud_passes_at(lat, lon, s, assign, self.silence_radius(quiet_d))
            .into_iter()
            .filter(|p| p.inside && p.assigned.is_none() && p.near_det.is_none())
            .filter_map(|p| {
                let (p_detect, cost) = self.loud_pass_cost(&p, assign);
                let f = &self.flights[p.flight as usize];
                (cost > 0.0).then(|| Violation {
                    hex: f.hex.clone(),
                    type_code: f.type_code.clone(),
                    heard_rec: p.heard_rec,
                    slant_m: p.slant_m,
                    p_detect,
                    cost,
                })
            })
            .collect();
        out.sort_by(|a, b| a.heard_rec.total_cmp(&b.heard_rec));
        // one pass (the closest of its local minima of range) per flight and minute
        let mut kept: Vec<Violation> = Vec::with_capacity(out.len());
        for v in out {
            match kept.last_mut() {
                Some(k) if k.hex == v.hex && (v.heard_rec - k.heard_rec).abs() < 60.0 => {
                    if v.cost > k.cost {
                        *k = v;
                    }
                }
                _ => kept.push(v),
            }
        }
        kept
    }

    fn uncertainty(&self, lat: f64, lon: f64, s: &[f64], c0: f64, assign: &[Assign], best: f64) -> Uncertainty {
        let m_lon = M_PER_DEG_LAT * lat.to_radians().cos();
        let step = 40.0;
        let n = 15;
        let temp = self.cfg.unc_temp * assign.len().max(1) as f64;
        let pts: Vec<(f64, f64)> = (-n..=n).flat_map(|i| (-n..=n).map(move |j| (i as f64 * step, j as f64 * step))).collect();
        let vals: Vec<(f64, f64, f64)> = pts
            .par_iter()
            .map(|&(e, nn)| {
                let v = self.curve_scores_plain(lat + nn / M_PER_DEG_LAT, lon + e / m_lon, s, c0, assign, 2);
                (e, nn, v)
            })
            .collect();
        let top = vals.iter().map(|v| v.2).fold(best, f64::max);
        let (mut sw, mut se, mut sn) = (0.0, 0.0, 0.0);
        for &(e, nn, v) in &vals {
            let w = ((v - top) / temp).exp();
            sw += w;
            se += w * e;
            sn += w * nn;
        }
        let (me, mn) = (se / sw, sn / sw);
        let (mut cee, mut cnn, mut cen) = (0.0, 0.0, 0.0);
        for &(e, nn, v) in &vals {
            let w = ((v - top) / temp).exp();
            cee += w * (e - me).powi(2);
            cnn += w * (nn - mn).powi(2);
            cen += w * (e - me) * (nn - mn);
        }
        let (cee, cnn, cen) = (cee / sw + step * step / 12.0, cnn / sw + step * step / 12.0, cen / sw);
        let tr = cee + cnn;
        let det = cee * cnn - cen * cen;
        let disc = (0.25 * tr * tr - det).max(0.0).sqrt();
        let (l1, l2) = (0.5 * tr + disc, (0.5 * tr - disc).max(0.0));
        let az = (0.5 * (2.0 * cen).atan2(cee - cnn)).to_degrees();
        // plus an isotropic floor for what the score surface cannot see (track and altitude errors)
        let f2 = 0.5 * self.cfg.unc_floor_m.powi(2);
        let (l1, l2) = (l1 + f2, l2 + f2);
        Uncertainty {
            radius_m: (l1 + l2).sqrt(),
            major_m: l1.sqrt(),
            minor_m: l2.sqrt(),
            azimuth_deg: (90.0 - az).rem_euclid(180.0),
        }
    }

    /// Diagnostics (stderr) for one flight at a fixed point and start: its
    /// passes, which per-detection gates they pass, and their curve scores
    /// over a small start/c0 scan.
    pub fn probe(&self, lat: f64, lon: f64, s: f64, hex: &str) {
        let wins = windows(self.dets, &self.cfg);
        let quiet_d = quiet_distance(self.dets, &self.cfg);
        let Some(fi) = self.flights.iter().position(|f| f.hex == hex || f.reg.as_deref() == Some(hex)) else {
            eprintln!("[probe] {hex}: not among the {} candidate flights", self.flights.len());
            return;
        };
        let f = &self.flights[fi];
        eprintln!("[probe] {} {:?} {:?} {:?}: {} segments", f.hex, f.reg, f.type_code, f.class, f.segments.len());
        for (k, seg) in f.segments.iter().enumerate() {
            eprintln!("[probe]   seg {k}: {} .. {} ({} knots)", utc(seg.t0()), utc(seg.t1()), seg.knots.len());
            if std::env::var_os("OVERHEAD_PROBE_KNOTS").is_some() {
                for w in seg.knots.windows(2) {
                    if (w[1].t - s).abs() < 200.0 {
                        let chord = norm(sub(w[1].p, w[0].p)) / (w[1].t - w[0].t);
                        eprintln!(
                            "[probe]     knot {} dt {:.1} chord {:.1} |v| {:.1} h {:.0} mlat {}",
                            utc(w[1].t),
                            w[1].t - w[0].t,
                            chord,
                            norm(w[1].v),
                            w[1].h,
                            w[1].mlat
                        );
                    }
                }
            }
        }
        for (i, w) in wins.iter().enumerate() {
            eprintln!(
                "[probe] window {i}: tc {:.1} v {:.0} [{:.0}..{:.0}] d {:.0} [{:.0}..{:.0}] tau {:.1}",
                w.tc, w.v, w.v_lo, w.v_hi, w.d, w.d_lo, w.d_hi, w.tau
            );
        }
        // passes found by the coarse search at the probe's cell
        let grid = Grid::new(self.cfg.grid_m);
        let cell = grid.cell(lat, lon);
        let mut recs = Vec::new();
        let tile = BBox::around(lat, lon, 2.0 * self.cfg.grid_m);
        self.passes(self.dem, fi, &tile, &wins, quiet_d, &grid, &CellSites::new(&tile, &grid), &mut recs);
        for r in recs.iter().filter(|r| r.cell == cell) {
            eprintln!(
                "[probe] coarse pass at the cell: cpa {} d {:.0} v {:.1} dets {:b} loud {} heard_rec {:.1}",
                utc(r.t_cpa),
                r.d,
                r.v,
                r.dets,
                r.loud,
                r.t_cpa + r.d as f64 / DEFAULT_C - s
            );
        }
        if !recs.iter().any(|r| r.cell == cell) {
            eprintln!("[probe] no coarse pass at the cell ({} passes in the 2-cell box)", recs.len());
        }
        for &(fj, tc, d, heard) in self.approaches_at(lat, lon, s, 340.0, self.cfg.d_cap).iter().filter(|a| a.0 as usize == fi) {
            let _ = fj;
            eprintln!("[probe] approach: cpa {} d {:.0} heard_rec {:.1}", utc(tc), d, heard);
            for (i, w) in wins.iter().enumerate() {
                let a = Assign { det: i, flight: fi as u32, t_cpa: tc };
                let mut best = (f64::NEG_INFINITY, 0.0, 0.0, 0.0);
                for ds in (-30..=30).map(|x| x as f64) {
                    for c0 in [328.0, 334.0, 340.0, 346.0] {
                        let (tot, parts) = self.curve_scores(lat, lon, &vec![s + ds; self.n_parts()], c0, std::slice::from_ref(&a), 1);
                        if parts[0].0 > best.0 {
                            best = (parts[0].0, ds, c0, tot);
                        }
                    }
                }
                let (_, p0) = self.curve_scores(lat, lon, &vec![s; self.n_parts()], 340.0, std::slice::from_ref(&a), 1);
                eprintln!(
                    "[probe]   det {i} (tc {:.1}): at the start curve {:.3} (heard CPA {:?}); best curve {:.3} at ds {:+.0} s c0 {:.0} (total {:.3}); |heard-tc| {:.1} vs tau {:.1}",
                    w.tc, p0[0].0, p0[0].2, best.0, best.1, best.2, best.3, (heard - w.tc).abs(), w.tau
                );
            }
        }
    }

    pub fn run(&self) -> (Vec<Candidate>, SearchStats) {
        let (c, s, _) = self.run_with_alternatives();
        (c, s)
    }

    /// [`Search::run`], plus every refined hypothesis that matches at least
    /// one detection (best first), for the ambiguity assessment.
    pub fn run_with_alternatives(&self) -> (Vec<Candidate>, SearchStats, Vec<Alternative>) {
        let d = self.run_detailed();
        (d.candidates, d.stats, d.alternatives)
    }

    /// [`Search::run_with_alternatives`], plus every refined hypothesis with
    /// its per-detection scores, and the coarse score map when
    /// [`Collect::coarse_cell_m`] is set (none of it changes the result).
    pub fn run_detailed(&self) -> Detailed {
        let started = std::time::Instant::now();
        let mut laps = Laps::new();
        let cfg = &self.cfg;
        let wins = windows(self.dets, cfg);
        let quiet_d = quiet_distance(self.dets, cfg);
        let grid = Grid::new(cfg.grid_m);
        let mut stats = SearchStats { flights: self.flights.len(), quiet_d, ..Default::default() };
        let coarse_cell_m = self.collect.coarse_cell_m.unwrap_or(0.0).max(cfg.grid_m);
        let coarse_grid = Grid::new(coarse_cell_m);
        let mut coarse_cells: HashMap<Cell, CoarseCell, FxBuild> = HashMap::default();
        if wins.is_empty() || self.flights.is_empty() {
            return Detailed {
                candidates: Vec::new(),
                stats,
                alternatives: Vec::new(),
                refined: Vec::new(),
                candidate_refined: Vec::new(),
                coarse: Vec::new(),
                coarse_cell_m,
            };
        }
        let d_max = wins.iter().map(|w| w.d_hi).fold(quiet_d, f64::max);
        // per-flight geographic extent for tiling
        let extents: Vec<BBox> = self
            .flights
            .iter()
            .map(|f| {
                let mut b = BBox::empty();
                for s in &f.segments {
                    // the box of every knot (min/max are exact)
                    b.extend(s.geo.lat_min, s.geo.lon_min);
                    b.extend(s.geo.lat_max, s.geo.lon_max);
                }
                b
            })
            .collect();
        // tiles: only where flights are (and their neighbours), inside the region
        let td = cfg.tile_deg;
        let mut keys: Vec<(i32, i32)> = Vec::new();
        // only where flights are while they could be heard
        let audible = |t: f64| {
            self.start_windows.iter().chain(self.cuts.windows.iter().flatten()).any(|w| t >= w.0 - 600.0 && t <= w.1 + self.duration + 300.0)
        };
        for f in self.flights {
            for s in &f.segments {
                for k in s.knots.iter().step_by(4).filter(|k| audible(k.t)) {
                    let (la, lo, _) = geo::geodetic(k.p);
                    let (i, j) = ((la / td).floor() as i32, (lo / td).floor() as i32);
                    for di in -1..=1 {
                        for dj in -1..=1 {
                            keys.push((i + di, j + dj));
                        }
                    }
                }
            }
        }
        keys.sort_unstable();
        keys.dedup();
        let tiles: Vec<BBox> = keys
            .into_iter()
            .filter_map(|(i, j)| {
                let t = BBox {
                    lat_min: i as f64 * td,
                    lat_max: (i + 1) as f64 * td,
                    lon_min: j as f64 * td,
                    lon_max: (j + 1) as f64 * td,
                };
                t.intersection(&self.region)
            })
            .collect();
        let key_of = |h: &Hyp| -> Vec<(usize, u32, i64)> {
            h.assign.iter().map(|a| (a.det, a.flight, (a.t_cpa / 60.0).round() as i64)).collect()
        };
        let mut truth_key: Option<Vec<(usize, u32, i64)>> = None;
        let mut truth_seed: Option<Hyp> = None;
        // over very large areas the coarse pass search uses coarse terrain (SRTM
        // for every tile with traffic would be tens of GB); scoring stays on SRTM
        let coarse = if tiles.len() > cfg.coarse_dem_tiles { self.dem.coarse_twin() } else { None };
        let pass_dem: &Dem = coarse.as_ref().unwrap_or(self.dem);
        if coarse.is_some() {
            eprintln!("{} search tiles: coarse terrain (Terrarium z7) for the pass search", tiles.len());
        }
        stats.coarse_terrain = coarse.is_some();
        // groups: assignment key -> best hypotheses
        let mut groups: HashMap<Vec<(usize, u32, i64)>, Vec<Hyp>> = HashMap::new();
        for tile in &tiles {
            let reach = tile.expand(d_max + 2000.0);
            let fl: Vec<usize> = (0..self.flights.len()).filter(|&i| extents[i].intersects(&reach)).collect();
            if fl.is_empty() {
                continue;
            }
            stats.tiles += 1;
            laps.lap("setup");
            pass_dem.prefetch(&reach);
            // the observer position of a cell, shared by all the tile's flights
            let sites = CellSites::new(tile, &grid);
            // (in the order of the flights, copied once)
            let per_flight: Vec<Vec<PassRec>> = fl
                .par_iter()
                .map(|&fi| {
                    let mut acc = Vec::new();
                    self.passes(pass_dem, fi, tile, &wins, quiet_d, &grid, &sites, &mut acc);
                    acc
                })
                .collect();
            let mut recs: Vec<PassRec> = per_flight.concat();
            drop(per_flight);
            stats.passes += recs.len();
            laps.lap("passes");
            recs.par_sort_unstable_by(|a, b| a.cell.cmp(&b.cell).then(a.t_cpa.total_cmp(&b.t_cpa)));
            let mut bounds = Vec::new();
            let mut i = 0;
            while i < recs.len() {
                let mut j = i + 1;
                while j < recs.len() && recs[j].cell == recs[i].cell {
                    j += 1;
                }
                if recs[i..j].iter().any(|r| r.dets != 0) {
                    bounds.push((i, j));
                }
                i = j;
            }
            stats.cells += bounds.len();
            laps.lap("sort");
            if let Some((tl, tn, _)) = self.truth {
                let tc = grid.cell(tl, tn);
                if let Some(&(a, b)) = bounds.iter().find(|(a, _)| recs[*a].cell == tc)
                    && let Some(h) = self.evaluate_cell(&recs[a..b], &wins, &grid)
                {
                    // refined later, alongside the search's own refinement (it is independent of it)
                    truth_seed = Some(h.clone());
                    truth_key = Some(key_of(&h));
                    stats.truth_hypothesis = Some(TruthHyp {
                        group_rank: None,
                        sieve_rank: None,
                        refined_in_search: false,
                        coarse: h.score,
                        refined: f64::NAN,
                        dist_m: f64::NAN,
                        assign: h
                            .assign
                            .iter()
                            .map(|x| (x.det, self.flights[x.flight as usize].hex.clone(), x.t_cpa))
                            .collect(),
                    });
                }
            }
            laps.lap("truth");
            let hyps: Vec<Hyp> =
                bounds.par_iter().filter_map(|&(a, b)| self.evaluate_cell(&recs[a..b], &wins, &grid)).collect();
            if self.collect.coarse_cell_m.is_some() {
                for h in &hyps {
                    let c = coarse_grid.cell(h.lat, h.lon);
                    let e = coarse_cells.entry(c).or_insert_with(|| {
                        let (lat, lon) = coarse_grid.centre(c);
                        CoarseCell { lat, lon, combined: f64::NEG_INFINITY, assign: Vec::new(), starts: Vec::new(), per_det: vec![None; wins.len()] }
                    });
                    if h.score > e.combined {
                        e.combined = h.score;
                        e.assign = h.assign.clone();
                        e.starts = h.starts.clone();
                    }
                    for (a, &q) in h.assign.iter().zip(&h.q) {
                        let s = h.starts[self.part_of_det(a.det)];
                        if e.per_det[a.det].is_none_or(|p| q > p.0) {
                            e.per_det[a.det] = Some((q, *a, s));
                        }
                    }
                }
            }
            for h in hyps {
                let key = key_of(&h);
                let g = groups.entry(key).or_default();
                g.push(h);
                if g.len() > 12 {
                    g.sort_by(|a, b| b.score.total_cmp(&a.score));
                    g.truncate(6);
                }
            }
            laps.lap("cells");
        }
        laps.lap("groups");
        stats.groups = groups.len();
        // best few cells of the best groups
        let mut group_list: Vec<(Vec<(usize, u32, i64)>, Vec<Hyp>)> = groups
            .into_iter()
            .map(|(k, mut g)| {
                g.sort_by(|a, b| b.score.total_cmp(&a.score));
                g.truncate(3);
                (k, g)
            })
            .collect();
        group_list.sort_by(|a, b| b.1[0].score.total_cmp(&a.1[0].score));
        if let (Some(t), Some(k)) = (stats.truth_hypothesis.as_mut(), &truth_key) {
            t.group_rank = group_list.iter().position(|g| &g.0 == k).map(|r| r + 1);
        }
        let mut group_list: Vec<Vec<Hyp>> = group_list.into_iter().map(|g| g.1).collect();
        // first sieve: the curve score at each group's best cell with the start
        // its passes imply there (cheap, so far more groups than the coarse
        // ranking alone could afford)
        group_list.truncate(if wins.len() <= 1 { cfg.sieve * 2 } else { cfg.sieve });
        let mut sieve: Vec<(f64, Vec<Hyp>)> = group_list
            .into_par_iter()
            .map(|g| {
                let h = &g[0];
                let mut best = f64::NEG_INFINITY;
                for c0 in [334.0, 346.0] {
                    if let Some(mut s) = self.implied_starts(h.lat, h.lon, c0, &h.assign) {
                        // (a part whose start cannot be implied keeps the coarse one; never without cuts)
                        for (sk, hk) in s.iter_mut().zip(&h.starts) {
                            if !sk.is_finite() {
                                *sk = *hk;
                            }
                        }
                        best = best.max(self.curve_scores_plain(h.lat, h.lon, &s, c0, &h.assign, 4));
                    }
                    best = best.max(self.curve_scores_plain(h.lat, h.lon, &h.starts, c0, &h.assign, 4));
                }
                // ties (e.g. nothing fits yet) keep the coarse order
                (best + 1e-3 * h.score, g)
            })
            .collect();
        laps.lap("sieve");
        sieve.sort_by(|a, b| b.0.total_cmp(&a.0));
        if let (Some(t), Some(k)) = (stats.truth_hypothesis.as_mut(), &truth_key) {
            t.sieve_rank = sieve.iter().position(|g| &key_of(&g.1[0]) == k).map(|r| r + 1);
        }
        let n_refine = if wins.len() <= 1 { cfg.refine * 4 } else { cfg.refine };
        sieve.truncate(n_refine);
        let group_list: Vec<Vec<Hyp>> = sieve.into_iter().map(|x| x.1).collect();
        let seeds: Vec<Hyp> = group_list.into_iter().flat_map(|g| g.into_iter().take(2)).collect();
        // cheap curve score at the seed (small start/c0 scan) decides which seeds get the full refinement
        let mut quick: Vec<(f64, Hyp)> = seeds
            .into_par_iter()
            .map(|h| {
                let mut best = f64::NEG_INFINITY;
                for ds in [-6.0, -3.0, 0.0, 3.0, 6.0] {
                    for c0 in [334.0, 343.0] {
                        let s: Vec<f64> = h.starts.iter().map(|s| s + ds).collect();
                        best = best.max(self.curve_scores_plain(h.lat, h.lon, &s, c0, &h.assign, 4));
                    }
                }
                (best, h)
            })
            .collect();
        laps.lap("quick");
        quick.sort_by(|a, b| b.0.total_cmp(&a.0));
        quick.truncate(if wins.len() <= 1 { cfg.refine_full * 2 } else { cfg.refine_full });
        let seeds: Vec<Hyp> = quick.into_iter().map(|q| q.1).collect();
        stats.refined = seeds.len();
        if let (Some(t), Some(k)) = (stats.truth_hypothesis.as_mut(), &truth_key) {
            t.refined_in_search = seeds.iter().any(|h| &key_of(h) == k);
        }
        let ground = |mut r: RefinedHyp| {
            r.0.ground = self.dem.height(r.1, r.2);
            r
        };
        let (mut refined, truth_refined): (Vec<RefinedHyp>, _) = rayon::join(
            || seeds.par_iter().flat_map_iter(|h| self.refine_full(h, &wins).into_iter().map(ground)).collect(),
            || truth_seed.as_ref().and_then(|h| self.refine_full(h, &wins).into_iter().max_by(|a, b| a.5.total_cmp(&b.5))),
        );
        if let (Some(t), Some((_, lat, lon, _, _, score)), Some((tl, tn, _))) =
            (stats.truth_hypothesis.as_mut(), truth_refined, self.truth)
        {
            t.refined = score;
            t.dist_m = geo::ground_distance(lat, lon, tl, tn);
        }
        laps.lap("refine");
        // the best distinct assignments also get a wide position scan
        let mut order: Vec<usize> = (0..refined.len()).collect();
        order.sort_by(|a, b| refined[*b].5.total_cmp(&refined[*a].5));
        let mut picked: Vec<usize> = Vec::new();
        for i in order {
            if picked.len() >= cfg.widen {
                break;
            }
            let k = key_of(&refined[i].0);
            if refined[i].5 > 0.0 && picked.iter().all(|&j| key_of(&refined[j].0) != k) {
                picked.push(i);
            }
        }
        let extra: Vec<RefinedHyp> = picked
            .iter()
            .flat_map(|&i| {
                let (h, lat, lon, _, _, _) = &refined[i];
                self.widen(*lat, *lon, &h.assign, cfg.widen_radius, 250.0, 2)
                    .into_iter()
                    .filter(|v| geo::ground_distance(v.1, v.2, *lat, *lon) > 400.0)
                    .map(|v| Hyp { lat: v.1, lon: v.2, ground: 0.0, starts: v.3, score: h.score, assign: h.assign.clone(), q: Vec::new() })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .par_iter()
            .flat_map_iter(|h| self.refine_full(h, &wins).into_iter().map(ground))
            .collect();
        laps.lap("widen");
        stats.widened = extra.len();
        refined.extend(extra);
        // silence at the refined point, then rank
        // ranked by the score with the silence cost and the soft
        // speed/distance evidence; for the margin also without either
        // (`.3`, see `margin`; the flat silence term stays in it)
        // and repeated passes of one flight count less (both scores)
        #[allow(clippy::type_complexity)]
        let mut ranked: Vec<(f64, usize, Vec<Violation>, f64, (Vec<Option<Repeat>>, [f64; 2]))> = refined
            .par_iter()
            .enumerate()
            .map(|(i, r)| {
                let (h, lat, lon, s, c0, curve) = r;
                let v = self.violations_at(*lat, *lon, s, &h.assign, quiet_d);
                let silence = silence_penalty(&v);
                let (_, parts) = self.curve_parts(*lat, *lon, s, *c0, &h.assign, 1, cfg.soft_vd);
                let vd = if cfg.soft_vd { self.vd_loss(&h.assign, &parts) } else { 0.0 };
                let reps = self.repeats_at(*lat, *lon, s, *c0, &h.assign, &parts);
                let (rep, rep_plain) = self.repeat_loss(&h.assign, &parts, &reps);
                let plain = if cfg.soft_silence() { *curve } else { curve - silence } - rep_plain;
                (curve - silence - vd - rep, i, v, plain, (reps, [rep, rep_plain]))
            })
            .collect();
        laps.lap("violations");
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
        // every refined hypothesis that would be reportable (some match above
        // MIN_CURVE at full resolution), not only the top list, counts as an
        // alternative for the margin
        let alt_parts: Vec<(Option<Alternative>, Vec<(f64, f64)>)> = ranked
            .par_iter()
            .map(|r| {
                let (h, lat, lon, s, c0, _) = &refined[r.1];
                let parts = self.curve_scores(*lat, *lon, s, *c0, &h.assign, 1).1;
                let kept: Vec<(f64, f64)> = parts.iter().map(|p| (p.0, p.1)).collect();
                if !parts.iter().any(|p| p.0 >= MIN_CURVE) {
                    return (None, kept);
                }
                let plain = r.3;
                let flights = h
                    .assign
                    .iter()
                    .zip(&parts)
                    .filter(|(_, p)| p.0 >= MIN_CURVE)
                    .map(|(a, _)| {
                        let f = &self.flights[a.flight as usize];
                        let id = f.reg.clone().unwrap_or_else(|| f.hex.clone());
                        match &f.type_code {
                            Some(t) => format!("{id} {t}"),
                            None => id,
                        }
                    })
                    .collect();
                let rl = (r.4).1;
                let repeat_loss = (rl[0] > 0.0 || rl[1] > 0.0).then_some(rl);
                (Some(Alternative { lat: *lat, lon: *lon, score: r.0, plain_score: Some(plain), repeat_loss, start_unix: first_start(s), flights }), kept)
            })
            .collect();
        let mut alternatives: Vec<Alternative> = Vec::new();
        let mut all_parts: Vec<Vec<(f64, f64)>> = Vec::new();
        for (a, p) in alt_parts {
            alternatives.extend(a);
            all_parts.push(p);
        }
        laps.lap("alternatives");
        // every refined hypothesis, ranked (for score scatter plots and maps)
        let mut refined_out: Vec<Refined> = ranked
            .iter()
            .zip(all_parts)
            .map(|((total, i, viol, _, _), parts)| {
                let (h, lat, lon, s, c0, curve) = &refined[*i];
                Refined {
                    lat: *lat,
                    lon: *lon,
                    starts: s.clone(),
                    c0: *c0,
                    curve: *curve,
                    total: *total,
                    coarse: h.score,
                    assign: h.assign.clone(),
                    parts,
                    violations: viol.clone(),
                }
            })
            .collect();
        let mut candidate_refined: Vec<usize> = Vec::new();
        let mut out: Vec<Candidate> = Vec::new();
        // each candidate's score without the soft speed/distance evidence,
        // and what the repeat discount took off both
        let mut plain_scores: Vec<(f64, [f64; 2])> = Vec::new();
        for (ri, (total, i, viol, plain, (reps, rep_loss))) in ranked.into_iter().enumerate() {
            if out.len() >= cfg.top {
                break;
            }
            let (h, lat, lon, s, c0, _) = &refined[i];
            if out.iter().any(|c| geo::ground_distance(c.lat, c.lon, *lat, *lon) < 500.0) {
                continue;
            }
            let (_, parts) = self.curve_parts(*lat, *lon, s, *c0, &h.assign, 1, cfg.soft_vd);
            // (the score surface is the curve score, before the repeat discount)
            let unc = self.uncertainty(*lat, *lon, s, *c0, &h.assign, plain + rep_loss[1]);
            let gh = self.dem.height(*lat, *lon);
            let o = ecef(*lat, *lon, gh + cfg.mic_height);
            let (_, _, up) = enu_axes(*lat, *lon);
            let flights: Vec<MatchedFlight> = h
                .assign
                .iter()
                .zip(&parts)
                .zip(&reps)
                .map(|((a, &(r, m, cross, g)), rep)| {
                    let f = &self.flights[a.flight as usize];
                    let seg = f.segment_at(a.t_cpa).expect("assigned segment");
                    let sound = self.sound(*c0);
                    let s = s[self.part_of_det(a.det)];
                    // the pass that was scored: the heard CPA nearest the detection's tc
                    let (tc, d) = match cross {
                        Some(hr) => {
                            let mut hint = 0;
                            let h = flight::heard(seg, o, up, &sound, s + hr, None, &mut hint);
                            match flight::closest_approach(seg, o, h.tau - 4.0, h.tau + 4.0) {
                                // within EXTRAP_S past the last (or before the first) report
                                (_, d) if !d.is_finite() => (h.tau, h.range),
                                x => x,
                            }
                        }
                        None => flight::closest_approach(seg, o, a.t_cpa - 30.0, a.t_cpa + 30.0),
                    };
                    let (p, v, _) = seg.state(tc);
                    let (_, _, hp) = geo::geodetic(p);
                    let heard = tc + d / sound.ray_c(hp - gh);
                    MatchedFlight {
                        detection: a.det,
                        tc: self.dets[a.det].flyby.tc,
                        hex: f.hex.clone(),
                        reg: f.reg.clone(),
                        type_code: f.type_code.clone(),
                        class: f.class,
                        callsign: f.callsign_at(tc).map(str::to_string),
                        cpa_utc: utc(tc),
                        heard_utc: utc(heard),
                        heard_rec: heard - s,
                        slant_m: d,
                        speed_ms: norm(v),
                        height_agl_m: hp - gh,
                        curve_score: r,
                        f0_scale: m,
                        vd_evidence: g,
                        repeat: *rep,
                    }
                })
                .collect();
            let flights: Vec<MatchedFlight> = flights.into_iter().filter(|m| m.curve_score >= MIN_CURVE).collect();
            if flights.is_empty() {
                continue;
            }
            let s0 = first_start(s);
            out.push(Candidate {
                rank: out.len() + 1,
                lat: *lat,
                lon: *lon,
                ground_m: self.dem.height_msl(*lat, *lon),
                score: total,
                margin: 0.0,
                coarse_score: h.score,
                matched: flights.len(),
                start_utc: utc(s0),
                start_unix: s0,
                c0: *c0,
                uncertainty: unc,
                flights,
                violations: viol,
                truth_score: self.truth.map(|(tl, tn, ts)| {
                    (0..=34)
                        .map(|k| {
                            let c0 = 322.0 + k as f64;
                            self.curve_scores(tl, tn, &vec![ts; self.n_parts()], c0, &h.assign, 1).0 - self.c_penalty(c0)
                        })
                        .fold(f64::NEG_INFINITY, f64::max)
                }),
                probability: None,
                same_place_as: None,
                part_starts_unix: (!self.cuts.at.is_empty()).then(|| s.iter().map(|x| x.is_finite().then_some(*x)).collect()),
                start_part: (!self.cuts.at.is_empty()).then(|| s.iter().position(|x| x.is_finite()).unwrap_or(0)),
            });
            candidate_refined.push(ri);
            plain_scores.push((plain, rep_loss));
        }
        for (c, &(plain, loss)) in out.iter_mut().zip(&plain_scores) {
            c.margin = margin_with_repeats(c.lat, c.lon, c.score, plain, loss, &alternatives);
        }
        laps.lap("output");
        laps.print();
        stats.seconds = started.elapsed().as_secs_f64();
        for r in refined_out.iter_mut() {
            r.starts.shrink_to_fit();
        }
        let mut coarse: Vec<CoarseCell> = coarse_cells.into_values().collect();
        coarse.sort_by(|a, b| a.lat.total_cmp(&b.lat).then(a.lon.total_cmp(&b.lon)));
        Detailed { candidates: out, stats, alternatives, refined: refined_out, candidate_refined, coarse, coarse_cell_m }
    }
}

/// Margin of a candidate at `lat`, `lon` scoring `score` (`plain` without
/// the soft speed/distance evidence and the soft silence cost) over the best
/// alternative more than 2 km away (alternatives score 0 when there is none).
///
/// The evidence and the silence rank, but they must not make an answer look
/// surer than the curves alone do: the evidence weighs which flight explains
/// a detection (a likelihood ratio of 0.3-1), the silence model rests on a
/// few dozen real passes, neither was calibrated for the margin, and
/// discounting a wrong place's competitor would raise that wrong place's
/// margin. So the margin is the smaller of the one with and the one without
/// them.
pub fn margin(lat: f64, lon: f64, score: f64, plain: f64, alternatives: &[Alternative]) -> f64 {
    let far = || alternatives.iter().filter(|o| geo::ground_distance(o.lat, o.lon, lat, lon) > 2000.0);
    let other = far().map(|o| o.score).fold(0.0, f64::max);
    let other_plain = far().map(|o| o.plain()).fold(0.0, f64::max);
    (score - other).min(plain - other_plain)
}

/// [`margin`] with the repeat discount ([`crate::repeat`]; `repeat_loss` =
/// what it took off `score` and `plain`): the smaller of the margin with the
/// discount and the one without it on every score. The discount is meant to
/// keep one aircraft flying circuits from making a wrong place look sure; a
/// circuit at a competitor must not make #1 look surer for it, so it never
/// raises a margin (as the soft evidence and the silence).
pub fn margin_with_repeats(lat: f64, lon: f64, score: f64, plain: f64, repeat_loss: [f64; 2], alternatives: &[Alternative]) -> f64 {
    let with = margin(lat, lon, score, plain, alternatives);
    if alternatives.iter().all(|a| a.repeat_loss.is_none()) && repeat_loss == [0.0; 2] {
        return with;
    }
    let undiscounted: Vec<Alternative> = alternatives.iter().map(Alternative::undiscounted).collect();
    with.min(margin(lat, lon, score + repeat_loss[0], plain + repeat_loss[1], &undiscounted))
}

/// The first known start of a hypothesis (the only one without cuts).
fn first_start(s: &[f64]) -> f64 {
    s.iter().copied().find(|x| x.is_finite()).unwrap_or(f64::NAN)
}

/// One pass of [`Search::heard_factors`] in progress.
struct HeardSweep<'a> {
    seg: &'a flight::Segment,
    s: f64,
    k: Vec<f64>,
    crossings: Vec<f64>,
    hint: usize,
    guess: Option<f64>,
    /// The heard state at `guess` (the last time's).
    last: Option<flight::Heard>,
    prev: Option<(f64, f64)>,
    lo: f64,
    hi: f64,
}

impl<'a> HeardSweep<'a> {
    fn new(seg: &'a flight::Segment, s: f64, n: usize) -> Self {
        // the track is only known between its first and last report (plus
        // one report interval, EXTRAP_S): beyond that neither a closest
        // approach nor a Doppler sample may be scored
        let (lo, hi) = (seg.t0() - EXTRAP_S, seg.t1() + EXTRAP_S);
        Self { seg, s, k: Vec::with_capacity(n), crossings: Vec::new(), hint: 0, guess: None, last: None, prev: None, lo, hi }
    }

    /// The next recording time.
    #[inline(always)]
    fn step(&mut self, t: f64, o: V3, up: V3, sound: &Sound) {
        let h = flight::heard(self.seg, o, up, sound, self.s + t, self.guess, &mut self.hint);
        self.finish(t, h, sound);
    }

    /// The rest of [`Self::step`] from the heard state at recording time `t`.
    #[inline(always)]
    fn finish(&mut self, t: f64, h: flight::Heard, sound: &Sound) {
        self.guess = Some(h.tau);
        self.last = Some(h);
        let inside = h.tau >= self.lo && h.tau <= self.hi;
        if !inside {
            // unknown: scored as the noise floor (see `Observed::score`)
            self.k.push(f64::NAN);
            self.prev = None;
            return;
        }
        if let Some((tp, vp)) = self.prev
            && vp < 0.0
            && h.v_radial >= 0.0
        {
            self.crossings.push(tp + (t - tp) * (-vp) / (h.v_radial - vp).max(1e-9));
        }
        self.prev = Some((t, h.v_radial));
        self.k.push(flight::doppler_factor(&h, sound));
    }
}

/// Phase timings on stderr when `OVERHEAD_TIMING` is set.
struct Laps {
    on: bool,
    last: std::time::Instant,
    acc: Vec<(&'static str, f64)>,
}

impl Laps {
    fn new() -> Self {
        Self { on: std::env::var_os("OVERHEAD_TIMING").is_some(), last: std::time::Instant::now(), acc: Vec::new() }
    }
    /// Time since the previous lap, added to `name`.
    fn lap(&mut self, name: &'static str) {
        let now = std::time::Instant::now();
        let dt = (now - self.last).as_secs_f64();
        self.last = now;
        match self.acc.iter_mut().find(|a| a.0 == name) {
            Some(a) => a.1 += dt,
            None => self.acc.push((name, dt)),
        }
    }
    fn print(&self) {
        if self.on {
            let parts: Vec<String> = self.acc.iter().map(|(n, t)| format!("{n} {t:.2}s")).collect();
            eprintln!("[timing] {}", parts.join(", "));
        }
    }
}

/// Lazily computed values (the observer position and ground height) of the
/// cells of one search tile, shared by the threads working on its flights:
/// one slot per cell, in rows of the tile's latitude span (a column range per
/// row). A cell outside the slots (or a tile too large for them) has none.
struct CellSites {
    row0: i32,
    col0: Vec<i32>,
    width: usize,
    slots: Vec<std::sync::OnceLock<(V3, f64)>>,
}

impl CellSites {
    /// At most this many slots (≈ 40 bytes each; a 1° tile of 200 m cells
    /// has ≈ 0.25 M).
    const MAX_SLOTS: usize = 1 << 21;

    fn new(tile: &BBox, grid: &Grid) -> Self {
        let none = Self { row0: 0, col0: Vec::new(), width: 0, slots: Vec::new() };
        let (r0, r1) = (geo::floor_i32(tile.lat_min / grid.dlat).saturating_sub(1), geo::floor_i32(tile.lat_max / grid.dlat).saturating_add(1));
        let n_rows = (r1 as i64 - r0 as i64 + 1).max(0) as usize;
        if n_rows == 0 || n_rows > Self::MAX_SLOTS {
            return none;
        }
        let mut col0 = Vec::with_capacity(n_rows);
        let mut width = 0i64;
        for r in r0..=r1 {
            let d = grid.dlon(r);
            let (c0, c1) = (geo::floor_i32(tile.lon_min / d).saturating_sub(1), geo::floor_i32(tile.lon_max / d).saturating_add(1));
            col0.push(c0);
            width = width.max(c1 as i64 - c0 as i64 + 1);
        }
        let width = width.max(0) as usize;
        if width == 0 || n_rows.saturating_mul(width) > Self::MAX_SLOTS {
            return none;
        }
        let slots = (0..n_rows * width).map(|_| std::sync::OnceLock::new()).collect();
        Self { row0: r0, col0, width, slots }
    }

    #[inline]
    fn slot(&self, c: Cell) -> Option<&std::sync::OnceLock<(V3, f64)>> {
        let r = (c.0 as i64 - self.row0 as i64) as usize;
        let c0 = *self.col0.get(r)?;
        let k = (c.1 as i64 - c0 as i64) as usize;
        if k >= self.width {
            return None;
        }
        self.slots.get(r * self.width + k)
    }
}

/// The cells of the lattice points `a + s` (east, north offsets in m) around
/// `lat_ctx` (`s` given as its east and north parts, `s0` and `s1`), in
/// any order (they are sorted afterwards): eight at a time with AVX-512 (the
/// same divisions and floors as [`geo::GridRows::cell`], a lane out of `i32`
/// range done by it), else one by one. `s0` and `s1` may be padded.
fn lattice_cells(rows: &mut geo::GridRows, lat_ctx: &Lattice, a: &[(f64, f64)], s0: &mut Vec<f64>, s1: &mut Vec<f64>, cells: &mut Vec<Cell>) {
    #[cfg(target_arch = "x86_64")]
    if lattice_avx512() {
        let n = s0.len();
        s0.resize(n.div_ceil(8) * 8, 0.0);
        s1.resize(n.div_ceil(8) * 8, 0.0);
        // SAFETY: AVX-512F/DQ checked; s0 and s1 padded to whole vectors
        unsafe { lattice_cells_avx512(rows, lat_ctx, a, s0, s1, n, cells) };
        return;
    }
    for &(a0, a1) in a {
        for (&x, &y) in s0.iter().zip(s1.iter()) {
            let (la, lo) = lat_ctx.offset(a0 + x, a1 + y);
            cells.push(rows.cell(la, lo));
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn lattice_avx512() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        std::arch::is_x86_feature_detected!("avx512f")
            && std::arch::is_x86_feature_detected!("avx512dq")
            && std::env::var_os("OVERHEAD_NO_AVX512").is_none()
    })
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f,avx512dq")]
unsafe fn lattice_cells_avx512(rows: &mut geo::GridRows, lat_ctx: &Lattice, a: &[(f64, f64)], s0: &[f64], s1: &[f64], n: usize, cells: &mut Vec<Cell>) {
    use std::arch::x86_64::*;
    let set = _mm512_set1_pd;
    let (lat, lon, mpd, m_lon, dlat) = (set(lat_ctx.lat), set(lat_ctx.lon), set(M_PER_DEG_LAT), set(lat_ctx.m_lon), set(rows.dlat()));
    let (i_lo, i_hi) = (set(i32::MIN as f64), set(i32::MAX as f64));
    let np = n.div_ceil(8) * 8;
    assert!(s0.len() >= np && s1.len() >= np);
    let floor = |x: __m512d| _mm512_roundscale_pd::<{ _MM_FROUND_TO_NEG_INF | _MM_FROUND_NO_EXC }>(x);
    // (floored values within i32, false for NaN)
    let in_i32 = |x: __m512d| _mm512_cmp_pd_mask::<_CMP_GE_OQ>(x, i_lo) & _mm512_cmp_pd_mask::<_CMP_LE_OQ>(x, i_hi);
    let (mut la_a, mut lo_a, mut dl) = ([0.0f64; 8], [0.0f64; 8], [1.0f64; 8]);
    let (mut row_a, mut col_a) = ([0i32; 8], [0i32; 8]);
    for &(a0, a1) in a {
        let (a0, a1) = (set(a0), set(a1));
        for k in (0..np).step_by(8) {
            let m = (n - k).min(8);
            let mask: __mmask8 = ((1u32 << m) - 1) as u8;
            // SAFETY: k + 8 <= np
            let (e, nn) = unsafe { (_mm512_add_pd(a0, _mm512_loadu_pd(s0.as_ptr().add(k))), _mm512_add_pd(a1, _mm512_loadu_pd(s1.as_ptr().add(k)))) };
            // `Lattice::offset`
            let la = _mm512_add_pd(lat, _mm512_div_pd(nn, mpd));
            let lo = _mm512_add_pd(lon, _mm512_div_pd(e, m_lon));
            let rf = floor(_mm512_div_pd(la, dlat));
            if in_i32(rf) & mask != mask {
                // SAFETY: 8 lanes each
                unsafe {
                    _mm512_storeu_pd(la_a.as_mut_ptr(), la);
                    _mm512_storeu_pd(lo_a.as_mut_ptr(), lo);
                }
                for l in 0..m {
                    cells.push(rows.cell(la_a[l], lo_a[l]));
                }
                continue;
            }
            // SAFETY: 8 lanes
            unsafe { _mm256_storeu_si256(row_a.as_mut_ptr().cast(), _mm512_cvtpd_epi32(rf)) };
            for l in 0..m {
                dl[l] = rows.dlon(row_a[l]);
            }
            // SAFETY: 8 lanes
            let cf = floor(_mm512_div_pd(lo, unsafe { _mm512_loadu_pd(dl.as_ptr()) }));
            if in_i32(cf) & mask != mask {
                unsafe {
                    _mm512_storeu_pd(la_a.as_mut_ptr(), la);
                    _mm512_storeu_pd(lo_a.as_mut_ptr(), lo);
                }
                for l in 0..m {
                    cells.push(rows.cell(la_a[l], lo_a[l]));
                }
                continue;
            }
            // SAFETY: 8 lanes
            unsafe { _mm256_storeu_si256(col_a.as_mut_ptr().cast(), _mm512_cvtpd_epi32(cf)) };
            for l in 0..m {
                cells.push((row_a[l], col_a[l]));
            }
        }
    }
}

/// Sorting and deduplicating a sample's cells: when they span a small
/// block of rows and columns (they do, a strip around one path segment),
/// through a bitmap of that block read in (row, column) order, which gives
/// exactly `sort_unstable` + `dedup`.
#[derive(Default)]
struct CellSet {
    bits: Vec<u64>,
}

impl CellSet {
    /// Largest block (cells) done through the bitmap.
    const MAX_BITS: usize = 1 << 16;

    fn sort_dedup(&mut self, cells: &mut Vec<Cell>) {
        let Some(&first) = cells.first() else { return };
        let (mut r_lo, mut r_hi, mut c_lo, mut c_hi) = (first.0, first.0, first.1, first.1);
        for &(r, c) in cells.iter() {
            r_lo = r_lo.min(r);
            r_hi = r_hi.max(r);
            c_lo = c_lo.min(c);
            c_hi = c_hi.max(c);
        }
        let (h, w) = ((r_hi as i64 - r_lo as i64 + 1) as u64, (c_hi as i64 - c_lo as i64 + 1) as u64);
        if h.saturating_mul(w) > Self::MAX_BITS as u64 {
            cells.sort_unstable();
            cells.dedup();
            return;
        }
        let w = w as usize;
        let n = h as usize * w;
        self.bits.clear();
        self.bits.resize(n.div_ceil(64), 0);
        for &(r, c) in cells.iter() {
            let i = (r - r_lo) as usize * w + (c - c_lo) as usize;
            self.bits[i >> 6] |= 1 << (i & 63);
        }
        cells.clear();
        for (wi, &word) in self.bits.iter().enumerate() {
            let mut b = word;
            while b != 0 {
                let i = wi * 64 + b.trailing_zeros() as usize;
                b &= b - 1;
                cells.push((r_lo + (i / w) as i32, c_lo + (i % w) as i32));
            }
        }
    }
}

/// A small multiplicative hasher for cell keys (lookups only; iteration
/// order is never used).
#[derive(Default)]
struct FxHasher(u64);

impl std::hash::Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
    fn write_i32(&mut self, i: i32) {
        self.0 = (self.0.rotate_left(5) ^ i as u32 as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type FxBuild = std::hash::BuildHasherDefault<FxHasher>;

/// What a candidate's silence violations cost: every flight its most
/// expensive unheard pass (the flat silence: `quiet_penalty` per flight).
pub fn silence_penalty(v: &[Violation]) -> f64 {
    let mut worst: Vec<(&str, f64)> = Vec::new();
    for x in v {
        match worst.iter_mut().find(|w| w.0 == x.hex) {
            Some(w) => w.1 = w.1.max(x.cost),
            None => worst.push((&x.hex, x.cost)),
        }
    }
    worst.iter().map(|w| w.1).sum()
}

/// Weight of a detection in the combined score.
pub trait WeightHint {
    fn weight_hint(&self) -> f64;
}

impl WeightHint for Detection {
    fn weight_hint(&self) -> f64 {
        let mut w = if self.confident { 1.0 } else { 0.6 };
        if self.likely_ground_vehicle {
            w *= 0.5;
        }
        w
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use overhead_core::fit::{Ranges, Region};
    use overhead_core::model::Flyby;

    fn xorshift(seed: u64) -> impl FnMut() -> f64 {
        let mut st = seed;
        move || {
            st ^= st << 13;
            st ^= st >> 7;
            st ^= st << 17;
            (st >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// The vector lattice gives the cells `GridRows::cell` gives, point by
    /// point (as sorted sets: the order is not used), also where a lane is
    /// out of `i32` range, NaN or right on a cell edge.
    #[test]
    fn lattice_cells_match_grid_rows() {
        let mut rnd = xorshift(0x1234_5678_9abc_def1);
        let grid = Grid::new(200.0);
        for case in 0..3000 {
            let lat = (rnd() - 0.5) * 170.0;
            let lon = (rnd() - 0.5) * 360.0;
            let ctx = Lattice { lat, lon, m_lon: M_PER_DEG_LAT * lat.to_radians().cos() };
            let na = (rnd() * 8.0) as usize;
            let ang = rnd() * std::f64::consts::TAU;
            let a: Vec<(f64, f64)> = (0..=na).map(|i| (-100.0 + i as f64 * 100.0) * ang.cos()).map(|x| (x, x * 0.7)).collect();
            let n = (rnd() * 40.0) as usize;
            let mut s0: Vec<f64> = (0..n).map(|_| (rnd() - 0.5) * 20000.0).collect();
            let mut s1: Vec<f64> = (0..n).map(|_| (rnd() - 0.5) * 20000.0).collect();
            if case % 50 == 7 && n > 0 {
                s0[0] = f64::NAN;
                s1[n - 1] = 1e300;
            }
            if case % 50 == 9 && n > 1 {
                // a point right on a row edge
                s1[1] = ((lat / grid.dlat).round() * grid.dlat - lat) * M_PER_DEG_LAT;
            }
            let mut want: Vec<Cell> = Vec::new();
            let mut rows = geo::GridRows::new(grid, lat - 1.0, lat + 1.0);
            for &(a0, a1) in &a {
                for k in 0..n {
                    let (la, lo) = ctx.offset(a0 + s0[k], a1 + s1[k]);
                    want.push(rows.cell(la, lo));
                }
            }
            let mut got = Vec::new();
            let mut rows = geo::GridRows::new(grid, lat - 1.0, lat + 1.0);
            lattice_cells(&mut rows, &ctx, &a, &mut s0, &mut s1, &mut got);
            want.sort_unstable();
            got.sort_unstable();
            assert_eq!(got, want, "case {case}");
        }
    }

    /// The bitmap path of `CellSet` is `sort_unstable` + `dedup`.
    #[test]
    fn cell_set_sorts_and_dedups() {
        let mut rnd = xorshift(0x0bad_cafe_f00d_1234);
        let mut set = CellSet::default();
        for case in 0..2000 {
            let n = (rnd() * 400.0) as usize;
            let (r0, c0) = (((rnd() - 0.5) * 2e5) as i32, ((rnd() - 0.5) * 2e5) as i32);
            let span = if case % 10 == 0 { 1e6 } else { 1.0 + rnd() * 120.0 };
            let mut cells: Vec<Cell> = (0..n).map(|_| (r0 + (rnd() * span) as i32, c0 - (rnd() * span) as i32)).collect();
            let mut want = cells.clone();
            want.sort_unstable();
            want.dedup();
            set.sort_dedup(&mut cells);
            assert_eq!(cells, want, "case {case}");
        }
    }

    fn det(v: f64, d: f64, v_range: (f64, f64), d_range: (f64, f64)) -> Detection {
        Detection {
            flyby: Flyby { f0: 100.0, v, d, tc: 300.0 },
            harmonics: vec![1.0],
            ranges: Ranges { v: v_range, d: d_range, tc: (295.0, 305.0), clipped: [false; 3] },
            score: 1.0,
            contrast: 10.0,
            quality: Default::default(),
            likely_ground_vehicle: false,
            confident: true,
            other_lines: vec![],
            region: Region { t_start: 200.0, t_end: 400.0, f_min: 80.0, f_max: 120.0 },
            trace: None,
            origin: Default::default(),
        }
    }

    #[test]
    fn soft_vd_is_flat_inside_the_ranges_and_along_the_ridge() {
        let d = det(40.0, 500.0, (35.0, 46.0), (350.0, 800.0));
        assert_eq!(vd_evidence(&d, 40.0, 500.0), 1.0);
        assert_eq!(vd_evidence(&d, 44.0, 700.0), 1.0);
        // faster and farther along v^2/d: what a single line leaves open
        assert_eq!(vd_evidence(&d, 60.0, 500.0 * 1.5 * 1.5), 1.0);
        // much faster than the range costs nothing either
        assert_eq!(vd_evidence(&d, 120.0, 500.0 * 9.0), 1.0);
        // farther than the ridge, within its core: log ratio -2.4 on a
        // confident detection, -1.4 on an unconfident one
        assert_eq!(vd_evidence(&d, 40.0, 500.0 * 2.4f64.exp()), 1.0);
        let u = Detection { confident: false, ..d.clone() };
        assert_eq!(vd_evidence(&u, 40.0, 500.0 * 1.4f64.exp()), 1.0);
        assert!(vd_evidence(&u, 40.0, 500.0 * 2.4f64.exp()) < 0.6);
    }

    #[test]
    fn soft_vd_penalises_far_off_the_ridge_more_than_along_it() {
        for confident in [true, false] {
            let d = Detection { confident, ..det(40.0, 500.0, (35.0, 46.0), (350.0, 800.0)) };
            let lo = if confident { VD_RIDGE_LO_CONFIDENT } else { VD_RIDGE_LO };
            // same speed, one sigma beyond the core's far end
            let k = (-lo + VD_RIDGE_SIGMA).exp();
            let off = vd_evidence(&d, 40.0, 500.0 * k);
            assert!((off - (-0.5f64).exp()).abs() < 0.01, "{confident} {off}");
            // as far, but at the speed that keeps v^2/d: no cost
            assert_eq!(vd_evidence(&d, 40.0 * k.sqrt(), 500.0 * k), 1.0);
            // nearer than the ridge (a steeper curve than measured): one sigma beyond +0.7
            let near = vd_evidence(&d, 40.0, 500.0 * (-VD_RIDGE_HI - VD_RIDGE_SIGMA).exp());
            assert!((near - (-0.5f64).exp()).abs() < 0.01, "{confident} {near}");
            // and falls monotonically to the floor
            let mut last = 1.0;
            for k in 0..80 {
                let g = vd_evidence(&d, 40.0, 500.0 * (1.0 + k as f64));
                assert!(g <= last + 1e-12 && g >= VD_FLOOR, "{k} {g}");
                last = g;
            }
            assert_eq!(vd_evidence(&d, 40.0, 500.0 * 1000.0), VD_FLOOR);
        }
    }

    #[test]
    fn soft_vd_penalises_a_pass_much_slower_than_the_range() {
        let d = det(40.0, 500.0, (35.0, 46.0), (350.0, 800.0));
        let s = (40.0f64 / 35.0).ln();
        // on the ridge, at 1.5 half-widths below the estimate: still free
        let v = 40.0 * (-1.5 * s).exp();
        assert!(vd_evidence(&d, v, 500.0 * (v / 40.0).powi(2)) > 0.999);
        // 2 half-widths below: one sigma (0.5 half-widths) beyond the core
        let v = 40.0 * (-2.0 * s).exp();
        let g = vd_evidence(&d, v, 500.0 * (v / 40.0).powi(2));
        assert!((g - (-0.5f64).exp()).abs() < 0.01, "{g}");
        // 3 half-widths below: at the floor
        let v3 = 40.0 * (-3.0 * s).exp();
        assert_eq!(vd_evidence(&d, v3, 500.0 * (v3 / 40.0).powi(2)), VD_FLOOR);
        // a clipped speed range reaches 1.4x lower before it costs
        let mut c = d.clone();
        c.ranges.clipped[0] = true;
        assert!(vd_evidence(&c, v, 500.0 * (v / 40.0).powi(2)) > 0.999);
        // a narrow range is never taken below the speed-of-sound / wind floor
        let n = det(40.0, 500.0, (39.9, 40.1), (450.0, 550.0));
        let v = 40.0 * (-1.5 * VD_V_FLOOR).exp();
        assert!(vd_evidence(&n, v, 500.0 * (v / 40.0).powi(2)) > 0.999);
    }

    #[test]
    fn repeat_discount_never_raises_a_margin() {
        let alt = |lat: f64, score: f64, loss: Option<[f64; 2]>| Alternative {
            lat,
            lon: 4.7,
            score,
            plain_score: Some(score),
            repeat_loss: loss,
            start_unix: 0.0,
            flights: vec![],
        };
        // #1 at 51.70 (2.0 after losing 0.3 to a repeat), a competitor 10 km away (1.2 after losing 0.5)
        let alts = vec![alt(51.70, 2.0, Some([0.3, 0.3])), alt(51.79, 1.2, Some([0.5, 0.5]))];
        // with the discount 0.8, without it 2.3 - 1.7 = 0.6: the smaller
        let m = margin_with_repeats(51.70, 4.7, 2.0, 2.0, [0.3, 0.3], &alts);
        assert!((m - 0.6).abs() < 1e-9, "{m}");
        // a discount on #1 alone lowers its margin
        let alts = vec![alt(51.70, 2.0, Some([0.3, 0.3])), alt(51.79, 1.2, None)];
        let m = margin_with_repeats(51.70, 4.7, 2.0, 2.0, [0.3, 0.3], &alts);
        assert!((m - 0.8).abs() < 1e-9, "{m}");
        // nothing discounted: the plain margin
        let alts = vec![alt(51.70, 2.0, None), alt(51.79, 1.2, None)];
        assert!((margin_with_repeats(51.70, 4.7, 2.0, 2.0, [0.0; 2], &alts) - 0.8).abs() < 1e-9);
        // the undiscounted alternative adds the loss back to both scores
        let u = alt(51.70, 2.0, Some([0.3, 0.2])).undiscounted();
        assert!((u.score - 2.3).abs() < 1e-12 && (u.plain() - 2.2).abs() < 1e-12 && u.repeat_loss.is_none());
    }

    #[test]
    fn soft_vd_never_raises_a_margin() {
        let alt = |lat: f64, score: f64, plain: f64| Alternative {
            repeat_loss: None,
            lat,
            lon: 4.7,
            score,
            plain_score: Some(plain),
            start_unix: 0.0,
            flights: vec![],
        };
        // #1 at 51.70 (score 2.48, nothing taken off); a competitor 10 km
        // away lost 0.9 of its 1.72 to the evidence, the next one scores 1.44
        let alts = vec![alt(51.70, 2.48, 2.48), alt(51.79, 0.82, 1.72), alt(51.62, 1.44, 1.44), alt(51.701, 2.3, 2.3)];
        let m = margin(51.70, 4.7, 2.48, 2.48, &alts);
        assert!((m - (2.48 - 1.72)).abs() < 1e-9, "{m}");
        // the evidence lowers a margin: #1 itself lost 0.5
        let m = margin(51.70, 4.7, 1.98, 2.48, &alts);
        assert!((m - (1.98 - 1.44)).abs() < 1e-9, "{m}");
        // without the evidence both are the same; old alternatives carry no plain score
        let old = vec![Alternative { plain_score: None, ..alt(51.79, 1.72, 0.0) }];
        assert!((margin(51.70, 4.7, 2.48, 2.48, &old) - 0.76).abs() < 1e-9);
        // nothing more than 2 km away: alternatives score 0
        assert!((margin(51.70, 4.7, 2.48, 2.48, &alts[3..]) - 2.48).abs() < 1e-9);
    }

    #[test]
    fn soft_vd_cpa_speed_and_range_of_a_straight_pass() {
        // a straight level pass 800 m east of the observer at 1000 m up, 50 m/s north
        let (lat, lon) = (52.0, 21.0);
        let o = ecef(lat, lon, 100.0);
        let (e, n, up) = enu_axes(lat, lon);
        let at = |t: f64| -> V3 {
            let p = [800.0, 50.0 * (t - 1000.0), 1000.0];
            let c = |k: usize| o[k] + p[0] * e[k] + p[1] * n[k] + p[2] * up[k];
            [c(0), c(1), c(2)]
        };
        let vel = [50.0 * n[0], 50.0 * n[1], 50.0 * n[2]];
        let knots: Vec<flight::Knot> = (0..21)
            .map(|k| {
                let t = 900.0 + 10.0 * k as f64;
                let p = at(t);
                flight::Knot { t, p, v: vel, h: geo::geodetic(p).2, mlat: false }
            })
            .collect();
        let seg = flight::Segment::new(knots);
        let (v, d) = cpa_speed_range(&seg, o, 1002.5, f64::NAN);
        assert!((v - 50.0).abs() < 0.01, "{v}");
        assert!((d - (800.0f64.powi(2) + 1000.0f64.powi(2)).sqrt()).abs() < 0.05, "{d}");
    }
}
