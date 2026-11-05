//! Moving sources (aircraft, vehicles) rendered through retarded-time propagation.
//!
//! A source radiates harmonic series ("tone families": propeller blade-pass,
//! rotor, engine firing) and optionally broadband noise, all defined in its own
//! emission time `τ`. The receiver hears, for each path,
//!
//! `p(t) = Σ_k A_k(τ) · 10^(−(α(f_k) R + excess)/20) / R · sin(2π k Φ(τ) + θ_k)`
//!
//! with `τ = τ(t)` from [`propagate::solve`], `R` the range at emission
//! (spherical spreading), `α` the air absorption at the *heard* frequency and
//! `Φ` the integrated source rotation (cycles). Two paths are summed: direct,
//! and the ground reflection modelled as an image receiver mirrored below the
//! ground plane with reflection coefficient Γ (Lloyd's mirror: the
//! slowly changing path difference sweeps interference notches along the lines).
//!
//! Propagation close to the ground adds excess attenuation that grows as the
//! elevation angle drops (ground effect, upward refraction into a shadow zone),
//! and turbulence makes the received level scintillate more with range; both
//! make distant lines fade in and out as in real recordings.
//!
//! Emission time is solved exactly every [`BLOCK`] samples; in between it is a
//! cubic Hermite interpolant using `dτ/dt = c/(c + v_r)`, whose error is far
//! below 1 ns, so the phase is continuous and click-free.

use std::sync::OnceLock;

use crate::dsp::{OnePole, Smooth, absorption_cutoff, absorption_db_per_m, fade};
use crate::propagate::{self, Ray};
use crate::trajectory::{Trajectory, V3};

pub const BLOCK: usize = 32;

/// Integrated phase (cycles) of a slowly varying fundamental frequency.
/// `f` is piecewise linear between knots, so the phase is exactly quadratic.
#[derive(Clone, Debug)]
pub struct PhaseTable {
    t0: f64,
    h: f64,
    f: Vec<f64>,
    cyc: Vec<f64>,
}

impl PhaseTable {
    pub fn new(t0: f64, t1: f64, h: f64, f: impl Fn(f64) -> f64) -> Self {
        let n = ((t1 - t0) / h).ceil().max(1.0) as usize + 1;
        let f: Vec<f64> = (0..n).map(|i| f(t0 + i as f64 * h)).collect();
        let mut cyc = vec![0.0; n];
        for i in 1..n {
            cyc[i] = cyc[i - 1] + 0.5 * h * (f[i - 1] + f[i]);
        }
        Self { t0, h, f, cyc }
    }

    pub fn constant(f: f64) -> Self {
        Self { t0: 0.0, h: 1.0, f: vec![f, f], cyc: vec![0.0, f] }
    }

    #[inline]
    fn locate(&self, t: f64) -> (usize, f64) {
        let x = ((t - self.t0) / self.h).max(0.0);
        let i = (x.floor() as usize).min(self.f.len() - 2);
        (i, (t - self.t0) - i as f64 * self.h)
    }

    #[inline]
    pub fn freq(&self, t: f64) -> f64 {
        let (i, u) = self.locate(t);
        let s = (u / self.h).clamp(0.0, 1.0);
        self.f[i] + (self.f[i + 1] - self.f[i]) * s
    }

    /// Phase (cycles) at time `t`; constant-frequency extrapolation outside the table.
    #[inline]
    pub fn cycles(&self, t: f64) -> f64 {
        let (i, u) = self.locate(t);
        if u > self.h {
            return self.cyc[i + 1] + self.f[i + 1] * (u - self.h);
        }
        if u < 0.0 {
            return self.cyc[0] + self.f[0] * u;
        }
        self.cyc[i] + u * (self.f[i] + (self.f[i + 1] - self.f[i]) * u / (2.0 * self.h))
    }
}

/// A harmonic series radiated by one rotating part.
#[derive(Clone, Debug)]
pub struct ToneFamily {
    pub name: String,
    pub table: PhaseTable,
    /// RMS pressure at 1 m of harmonic `k + 1` (Pa).
    pub amps: Vec<f64>,
    /// Fixed starting phase of each harmonic (rad).
    pub phases: Vec<f64>,
    /// Slow amplitude fluctuation (unit process, scaled by `fading_db`).
    pub fading: Option<Smooth>,
    pub fading_db: f64,
}

/// Broadband emission: pressure at 1 m sampled in emission time.
#[derive(Clone, Debug)]
pub struct Broadband {
    pub t0: f64,
    pub fs: f64,
    pub x: Vec<f32>,
}

