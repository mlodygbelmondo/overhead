//! Manual correction of fly-bys: refit a curve the user moved on the
//! spectrogram, and fit the Doppler model to a curve traced by hand.
//!
//! Both return an ordinary [`Detection`], so the ADS-B stage takes them like
//! any other. A traced detection carries its points (`Detection::trace`) so
//! the matcher can score candidates by their distance to the traced line.

use crate::audio::Audio;
use crate::detect::{self, Detection, Origin};
use crate::fit::{self, FitConfig, Ranges, Region, RidgePoint, ScoreMap};
use crate::model::Flyby;

/// Fewest traced points accepted.
pub const MIN_TRACE_POINTS: usize = 4;
/// Fewest traced points wanted on each side of the closest approach; with
/// fewer the speed and distance are poorly measured (the fit is still made,
/// its ranges come out open).
pub const MIN_TRACE_SIDE: usize = 2;
/// How precisely a person traces a line on the spectrogram, relative to its
/// frequency (and never below 0.3 Hz). Sets the floor of the fit's
/// uncertainty.
const TRACE_PRECISION: f64 = 0.004;
/// Profile threshold (Δχ²) for the ranges: about 3σ, as hand-traced errors
/// are correlated along the line.
const TRACE_DCHI2: f64 = 9.0;

/// The Doppler model fitted to traced points.
#[derive(Clone, Debug)]
pub struct TraceFit {
    pub flyby: Flyby,
    pub ranges: Ranges,
    /// RMS distance of the points from the fitted curve (Hz).
    pub rms_hz: f64,
    /// Points before / after the heard closest approach.
    pub sides: (usize, usize),
}

