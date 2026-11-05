//! Repeated passes of one flight.
//!
//! When the start time is free, one aircraft flying circuits or repeated
//! passes can explain several detections at a wrong place: a circuit gives
//! the same pass (same leg, same heading, same distance) every few minutes,
//! so once one pass of it fits a detection, a second detection a few laps
//! later fits too at little extra chance. Such a second pass adds little
//! evidence. Passes that are independent (crossing tracks, different sides
//! of the observer, clearly different distances, or far apart in time)
//! count fully.
//!
//! The track is compared as an axis, not a direction: a flight that comes
//! back along the same line (out and back, or a circuit's legs seen from
//! a place beside both) gives the same Doppler curve either way, and along
//! such a line some point has any time difference between the two passes,
//! so the reversed pass is no more independent than the same one again
//! (River Amer, hours and day: the 12 km wrong #1 matches PH-PXA flying
//! out and back past it, 14 min apart, same side, 549 and 710 m).
//!
//! Per flight, its matches are taken strongest first; each further match
//! counts `1 - weight · dependence` of its share, the dependence being the
//! largest [`dependence`] on any stronger match of the same flight.

use serde::{Deserialize, Serialize};

/// Default weight of the discount (`MatchConfig::repeat_weight`).
pub const REPEAT_WEIGHT: f64 = 0.6;
/// Track-axis and direction similarity: `cos^2` of the angle between the
/// tracks (1 parallel or reversed, 0.5 at 45°, 0 crossing at 90°) and
/// `max(0, cos)^2` of the angle between the directions from the observer
/// (1 the same, 0 from 90° apart: the other side).
const ANGLE_POW: i32 = 2;
/// Distance similarity: Gaussian in log slant ratio (0.61 at a ratio of 1.35, 0.14 at 1.9).
const LOG_D_SIGMA: f64 = 0.3;
/// Time similarity: `1 / (1 + (dt / T)^4)` (0.95 at 14 min, 0.5 at 30, 0.16 at 45).
const TIME_SCALE_S: f64 = 1800.0;

/// Geometry of a matched pass at its closest approach.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PassGeom {
    /// Closest approach (emission, unix s).
    pub t: f64,
    /// Slant range there (m).
    pub slant_m: f64,
    /// Unit vector observer → aircraft there (east, north, up).
    pub dir: [f64; 3],
    /// Horizontal velocity direction there (east, north), unit; `[0, 0]` when hovering.
    pub track: [f64; 2],
}

/// How much a second pass repeats the first one's geometry (0..1): the
/// product of the track-axis, direction (side of the observer), distance
/// and time similarities.
pub fn dependence(a: &PassGeom, b: &PassGeom) -> f64 {
    let sim = |c: f64| c.max(0.0).powi(ANGLE_POW);
    let axis = if a.track == [0.0; 2] || b.track == [0.0; 2] {
        1.0
    } else {
        sim((a.track[0] * b.track[0] + a.track[1] * b.track[1]).abs())
    };
    let side = sim(a.dir[0] * b.dir[0] + a.dir[1] * b.dir[1] + a.dir[2] * b.dir[2]);
    let dist = if a.slant_m > 0.0 && b.slant_m > 0.0 {
        let x = (a.slant_m / b.slant_m).ln() / LOG_D_SIGMA;
        (-0.5 * x * x).exp()
    } else {
        1.0
    };
    let time = 1.0 / (1.0 + ((a.t - b.t) / TIME_SCALE_S).powi(4));
    axis * side * dist * time
}

/// A match that repeats a stronger match of the same flight.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Repeat {
    /// Detection of the stronger match it repeats most.
    pub of_detection: usize,
    /// [`dependence`] on it.
    pub dependence: f64,
    /// Factor on this match's share of the score.
    pub factor: f64,
}

/// One match: detection, flight, its share of the score (0 = not a match)
/// and the geometry of the scored pass.
#[derive(Clone, Copy, Debug)]
pub struct Match {
    pub det: usize,
    pub flight: u32,
    pub share: f64,
    pub geom: Option<PassGeom>,
}

