//! Physics checks of the numeric synthesizer against the closed-form model.

use overhead_bench::chain::Codec;
use overhead_bench::dsp::{absorption_db_per_m, welch_psd};
use overhead_bench::propagate;
use overhead_bench::scenario::{self, Difficulty, ScenarioConfig};
use overhead_bench::source::{self, MovingSource, PhaseTable, Scene, ToneFamily};
use overhead_bench::trajectory::{FlightPlan, Maneuver, Trajectory, norm, sub};
use overhead_core::model::Flyby;

const C: f64 = 343.0;
const FS: f64 = 8000.0;

fn scene() -> Scene {
    Scene { fs: FS, c: C, mic: [0.0, 0.0, 0.0], ground_z: -1.5, reflection: 0.0 }
}

/// Straight constant-velocity pass equivalent to `p` (CPA 30° above the horizon).
fn straight(p: &Flyby) -> Trajectory {
    let a = 30f64.to_radians();
    let p_cpa = [0.0, p.d * a.cos(), p.d * a.sin()];
    Trajectory::straight(p_cpa, [p.v, 0.0, 0.0], p.tc - p.d / C, -500.0, 1000.0)
}

fn tone(f: f64, amp: f64) -> ToneFamily {
    ToneFamily { name: "t".into(), table: PhaseTable::constant(f), amps: vec![amp], phases: vec![0.3], fading: None, fading_db: 0.0 }
}

fn source(traj: Trajectory, f: f64, t0: f64, t1: f64) -> MovingSource {
    MovingSource { traj, tones: vec![tone(f, 1.0)], broadband: None, excess_db: 0.0, grazing_db: 0.0, t_start: t0, t_end: t1, ramp: 1e-3 }
}

#[test]
fn retarded_frequency_matches_closed_form() {
    let p = Flyby { f0: 120.0, v: 80.0, d: 1500.0, tc: 100.0 };
    let traj = straight(&p);
    for i in 0..=400 {
        let t = i as f64 * 0.5;
        let ray = propagate::solve(&traj, [0.0; 3], C, t, None);
        let f = p.f0 * ray.dtau_dt(C);
        assert!((f - p.freq_at(t, C)).abs() < 1e-6, "t={t}: {f} vs {}", p.freq_at(t, C));
        assert!((ray.range - p.emission_range(t, C)).abs() < 1e-6);
    }
}

