//! Location results as the UI sees them, independent of the ADS-B crate.
//!
//! The UI talks to a search only through [`LocationSource`]. The real one is
//! [`crate::adsb::AdsbSource`] (overhead-adsb on adsb.lol history);
//! [`SampleSource`] is a debug aid only (`--sample-data`): clearly labelled
//! invented candidates for checking the layout without ADS-B data.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use overhead_core::audio::Audio;
use overhead_core::detect::Detection;

use crate::query::SearchQuery;

/// One matched aircraft: which detection it explains and its track.
#[derive(Clone, Debug)]
pub struct MatchedTrack {
    /// Index into the detection list.
    pub detection: usize,
    /// Short label for tables and the map: callsign, else registration, else hex.
    pub callsign: String,
    pub icao: String,
    pub reg: Option<String>,
    pub type_code: Option<String>,
    /// Piston, Turboprop, Rotor, ...
    pub class: String,
    /// Time residual between predicted and measured heard-CPA (s).
    pub dt_s: f64,
    /// Modelled heard closest approach (recording time s) and its UTC time.
    pub heard_rec: f64,
    pub heard_utc: String,
    /// Closest-approach geometry of the scored pass.
    pub slant_m: f64,
    pub speed_ms: f64,
    pub height_agl_m: f64,
    /// Doppler-curve score relative to the stage-1 fit (1.0 = as good).
    pub curve_score: f64,
    /// (recording time s, lat, lon, altitude m), sorted by time.
    pub points: Vec<(f64, f64, f64, f64)>,
}

impl MatchedTrack {
    /// Position at recording time `t`, linearly interpolated (None outside the track).
    pub fn position_at(&self, t: f64) -> Option<(f64, f64, f64)> {
        let p = &self.points;
        if p.len() < 2 || t < p[0].0 || t > p[p.len() - 1].0 {
            return None;
        }
        let i = p.partition_point(|q| q.0 <= t).clamp(1, p.len() - 1);
        let (a, b) = (p[i - 1], p[i]);
        let w = if b.0 > a.0 { (t - a.0) / (b.0 - a.0) } else { 0.0 };
        Some((a.1 + (b.1 - a.1) * w, a.2 + (b.2 - a.2) * w, a.3 + (b.3 - a.3) * w))
    }
}

#[derive(Clone, Debug)]
pub struct LocationCandidate {
    pub lat: f64,
    pub lon: f64,
    /// 1-sigma horizontal uncertainty (m, RMS radius).
    pub radius_m: f64,
    /// Uncertainty ellipse: semi-axes (m) and azimuth of the major axis (deg from north).
    pub ellipse: Option<(f64, f64, f64)>,
    /// 0..=1 relative to the best candidate, for the score bar.
    pub score: f64,
    /// The matcher's own score (curve matches minus silence penalties).
    pub raw_score: f64,
    /// Score gap to the best candidate more than 2 km away; below 1 = ambiguous.
    pub margin: Option<f64>,
    /// Detections explained.
    pub matched: usize,
    /// Prop aircraft that should have been heard here but no detection explains.
    pub violations: usize,
    /// Fitted recording start (UTC) and ground speed of sound.
    pub start_utc: Option<String>,
    pub c0: Option<f64>,
    /// Terrain height (m).
    pub ground_m: Option<f64>,
    /// Short label ("best", "alt 1", ...).
    #[allow(dead_code)]
    pub label: String,
    pub tracks: Vec<MatchedTrack>,
    /// Probability that the recording was made at this place (within 2 km).
    pub probability: Option<f64>,
    /// 1-based rank of a better candidate within 2 km (same place, no probability of its own).
    pub same_place_as: Option<usize>,
}

/// One cell of the plausible area: (lat_min, lon_min, lat_max, lon_max) and its probability.
#[derive(Clone, Debug)]
pub struct PlausibleCell {
    pub bounds: (f64, f64, f64, f64),
    pub probability: f64,
}

/// How sure the search is, as the UI shows it.
#[derive(Clone, Debug)]
pub struct Ambiguity {
    /// The margin of #1 is at least 1: the answer is reliable.
    pub confident: bool,
    /// Distinct places (> 2 km apart) whose score is within 1 of the best.
    pub similar_places: usize,
    /// Probability of the best place, and of the best 10 together.
    pub top1: f64,
    pub top10: f64,
    /// Probability that it is none of the places found.
    pub elsewhere: f64,
    pub cells: Vec<PlausibleCell>,
    /// What would narrow the answer down.
    pub hints: Vec<String>,
}

