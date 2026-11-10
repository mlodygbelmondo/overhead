//! An ADS-B track as a smooth 3D trajectory (ECEF), and what a ground
//! observer hears from it.
//!
//! Between reports the position is the cubic Hermite interpolant of the
//! reported positions and the reported velocities (ground speed, track and
//! vertical rate), so straight stretches with sparse reports (readsb thins
//! straight flight) are still exact and turns stay smooth. A track is split
//! into segments at gaps, new legs and ground contact.

use serde::{Deserialize, Serialize};

use crate::geo::{self, V3, add, dot, ecef, enu_axes, norm, scale, sub};
use crate::trace::Track;
use crate::types::AircraftClass;

/// Lapse of the effective speed of sound with source height: the temperature
/// falls ≈6.5 K/km and c by ≈0.6 m/s per K. A ray from height h crosses air
/// whose mean temperature is lower by ≈3.25 K/km·h.
pub const C_LAPSE_PER_M: f64 = 0.6 * 0.0065;

#[derive(Clone, Debug)]
pub struct Knot {
    pub t: f64,
    pub p: V3,
    pub v: V3,
    /// Height above the ellipsoid (≈ above sea level, see crate docs).
    pub h: f64,
    /// The report is a multilateration position.
    pub mlat: bool,
}

#[derive(Clone, Debug)]
pub struct Segment {
    pub knots: Vec<Knot>,
    /// Knot times are non-decreasing, so the interval of a time can be
    /// found by binary search (same interval as a linear scan).
    pub sorted: bool,
    /// ECEF bounding box of the knot positions.
    pub lo: V3,
    pub hi: V3,
    /// Latitude/longitude box of the knot positions.
    pub geo: geo::BBox,
}