/// Fit the model (f0, v, d, tc) to points `[t, f]` traced along one line.
/// Errors are short sentences for the user.
pub fn fit_trace(points: &[[f64; 2]], c: f64) -> Result<TraceFit, String> {
    let mut pts: Vec<RidgePoint> = points
        .iter()
        .filter(|p| p[0].is_finite() && p[1].is_finite() && p[1] > 0.0)
        .map(|p| RidgePoint { t: p[0], f: p[1], weight: 1.0 })
        .collect();
    if pts.len() < MIN_TRACE_POINTS {
        return Err(format!("Trace at least {MIN_TRACE_POINTS} points along the line ({} so far).", pts.len()));
    }
    pts.sort_by(|a, b| a.t.total_cmp(&b.t));
    let span = pts[pts.len() - 1].t - pts[0].t;
    if span < 2.0 {
        return Err("The points must spread along the line in time (a few seconds at least).".into());
    }
    let cfg = FitConfig { c, ..FitConfig::default() };
    let f_mean = pts.iter().map(|p| p.f).sum::<f64>() / pts.len() as f64;
    let first = pts.iter().take(2).map(|p| p.f).sum::<f64>() / 2.0;
    let last = pts.iter().rev().take(2).map(|p| p.f).sum::<f64>() / 2.0;
    if first <= last * 1.001 {
        return Err("A fly-by's tone falls in pitch: start the trace on the high side (before the closest approach) and end it on the low side.".into());
    }

    // Multi-start robust least squares (Cauchy loss at the tracing precision).
    let sigma = (TRACE_PRECISION * f_mean).max(0.3);
    let guess = fit::initial_guess(&pts, &cfg).ok_or("Could not start the fit from these points.")?;
    // the steepest step between neighbouring points is near the closest approach
    let tc_guess = pts
        .windows(2)
        .max_by(|a, b| {
            let sa = (a[0].f - a[1].f) / (a[1].t - a[0].t).max(1e-3);
            let sb = (b[0].f - b[1].f) / (b[1].t - b[0].t).max(1e-3);
            sa.total_cmp(&sb)
        })
        .map_or(guess.tc, |w| 0.5 * (w[0].t + w[1].t));
    let mut best: Option<(Vec<f64>, f64)> = None;
    for tc in [guess.tc, tc_guess] {
        for dd in [0.3, 0.6, 1.0, 1.7, 3.0] {
            for dv in [0.8, 1.0, 1.25] {
                let start = Flyby { v: (guess.v * dv).clamp(10.0, c - 20.0), d: (guess.d * dd).clamp(40.0, 25_000.0), tc, ..guess };
                let (x, fx) = fit::nelder_mead(
                    |x| fit::ls_cost(&pts, &fit::unpack(x), &cfg, sigma),
                    &fit::pack(&start),
                    &[0.5, 3.0, 0.3, 1.0],
                    800,
                );
                if best.as_ref().is_none_or(|(_, b)| fx < *b) {
                    best = Some((x, fx));
                }
            }
        }
    }
    let (x, _) = best.ok_or("The fit failed.")?;
    // polish with plain least squares
    let sq = |x: &[f64]| -> f64 {
        let p = fit::unpack(x);
        if !fit::in_bounds(&p, &cfg) {
            return 1e18;
        }
        pts.iter().map(|q| (p.freq_at(q.t, c) - q.f).powi(2)).sum()
    };
    let (x, cost) = fit::nelder_mead(sq, &x, &[0.2, 1.0, 0.1, 0.5], 1200);
    let flyby = fit::unpack(&x);
    if !fit::in_bounds(&flyby, &cfg) {
        return Err("No fly-by fits these points.".into());
    }
    let n = pts.len();
    let rms_hz = (cost / n as f64).sqrt();
    // noise of one point: what the fit leaves (with 4 parameters used), but
    // never below the tracing precision
    let s2 = (cost / (n as f64 - 4.0).max(1.0)).max(sigma * sigma);
    let thresh = cost + TRACE_DCHI2 * s2;
    let sides = (pts.iter().filter(|p| p.t < flyby.tc).count(), pts.iter().filter(|p| p.t >= flyby.tc).count());

    // profile v, d (log) and tc: re-fit the other parameters at each value
    let profile = |idx: usize, step: f64, lo_b: f64, hi_b: f64| -> (f64, f64, bool) {
        let centre = x[idx];
        let free: Vec<usize> = (0..4).filter(|i| *i != idx).collect();
        let steps = [0.2, 1.0, 0.1, 0.5];
        let walk = |dir: f64| -> (f64, bool) {
            let mut warm: Vec<f64> = free.iter().map(|i| x[*i]).collect();
            let mut prev = (centre, cost);
            for k in 1..=40 {
                let val = centre + dir * step * k as f64;
                if val < lo_b || val > hi_b {
                    return (prev.0, true);
                }
                let full = |s: &[f64]| {
                    let mut f = x.clone();
                    f[idx] = val;
                    for (j, i) in free.iter().enumerate() {
                        f[*i] = s[j];
                    }
                    f
                };
                let sub_steps: Vec<f64> = free.iter().map(|i| steps[*i]).collect();
                let (s, fx) = fit::nelder_mead(|s| sq(&full(s)), &warm, &sub_steps, 300);
                warm = s;
                if fx > thresh {
                    let frac = if fx > prev.1 { ((thresh - prev.1) / (fx - prev.1)).clamp(0.0, 1.0) } else { 0.5 };
                    return (prev.0 + frac * (val - prev.0), false);
                }
                prev = (val, fx);
            }
            (prev.0, true)
        };
        let (lo, lc) = walk(-1.0);
        let (hi, hc) = walk(1.0);
        (lo, hi, lc || hc)
    };
    let v = profile(1, (0.02 * flyby.v).max(0.5), cfg.v_bounds.0, cfg.v_bounds.1.min(c - 10.0));
    let d = profile(2, 0.04, cfg.d_bounds.0.ln(), cfg.d_bounds.1.ln());
    let tc = profile(3, 0.25, f64::NEG_INFINITY, f64::INFINITY);
    // floors: a few percent in speed, a fifth in distance, a second in time
    let ranges = Ranges {
        v: (v.0.min(flyby.v * 0.95), v.1.max(flyby.v * 1.05)),
        d: (d.0.exp().min(flyby.d * 0.8), d.1.exp().max(flyby.d * 1.25)),
        tc: (tc.0.min(flyby.tc - 1.0), tc.1.max(flyby.tc + 1.0)),
        clipped: [v.2, d.2, tc.2],
    };
    Ok(TraceFit { flyby, ranges, rms_hz, sides })
}

/// A detection made from traced points (`origin: traced`, `trace` set).
pub fn trace_detection(points: &[[f64; 2]], c: f64) -> Result<Detection, String> {
    let tf = fit_trace(points, c)?;
    let mut pts: Vec<[f64; 2]> = points.iter().copied().filter(|p| p[0].is_finite() && p[1].is_finite()).collect();
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let (t0, t1) = (pts[0][0], pts[pts.len() - 1][0]);
    let (f_lo, f_hi) = pts.iter().fold((f64::INFINITY, 0.0f64), |(a, b), p| (a.min(p[1]), b.max(p[1])));
    let fb = tf.flyby;
    let one_sided = tf.sides.0 < MIN_TRACE_SIDE || tf.sides.1 < MIN_TRACE_SIDE;
    Ok(Detection {
        flyby: fb,
        harmonics: vec![1.0],
        ranges: tf.ranges,
        score: 0.0,
        contrast: 0.0,
        quality: Default::default(),
        likely_ground_vehicle: detect::likely_ground(&fb, &tf.ranges),
        confident: !one_sided && !tf.ranges.clipped.iter().any(|c| *c),
        other_lines: vec![],
        region: Region { t_start: t0 - 5.0, t_end: t1 + 5.0, f_min: f_lo * 0.97, f_max: f_hi * 1.03 },
        trace: Some(pts),
        origin: Origin::Traced,
    })
}