impl Ambiguity {
    /// The box around the cells holding most of the probability, for framing the map.
    pub fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let p_max = self.cells.iter().map(|c| c.probability).fold(0.0, f64::max);
        self.cells.iter().filter(|c| c.probability >= 0.1 * p_max).fold(None, |b, c| {
            let (a0, o0, a1, o1) = c.bounds;
            Some(match b {
                None => (a0, o0, a1, o1),
                Some((b0, p0, b1, p1)) => (a0.min(b0), o0.min(p0), a1.max(b1), o1.max(p1)),
            })
        })
    }
}

#[derive(Clone, Debug)]
pub struct LocationResults {
    /// Who produced these, shown in the panel header.
    pub source: String,
    /// True for placeholder data that must not be read as a result.
    pub is_sample: bool,
    pub candidates: Vec<LocationCandidate>,
    /// Approximate search area (lat_min, lon_min, lat_max, lon_max), if any.
    pub search_box: Option<(f64, f64, f64, f64)>,
    pub notes: Vec<String>,
    /// Confidence assessment and plausible area (None for sample data or old outputs).
    pub ambiguity: Option<Ambiguity>,
    /// The finished ADS-B search, for explaining it without searching again:
    /// score maps, per-fly-by scores, modelled curves of any candidate,
    /// silence, map links (`overhead_adsb::explain`). None for sample data.
    pub session: Option<Arc<overhead_adsb::explain::Session>>,
    /// The session's detection `k` is the app's fly-by `det_index[k]`
    /// (the matcher takes at most 64 fly-bys).
    pub det_index: Vec<usize>,
    /// Cuts the search used (recording s).
    pub cuts: Vec<f64>,
}

impl LocationResults {
    /// Distinct places, best first (candidate indices): a candidate within
    /// 2 km of a better one is the same place and not listed again.
    pub fn places(&self) -> Vec<usize> {
        (0..self.candidates.len()).filter(|&k| self.candidates[k].same_place_as.is_none()).collect()
    }

    /// The search found something but cannot tell which of several places it is.
    pub fn ambiguous(&self) -> Option<&Ambiguity> {
        self.ambiguity.as_ref().filter(|a| !a.confident && !self.candidates.is_empty())
    }
}

/// Everything a location search gets: the user's hints, the detections, the
/// decoded recording (for spectrogram scoring) and a handle for progress and
/// cancellation.
pub struct LocateJob {
    pub query: SearchQuery,
    pub detections: Vec<Detection>,
    /// For spectrogram scoring; the sample source does not use it.
    pub audio: Option<Arc<Audio>>,
    /// Cuts in the recording (s): detections in different parts get their
    /// own start (`overhead_adsb::locate::Cut`); empty = none.
    pub cuts: Vec<f64>,
    pub ctl: Arc<LocateControl>,
}

/// Shared between the UI and a running search: the search writes progress,
/// the UI reads it every frame and sets `cancel` when the search is superseded
/// (new file, new search, app closing).
#[derive(Default)]
pub struct LocateControl {
    cancel: AtomicBool,
    progress: Mutex<(String, Option<f32>)>,
}

impl LocateControl {
    /// Report what the search is doing; `frac` in 0..=1 if known.
    pub fn progress(&self, msg: impl Into<String>, frac: Option<f32>) {
        *self.progress.lock().unwrap() = (msg.into(), frac.map(|f| f.clamp(0.0, 1.0)));
    }
    pub fn snapshot(&self) -> (String, Option<f32>) {
        self.progress.lock().unwrap().clone()
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    /// `Err` once cancelled, for `?` at convenient points in a long search.
    pub fn check(&self) -> anyhow::Result<()> {
        if self.is_cancelled() { Err(anyhow::anyhow!("cancelled")) } else { Ok(()) }
    }
}

/// Anything that turns detections + the user's hints into location candidates.
/// `locate` runs on a background thread and may take minutes (archive
/// downloads); it should report progress and poll `job.ctl.check()`.
pub trait LocationSource: Send + Sync {
    fn name(&self) -> String;
    fn locate(&self, job: &LocateJob) -> anyhow::Result<LocationResults>;
}

/// Debug aid only (`--sample-data`): plausible-looking candidates
/// around the region the user typed, with synthetic straight tracks that
/// reproduce each detection's speed, distance and CPA time.
pub struct SampleSource;

impl LocationSource for SampleSource {
    fn name(&self) -> String {
        "Sample data".into()
    }