impl Broadband {
    #[inline]
    fn at(&self, t: f64) -> f64 {
        let x = (t - self.t0) * self.fs;
        if x < 1.0 || x >= (self.x.len() - 3) as f64 {
            return 0.0;
        }
        let i = x.floor() as usize;
        let u = x - i as f64;
        let (p0, p1, p2, p3) = (self.x[i - 1] as f64, self.x[i] as f64, self.x[i + 1] as f64, self.x[i + 2] as f64);
        p1 + 0.5 * u * (p2 - p0 + u * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3 + u * (3.0 * (p1 - p2) + p3 - p0)))
    }
}

#[derive(Clone, Debug)]
pub struct MovingSource {
    pub traj: Trajectory,
    pub tones: Vec<ToneFamily>,
    pub broadband: Option<Broadband>,
    /// Extra attenuation (dB) for refraction, barriers, ground absorption.
    pub excess_db: f64,
    /// Additional attenuation (dB) reached at grazing incidence, see [`grazing_loss`].
    pub grazing_db: f64,
    /// Heard-time window that is rendered (s); faded in/out over `ramp` s.
    pub t_start: f64,
    pub t_end: f64,
    pub ramp: f64,
}

/// Receiver geometry and medium.
#[derive(Clone, Copy, Debug)]
pub struct Scene {
    pub fs: f64,
    pub c: f64,
    pub mic: V3,
    pub ground_z: f64,
    /// Ground reflection coefficient Γ (0 disables the reflected path).
    pub reflection: f64,
}

impl Scene {
    /// Receiver mirrored below the ground plane: the reflected path's source of sound.
    pub fn image_mic(&self) -> V3 {
        [self.mic[0], self.mic[1], 2.0 * self.ground_z - self.mic[2]]
    }

    fn paths(&self) -> Vec<(V3, f64)> {
        let mut p = vec![(self.mic, 1.0)];
        if self.reflection != 0.0 {
            p.push((self.image_mic(), self.reflection));
        }
        p
    }
}

/// Heard fundamental frequency of `family` at time `t` on the direct path.
pub fn heard_frequency(src: &MovingSource, family: usize, scene: &Scene, t: f64) -> f64 {
    let ray = propagate::solve(&src.traj, scene.mic, scene.c, t, None);
    src.tones[family].table.freq(ray.tau) * ray.dtau_dt(scene.c)
}

/// Excess attenuation (dB) of near-grazing propagation:
/// `grazing_db / (1 + (θ/5°)²) · min(1, R/500 m)`. Overhead sources are not
/// affected, a source a few degrees above the horizon loses most of `grazing_db`,
/// nearby sources keep a direct line of sight.
pub fn grazing_loss(grazing_db: f64, sin_elev: f64, range: f64) -> f64 {
    let theta = sin_elev.clamp(-1.0, 1.0).asin().abs().to_degrees();
    grazing_db / (1.0 + (theta / 5.0).powi(2)) * (range / 500.0).min(1.0)
}

/// Turbulent scintillation grows with range: the fading process is scaled by this factor.
pub fn scintillation_scale(range: f64) -> f64 {
    (0.5 + range / 3000.0).min(3.0)
}

fn cutoff_for_range(range: f64) -> f64 {
    static TABLE: OnceLock<Vec<f64>> = OnceLock::new();
    // log-spaced 1 m .. 1000 km, 20 per decade
    let table = TABLE.get_or_init(|| (0..=120).map(|i| absorption_cutoff(10f64.powf(i as f64 / 20.0))).collect());
    let x = (range.max(1.0).log10() * 20.0).min(119.999);
    let i = x.floor() as usize;
    table[i] + (table[i + 1] - table[i]) * (x - i as f64)
}