/// Per match (same order): the repeat it is, if any. Matches of a flight
/// are taken strongest first; a match without geometry or share is never
/// a repeat and repeats nothing.
pub fn discounts(m: &[Match], weight: f64) -> Vec<Option<Repeat>> {
    let mut out = vec![None; m.len()];
    if weight <= 0.0 {
        return out;
    }
    let mut order: Vec<usize> = (0..m.len()).filter(|&i| m[i].share > 0.0 && m[i].geom.is_some()).collect();
    order.sort_by(|&a, &b| m[b].share.total_cmp(&m[a].share).then(a.cmp(&b)));
    for (k, &i) in order.iter().enumerate() {
        let g = m[i].geom.unwrap();
        let best = order[..k]
            .iter()
            .filter(|&&j| m[j].flight == m[i].flight)
            .map(|&j| (j, dependence(&m[j].geom.unwrap(), &g)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((j, dep)) = best {
            out[i] = Some(Repeat { of_detection: m[j].det, dependence: dep, factor: 1.0 - weight.min(1.0) * dep });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: [f64; 3]) -> [f64; 3] {
        let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        [v[0] / n, v[1] / n, v[2] / n]
    }

    /// A pass at `t`, the aircraft at east/north/up offset `p` (m) from the observer, flying `track` (deg).
    fn pass(t: f64, p: [f64; 3], track_deg: f64) -> PassGeom {
        let n = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        let (s, c) = track_deg.to_radians().sin_cos();
        PassGeom { t, slant_m: n, dir: unit(p), track: [s, c] }
    }

    #[test]
    fn a_circuit_lap_repeats_and_independent_passes_do_not() {
        let a = pass(0.0, [500.0, 0.0, 400.0], 0.0);
        // the next lap, 5 min later, the same leg
        let lap = pass(300.0, [520.0, 0.0, 390.0], 3.0);
        assert!(dependence(&a, &lap) > 0.95, "{}", dependence(&a, &lap));
        // back along the same line: the same curve, as dependent
        assert!(dependence(&a, &pass(300.0, [510.0, 0.0, 400.0], 180.0)) > 0.95);
        // a crossing track
        assert!(dependence(&a, &pass(300.0, [500.0, 0.0, 400.0], 90.0)) < 1e-9);
        assert!(dependence(&a, &pass(300.0, [500.0, 0.0, 400.0], 270.0)) < 1e-9);
        // the same heading on the other side of the observer
        assert!(dependence(&a, &pass(300.0, [-500.0, 0.0, 400.0], 0.0)) < 0.01);
        // twice the distance
        let far = pass(300.0, [1100.0, 0.0, 800.0], 0.0);
        assert!(dependence(&a, &far) < 0.1, "{}", dependence(&a, &far));
        // an hour apart
        assert!(dependence(&a, &pass(3600.0, [500.0, 0.0, 400.0], 0.0)) < 0.1);
        // symmetric
        assert!((dependence(&a, &far) - dependence(&far, &a)).abs() < 1e-12);
    }

    #[test]
    fn the_weaker_repeat_of_a_flight_is_discounted() {
        let g = pass(0.0, [500.0, 0.0, 400.0], 0.0);
        let lap = pass(300.0, [500.0, 0.0, 400.0], 0.0);
        let m = [
            Match { det: 0, flight: 7, share: 0.8, geom: Some(lap) },
            Match { det: 1, flight: 3, share: 1.0, geom: Some(g) },
            Match { det: 2, flight: 7, share: 0.9, geom: Some(g) },
            Match { det: 3, flight: 7, share: 0.0, geom: Some(g) },
        ];
        let d = discounts(&m, 0.6);
        // flight 3 once, flight 7's strongest match (det 2) counts fully
        assert_eq!(d[1], None);
        assert_eq!(d[2], None);
        // its weaker one repeats det 2
        let r = d[0].unwrap();
        assert_eq!(r.of_detection, 2);
        // (the same geometry 5 min later: only the time term is below 1, 0.9992)
        assert!((r.dependence - 1.0).abs() < 1e-3 && (r.factor - (1.0 - 0.6 * r.dependence)).abs() < 1e-12, "{r:?}");
        // no share, no repeat
        assert_eq!(d[3], None);
        // off
        assert!(discounts(&m, 0.0).iter().all(|x| x.is_none()));
        // independent passes of one flight count fully (factor 1)
        let other = pass(300.0, [500.0, 0.0, 400.0], 90.0);
        let m2 = [Match { det: 0, flight: 7, share: 0.8, geom: Some(other) }, Match { det: 2, flight: 7, share: 0.9, geom: Some(g) }];
        assert!((discounts(&m2, 0.6)[0].unwrap().factor - 1.0).abs() < 1e-9);
    }
}