    fn locate(&self, job: &LocateJob) -> anyhow::Result<LocationResults> {
        let (query, detections) = (&job.query, &job.detections[..]);
        // Pretend to work a little so the progress state is visible.
        for (k, step) in ["Loading sample day", "Matching fly-bys", "Ranking candidates"].iter().enumerate() {
            job.ctl.check()?;
            job.ctl.progress(*step, Some(k as f32 / 3.0));
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        job.ctl.check()?;
        let (center, known, half_deg) = gazetteer(&query.region);
        let mut rng = Lcg(0x9e37_79b9_7f4a_7c15 ^ query.day.days() as u64);
        let offsets = [(0.0, 0.0, 0.62, 1800.0), (0.11, -0.19, 0.31, 4200.0), (-0.16, 0.08, 0.17, 7600.0)];
        let mut candidates = Vec::new();
        for (rank, (dlat, dlon, score, radius)) in offsets.into_iter().enumerate() {
            let lat = center.0 + dlat * half_deg + rng.next_f64(-0.02, 0.02);
            let lon = center.1 + dlon * half_deg * 1.6 + rng.next_f64(-0.02, 0.02);
            let mut tracks = Vec::new();
            for (i, det) in detections.iter().enumerate() {
                if det.likely_ground_vehicle || (rank > 0 && i % (rank + 1) == 0) {
                    continue;
                }
                tracks.push(synthetic_track(i, det, lat, lon, &mut rng));
            }
            candidates.push(LocationCandidate {
                lat,
                lon,
                radius_m: radius,
                ellipse: None,
                score,
                raw_score: score,
                margin: None,
                matched: tracks.len(),
                violations: 0,
                start_utc: None,
                c0: None,
                ground_m: None,
                label: if rank == 0 { "best".into() } else { format!("alt {rank}") },
                tracks,
                probability: None,
                same_place_as: None,
            });
        }
        let mut notes = vec!["Sample data (--sample-data debug flag): positions and callsigns are invented.".into()];
        if !known {
            notes.push("Region not in the sample gazetteer; centred on Poland.".into());
        }
        let search_box = query.region.finest().map(|_| {
            (center.0 - half_deg, center.1 - half_deg * 1.6, center.0 + half_deg, center.1 + half_deg * 1.6)
        });
        Ok(LocationResults { source: self.name(), is_sample: true, candidates, search_box, notes, ambiguity: None, session: None, det_index: (0..detections.len()).collect(), cuts: job.cuts.clone() })
    }
}

fn synthetic_track(idx: usize, det: &Detection, lat: f64, lon: f64, rng: &mut Lcg) -> MatchedTrack {
    let f = det.flyby;
    let heading = rng.next_f64(0.0, std::f64::consts::TAU);
    let alt = (f.d * rng.next_f64(0.35, 0.8)).clamp(150.0, 11_000.0);
    let horiz = (f.d * f.d - alt * alt).max(0.0).sqrt();
    let m_per_deg_lat = 111_320.0;
    let m_per_deg_lon = 111_320.0 * lat.to_radians().cos();
    // CPA point: perpendicular to the heading, `horiz` metres from the observer.
    let (hx, hy) = (heading.sin(), heading.cos());
    let (px, py) = (-hy * horiz, hx * horiz);
    let t_cpa = f.tc - f.d / overhead_core::model::DEFAULT_C;
    let points = (-60..=60)
        .map(|k| {
            let dt = k as f64 * 10.0;
            let (x, y) = (px + hx * f.v * dt, py + hy * f.v * dt);
            (t_cpa + dt, lat + y / m_per_deg_lat, lon + x / m_per_deg_lon, alt)
        })
        .collect();
    const AIRLINES: [&str; 6] = ["LOT", "RYR", "WZZ", "DLH", "ENT", "SPL"];
    let callsign = format!("{}{}", AIRLINES[rng.next_u64() as usize % AIRLINES.len()], 100 + rng.next_u64() % 900);
    let icao = format!("{:06x}", 0x48_0000 + (rng.next_u64() % 0xffff));
    let dt_s = rng.next_f64(-2.5, 2.5);
    MatchedTrack {
        detection: idx,
        callsign,
        icao,
        reg: None,
        type_code: None,
        class: "sample".into(),
        dt_s,
        heard_rec: f.tc + dt_s,
        heard_utc: String::new(),
        slant_m: f.d,
        speed_ms: f.v,
        height_agl_m: alt,
        curve_score: 0.0,
        points,
    }
}

struct Lcg(u64);

impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 11
    }
    fn next_f64(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * (self.next_u64() as f64 / (1u64 << 53) as f64)
    }
}