/// Render `src` into a buffer starting at sample `i0` (returned).
pub fn render(src: &MovingSource, scene: &Scene) -> (usize, Vec<f32>) {
    let fs = scene.fs;
    let i0 = (src.t_start.max(0.0) * fs).ceil() as usize;
    let i1 = (src.t_end * fs).floor().max(i0 as f64) as usize;
    let mut out = vec![0.0f32; i1 - i0];
    if out.is_empty() {
        return (i0, out);
    }
    let paths = scene.paths();
    let nyq = 0.45 * fs;
    let dtb = BLOCK as f64 / fs;
    let excess = 10f64.powf(-src.excess_db / 20.0);

    // per-harmonic rotation offsets e^{iθ}
    let offsets: Vec<Vec<(f64, f64)>> =
        src.tones.iter().map(|f| f.phases.iter().map(|p| (p.cos(), p.sin())).collect()).collect();
    let n_gain: usize = src.tones.iter().map(|f| f.amps.len()).sum();

    let gains = |ray: &Ray, coef: f64, out: &mut Vec<f64>| {
        out.clear();
        let dtau = ray.dtau_dt(scene.c);
        let spread = coef * excess * std::f64::consts::SQRT_2 / ray.range.max(1.0);
        let graze = grazing_loss(src.grazing_db, ray.sin_elev, ray.range);
        let scint = scintillation_scale(ray.range);
        for fam in &src.tones {
            let f1 = fam.table.freq(ray.tau) * dtau;
            let fade_db = fam.fading.as_ref().map_or(0.0, |s| s.at(ray.tau) * fam.fading_db * scint) - graze;
            for (k, a) in fam.amps.iter().enumerate() {
                let f = f1 * (k + 1) as f64;
                if f >= nyq {
                    out.push(0.0);
                    continue;
                }
                let db = fade_db - absorption_db_per_m(f) * ray.range;
                out.push(a * spread * 10f64.powf(db / 20.0));
            }
        }
    };

    struct PathState {
        o: V3,
        coef: f64,
        ray: Ray,
        g: Vec<f64>,
    }
    let t_first = i0 as f64 / fs;
    let mut states: Vec<PathState> = paths
        .iter()
        .map(|&(o, coef)| {
            let ray = propagate::solve(&src.traj, o, scene.c, t_first, None);
            let mut g = Vec::with_capacity(n_gain);
            gains(&ray, coef, &mut g);
            PathState { o, coef, ray, g }
        })
        .collect();
    let mut g_next = Vec::with_capacity(n_gain);
    let mut lp = OnePole::default();

    let mut start = 0;
    while start < out.len() {
        let len = BLOCK.min(out.len() - start);
        let ta = (i0 + start) as f64 / fs;
        let tb = ta + dtb;
        let mut bb_acc = [0.0f64; BLOCK];
        let mut range_a = 0.0;
        let mut range_b = 0.0;
        let mut graze_db = 0.0;
        for (pi, st) in states.iter_mut().enumerate() {
            let ra = st.ray;
            let guess = ra.tau + dtb * ra.dtau_dt(scene.c);
            let rb = propagate::solve(&src.traj, st.o, scene.c, tb, Some(guess));
            gains(&rb, st.coef, &mut g_next);
            let (da, db) = (ra.dtau_dt(scene.c) * dtb, rb.dtau_dt(scene.c) * dtb);
            if pi == 0 {
                (range_a, range_b) = (ra.range, rb.range);
                graze_db = grazing_loss(src.grazing_db, rb.sin_elev, rb.range);
            }
            for j in 0..len {
                let s = j as f64 / BLOCK as f64;
                let (s2, s3) = (s * s, s * s * s);
                let tau = (2.0 * s3 - 3.0 * s2 + 1.0) * ra.tau
                    + (s3 - 2.0 * s2 + s) * da
                    + (-2.0 * s3 + 3.0 * s2) * rb.tau
                    + (s3 - s2) * db;
                let mut acc = 0.0;
                let mut gi = 0;
                for (fi, fam) in src.tones.iter().enumerate() {
                    let cyc = fam.table.cycles(tau);
                    let (sn, cs) = (std::f64::consts::TAU * cyc.fract()).sin_cos();
                    let (mut zr, mut zi) = (1.0, 0.0);
                    for &(oc, os) in &offsets[fi] {
                        (zr, zi) = (zr * cs - zi * sn, zr * sn + zi * cs);
                        let g = st.g[gi] + (g_next[gi] - st.g[gi]) * s;
                        acc += g * (zr * os + zi * oc);
                        gi += 1;
                    }
                }
                out[start + j] += acc as f32;
                if let Some(bb) = &src.broadband {
                    let r = ra.range + (rb.range - ra.range) * s;
                    bb_acc[j] += st.coef * bb.at(tau) / r.max(1.0);
                }
            }
            st.ray = rb;
            std::mem::swap(&mut st.g, &mut g_next);
        }
        if src.broadband.is_some() {
            let a = OnePole::coeff(fs, cutoff_for_range(0.5 * (range_a + range_b)));
            let g = excess * 10f64.powf(-graze_db / 20.0);
            for j in 0..len {
                out[start + j] += (g * lp.process(bb_acc[j], a)) as f32;
            }
        }
        for j in 0..len {
            let t = (i0 + start + j) as f64 / fs;
            out[start + j] *= fade(t, src.t_start, src.t_end, src.ramp) as f32;
        }
        start += len;
    }
    (i0, out)
}