/// Refit a fly-by near where the user placed it (`guess`): the model snaps
/// to the brightest matching line close by (within ±15 s of `tc` and ±4 % of
/// `f0`); if nothing brighter is that close, the placement itself is kept.
/// Ranges, contrast and quality are measured as for a detection.
pub fn refit_near(audio: &Audio, guess: Flyby, harmonics: &[f64], c: f64) -> Option<Detection> {
    let hs: Vec<f64> = if harmonics.is_empty() { vec![1.0] } else { harmonics.to_vec() };
    let (hmin, hmax) = hs.iter().fold((f64::INFINITY, 0.0f64), |(a, b), h| (a.min(*h), b.max(*h)));
    let cfg = FitConfig { c, harmonics: hs.clone(), ..FitConfig::default() };
    if !fit::in_bounds(&guess, &cfg) {
        return None;
    }
    // long enough for the line to settle on both sides
    let half = (4.0 * guess.d / guess.v).clamp(30.0, 150.0) + 10.0;
    let v = guess.v.min(0.8 * c);
    // room for the contrast's copies shifted by up to 20 bins at the lowest
    // line (and proportionally more at the higher ones)
    let margin = 21.0 * audio.sample_rate / cfg.n_fft as f64 + 5.0;
    let region = Region {
        t_start: (guess.tc - half).max(0.0),
        t_end: (guess.tc + half).min(audio.duration()),
        f_min: (guess.f0 * hmin * c / (c + v) * 0.95 - margin).max(10.0),
        f_max: (guess.f0 * hmax * c / (c - v) * 1.05 + margin * hmax / hmin).min(0.48 * audio.sample_rate),
    };
    if region.t_end - region.t_start < 5.0 || region.f_max <= region.f_min {
        return None;
    }
    let spec = fit::region_spectrogram(audio, &region, &cfg);
    let map = ScoreMap::new(spec);
    let seed = fit::guided_points(&map, &guess, c);
    let placed = map.score(&guess, c, &hs);
    let near = |p: &Flyby| (p.tc - guess.tc).abs() <= 15.0 && (p.f0 / guess.f0 - 1.0).abs() <= 0.04;
    let (flyby, score) = match fit::fit_from(&map, if seed.len() >= 8 { &seed } else { &[] }, guess, &cfg) {
        Some((p, s)) if near(&p) && s > placed => (p, s),
        _ => (guess, placed),
    };
    let ranges = fit::profile_ranges(&map, &flyby, score, &cfg);
    let contrast = fit::contrast(&map, &flyby, &cfg);
    let quality = fit::transition_quality(&map, &flyby, c);
    // the lines other than the strongest, as for a detection
    let one = FitConfig { harmonics: vec![1.0], ..cfg.clone() };
    let strongest = (0..hs.len())
        .map(|i| (i, fit::contrast(&map, &Flyby { f0: hs[i] * flyby.f0, ..flyby }, &one)))
        .fold((0, f64::NEG_INFINITY), |a, b| if b.1 > a.1 { b } else { a })
        .0;
    let mut other_lines: Vec<f64> =
        hs.iter().enumerate().filter(|(i, _)| *i != strongest).map(|(_, h)| h * flyby.f0).collect();
    other_lines.sort_by(f64::total_cmp);
    Some(Detection {
        flyby,
        harmonics: hs,
        ranges,
        score,
        contrast,
        quality,
        likely_ground_vehicle: detect::likely_ground(&flyby, &ranges),
        confident: !ranges.clipped.iter().any(|c| *c),
        other_lines,
        region,
        trace: None,
        origin: Origin::Edited,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DEFAULT_C;

    fn truth() -> Flyby {
        Flyby { f0: 110.0, v: 60.0, d: 900.0, tc: 100.0 }
    }

    /// Hand-like points: every 4 s from 60 to 140 s, with a small deterministic wobble.
    fn traced(p: &Flyby, wobble_hz: f64) -> Vec<[f64; 2]> {
        (0..=20)
            .map(|k| {
                let t = 60.0 + 4.0 * k as f64;
                let w = wobble_hz * ((k * 7 % 5) as f64 - 2.0) / 2.0;
                [t, p.freq_at(t, DEFAULT_C) + w]
            })
            .collect()
    }

    #[test]
    fn trace_fit_recovers_the_fly_by() {
        let p = truth();
        let tf = fit_trace(&traced(&p, 0.4), DEFAULT_C).unwrap();
        let f = tf.flyby;
        assert!((f.f0 - p.f0).abs() < 0.5, "f0 {}", f.f0);
        assert!((f.v - p.v).abs() / p.v < 0.06, "v {}", f.v);
        assert!((f.d - p.d).abs() / p.d < 0.15, "d {}", f.d);
        assert!((f.tc - p.tc).abs() < 1.5, "tc {}", f.tc);
        let r = tf.ranges;
        assert!(r.v.0 <= p.v && p.v <= r.v.1, "v range {:?}", r.v);
        assert!(r.d.0 <= p.d && p.d <= r.d.1, "d range {:?}", r.d);
        assert!(r.tc.0 <= p.tc && p.tc <= r.tc.1, "tc range {:?}", r.tc);
        assert_eq!(tf.sides.0 + tf.sides.1, 21);
    }

    #[test]
    fn sparse_trace_four_points_each_side() {
        let p = Flyby { f0: 95.0, v: 45.0, d: 400.0, tc: 50.0 };
        let pts: Vec<[f64; 2]> = [20.0, 32.0, 40.0, 46.0, 54.0, 60.0, 68.0, 80.0].iter().map(|&t| [t, p.freq_at(t, DEFAULT_C)]).collect();
        let d = trace_detection(&pts, DEFAULT_C).unwrap();
        assert_eq!(d.origin, Origin::Traced);
        assert_eq!(d.trace.as_ref().unwrap().len(), 8);
        assert!((d.flyby.tc - p.tc).abs() < 1.0, "tc {}", d.flyby.tc);
        assert!((d.flyby.v - p.v).abs() / p.v < 0.05, "v {}", d.flyby.v);
        assert!(d.ranges.d.0 <= p.d && p.d <= d.ranges.d.1, "d range {:?}", d.ranges.d);
    }

    #[test]
    fn trace_errors_are_explained() {
        assert!(fit_trace(&[[1.0, 100.0], [2.0, 99.0]], DEFAULT_C).unwrap_err().contains("at least"));
        // rising tone: not a fly-by
        let rising: Vec<[f64; 2]> = (0..6).map(|k| [k as f64 * 3.0, 100.0 + k as f64]).collect();
        assert!(fit_trace(&rising, DEFAULT_C).unwrap_err().contains("falls"));
    }

    /// An edited detection on an unheard fundamental (lines at its 3rd, 4th
    /// and 7th multiples): contrast and ranges are measured, and the other
    /// lines leave out the strongest.
    #[test]
    fn refit_of_lines_above_an_unheard_fundamental() {
        use crate::synth;
        let p = Flyby { f0: 87.5, v: 55.0, d: 900.0, tc: 100.0 };
        let mut amps = vec![0.0; 7];
        (amps[2], amps[3], amps[6]) = (0.4, 0.4, 1.0);
        let src = synth::SynthFlyby { flyby: p, harmonics: amps, amplitude: 0.05 };
        let audio = synth::synthesize(8000.0, 200.0, &[src], 0.3, &[], 11);
        let d = refit_near(&audio, p, &[3.0, 4.0, 7.0], DEFAULT_C).unwrap();
        assert!(d.contrast > 10.0, "{d:#?}");
        assert!(d.ranges.v.0 <= p.v && p.v <= d.ranges.v.1, "{d:#?}");
        let strongest = 7.0 * d.flyby.f0;
        assert_eq!(d.other_lines.len(), 2, "{d:#?}");
        assert!(d.other_lines.iter().all(|f| (f - strongest).abs() > 1.0), "{d:#?}");
    }

    #[test]
    fn refit_snaps_to_the_line_near_the_placement() {
        use crate::synth;
        let p = truth();
        let src = synth::SynthFlyby { flyby: p, harmonics: vec![1.0], amplitude: 0.05 };
        let audio = synth::synthesize(4000.0, 200.0, &[src], 0.3, &[], 7);
        // the user dropped it 6 s late and 2 % high, too steep
        let guess = Flyby { f0: p.f0 * 1.02, v: p.v * 1.15, d: p.d * 0.7, tc: p.tc + 6.0 };
        let d = refit_near(&audio, guess, &[1.0], DEFAULT_C).unwrap();
        assert_eq!(d.origin, Origin::Edited);
        assert!((d.flyby.tc - p.tc).abs() < 1.5, "tc {}", d.flyby.tc);
        assert!((d.flyby.f0 - p.f0).abs() < 0.6, "f0 {}", d.flyby.f0);
        assert!((d.flyby.v - p.v).abs() / p.v < 0.1, "v {}", d.flyby.v);
    }
}