/// Tiny lookup for the sample source: ((lat, lon), known, half-size in degrees).
fn gazetteer(region: &crate::query::RegionHint) -> ((f64, f64), bool, f64) {
    const PLACES: &[(&str, f64, f64, f64)] = &[
        ("warszawa warsaw mazowieckie masovian", 52.23, 21.01, 0.25),
        ("kraków krakow cracow małopolskie malopolskie lesser poland", 50.06, 19.94, 0.25),
        ("łódź lodz łódzkie lodzkie", 51.76, 19.46, 0.25),
        ("wrocław wroclaw dolnośląskie dolnoslaskie lower silesia", 51.11, 17.03, 0.25),
        ("poznań poznan wielkopolskie greater poland", 52.41, 16.93, 0.25),
        ("gdańsk gdansk pomorskie pomeranian", 54.35, 18.65, 0.25),
        ("szczecin zachodniopomorskie", 53.43, 14.55, 0.25),
        ("lublin lubelskie", 51.25, 22.57, 0.25),
        ("katowice śląskie slaskie silesia", 50.26, 19.02, 0.25),
        ("białystok bialystok podlaskie", 53.13, 23.16, 0.25),
        ("rzeszów rzeszow podkarpackie", 50.04, 22.0, 0.25),
        ("berlin", 52.52, 13.40, 0.25),
        ("london", 51.51, -0.13, 0.25),
        ("amsterdam", 52.37, 4.90, 0.25),
        ("praha prague", 50.08, 14.44, 0.25),
    ];
    const COUNTRIES: &[(&str, f64, f64, f64)] = &[
        ("polska poland pl", 52.0, 19.4, 2.6),
        ("niemcy germany deutschland de", 51.1, 10.4, 3.0),
        ("czechy czechia czech republic cz", 49.8, 15.5, 1.5),
        ("united kingdom uk england gb", 52.8, -1.6, 2.5),
        ("netherlands holandia nl", 52.2, 5.5, 1.0),
    ];
    let norm = |s: &str| s.trim().to_lowercase();
    let find = |table: &[(&str, f64, f64, f64)], q: &str| {
        let q = norm(q);
        if q.is_empty() {
            return None;
        }
        table.iter().find(|(names, ..)| names.contains(q.as_str())).map(|&(_, la, lo, h)| ((la, lo), h))
    };
    for (q, scale) in [(&region.city, 1.0), (&region.state, 3.0)] {
        if let Some((c, h)) = find(PLACES, q) {
            return (c, true, h * scale);
        }
    }
    if let Some((c, h)) = find(COUNTRIES, &region.country) {
        return (c, true, h);
    }
    ((52.0, 19.4), region.finest().is_none(), 2.6)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(lat: f64, lon: f64, p: Option<f64>) -> LocationCandidate {
        LocationCandidate {
            lat,
            lon,
            radius_m: 100.0,
            ellipse: None,
            score: 1.0,
            raw_score: 1.0,
            margin: Some(0.1),
            matched: 1,
            violations: 0,
            start_utc: None,
            c0: None,
            ground_m: None,
            label: String::new(),
            tracks: vec![],
            probability: p,
            same_place_as: None,
        }
    }

    fn amb(confident: bool) -> Ambiguity {
        let cell = |a: f64, o: f64, p: f64| PlausibleCell { bounds: (a, o, a + 0.02, o + 0.03), probability: p };
        Ambiguity {
            confident,
            similar_places: 3,
            top1: 0.4,
            top10: 0.9,
            elsewhere: 0.1,
            // the third cell is under 10 % of the best and does not widen the framing
            cells: vec![cell(52.0, 13.0, 0.4), cell(52.3, 13.5, 0.3), cell(53.0, 15.0, 0.02)],
            hints: vec![],
        }
    }

    fn results(a: Option<Ambiguity>, n: usize) -> LocationResults {
        LocationResults {
            source: "test".into(),
            is_sample: false,
            candidates: (0..n).map(|_| cand(52.0, 13.0, Some(0.4))).collect(),
            search_box: None,
            notes: vec![],
            ambiguity: a,
            session: None,
            det_index: vec![],
            cuts: vec![],
        }
    }

    #[test]
    fn ambiguous_only_when_not_confident_and_found() {
        assert!(results(Some(amb(false)), 2).ambiguous().is_some());
        assert!(results(Some(amb(true)), 2).ambiguous().is_none());
        assert!(results(Some(amb(false)), 0).ambiguous().is_none());
        assert!(results(None, 2).ambiguous().is_none());
    }

    #[test]
    fn plausible_bounds_cover_the_likely_cells() {
        let (a0, o0, a1, o1) = amb(false).bounds().unwrap();
        assert_eq!((a0, o0), (52.0, 13.0));
        assert!((a1 - 52.32).abs() < 1e-12 && (o1 - 13.53).abs() < 1e-12);
        let none = Ambiguity { cells: vec![], ..amb(false) };
        assert!(none.bounds().is_none());
    }
}