/// One aircraft's airborne trajectory on the day.
#[derive(Clone, Debug)]
pub struct Flight {
    pub hex: String,
    pub reg: Option<String>,
    pub type_code: Option<String>,
    pub class: AircraftClass,
    pub callsigns: Vec<(f64, String)>,
    pub segments: Vec<Segment>,
    /// No geometric altitude in the whole track: heights are corrected pressure altitudes.
    pub baro_only: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct FlightOptions {
    /// Split the track where reports are further apart than this (s).
    pub max_gap: f64,
    /// Ignore reports above this altitude (m); they cannot be heard as a fly-by.
    pub max_alt: f64,
    /// Geometric minus barometric altitude (m) for tracks that never report a
    /// geometric altitude: the regional median from the other aircraft
    /// ([`regional_baro_offset`]); `None` = pressure altitude taken as height
    /// above the geoid (no QNH correction).
    pub baro_offset: Option<f64>,
    /// Smooth multilateration positions ([`smooth_mlat`]). The synthetic
    /// validation turns it off to draw its truth from the reports with a
    /// smoother of its own, so that the matcher's is tested, not shared.
    #[serde(default = "yes")]
    pub smooth_mlat: bool,
}

fn yes() -> bool {
    true
}

impl Default for FlightOptions {
    fn default() -> Self {
        Self { max_gap: 120.0, max_alt: 9000.0, baro_offset: None, smooth_mlat: true }
    }
}

/// Median over aircraft of the per-aircraft median `alt_geom − alt_baro`
/// below 3000 m: the pressure-altitude correction (QNH, temperature and
/// geoid together) for the region and time of the given tracks.
pub fn regional_baro_offset(tracks: &[Track]) -> Option<f64> {
    let per: Vec<f64> = tracks
        .iter()
        .filter_map(|t| {
            median(
                t.points
                    .iter()
                    .filter(|p| !p.ground && p.alt_baro.is_some_and(|a| a < 3000.0))
                    .filter_map(|p| Some(p.alt_geom? - p.alt_baro?))
                    .collect(),
            )
        })
        .collect();
    if per.len() < 3 { None } else { median(per) }
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

impl Flight {
    pub fn from_track(tr: &Track, opt: &FlightOptions) -> Option<Self> {
        // offset geometric - baro, to put baro-only reports on the geometric scale
        // (else the regional offset, else pressure altitude as height above the geoid)
        let geo_offset = median(tr.points.iter().filter_map(|p| Some(p.alt_geom? - p.alt_baro?)).collect())
            .or(opt.baro_offset)
            .unwrap_or_else(|| tr.points.first().map_or(0.0, |p| crate::geoid::undulation(p.lat, p.lon)));
        let mut segments = Vec::new();
        let mut cur: Vec<Knot> = Vec::new();
        let mut last_t = f64::NEG_INFINITY;
        for p in &tr.points {
            let h = if p.ground { None } else { p.alt_geom.or(p.alt_baro.map(|a| a + geo_offset)) };
            let gap = p.t - last_t > opt.max_gap || p.new_leg;
            let Some(h) = h.filter(|h| *h < opt.max_alt) else {
                if cur.len() >= 2 {
                    segments.push(Segment::new(std::mem::take(&mut cur)));
                }
                cur.clear();
                last_t = p.t;
                continue;
            };
            if gap && !cur.is_empty() {
                if cur.len() >= 2 {
                    segments.push(Segment::new(std::mem::take(&mut cur)));
                }
                cur.clear();
            }
            if cur.last().is_some_and(|k| p.t - k.t < 0.05) {
                continue;
            }
            let pos = ecef(p.lat, p.lon, h);
            let (e, n, u) = enu_axes(p.lat, p.lon);
            let v = match (p.gs, p.track) {
                (Some(gs), Some(trk)) => {
                    let (s, c) = trk.to_radians().sin_cos();
                    let vz = p.vrate.unwrap_or(0.0);
                    add(add(scale(e, gs * s), scale(n, gs * c)), scale(u, vz))
                }
                _ => [f64::NAN; 3],
            };
            cur.push(Knot { t: p.t, p: pos, v, h, mlat: p.mlat });
            last_t = p.t;
        }
        if cur.len() >= 2 {
            segments.push(Segment::new(cur));
        }
        // fastest plausible motion: class limit, and not far above what the aircraft reported
        let mut gs: Vec<f64> = tr.points.iter().filter_map(|p| p.gs).collect();
        gs.sort_by(f64::total_cmp);
        let class_max = match tr.class {
            AircraftClass::Piston | AircraftClass::Electric => 110.0,
            AircraftClass::Rotor => 95.0,
            _ => 200.0,
        };
        let max_speed = match gs.get(gs.len().saturating_sub(1) * 98 / 100) {
            Some(g) => (1.3 * g + 25.0).min(class_max).max(40.0),
            None => class_max,
        };
        for s in &mut segments {
            clean(&mut s.knots, max_speed);
            if opt.smooth_mlat {
                smooth_mlat(&mut s.knots);
            }
            s.finish();
        }
        segments.retain(|s| s.knots.len() >= 2);
        if segments.is_empty() {
            return None;
        }
        Some(Self {
            hex: tr.hex.clone(),
            reg: tr.reg.clone(),
            type_code: tr.type_code.clone(),
            class: tr.class,
            callsigns: tr.callsigns.clone(),
            segments,
            baro_only: !tr.points.iter().any(|p| p.alt_geom.is_some()),
        })
    }

    pub fn callsign_at(&self, t: f64) -> Option<&str> {
        self.callsigns.iter().rev().find(|c| c.0 <= t).or(self.callsigns.first()).map(|c| c.1.as_str())
    }

    pub fn span(&self) -> (f64, f64) {
        (self.segments[0].knots[0].t, self.segments.last().unwrap().knots.last().unwrap().t)
    }

    /// Segment containing `t`.
    pub fn segment_at(&self, t: f64) -> Option<&Segment> {
        self.segments.iter().find(|s| t >= s.knots[0].t && t <= s.knots.last().unwrap().t)
    }

    /// Uniformly resampled states `(t, p, v, h)` of every segment, step `dt`.
    pub fn resample(&self, dt: f64, t0: f64, t1: f64) -> Vec<Vec<(f64, V3, V3, f64)>> {
        self.segments
            .iter()
            .filter_map(|s| {
                let (a, b) = (s.knots[0].t.max(t0), s.knots.last().unwrap().t.min(t1));
                if b - a < dt {
                    return None;
                }
                let n = ((b - a) / dt).ceil() as usize;
                let mut out = Vec::with_capacity(n + 1);
                let mut hint = 0;
                for i in 0..=n {
                    let t = (a + i as f64 * dt).min(b);
                    let (p, v, h) = s.state_hint(t, &mut hint);
                    out.push((t, p, v, h));
                }
                Some(out)
            })
            .collect()
    }
}

fn chord_speed(a: &Knot, b: &Knot) -> f64 {
    norm(sub(b.p, a.p)) / (b.t - a.t).max(0.05)
}

/// Remove position jumps, altitude spikes and inconsistent velocities.
fn clean(k: &mut Vec<Knot>, max_speed: f64) {
    // isolated position outliers: too fast to reach and to leave
    let mut i = 1;
    while i + 1 < k.len() {
        if chord_speed(&k[i - 1], &k[i]) > max_speed
            && chord_speed(&k[i], &k[i + 1]) > max_speed
            && chord_speed(&k[i - 1], &k[i + 1]) <= max_speed
        {
            k.remove(i);
        } else {
            i += 1;
        }
    }
    // a jump that persists: split by dropping the shorter side is overkill; drop the second point
    let mut i = 1;
    while i < k.len() {
        if chord_speed(&k[i - 1], &k[i]) > max_speed {
            k.remove(i);
        } else {
            i += 1;
        }
    }
    let n = k.len();
    if n < 2 {
        return;
    }
    // altitude spikes against a 5-point median
    let hs: Vec<f64> = k.iter().map(|x| x.h).collect();
    for i in 0..n {
        let (a, b) = (i.saturating_sub(2), (i + 3).min(n));
        let mut w: Vec<f64> = hs[a..b].to_vec();
        w.sort_by(f64::total_cmp);
        let med = w[w.len() / 2];
        if (hs[i] - med).abs() > 100.0 {
            let up = scale(k[i].p, 1.0 / norm(k[i].p));
            k[i].p = add(k[i].p, scale(up, med - hs[i]));
            k[i].h = med;
        }
    }
    // velocities: missing or inconsistent with the positions -> finite differences
    for i in 0..n {
        let (a, b) = (i.saturating_sub(1), (i + 1).min(n - 1));
        let dt = k[b].t - k[a].t;
        let vc = if dt > 0.0 { scale(sub(k[b].p, k[a].p), 1.0 / dt) } else { [0.0; 3] };
        let v = k[i].v;
        let bad = v[0].is_nan() || norm(v) > max_speed || (dt < 60.0 && norm(sub(v, vc)) > 40.0 + 0.5 * norm(vc));
        if bad {
            k[i].v = vc;
        }
        clamp_vz(&mut k[i]);
    }
}

/// Vertical rate sanity: at most 25 m/s.
fn clamp_vz(k: &mut Knot) {
    let up = scale(k.p, 1.0 / norm(k.p));
    let vz = dot(k.v, up);
    if vz.abs() > 25.0 {
        k.v = sub(k.v, scale(up, vz - 25.0f64.copysign(vz)));
    }
}

/// Half-widths (s) tried for the local fit that smooths multilateration
/// positions, widest first, and the residual (m, weighted RMS) up to which a
/// fit counts as following the path: about the MLAT scatter itself (on the
/// Noord-Brabant traffic of 2025-07-18 the median residual of 61 000 such fits
/// was 106 m at ±25 s and 129 m at ±40 s, where a quadratic follows any
/// light-aircraft path), with a little room.
pub const MLAT_SMOOTH_S: [f64; 4] = [90.0, 60.0, 40.0, 25.0];
pub const MLAT_NOISE_M: f64 = 150.0;
/// Largest turn correction (fraction of the speed, see [`smooth_mlat`]) a
/// smoothing window may need: beyond that the window spans too much of the
/// turn for a constant-rate model of it to be trusted, and a narrower
/// window (or the raw report) is used.
pub const MLAT_TURN_BIAS: f64 = 0.15;

/// Multilateration positions scatter by ~50-150 m from report to report
/// (8-10 s apart for a light aircraft), and such reports carry no usable
/// velocity, so a Hermite curve through them with finite-difference
/// tangents makes the speed swing between ~30 and ~80 m/s where the
/// aircraft flew a steady 52 (PH-TRG at Kampina). The Doppler curve depends
/// on that speed, most of all at the closest approach. So an MLAT knot is
/// replaced by a local quadratic least-squares fit of the positions around
/// it (tricube weights; GNSS knots in the window weigh 4×), which gives its
/// position and velocity.
///
/// A quadratic's slope is biased in a turn: in a level turn of rate ω a
/// quadratic over ±W returns a speed too low by about ω²·W²/10 (the cubic
/// term of the arc), which a residual test does not see (a ±40 s quadratic
/// follows a standard-rate turn within 150 m and is 20 % slow). So the turn
/// is modelled: the fit's own weights applied to a constant-rate level
/// circle give what the fit returns for it ([`turn_response`]); the turn
/// rate that reproduces the fitted acceleration is solved for, and the
/// fitted position and velocity are corrected for that circle (exact for a
/// constant-rate turn, a no-op in straight flight). A window is used if its
/// residual is within [`MLAT_NOISE_M`] (the path is followed) and its turn
/// correction within [`MLAT_TURN_BIAS`]; the widest such window wins. If
/// none passes (a tight turn, or a knot at the end of a segment with no
/// reports on one side), the knot is left as reported.
fn smooth_mlat(k: &mut [Knot]) {
    // ablation switch for the evidence runs
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *OFF.get_or_init(|| std::env::var_os("OVERHEAD_NO_MLAT_SMOOTH").is_some()) {
        return;
    }
    let n = k.len();
    if n < 3 || !k.iter().any(|x| x.mlat) {
        return;
    }
    let orig: Vec<(f64, V3, bool)> = k.iter().map(|x| (x.t, x.p, x.mlat)).collect();
    for i in 0..n {
        if !orig[i].2 {
            continue;
        }
        let fit = MLAT_SMOOTH_S
            .iter()
            .filter_map(|&w| local_fit(&orig, i, w).map(|f| (w, f)))
            .find(|(_, f)| f.rms <= MLAT_NOISE_M && f.turn_bias.is_some_and(|b| b <= MLAT_TURN_BIAS));
        let fit = fit.map(|x| x.1);
        if let Some(f) = fit
            && f.v.iter().chain(f.p.iter()).all(|x| x.is_finite())
        {
            k[i].p = f.p;
            k[i].v = f.v;
            k[i].h = geo::geodetic(f.p).2;
            clamp_vz(&mut k[i]);
        }
    }
}

struct LocalFit {
    p: V3,
    v: V3,
    /// Weighted RMS residual of the MLAT points (m).
    rms: f64,
    /// Predicted relative speed bias in the turn being flown (`None`: the
    /// turn is too tight for the window to say).
    turn_bias: Option<f64>,
}


/// Weighted least-squares quadratic fit of the knot positions within
/// ±`w_half` of knot `i` (needs points on both sides).
fn local_fit(orig: &[(f64, V3, bool)], i: usize, w_half: f64) -> Option<LocalFit> {
    let (ti, pi) = (orig[i].0, orig[i].1);
    let mut pts: Vec<(f64, V3, f64, bool)> = Vec::new();
    let (mut before, mut after) = (false, false);
    for &(tj, pj, mj) in orig {
        let tau = tj - ti;
        if tau.abs() >= w_half {
            continue;
        }
        let u = 1.0 - (tau.abs() / w_half).powi(3);
        let w = u * u * u * if mj { 1.0 } else { 4.0 };
        if w <= 0.0 {
            continue;
        }
        before |= tau < -1.0;
        after |= tau > 1.0;
        pts.push((tau, sub(pj, pi), w, mj));
    }
    if pts.len() < 5 || !before || !after {
        return None;
    }
    // normal matrix and its inverse (adjugate / determinant)
    let mut m = [[0.0f64; 3]; 3];
    for &(tau, _, w, _) in &pts {
        let b = [1.0, tau, tau * tau];
        for a in 0..3 {
            for c in 0..3 {
                m[a][c] += w * b[a] * b[c];
            }
        }
    }
    let det = det3(&m);
    if !(det.abs() > 1e-9 * m[0][0] * m[1][1] * m[2][2]) {
        return None;
    }
    let mut inv = [[0.0f64; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            let (r1, r2) = ((c + 1) % 3, (c + 2) % 3);
            let (c1, c2) = ((r + 1) % 3, (r + 2) % 3);
            inv[r][c] = (m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]) / det;
        }
    }
    // operator rows: coef[k] = Σ_j h[j][k] · y_j
    let h: Vec<[f64; 3]> = pts
        .iter()
        .map(|&(tau, _, w, _)| {
            let b = [1.0, tau, tau * tau];
            let mut o = [0.0; 3];
            for (kk, ok) in o.iter_mut().enumerate() {
                *ok = w * (inv[kk][0] * b[0] + inv[kk][1] * b[1] + inv[kk][2] * b[2]);
            }
            o
        })
        .collect();
    let mut coef = [[0.0f64; 3]; 3]; // coef[basis][axis]
    for (hj, &(_, y, _, _)) in h.iter().zip(&pts) {
        for kk in 0..3 {
            for ax in 0..3 {
                coef[kk][ax] += hj[kk] * y[ax];
            }
        }
    }
    let (mut sw, mut sr) = (0.0, 0.0);
    for &(tau, y, w, mj) in &pts {
        if !mj {
            continue;
        }
        let mut e2 = 0.0;
        for ax in 0..3 {
            let f = coef[0][ax] + coef[1][ax] * tau + coef[2][ax] * tau * tau;
            e2 += (y[ax] - f).powi(2);
        }
        sw += w;
        sr += w * e2;
    }
    let rms = if sw > 0.0 { (sr / sw).sqrt() } else { 0.0 };
    // observed turn rate: the acceleration across the fitted velocity
    let v = coef[1];
    let speed = norm(v);
    let acc = scale(coef[2], 2.0);
    let along = if speed > 0.0 { dot(acc, v) / (speed * speed) } else { 0.0 };
    let a_perp = norm(sub(acc, scale(v, along)));
    let omega_obs = if speed > 1.0 { a_perp / speed } else { 0.0 };
    let taus: Vec<f64> = pts.iter().map(|p| p.0).collect();
    let Some(c) = turn_response(&taus, &h, omega_obs, w_half) else {
        return Some(LocalFit { p: add(pi, coef[0]), v, rms, turn_bias: None });
    };
    // undo the turn's effect: the fitted velocity is V·(s1·T + e1·N) and the
    // fitted position offset V·(c0a·T + q0·N), with T the true direction of
    // flight and N toward the centre of the turn
    let g = c.s1.hypot(c.e1);
    let true_speed = speed / g;
    let (p, v) = if a_perp > 0.0 && speed > 1.0 {
        let c1 = scale(v, 1.0 / speed);
        let nn = scale(sub(acc, scale(v, along)), 1.0 / a_perp);
        let (sp, cp) = c.e1.atan2(c.s1).sin_cos();
        let t_dir = sub(scale(c1, cp), scale(nn, sp));
        let n_dir = add(scale(c1, sp), scale(nn, cp));
        let off = add(scale(t_dir, c.c0a * true_speed), scale(n_dir, c.q0 * true_speed));
        (sub(add(pi, coef[0]), off), scale(t_dir, true_speed))
    } else {
        (add(pi, coef[0]), v)
    };
    Some(LocalFit { p, v, rms, turn_bias: Some((1.0 - g).abs()) })
}

/// What a quadratic fit (operator rows `h` at times `taus`) returns for a
/// unit-speed level turn of rate ω through τ = 0 (tangent T, normal N toward
/// the centre): slope `s1·T + e1·N`, quadratic coefficient `g2·T + q2·N`,
/// offset `c0a·T + q0·N`.
#[derive(Clone, Copy, Debug)]
struct TurnResp {
    s1: f64,
    e1: f64,
    g2: f64,
    q2: f64,
    c0a: f64,
    q0: f64,
}

fn circle_response(taus: &[f64], h: &[[f64; 3]], om: f64) -> TurnResp {
    if om < 1e-9 {
        return TurnResp { s1: 1.0, e1: 0.0, g2: 0.0, q2: 0.0, c0a: 0.0, q0: 0.0 };
    }
    let mut r = TurnResp { s1: 0.0, e1: 0.0, g2: 0.0, q2: 0.0, c0a: 0.0, q0: 0.0 };
    for (&tau, hj) in taus.iter().zip(h) {
        let (x, y) = ((om * tau).sin() / om, (1.0 - (om * tau).cos()) / om);
        r.c0a += hj[0] * x;
        r.q0 += hj[0] * y;
        r.s1 += hj[1] * x;
        r.e1 += hj[1] * y;
        r.g2 += hj[2] * x;
        r.q2 += hj[2] * y;
    }
    r
}

/// The level turn that explains a fit whose turn rate (acceleration across
/// the fitted velocity over the fitted speed) is `omega_obs`: for the circle
/// of rate ω the fit's own weights give `2·|c1 × c2| / |c1|²`, which is ω
/// for small ω and less over a wide window; the ω where it equals
/// `omega_obs` (bisection), and the fit's response to that circle. `None`
/// if no turn of up to 115° over the half-width explains the fit.
fn turn_response(taus: &[f64], h: &[[f64; 3]], omega_obs: f64, w_half: f64) -> Option<TurnResp> {
    let rate = |om: f64| {
        if om < 1e-9 {
            return om;
        }
        let r = circle_response(taus, h, om);
        let c1 = r.s1 * r.s1 + r.e1 * r.e1;
        if c1 > 1e-6 { 2.0 * (r.s1 * r.q2 - r.e1 * r.g2) / c1 } else { f64::INFINITY }
    };
    let hi = 2.0 / w_half;
    if !(rate(hi) >= omega_obs) {
        return None;
    }
    let (mut lo, mut up) = (0.0, hi);
    for _ in 0..40 {
        let mid = 0.5 * (lo + up);
        if rate(mid) < omega_obs { lo = mid } else { up = mid }
    }
    let r = circle_response(taus, h, 0.5 * (lo + up));
    (r.s1 > 0.5).then_some(r)
}

fn det3(m: &[[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

impl Segment {
    pub fn new(knots: Vec<Knot>) -> Self {
        let mut s = Self { knots, sorted: false, lo: [0.0; 3], hi: [0.0; 3], geo: geo::BBox::empty() };
        s.finish();
        s
    }

    /// Recompute what is derived from the knots (after they were edited).
    pub fn finish(&mut self) {
        self.sorted = self.knots.windows(2).all(|w| w[0].t <= w[1].t);
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        let mut g = geo::BBox::empty();
        for k in &self.knots {
            for j in 0..3 {
                lo[j] = lo[j].min(k.p[j]);
                hi[j] = hi[j].max(k.p[j]);
            }
            let (la, lo2, _) = geo::geodetic(k.p);
            g.extend(la, lo2);
        }
        (self.lo, self.hi, self.geo) = (lo, hi, g);
    }

    /// Lower bound on the distance (m) from `o` to any knot (ECEF box of the knots).
    pub fn knot_distance_bound(&self, o: V3) -> f64 {
        let mut d2 = 0.0;
        for j in 0..3 {
            let e = (self.lo[j] - o[j]).max(o[j] - self.hi[j]).max(0.0);
            d2 += e * e;
        }
        d2.sqrt()
    }

    pub fn t0(&self) -> f64 {
        self.knots[0].t
    }
    pub fn t1(&self) -> f64 {
        self.knots.last().unwrap().t
    }

    /// Position, velocity and height at `t` (clamped to the segment, extrapolated linearly).
    pub fn state(&self, t: f64) -> (V3, V3, f64) {
        let mut hint = 0;
        self.state_hint(t, &mut hint)
    }

    /// `state` with a search hint (index of the last interval used).
    pub fn state_hint(&self, t: f64, hint: &mut usize) -> (V3, V3, f64) {
        let k = &self.knots;
        match self.locate_hint(t, hint) {
            Located::Ends(e) => {
                let l = &k[e];
                (add(l.p, scale(l.v, t - l.t)), l.v, l.h + dot_up(l.v, &l.p) * (t - l.t))
            }
            Located::Between(i) => self.hermite(i, t),
        }
    }

    /// Where `t` falls for [`Self::state_hint`]: before the first knot or
    /// after the last (extrapolated linearly from that knot, `hint` kept), or
    /// in the interval from knot `i` (`hint` set to `i`).
    #[inline(always)]
    fn locate_hint(&self, t: f64, hint: &mut usize) -> Located {
        let k = &self.knots;
        let n = k.len();
        if t <= k[0].t {
            return Located::Ends(0);
        }
        if t >= k[n - 1].t {
            return Located::Ends(n - 1);
        }
        let mut i = (*hint).min(n - 2);
        // the last knot at or before `t` (at most n - 2); with sorted knots a
        // binary search finds the same one as the linear scan from the hint
        let fast = self.sorted && !t.is_nan();
        if k[i].t > t {
            i = if fast { k[..n - 1].partition_point(|x| x.t <= t).saturating_sub(1) } else { 0 };
        }
        let mut steps = 0;
        while i + 1 < n - 1 && k[i + 1].t <= t {
            i += 1;
            steps += 1;
            if fast && steps == 8 {
                i = k[..n - 1].partition_point(|x| x.t <= t).saturating_sub(1);
                break;
            }
        }
        *hint = i;
        Located::Between(i)
    }

    /// The Hermite interpolant between knots `i` and `i + 1` at `t`.
    #[inline]
    fn hermite(&self, i: usize, t: f64) -> (V3, V3, f64) {
        let k = &self.knots;
        let (a, b) = (&k[i], &k[i + 1]);
        let h = b.t - a.t;
        let u = (t - a.t) / h;
        let (u2, u3) = (u * u, u * u * u);
        let (h00, h10, h01, h11) = (2.0 * u3 - 3.0 * u2 + 1.0, u3 - 2.0 * u2 + u, -2.0 * u3 + 3.0 * u2, u3 - u2);
        let (d00, d10, d01, d11) = (6.0 * u2 - 6.0 * u, 3.0 * u2 - 4.0 * u + 1.0, -6.0 * u2 + 6.0 * u, 3.0 * u2 - 2.0 * u);
        let mut p = [0.0; 3];
        let mut v = [0.0; 3];
        for j in 0..3 {
            p[j] = h00 * a.p[j] + h10 * h * a.v[j] + h01 * b.p[j] + h11 * h * b.v[j];
            v[j] = (d00 * a.p[j] + d01 * b.p[j]) / h + d10 * a.v[j] + d11 * b.v[j];
        }
        (p, v, a.h + (b.h - a.h) * u)
    }
}

/// See [`Segment::locate_hint`].
#[derive(Clone, Copy, PartialEq, Debug)]
enum Located {
    Ends(usize),
    Between(usize),
}

fn dot_up(v: V3, p: &V3) -> f64 {
    dot(v, scale(*p, 1.0 / norm(*p)))
}

/// What an observer at `o` (ECEF) hears from a segment at absolute time `t`:
/// emission time, range, radial velocity (+ receding) and the source height
/// above the observer.
#[derive(Clone, Copy, Debug)]
pub struct Heard {
    pub tau: f64,
    pub range: f64,
    pub v_radial: f64,
    pub dz: f64,
}

/// Speed of sound model: `c0` at the ground, decreasing with source height
/// (mean along the ray) when `lapse` is on.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Sound {
    pub c0: f64,
    pub lapse: bool,
}

impl Sound {
    pub fn ray_c(&self, dz: f64) -> f64 {
        if self.lapse { self.c0 - 0.5 * C_LAPSE_PER_M * dz.max(0.0) } else { self.c0 }
    }
    pub fn source_c(&self, dz: f64) -> f64 {
        if self.lapse { self.c0 - C_LAPSE_PER_M * dz.max(0.0) } else { self.c0 }
    }
}

/// Most Newton iterations of [`heard`].
const NEWTON_ITERS: usize = 12;

/// Solve the retarded time `τ + |p(τ) − o| / c = t` by Newton's method.
pub fn heard(seg: &Segment, o: V3, up: V3, sound: &Sound, t: f64, guess: Option<f64>, hint: &mut usize) -> Heard {
    let tau = guess.unwrap_or_else(|| {
        let (p, _, _) = seg.state_hint(t, hint);
        t - norm(sub(p, o)) / sound.c0
    });
    let out = Heard { tau, range: 0.0, v_radial: 0.0, dz: 0.0 };
    newton(seg, o, up, sound, t, tau, out, NEWTON_ITERS, hint)
}

/// [`heard`] with `guess = Some(at.tau)`, where `at` is the state its first
/// iteration computes there: a [`heard`] result (for another time, the same
/// segment, observer and sound, `hint` as that call left it) is the state at
/// its own `tau`. That iteration then only takes its step: computing the
/// state again would give the same bits, and `state_hint` would find the
/// interval it found before and leave the hint as it is.
pub fn heard_from(seg: &Segment, o: V3, up: V3, sound: &Sound, t: f64, at: Heard, hint: &mut usize) -> Heard {
    let c = sound.ray_c(at.dz);
    let g = at.tau + at.range / c - t;
    let step = g / (1.0 + at.v_radial / c);
    if step.abs() < 1e-6 {
        return at;
    }
    newton(seg, o, up, sound, t, at.tau - step, at, NEWTON_ITERS - 1, hint)
}

/// [`heard`]'s Newton iterations from `tau` (at most `iters`), `out` the
/// state so far.
#[allow(clippy::too_many_arguments)]
fn newton(seg: &Segment, o: V3, up: V3, sound: &Sound, t: f64, mut tau: f64, mut out: Heard, iters: usize, hint: &mut usize) -> Heard {
    for _ in 0..iters {
        let (p, v, _) = seg.state_hint(tau, hint);
        let r = sub(p, o);
        let range = norm(r).max(1e-6);
        let dz = dot(r, up);
        let c = sound.ray_c(dz);
        let vr = dot(r, v) / range;
        let g = tau + range / c - t;
        out = Heard { tau, range, v_radial: vr, dz };
        let step = g / (1.0 + vr / c);
        tau -= step;
        if step.abs() < 1e-6 {
            break;
        }
    }
    out
}

/// One pass of [`heard_lanes`]: its segment and search hint, the absolute
/// recording time and the first guess of the emission time, and the state
/// there when known (see [`heard_from`]; `at.tau == tau`).
pub struct Lane<'a> {
    pub seg: &'a Segment,
    pub hint: usize,
    pub t: f64,
    pub tau: f64,
    pub at: Option<Heard>,
}

/// [`heard`] with `guess = Some(lane.tau)` (or [`heard_from`] `lane.at`) for
/// every lane (any number), the same to the bit and with the same hints
/// afterwards: up to four lanes go through the Newton iteration together in
/// vector registers, each with the same IEEE operations in the same order as
/// on its own (no fused multiply-add), a lane stopping where it would on its
/// own.
pub fn heard_lanes(lanes: &mut [Lane], o: V3, up: V3, sound: &Sound, out: &mut Vec<Heard>) {
    out.clear();
    #[cfg(target_arch = "x86_64")]
    if lanes_ok() {
        for chunk in lanes.chunks_mut(4) {
            // SAFETY: AVX2 checked by `lanes_ok`
            let h = unsafe { heard_x4(chunk, o, up, sound) };
            out.extend_from_slice(&h[..chunk.len()]);
        }
        return;
    }
    for l in lanes.iter_mut() {
        out.push(match l.at {
            Some(at) => heard_from(l.seg, o, up, sound, l.t, at, &mut l.hint),
            None => heard(l.seg, o, up, sound, l.t, Some(l.tau), &mut l.hint),
        });
    }
}

#[cfg(target_arch = "x86_64")]
fn lanes_ok() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| std::arch::is_x86_feature_detected!("avx2") && std::env::var_os("OVERHEAD_NO_LANES").is_none())
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn heard_x4(lanes: &mut [Lane], o: V3, up: V3, sound: &Sound) -> [Heard; 4] {
    use std::arch::x86_64::*;
    let n = lanes.len();
    debug_assert!((1..=4).contains(&n));
    let set = _mm256_set1_pd;
    let ld = |x: &[f64; 4]| unsafe { _mm256_loadu_pd(x.as_ptr()) };
    let (mul, add_, sub_, div) = (_mm256_mul_pd, _mm256_add_pd, _mm256_sub_pd, _mm256_div_pd);
    let mut tau0 = [0.0f64; 4];
    let mut t = [0.0f64; 4];
    // the first and last knot times of every lane (outside: the ends' lines)
    let (mut first, mut last) = ([0.0f64; 4], [0.0f64; 4]);
    for (i, l) in lanes.iter().enumerate() {
        tau0[i] = l.tau;
        t[i] = l.t;
        first[i] = l.seg.knots[0].t;
        last[i] = l.seg.knots[l.seg.knots.len() - 1].t;
    }
    let (first_v, last_v) = (ld(&first), ld(&last));
    // the knots each lane interpolates between (structure of arrays); a
    // lane whose time stays strictly inside its interval (and inside the
    // track) is in the same interval as `state_hint` would find from the
    // hint, with the hint kept, so only the others go through it
    let mut cur = [usize::MAX; 4];
    let (mut at, mut bt) = ([f64::NAN; 4], [f64::NAN; 4]);
    let (mut ap, mut av, mut bp, mut bv) = ([[0.0f64; 4]; 3], [[0.0f64; 4]; 3], [[0.0f64; 4]; 3], [[0.0f64; 4]; 3]);
    let (o_v, up_v) = ([set(o[0]), set(o[1]), set(o[2])], [set(up[0]), set(up[1]), set(up[2])]);
    let (c0, half_lapse) = (set(sound.c0), set(0.5 * C_LAPSE_PER_M));
    let (one, two, three, four, six, m2, m6) = (set(1.0), set(2.0), set(3.0), set(4.0), set(6.0), set(-2.0), set(-6.0));
    let (tiny, eps, sign) = (set(1e-6), set(1e-6), set(-0.0));
    let t_v = ld(&t);
    let mut tau_v = ld(&tau0);
    // (lanes past `n` start converged)
    let mut live: i32 = (1 << n) - 1;
    let (mut o_tau, mut o_range, mut o_vr, mut o_dz) = (tau_v, _mm256_setzero_pd(), _mm256_setzero_pd(), _mm256_setzero_pd());
    // lanes with the state at their guess: `heard_from`'s first step
    let mut pre: i32 = 0;
    let (mut r_a, mut v_a, mut z_a) = ([0.0f64; 4], [0.0f64; 4], [0.0f64; 4]);
    for (i, l) in lanes.iter().enumerate() {
        if let Some(h) = l.at {
            pre |= 1 << i;
            (r_a[i], v_a[i], z_a[i]) = (h.range, h.v_radial, h.dz);
        }
    }
    let mask_of = |m: i32| {
        _mm256_castsi256_pd(_mm256_cmpgt_epi64(
            _mm256_and_si256(_mm256_set1_epi64x(m as i64), _mm256_setr_epi64x(1, 2, 4, 8)),
            _mm256_setzero_si256(),
        ))
    };
    if pre != 0 {
        let (range, vr, dz) = (ld(&r_a), ld(&v_a), ld(&z_a));
        let c = if sound.lapse { sub_(c0, mul(half_lapse, _mm256_max_pd(dz, _mm256_setzero_pd()))) } else { c0 };
        let g = sub_(add_(tau_v, div(range, c)), t_v);
        let step = div(g, add_(one, div(vr, c)));
        let pm = mask_of(pre);
        o_range = _mm256_blendv_pd(o_range, range, pm);
        o_vr = _mm256_blendv_pd(o_vr, vr, pm);
        o_dz = _mm256_blendv_pd(o_dz, dz, pm);
        tau_v = _mm256_blendv_pd(tau_v, sub_(tau_v, step), pm);
        live &= !(pre & _mm256_movemask_pd(_mm256_cmp_pd::<_CMP_LT_OQ>(_mm256_andnot_pd(sign, step), eps)));
    }
    let (mut at_v, mut bt_v) = (ld(&at), ld(&bt));
    let mut knots_v = [[_mm256_setzero_pd(); 3]; 4];
    let mut tau_a = [0.0f64; 4];
    for it in 0..NEWTON_ITERS {
        if it == NEWTON_ITERS - 1 {
            // (the lanes that took the first step have had all their iterations)
            live &= !pre;
            if live == 0 {
                break;
            }
        }
        if live == 0 {
            break;
        }
        let inside = _mm256_and_pd(
            _mm256_and_pd(_mm256_cmp_pd::<_CMP_LT_OQ>(first_v, tau_v), _mm256_cmp_pd::<_CMP_LT_OQ>(tau_v, last_v)),
            _mm256_and_pd(_mm256_cmp_pd::<_CMP_LT_OQ>(at_v, tau_v), _mm256_cmp_pd::<_CMP_LT_OQ>(tau_v, bt_v)),
        );
        let slow = live & !_mm256_movemask_pd(inside);
        let mut ends = [usize::MAX; 4];
        if slow != 0 {
            unsafe { _mm256_storeu_pd(tau_a.as_mut_ptr(), tau_v) };
            for i in 0..n {
                if slow >> i & 1 == 0 {
                    continue;
                }
                let l = &mut lanes[i];
                match l.seg.locate_hint(tau_a[i], &mut l.hint) {
                    Located::Ends(e) => ends[i] = e,
                    Located::Between(k) => {
                        if cur[i] != k {
                            cur[i] = k;
                            let (a, b) = (&l.seg.knots[k], &l.seg.knots[k + 1]);
                            (at[i], bt[i]) = (a.t, b.t);
                            for j in 0..3 {
                                (ap[j][i], av[j][i], bp[j][i], bv[j][i]) = (a.p[j], a.v[j], b.p[j], b.v[j]);
                            }
                        }
                    }
                }
            }
            (at_v, bt_v) = (ld(&at), ld(&bt));
            for j in 0..3 {
                knots_v[0][j] = ld(&ap[j]);
                knots_v[1][j] = ld(&av[j]);
                knots_v[2][j] = ld(&bp[j]);
                knots_v[3][j] = ld(&bv[j]);
            }
        }
        // `Segment::hermite`, term by term
        let h = sub_(bt_v, at_v);
        let u = div(sub_(tau_v, at_v), h);
        let u2 = mul(u, u);
        let u3 = mul(u2, u);
        let h00 = add_(sub_(mul(two, u3), mul(three, u2)), one);
        let h10 = add_(sub_(u3, mul(two, u2)), u);
        let h01 = add_(mul(m2, u3), mul(three, u2));
        let h11 = sub_(u3, u2);
        let d00 = sub_(mul(six, u2), mul(six, u));
        let d10 = add_(sub_(mul(three, u2), mul(four, u)), one);
        let d01 = add_(mul(m6, u2), mul(six, u));
        let d11 = sub_(mul(three, u2), mul(two, u));
        let (h10h, h11h) = (mul(h10, h), mul(h11, h));
        let mut p = [h; 3];
        let mut v = [h; 3];
        for j in 0..3 {
            let (apj, avj, bpj, bvj) = (knots_v[0][j], knots_v[1][j], knots_v[2][j], knots_v[3][j]);
            p[j] = add_(add_(add_(mul(h00, apj), mul(h10h, avj)), mul(h01, bpj)), mul(h11h, bvj));
            v[j] = add_(add_(div(add_(mul(d00, apj), mul(d01, bpj)), h), mul(d10, avj)), mul(d11, bvj));
        }
        if ends.iter().any(|&e| e != usize::MAX) {
            // lanes beyond the track's ends: linear from the end knot
            let (mut pa, mut va) = ([[0.0f64; 4]; 3], [[0.0f64; 4]; 3]);
            for j in 0..3 {
                unsafe {
                    _mm256_storeu_pd(pa[j].as_mut_ptr(), p[j]);
                    _mm256_storeu_pd(va[j].as_mut_ptr(), v[j]);
                }
            }
            for i in 0..n {
                if ends[i] != usize::MAX {
                    let l = &lanes[i].seg.knots[ends[i]];
                    let pe = add(l.p, scale(l.v, tau_a[i] - l.t));
                    for j in 0..3 {
                        (pa[j][i], va[j][i]) = (pe[j], l.v[j]);
                    }
                }
            }
            for j in 0..3 {
                (p[j], v[j]) = (ld(&pa[j]), ld(&va[j]));
            }
        }
        // `heard`'s step
        let r = [sub_(p[0], o_v[0]), sub_(p[1], o_v[1]), sub_(p[2], o_v[2])];
        let dot3 = |a: &[__m256d; 3], b: &[__m256d; 3]| add_(add_(mul(a[0], b[0]), mul(a[1], b[1])), mul(a[2], b[2]));
        // (max returns its second operand for NaN, as f64::max(NaN, x) = x)
        let range = _mm256_max_pd(_mm256_sqrt_pd(dot3(&r, &r)), tiny);
        let dz = dot3(&r, &up_v);
        let c = if sound.lapse { sub_(c0, mul(half_lapse, _mm256_max_pd(dz, _mm256_setzero_pd()))) } else { c0 };
        let vr = div(dot3(&r, &v), range);
        let g = sub_(add_(tau_v, div(range, c)), t_v);
        let step = div(g, add_(one, div(vr, c)));
        // the lanes still iterating take this step's state and time
        let lv = mask_of(live);
        o_tau = _mm256_blendv_pd(o_tau, tau_v, lv);
        o_range = _mm256_blendv_pd(o_range, range, lv);
        o_vr = _mm256_blendv_pd(o_vr, vr, lv);
        o_dz = _mm256_blendv_pd(o_dz, dz, lv);
        tau_v = _mm256_blendv_pd(tau_v, sub_(tau_v, step), lv);
        let conv = _mm256_movemask_pd(_mm256_cmp_pd::<_CMP_LT_OQ>(_mm256_andnot_pd(sign, step), eps));
        live &= !conv;
    }
    let (mut ta, mut ra, mut va, mut da) = ([0.0f64; 4], [0.0f64; 4], [0.0f64; 4], [0.0f64; 4]);
    unsafe {
        _mm256_storeu_pd(ta.as_mut_ptr(), o_tau);
        _mm256_storeu_pd(ra.as_mut_ptr(), o_range);
        _mm256_storeu_pd(va.as_mut_ptr(), o_vr);
        _mm256_storeu_pd(da.as_mut_ptr(), o_dz);
    }
    std::array::from_fn(|i| Heard { tau: ta[i], range: ra[i], v_radial: va[i], dz: da[i] })
}

/// Doppler factor `f_heard / f_rest` for a [`Heard`] state.
///
/// The radial velocity is taken along the straight line from the aircraft
/// to the observer, and the speed of sound is the mean along the path
/// ([`Sound::ray_c`]), not the one at the aircraft. The shift of a moving
/// source is `v · n / c` at the source, with `n` the direction in which the
/// ray that reaches the observer leaves it. In air that warms towards the
/// ground that ray is an arc, steeper at the aircraft than the straight
/// line; its horizontal slowness `cos θ / c` is the same all along it
/// (Snell), and for an arc the chord's direction is the mean of the
/// directions at its ends, which gives `cos θ_chord / c_mean` for it. With
/// the sound speed at the aircraft (as it was) the swing of an aircraft at
/// 5.5 km came out 3.3 % too large, a fit wanted a ground speed of sound of
/// 355 m/s to undo it, and aircraft at different heights could not be
/// fitted with one. (For a climb or descent rate the vertical slowness at
/// the source would be the right one; at a few m/s the difference is
/// nothing.) `OVERHEAD_SOURCE_C=1` uses the speed at the aircraft, for
/// comparison.
pub fn doppler_factor(h: &Heard, sound: &Sound) -> f64 {
    static AT_SOURCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let c = if *AT_SOURCE.get_or_init(|| std::env::var_os("OVERHEAD_SOURCE_C").is_some()) { sound.source_c(h.dz) } else { sound.ray_c(h.dz) };
    c / (c + h.v_radial)
}

/// Closest approach of a segment to `o` within `[t0, t1]` (emission time):
/// `(t, range)`; golden-section refinement around a coarse scan.
pub fn closest_approach(seg: &Segment, o: V3, t0: f64, t1: f64) -> (f64, f64) {
    let (a, b) = (t0.max(seg.t0()), t1.min(seg.t1()));
    if b <= a {
        return (a, f64::INFINITY);
    }
    let range = |t: f64| norm(sub(seg.state(t).0, o));
    let step = 1.0;
    let n = ((b - a) / step).ceil().max(1.0) as usize;
    let (mut bt, mut br) = (a, f64::INFINITY);
    for i in 0..=n {
        let t = (a + i as f64 * step).min(b);
        let r = range(t);
        if r < br {
            (bt, br) = (t, r);
        }
    }
    let (mut lo, mut hi) = ((bt - step).max(a), (bt + step).min(b));
    let g = 0.5 * (5f64.sqrt() - 1.0);
    for _ in 0..50 {
        let (c, d) = (hi - g * (hi - lo), lo + g * (hi - lo));
        if range(c) < range(d) { hi = d } else { lo = c }
    }
    let t = 0.5 * (lo + hi);
    (t, range(t))
}

/// Geodetic position of a segment at `t`.
pub fn position_at(seg: &Segment, t: f64) -> (f64, f64, f64) {
    geo::geodetic(seg.state(t).0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::Point;

    fn straight_track() -> Track {
        // eastbound at 60 m/s, 500 m, from lon 21.0 at t=1000
        let lat: f64 = 52.0;
        let m_per_deg_lon = norm(sub(ecef(lat, 21.001, 500.0), ecef(lat, 21.0, 500.0))) / 0.001;
        let points = (0..20)
            .map(|i| {
                let t = 1000.0 + 30.0 * i as f64;
                Point {
                    t,
                    lat,
                    lon: 21.0 + 60.0 * (t - 1000.0) / m_per_deg_lon,
                    alt_baro: None,
                    alt_geom: Some(500.0),
                    ground: false,
                    gs: Some(60.0),
                    track: Some(90.0),
                    vrate: Some(0.0),
                    stale: false,
                    new_leg: false,
                    mlat: false,
                }
            })
            .collect();
        Track {
            hex: "abc123".into(),
            reg: None,
            type_code: Some("C172".into()),
            desc: None,
            class: AircraftClass::Piston,
            callsigns: vec![],
            points,
        }
    }

    #[test]
    fn heard_curve_matches_straight_line_model() {
        let f = Flight::from_track(&straight_track(), &FlightOptions::default()).unwrap();
        let seg = &f.segments[0];
        // observer 800 m south of the track at the point passed at t=1300
        let (lat_c, lon_c, _) = position_at(seg, 1300.0);
        let o_lat = lat_c - 800.0 / geo::M_PER_DEG_LAT;
        let o = ecef(o_lat, lon_c, 0.0);
        let (_, _, up) = enu_axes(o_lat, lon_c);
        let (tc, d) = closest_approach(seg, o, 1000.0, 1570.0);
        assert!((tc - 1300.0).abs() < 1.0, "{tc}");
        assert!((d - (800f64.powi(2) + 500f64.powi(2)).sqrt()).abs() < 5.0, "{d}");
        let sound = Sound { c0: 343.0, lapse: false };
        let model = overhead_core::model::Flyby { f0: 100.0, v: 60.0, d, tc: tc + d / 343.0 };
        let mut hint = 0;
        for dt in [-120.0, -40.0, -5.0, 0.0, 7.0, 60.0, 150.0] {
            let t = model.tc + dt;
            let h = heard(seg, o, up, &sound, t, None, &mut hint);
            let f = 100.0 * doppler_factor(&h, &sound);
            assert!((f - model.freq_at(t, 343.0)).abs() < 0.05, "dt={dt}: {f} vs {}", model.freq_at(t, 343.0));
        }
    }

    /// MLAT reports scattered by ~100 m along and across a straight 52 m/s
    /// path, 8 s apart and without velocities: after cleaning, the speed of
    /// the modelled path stays near 52 m/s instead of swinging with the scatter.
    #[test]
    fn mlat_scatter_is_smoothed() {
        let mut tr = straight_track();
        let lat: f64 = 52.0;
        let m_per_deg_lon = norm(sub(ecef(lat, 21.001, 500.0), ecef(lat, 21.0, 500.0))) / 0.001;
        let mut s = 0x9e3779b97f4a7c15u64;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        tr.points = (0..60)
            .map(|i| {
                let t = 1000.0 + 8.0 * i as f64;
                let (ex, ny) = (200.0 * rnd(), 200.0 * rnd());
                Point {
                    t,
                    lat: lat + ny / geo::M_PER_DEG_LAT,
                    lon: 21.0 + (52.0 * (t - 1000.0) + ex) / m_per_deg_lon,
                    alt_baro: Some(500.0),
                    alt_geom: None,
                    ground: false,
                    gs: None,
                    track: None,
                    vrate: None,
                    stale: false,
                    new_leg: false,
                    mlat: true,
                }
            })
            .collect();
        let f = Flight::from_track(&tr, &FlightOptions::default()).unwrap();
        let seg = &f.segments[0];
        let mut worst: f64 = 0.0;
        let mut t = seg.t0() + 60.0;
        while t < seg.t1() - 60.0 {
            worst = worst.max((norm(seg.state(t).1) - 52.0).abs());
            t += 1.0;
        }
        assert!(worst < 8.0, "speed off by up to {worst:.1} m/s");
    }

    /// Speed range (min, max) of a noise-free circular MLAT track flown at
    /// 52 m/s and `rate` °/s, reports 8 s apart (`noise` m of scatter), away
    /// from the segment ends.
    fn turn_speeds(rate: f64, noise: f64, smooth: bool) -> (f64, f64) {
        let mut tr = straight_track();
        let lat: f64 = 52.0;
        let m_per_deg_lon = norm(sub(ecef(lat, 21.001, 500.0), ecef(lat, 21.0, 500.0))) / 0.001;
        let mut s = 0x2545f4914f6cdd1du64;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let om = rate.to_radians();
        let r = 52.0 / om;
        tr.points = (0..60)
            .map(|i| {
                let t = 1000.0 + 8.0 * i as f64;
                let a = om * (t - 1000.0);
                let (ex, ny) = (r * a.sin() + noise * rnd(), r * (1.0 - a.cos()) + noise * rnd());
                Point {
                    t,
                    lat: lat + ny / geo::M_PER_DEG_LAT,
                    lon: 21.0 + ex / m_per_deg_lon,
                    alt_baro: Some(500.0),
                    alt_geom: None,
                    ground: false,
                    gs: None,
                    track: None,
                    vrate: None,
                    stale: false,
                    new_leg: false,
                    mlat: true,
                }
            })
            .collect();
        let opt = FlightOptions { smooth_mlat: smooth, ..FlightOptions::default() };
        let f = Flight::from_track(&tr, &opt).unwrap();
        let seg = &f.segments[0];
        let (mut lo, mut hi) = (f64::INFINITY, 0.0f64);
        let mut t = seg.t0() + 30.0;
        while t < seg.t1() - 30.0 {
            let v = norm(seg.state(t).1);
            (lo, hi) = (lo.min(v), hi.max(v));
            t += 1.0;
        }
        (lo, hi)
    }

    #[test]
    fn mlat_smoothing_keeps_turn_speed() {
        for rate in [0.0, 1.5, 3.0, 6.0, 10.0] {
            let rate = if rate == 0.0 { 1e-4 } else { rate };
            let (rl, rh) = turn_speeds(rate, 0.0, false);
            let (sl, sh) = turn_speeds(rate, 0.0, true);
            eprintln!("{rate:5.1} deg/s: raw {rl:.1}-{rh:.1}, smoothed {sl:.1}-{sh:.1} m/s");
            let (raw_err, err) = ((52.0 - rl).max(rh - 52.0), (52.0 - sl).max(sh - 52.0));
            // never much worse than the reports themselves, and within 3 % up to a standard-rate turn
            assert!(err <= raw_err + 0.5, "{rate} deg/s: smoothed {sl:.1}-{sh:.1}, raw {rl:.1}-{rh:.1}");
            if rate <= 3.0 {
                assert!(err <= 0.03 * 52.0, "{rate} deg/s: smoothed {sl:.1}-{sh:.1}");
            }
        }
        // with 100 m of scatter: never worse than the raw reports, and much
        // better up to a standard-rate turn
        for rate in [1e-4, 1.5, 3.0, 6.0] {
            let (rl, rh) = turn_speeds(rate, 100.0, false);
            let (sl, sh) = turn_speeds(rate, 100.0, true);
            eprintln!("{rate:5.1} deg/s, 100 m scatter: raw {rl:.1}-{rh:.1}, smoothed {sl:.1}-{sh:.1} m/s");
            let (raw_err, err) = ((52.0 - rl).max(rh - 52.0), (52.0 - sl).max(sh - 52.0));
            assert!(err <= raw_err + 0.5, "{rate} deg/s noisy: smoothed {sl:.1}-{sh:.1}, raw {rl:.1}-{rh:.1}");
            if rate <= 3.0 {
                assert!(err <= 0.5 * raw_err, "{rate} deg/s noisy: smoothed {sl:.1}-{sh:.1}, raw {rl:.1}-{rh:.1}");
            }
        }
    }

    /// `heard_lanes` must give `heard`'s bits (and leave the same hints)
    /// for any mix of passes: sorted and unsorted knots, repeated knot
    /// times, times before, inside and after the track, far guesses, NaN
    /// times, with and without the lapse.
    #[test]
    fn heard_lanes_are_bit_identical() {
        let mut st = 0x9e3779b97f4a7c15u64;
        let mut rnd = move || {
            st ^= st << 13;
            st ^= st >> 7;
            st ^= st << 17;
            (st >> 11) as f64 / (1u64 << 53) as f64
        };
        let o = ecef(52.0, 21.0, 100.0);
        let (_, _, up) = enu_axes(52.0, 21.0);
        let mut segs = Vec::new();
        for k in 0..40 {
            let n = 2 + (rnd() * 12.0) as usize;
            let mut t = 1000.0 + rnd() * 50.0;
            let knots: Vec<Knot> = (0..n)
                .map(|_| {
                    let dt = if rnd() < 0.15 { 0.0 } else { 1.0 + rnd() * 40.0 };
                    t += if k % 7 == 3 && rnd() < 0.3 { -dt } else { dt };
                    let lat = 52.0 + (rnd() - 0.5) * 0.1;
                    let lon = 21.0 + (rnd() - 0.5) * 0.1;
                    let p = ecef(lat, lon, 200.0 + rnd() * 3000.0);
                    let v = [(rnd() - 0.5) * 300.0, (rnd() - 0.5) * 300.0, (rnd() - 0.5) * 30.0];
                    Knot { t, p, v, h: 0.0, mlat: false }
                })
                .collect();
            segs.push(Segment::new(knots));
        }
        let mut out = Vec::new();
        let mut checked = 0;
        for case in 0..20_000 {
            let sound = Sound { c0: 330.0 + rnd() * 20.0, lapse: case % 3 != 0 };
            let n = 1 + (rnd() * 9.0) as usize;
            let mut lanes: Vec<Lane> = Vec::new();
            let mut want = Vec::new();
            for _ in 0..n {
                let seg = &segs[(rnd() * segs.len() as f64) as usize];
                let (t0, t1) = (seg.t0().min(seg.t1()), seg.t0().max(seg.t1()));
                let t = if rnd() < 0.02 { f64::NAN } else { t0 - 100.0 + rnd() * (t1 - t0 + 200.0) + 20.0 };
                let tau = if rnd() < 0.2 { t - 5.0 + rnd() * 100.0 } else { t - norm(sub(seg.state(t).0, o)) / sound.c0 + (rnd() - 0.5) };
                let mut hint = (rnd() * seg.knots.len() as f64) as usize;
                // half the lanes go on from an earlier time's state (as a sweep does)
                let at = if rnd() < 0.5 {
                    let t_before = t - rnd() * 2.0;
                    Some(heard(seg, o, up, &sound, t_before, Some(tau), &mut hint))
                } else {
                    None
                };
                let tau = at.map_or(tau, |h| h.tau);
                let mut h2 = hint;
                want.push((heard(seg, o, up, &sound, t, Some(tau), &mut h2), h2));
                if let Some(a) = at {
                    let mut h3 = hint;
                    let f = heard_from(seg, o, up, &sound, t, a, &mut h3);
                    let bits = |h: &Heard| [h.tau.to_bits(), h.range.to_bits(), h.v_radial.to_bits(), h.dz.to_bits()];
                    assert_eq!((bits(&f), h3), (bits(&want.last().unwrap().0), h2), "case {case}: heard_from");
                }
                lanes.push(Lane { seg, hint, t, tau, at });
            }
            heard_lanes(&mut lanes, o, up, &sound, &mut out);
            assert_eq!(out.len(), n);
            for ((g, l), (w, wh)) in out.iter().zip(&lanes).zip(&want) {
                let bits = |h: &Heard| [h.tau.to_bits(), h.range.to_bits(), h.v_radial.to_bits(), h.dz.to_bits()];
                assert_eq!(bits(g), bits(w), "case {case}: {g:?} vs {w:?}");
                assert_eq!(l.hint, *wh, "case {case}: hint");
                checked += 1;
            }
        }
        assert!(checked > 50_000);
    }
}