/// End to end: demodulate the rendered waveform with the model phase; the residual
/// phase drift (= frequency error) must stay below 0.01 Hz and the envelope follow 1/R.
#[test]
fn rendered_waveform_matches_model() {
    let p = Flyby { f0: 120.0, v: 80.0, d: 1500.0, tc: 100.0 };
    let (t0, t1) = (20.0, 180.0);
    let src = source(straight(&p), p.f0, t0, t1);
    let (i0, x) = source::render(&src, &scene());
    let win = (0.25 * FS) as usize;
    let mut model_cyc = 0.0;
    let mut prev_t = i0 as f64 / FS;
    let mut acc = (0.0, 0.0);
    let mut windows = Vec::new();
    for (j, v) in x.iter().enumerate() {
        let t = (i0 + j) as f64 / FS;
        // trapezoidal integration of the model frequency
        model_cyc += 0.5 * (p.freq_at(prev_t, C) + p.freq_at(t, C)) * (t - prev_t);
        prev_t = t;
        let a = std::f64::consts::TAU * model_cyc;
        // Hann weighting suppresses the 2f image of the real sinusoid
        let w = 1.0 - (std::f64::consts::TAU * ((j % win) as f64 + 0.5) / win as f64).cos();
        acc.0 += w * *v as f64 * a.cos();
        acc.1 += -w * (*v as f64) * a.sin();
        if (j + 1) % win == 0 {
            let tm = t - 0.5 * win as f64 / FS;
            windows.push((tm, acc.1.atan2(acc.0), 2.0 * (acc.0.hypot(acc.1)) / win as f64));
            acc = (0.0, 0.0);
        }
    }
    let mut worst: f64 = 0.0;
    for w in windows.windows(2).skip(2).take(windows.len() - 5) {
        let mut dphi = w[1].1 - w[0].1;
        dphi = (dphi + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
        let df = dphi / (std::f64::consts::TAU * (w[1].0 - w[0].0));
        worst = worst.max(df.abs());
        // amplitude: √2 · A / R (A = 1 Pa rms at 1 m)
        let r = p.emission_range(w[1].0, C);
        let expected = std::f64::consts::SQRT_2 / r * 10f64.powf(-absorption_db_per_m(p.freq_at(w[1].0, C)) * r / 20.0);
        assert!((w[1].2 / expected - 1.0).abs() < 0.01, "t={} amp {} vs {expected}", w[1].0, w[1].2);
    }
    eprintln!("max frequency error {worst:.2e} Hz over {} windows", windows.len());
    assert!(worst < 0.01, "max frequency error {worst} Hz");
}

#[test]
fn heard_cpa_time_matches() {
    let p = Flyby { f0: 120.0, v: 80.0, d: 1500.0, tc: 100.0 };
    let traj = straight(&p);
    let (tau, r) = traj.closest_approach([0.0; 3], -200.0, 400.0);
    assert!((r - p.d).abs() < 1e-3, "d {r}");
    assert!((tau + r / C - p.tc).abs() < 1e-3, "tc {}", tau + r / C);
    let src = source(traj, p.f0, 0.0, 200.0);
    let f = source::heard_frequency(&src, 0, &scene(), p.tc);
    assert!((f - p.f0).abs() < 1e-6, "{f}");
}

#[test]
fn retarded_time_solves_maneuvering_flight() {
    let plan = FlightPlan {
        t_ref: 100.0,
        p_ref: [0.0, 800.0, 600.0],
        heading: 0.3,
        speed: 70.0,
        maneuvers: vec![
            Maneuver { t_start: 80.0, t_end: 130.0, turn_rate: 3f64.to_radians(), ..Default::default() },
            Maneuver { t_start: 90.0, t_end: 160.0, climb: -5.0, accel: 0.5, ..Default::default() },
        ],
    };
    let traj = Trajectory::from_plan(&plan, -100.0, 400.0, 0.05, 100.0);
    let mut guess = None;
    for i in 0..3000 {
        let t = i as f64 * 0.1;
        let ray = propagate::solve(&traj, [0.0; 3], C, t, guess);
        guess = Some(ray.tau);
        let r = norm(sub(traj.state(ray.tau).0, [0.0; 3]));
        assert!((ray.tau + r / C - t).abs() < 1e-9, "t={t}");
    }
    // velocity is the derivative of the interpolated position
    for i in 0..500 {
        let t = -50.0 + i as f64 * 0.731;
        let h = 1e-4;
        let (pa, pb) = (traj.state(t - h).0, traj.state(t + h).0);
        let v = traj.state(t).1;
        for k in 0..3 {
            assert!(((pb[k] - pa[k]) / (2.0 * h) - v[k]).abs() < 1e-4);
        }
    }
}

fn peak_bin(x: &[f32], n_fft: usize) -> (usize, Vec<f64>) {
    let psd = welch_psd(x, FS, n_fft);
    let k = (1..psd.len()).max_by(|a, b| psd[*a].total_cmp(&psd[*b])).unwrap();
    (k, psd)
}

#[test]
fn stationary_tone_lands_in_its_bin() {
    let n_fft = 8192;
    for f in [123.046875, 87.3, 411.7] {
        let src = source(Trajectory::stationary([100.0, 0.0, 0.0]), f, 0.0, 6.0);
        let (_, x) = source::render(&src, &scene());
        let (k, psd) = peak_bin(&x[8000..], n_fft);
        assert_eq!(k, (f * n_fft as f64 / FS).round() as usize, "f={f}");
        // power ≈ (1/R)² (1 Pa rms at 1 m, R = 100 m), summed over the window's main lobe
        let df = FS / n_fft as f64;
        let power: f64 = psd[k - 3..=k + 3].iter().sum::<f64>() * df;
        let expected = (1.0 / 100f64).powi(2) * 10f64.powf(-absorption_db_per_m(f) * 100.0 / 10.0);
        assert!((power / expected - 1.0).abs() < 0.05, "f={f}: {power} vs {expected}");
    }
}

#[test]
fn receding_source_is_shifted_down() {
    // moving straight away at 50 m/s: heard f = f0 · c / (c + 50)
    let f0 = 200.0;
    let traj = Trajectory::straight([500.0, 0.0, 0.0], [50.0, 0.0, 0.0], 0.0, -10.0, 100.0);
    let src = source(traj, f0, 5.0, 25.0);
    let (_, x) = source::render(&src, &scene());
    let n_fft = 65536;
    let (k, _) = peak_bin(&x[..n_fft + 1000], n_fft);
    let f_heard = f0 * C / (C + 50.0);
    assert!((k as f64 * FS / n_fft as f64 - f_heard).abs() <= FS / n_fft as f64, "{k} vs {f_heard}");
}

#[test]
fn scenario_is_deterministic() {
    let cfg = ScenarioConfig { seed: 42, difficulty: Difficulty::Medium, sample_rate: 8000, duration: Some(240.0), codec: Some(Codec::None) };
    let a = scenario::generate(&cfg, "a").unwrap();
    let b = scenario::generate(&cfg, "a").unwrap();
    assert_eq!(a.samples, b.samples);
    let strip = |t: &scenario::Truth| {
        let mut t = t.clone();
        t.synth_seconds = 0.0;
        serde_json::to_string(&t).unwrap()
    };
    assert_eq!(strip(&a.truth), strip(&b.truth));
    assert!(a.samples.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
}

#[test]
fn absorption_follows_iso_table() {
    assert!((absorption_db_per_m(1000.0) - 0.005).abs() < 1e-9);
    assert!((absorption_db_per_m(125.0) - 0.0003).abs() < 1e-9);
    assert!(absorption_db_per_m(300.0) > absorption_db_per_m(250.0));
}
