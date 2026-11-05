//! Seeded scenario generation: a scene of fly-bys, vehicles and interference,
//! rendered to a recording plus a ground-truth description.
//!
//! # Ground truth of a fly-by
//! For each aircraft the equivalent [`Flyby`] is taken from its actual
//! (possibly maneuvering) trajectory: `d` = minimum 3D range to the microphone
//! over emission time, `v` = speed at that moment, `tc` = when the sound
//! emitted there is heard (`τ* + d/c`), and `f0` = the fundamental's rest
//! frequency at `τ*`.
//!
//! # `expected_detectable`
//! Estimated per-bin SNR at CPA of every line (family `j`, harmonic `k`) whose
//! rest frequency lies inside the default detector band:
//!
//! `SNR = a² / (N(f) · max(ENBW, |ḟ| · T))`
//!
//! * `a` — RMS tone pressure at CPA: `A_k · 10^(−(α(f)·d + excess + G(θ))/20) / d ·
//!   √(1 + Γ²)` (spherical spreading, air absorption, excess and grazing-angle
//!   attenuation at the CPA elevation θ, the ground-reflected path added
//!   incoherently; mean scintillation is 0 dB);
//! * `N(f)` — one-sided PSD of everything *except* aircraft, measured on the
//!   rendered background in `tc ± 4 s` (Welch, Hann, `T` ≈ 1.024 s windows),
//!   median over `f ± 5 Hz` so narrow interfering lines do not count;
//! * `ENBW = 1.5 / T` — Hann equivalent noise bandwidth of one bin;
//! * `ḟ = f v² / (c d)` — the line's slope at CPA; a line sweeping more than
//!   one bin per window spreads its power over `|ḟ|·T` Hz.
//!
//! A fly-by is expected detectable when its best line has `SNR ≥ 6 dB`, its
//! speed is at least the detector's `v_min` and `tc` lies at least 30 s inside
//! the recording. The chain (AGC, clipping, codec) is not included.

use std::sync::Mutex;

use anyhow::Result;
use overhead_core::detect::DetectConfig;
use overhead_core::model::{DEFAULT_C, Flyby};
use serde::{Deserialize, Serialize};

use crate::ambient::{self, Birds, Conversation, Dog, Floor, Fragment, Gust, Hum, Machine, Part, Rain};
use crate::chain::{self, Agc, Codec};
use crate::dsp::{Smooth, absorption_db_per_m, pa, pa2_per_hz, shaped_noise, to_db, welch_psd};
use crate::propagate;
use crate::rng::Rng;
use crate::source::{self, Broadband, MovingSource, PhaseTable, Scene, ToneFamily};
use crate::trajectory::{FlightPlan, Maneuver, Trajectory, norm, sub};

/// Bump when generation changes, so cached recordings are regenerated.
pub const GENERATOR_VERSION: u32 = 3;
pub const SNR_THRESHOLD_DB: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Difficulty {
    Easy,
    Medium,
    Hard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Aircraft,
    Vehicle,
    Speech,
    Hum,
    Machinery,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AircraftKind {
    Piston,
    Turboprop,
    Helicopter,
    Jet,
}

#[derive(Clone, Debug)]
pub struct ScenarioConfig {
    pub seed: u64,
    pub difficulty: Difficulty,
    pub sample_rate: u32,
    /// Override the drawn duration (s).
    pub duration: Option<f64>,
    /// Override the drawn codec.
    pub codec: Option<Codec>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LineTruth {
    pub family: String,
    pub harmonic: usize,
    /// Rest frequency at CPA (Hz).
    pub f_rest: f64,
    /// Heard frequency range over the pass (Hz).
    pub f_heard: (f64, f64),
    /// Received RMS level at CPA (dB SPL) and background PSD there (dB/Hz).
    pub level_db: f64,
    pub noise_db_hz: f64,
    pub snr_db: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AircraftTruth {
    pub id: usize,
    pub kind: AircraftKind,
    pub flyby: Flyby,
    pub expected_detectable: bool,
    /// Best line SNR at CPA (dB, see module docs); −99 when no line is in the detector band.
    pub snr_db: f64,
    pub lines: Vec<LineTruth>,
    pub altitude_agl: f64,
    /// Elevation angle of the source at CPA (deg).
    pub elevation_deg: f64,
    pub horizontal_offset: f64,
    pub heading_deg: f64,
    pub maneuvers: Vec<Maneuver>,
    /// A maneuver is active within ±30 s (emission time) of CPA.
    pub maneuver_near_cpa: bool,
    pub excess_db: f64,
    /// Grazing-incidence attenuation parameter (dB), see [`source::grazing_loss`].
    pub grazing_db: f64,
    /// Heard-time window in which the source was rendered (s).
    pub t_rendered: (f64, f64),
    /// Tone families: name, fundamental at CPA (Hz), harmonic levels at 1 m (dB SPL).
    pub families: Vec<(String, f64, Vec<f64>)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceTruth {
    pub id: usize,
    pub category: Category,
    pub kind: String,
    pub t_start: f64,
    pub t_end: f64,
    /// Frequency bands (Hz) the source occupies (tonal lines or broadband range).
    pub bands: Vec<(f64, f64)>,
    /// Equivalent fly-by for moving sources.
    pub flyby: Option<Flyby>,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Truth {
    pub version: u32,
    pub name: String,
    pub seed: u64,
    pub difficulty: Difficulty,
    pub duration: f64,
    pub sample_rate: u32,
    pub c: f64,
    pub ground_elevation: f64,
    pub mic_height: f64,
    pub reflection: f64,
    /// SPL (dB) of a full-scale sine's RMS before AGC.
    pub full_scale_db: f64,
    pub agc: Option<Agc>,
    pub clipped_samples: usize,
    pub codec: Codec,
    pub snr_threshold_db: f64,
    pub aircraft: Vec<AircraftTruth>,
    pub sources: Vec<SourceTruth>,
    /// Wall-clock synthesis time (s).
    pub synth_seconds: f64,
}

struct Params {
    dur: (f64, f64),
    n_aircraft: (usize, usize),
    kinds: [f64; 4],
    d_min: f64,
    d_max: f64,
    p_maneuver: f64,
    excess: (f64, f64),
    grazing: (f64, f64),
    pink: (f64, f64),
    rumble: (f64, f64),
    n_vehicles: (usize, usize),
    p_hum: f64,
    hum_snr: (f64, f64),
    n_machines: (usize, usize),
    gusts_per_hour: f64,
    gust_db: (f64, f64),
    talks_per_hour: f64,
    bird_bouts_per_hour: f64,
    p_rain: f64,
    dogs_per_hour: f64,
    p_agc: f64,
    full_scale: (f64, f64),
    codecs: [f64; 4],
    tc_margin: f64,
    clutter_per_hour: f64,
}

fn params(d: Difficulty) -> Params {
    match d {
        Difficulty::Easy => Params {
            dur: (600.0, 1200.0),
            n_aircraft: (1, 3),
            kinds: [0.55, 0.45, 0.0, 0.0],
            d_min: 300.0,
            d_max: 3000.0,
            p_maneuver: 0.0,
            excess: (0.0, 6.0),
            grazing: (0.0, 8.0),
            pink: (14.0, 20.0),
            rumble: (20.0, 28.0),
            n_vehicles: (0, 2),
            p_hum: 0.3,
            hum_snr: (5.0, 15.0),
            n_machines: (0, 1),
            gusts_per_hour: 3.0,
            gust_db: (30.0, 45.0),
            talks_per_hour: 0.0,
            bird_bouts_per_hour: 6.0,
            p_rain: 0.0,
            dogs_per_hour: 0.0,
            p_agc: 0.0,
            full_scale: (100.0, 106.0),
            codecs: [1.0, 0.0, 0.0, 0.0],
            tc_margin: 90.0,
            clutter_per_hour: 120.0,
        },
        Difficulty::Medium => Params {
            dur: (1200.0, 2400.0),
            n_aircraft: (0, 4),
            kinds: [0.4, 0.3, 0.15, 0.15],
            d_min: 200.0,
            d_max: 6000.0,
            p_maneuver: 0.3,
            excess: (3.0, 18.0),
            grazing: (5.0, 22.0),
            pink: (20.0, 28.0),
            rumble: (25.0, 33.0),
            n_vehicles: (2, 10),
            p_hum: 0.6,
            hum_snr: (8.0, 25.0),
            n_machines: (0, 2),
            gusts_per_hour: 10.0,
            gust_db: (35.0, 55.0),
            talks_per_hour: 3.0,
            bird_bouts_per_hour: 15.0,
            p_rain: 0.15,
            dogs_per_hour: 2.0,
            p_agc: 0.2,
            full_scale: (96.0, 104.0),
            codecs: [0.5, 0.5, 0.0, 0.0],
            tc_margin: 60.0,
            clutter_per_hour: 300.0,
        },
        Difficulty::Hard => Params {
            dur: (1800.0, 3600.0),
            n_aircraft: (0, 6),
            kinds: [0.3, 0.25, 0.2, 0.25],
            d_min: 200.0,
            d_max: 8000.0,
            p_maneuver: 0.5,
            excess: (5.0, 18.0),
            grazing: (8.0, 25.0),
            pink: (22.0, 30.0),
            rumble: (28.0, 38.0),
            n_vehicles: (5, 25),
            p_hum: 0.9,
            hum_snr: (10.0, 35.0),
            n_machines: (1, 4),
            gusts_per_hour: 25.0,
            gust_db: (40.0, 65.0),
            talks_per_hour: 6.0,
            bird_bouts_per_hour: 25.0,
            p_rain: 0.35,
            dogs_per_hour: 5.0,
            p_agc: 0.5,
            full_scale: (88.0, 100.0),
            codecs: [0.2, 0.3, 0.3, 0.2],
            tc_margin: 20.0,
            clutter_per_hour: 600.0,
        },
    }
}

/// Something to render on a worker thread.
enum Job {
    Moving { src: MovingSource, bb: Option<BroadbandSpec>, aircraft: bool },
    Floor(Floor, Rng),
    Hum(Hum, Rng),
    Machine(Machine, Rng),
    Gust(Gust, Rng),
    Talk(Conversation, Rng),
    Birds(Birds, Rng),
    Rain(Rain, Rng),
    Dog(Dog, Rng),
    Fragment(Fragment, Rng),
}

/// Broadband emission spectrum `P·x²/(1+x²)²`, `x = f/f_peak`, with total
/// level `level_db` at 1 m.
struct BroadbandSpec {
    level_db: f64,
    f_peak: f64,
    rng: Rng,
}

struct Planner<'a> {
    rng: Rng,
    p: Params,
    dur: f64,
    scene: Scene,
    floor: &'a Floor,
    jobs: Vec<Job>,
    sources: Vec<SourceTruth>,
    aircraft: Vec<AircraftTruth>,
}

/// Background PSD (dB re (20 µPa)²/Hz) of the ambient floor.
fn floor_db(fl: &Floor, f: f64) -> f64 {
    let f = f.max(5.0);
    to_db((pa2_per_hz(fl.pink_db_100) * 100.0 / f + pa2_per_hz(fl.rumble_db) / (1.0 + (f / 40.0).powi(2)).powi(2)) / pa2_per_hz(0.0))
}

impl Planner<'_> {
    fn next_id(&self) -> usize {
        self.sources.len()
    }

    /// Range at which a line of `level_db` (at 1 m) falls 20 dB below the floor in one bin.
    fn audible_range(&self, level_db: f64, f: f64, excess: f64) -> f64 {
        let noise = floor_db(self.floor, f) + 1.8;
        let (mut lo, mut hi) = (1.0f64, 80_000.0f64);
        for _ in 0..50 {
            let r = (lo * hi).sqrt();
            let l = level_db - 20.0 * r.log10() - absorption_db_per_m(f) * r - excess;
            if l > noise - 20.0 { lo = r } else { hi = r }
        }
        lo
    }

    /// Heard-time window where the source is within `r_cut` of the microphone.
    fn heard_window(&self, traj: &Trajectory, r_cut: f64, ramp: f64) -> Option<(f64, f64)> {
        let (mut first, mut last) = (None, None);
        let mut guess = None;
        let mut t = -ramp;
        while t <= self.dur + ramp {
            let ray = propagate::solve(traj, self.scene.mic, self.scene.c, t, guess);
            guess = Some(ray.tau + ray.dtau_dt(self.scene.c));
            if ray.range < r_cut {
                first.get_or_insert(t);
                last = Some(t);
            }
            t += 1.0;
        }
        Some(((first? - ramp).max(-ramp), (last? + ramp).min(self.dur + ramp)))
    }

    /// Tone family with slow ≈0.2 % frequency jitter and ±`fading_db` amplitude fading.
    fn family(&mut self, name: &str, f: f64, levels_db: &[f64], jitter: f64, fading_db: f64, span: (f64, f64)) -> ToneFamily {
        let rng = &mut self.rng;
        let (s1, s2) = (Smooth::new(rng, span.0, span.1, 5.0), Smooth::new(rng, span.0, span.1, 1.2));
        let table = PhaseTable::new(span.0, span.1, 0.05, |t| f * (1.0 + jitter * (0.8 * s1.at(t) + 0.6 * s2.at(t))));
        ToneFamily {
            name: name.to_string(),
            table,
            amps: levels_db.iter().map(|l| pa(*l)).collect(),
            phases: levels_db.iter().map(|_| rng.range(0.0, std::f64::consts::TAU)).collect(),
            fading: Some(Smooth::new(rng, span.0, span.1, 1.5)),
            fading_db,
        }
    }

    /// Harmonic levels: `l1` minus `slope` dB per octave, with random scatter.
    fn ladder(&mut self, l1: f64, n: usize, slope: f64, scatter: f64) -> Vec<f64> {
        (1..=n).map(|k| l1 - slope * (k as f64).log2() + if k == 1 { 0.0 } else { scatter * self.rng.gaussian() }).collect()
    }

    fn aircraft(&mut self, kind: AircraftKind, tc_hint: Option<f64>) {
        let rng = &mut self.rng;
        let (v_rng, alt_rng, d_cap) = match kind {
            AircraftKind::Piston => ((40.0, 90.0), (150.0, 1500.0), 6000.0),
            AircraftKind::Turboprop => ((90.0, 150.0), (500.0, 3000.0), 8000.0),
            AircraftKind::Helicopter => ((30.0, 70.0), (150.0, 800.0), 4000.0),
            AircraftKind::Jet => ((110.0, 200.0), (1500.0, 3000.0), 8000.0),
        };
        let speed = rng.range(v_rng.0, v_rng.1);
        let alt = rng.log_range(alt_rng.0, alt_rng.1);
        let d_hi = self.p.d_max.min(d_cap);
        let d_lo = alt.max(self.p.d_min);
        let d = if d_lo < d_hi { rng.log_range(d_lo, d_hi) } else { alt };
        let offset = (d * d - alt * alt).max(0.0).sqrt();
        let heading = rng.range(0.0, std::f64::consts::TAU);
        let m = self.p.tc_margin;
        let tc = tc_hint.unwrap_or_else(|| rng.range(m, self.dur - m));
        let side = rng.sign();
        let g = self.scene.ground_z;
        let p_cpa = [-heading.sin() * offset * side, heading.cos() * offset * side, g + alt];
        let t_ref = tc - d / self.scene.c;
        let mut maneuvers = Vec::new();
        if rng.chance(self.p.p_maneuver) {
            let heli = kind == AircraftKind::Helicopter;
            for _ in 0..rng.int(1, 3) {
                let near = rng.chance(0.3);
                let t_start = t_ref + if near { rng.range(-40.0, 20.0) } else { rng.range(-240.0, 240.0) };
                let mut mv = Maneuver { t_start, ..Default::default() };
                match rng.int(0, 2) {
                    0 => {
                        let rate = rng.range(1.5, if heli { 6.0 } else { 3.0 }).to_radians();
                        mv.turn_rate = rate * rng.sign();
                        mv.t_end = t_start + rng.range(15.0, 60.0);
                    }
                    1 => {
                        mv.climb = rng.range(2.0, if heli { 4.0 } else { 7.0 }) * rng.sign();
                        mv.t_end = t_start + rng.range(20.0, 120.0);
                    }
                    _ => {
                        mv.accel = rng.range(0.3, 1.0) * rng.sign();
                        mv.t_end = t_start + rng.range(10.0, 40.0);
                    }
                }
                maneuvers.push(mv);
            }
        }
        let plan = FlightPlan { t_ref, p_ref: p_cpa, heading, speed, maneuvers: maneuvers.clone() };
        let span = (-900.0, self.dur + 60.0);
        let z_floor = g + if kind == AircraftKind::Helicopter { 60.0 } else { 100.0 };
        let traj = Trajectory::from_plan(&plan, span.0, span.1, 0.05, z_floor);

        // sound sources: (name, fundamental, harmonic levels at 1 m)
        let mut fams: Vec<(String, f64, Vec<f64>)> = Vec::new();
        let (bb_level, bb_peak);
        match kind {
            AircraftKind::Piston => {
                let f0 = self.rng.range(60.0, 200.0);
                let l1 = self.rng.range(122.0, 132.0);
                let n = self.rng.int(3, 8);
                let slope = self.rng.range(4.0, 9.0);
                let lv = self.ladder(l1, n, slope, 2.5);
                fams.push(("propeller".into(), f0, lv.clone()));
                if self.rng.chance(0.25) {
                    let k = 1.0 + self.rng.sign() * self.rng.range(0.003, 0.015);
                    let dl = self.rng.range(0.0, 4.0);
                    fams.push(("propeller_2".into(), f0 * k, lv.iter().map(|l| l - dl).collect()));
                }
                if self.rng.chance(0.5) {
                    let n = self.rng.int(2, 4);
                    let el = l1 - self.rng.range(8.0, 16.0);
                    let lv = self.ladder(el, n, 3.0, 3.0);
                    fams.push(("engine".into(), f0 / 2.0, lv));
                }
                bb_level = l1 - 2.0;
                bb_peak = self.rng.range(300.0, 800.0);
            }
            AircraftKind::Turboprop => {
                let f0 = self.rng.range(85.0, 250.0);
                let l1 = self.rng.range(128.0, 138.0);
                let n = self.rng.int(3, 6);
                let slope = self.rng.range(5.0, 9.0);
                let lv = self.ladder(l1, n, slope, 2.5);
                fams.push(("propeller".into(), f0, lv.clone()));
                if self.rng.chance(0.6) {
                    let k = 1.0 + self.rng.sign() * self.rng.range(0.001, 0.008);
                    let dl = self.rng.range(0.0, 3.0);
                    fams.push(("propeller_2".into(), f0 * k, lv.iter().map(|l| l - dl).collect()));
                }
                let fh = self.rng.range(700.0, 2500.0);
                let lh = l1 - self.rng.range(15.0, 25.0);
                fams.push(("turbine".into(), fh, vec![lh, lh - 8.0]));
                bb_level = l1;
                bb_peak = self.rng.range(300.0, 800.0);
            }
            AircraftKind::Helicopter => {
                let f0 = self.rng.range(12.0, 25.0);
                let l1 = self.rng.range(128.0, 136.0);
                let n = ((450.0 / f0) as usize).min(30);
                let slope = self.rng.range(4.0, 7.0);
                let lv = self.ladder(l1, n, slope, 3.0);
                fams.push(("main_rotor".into(), f0, lv));
                let ft = self.rng.range(60.0, 120.0);
                let lt = self.rng.range(118.0, 128.0);
                let n = self.rng.int(3, 6);
                let slope = self.rng.range(5.0, 8.0);
                let lv = self.ladder(lt, n, slope, 2.0);
                fams.push(("tail_rotor".into(), ft, lv));
                bb_level = l1;
                bb_peak = 200.0;
            }
            AircraftKind::Jet => {
                let ff = self.rng.range(1200.0, 3500.0);
                let lf = self.rng.range(118.0, 126.0);
                fams.push(("fan".into(), ff, vec![lf, lf - 6.0]));
                bb_level = self.rng.range(145.0, 152.0);
                bb_peak = self.rng.range(150.0, 400.0);
            }
        }
        let excess = self.rng.range(self.p.excess.0, self.p.excess.1);
        let grazing = self.rng.range(self.p.grazing.0, self.p.grazing.1);
        let tones: Vec<ToneFamily> =
            fams.iter().map(|(name, f, lv)| self.family(name, *f, lv, 0.002, 1.5, span)).collect();

        // truth from the actual trajectory
        let mic = self.scene.mic;
        let (tau_star, r_star) = traj.closest_approach(mic, -600.0, self.dur);
        let v_star = norm(traj.state(tau_star).1);
        let tc_star = tau_star + r_star / self.scene.c;
        let near = maneuvers.iter().any(|m| m.t_start < tau_star + 30.0 && m.t_end > tau_star - 30.0);
        let f_star = tones[0].table.freq(tau_star);
        let sin_elev = (traj.state(tau_star).0[2] - mic[2]) / r_star;
        let flyby = Flyby { f0: f_star, v: v_star, d: r_star, tc: tc_star };

        let r_cut = if kind == AircraftKind::Jet {
            25_000.0
        } else {
            let mut r: f64 = 0.0;
            for (_, f, lv) in &fams {
                for (k, l) in lv.iter().enumerate() {
                    r = r.max(self.audible_range(*l, f * (k + 1) as f64, excess));
                }
            }
            r.clamp(1.3 * r_star, 40_000.0)
        };
        let ramp = 20.0;
        let Some((t0, t1)) = self.heard_window(&traj, r_cut, ramp) else { return };

        let id = self.next_id();
        let c = self.scene.c;
        let mut bands = Vec::new();
        let mut families = Vec::new();
        for (fam, (name, _, lv)) in tones.iter().zip(&fams) {
            let f = fam.table.freq(tau_star);
            for k in 1..=lv.len() {
                let fk = f * k as f64;
                bands.push((fk * c / (c + v_star), fk * c / (c - v_star)));
            }
            families.push((name.clone(), f, lv.clone()));
        }
        self.sources.push(SourceTruth {
            id,
            category: Category::Aircraft,
            kind: format!("{kind:?}").to_lowercase(),
            t_start: t0.max(0.0),
            t_end: t1.min(self.dur),
            bands,
            flyby: Some(flyby),
            note: String::new(),
        });
        self.aircraft.push(AircraftTruth {
            id,
            kind,
            flyby,
            expected_detectable: false,
            snr_db: -99.0,
            lines: Vec::new(),
            altitude_agl: alt,
            elevation_deg: sin_elev.asin().to_degrees(),
            horizontal_offset: offset,
            heading_deg: heading.to_degrees(),
            maneuvers,
            maneuver_near_cpa: near,
            excess_db: excess,
            grazing_db: grazing,
            t_rendered: (t0, t1),
            families,
        });
        let bb = Some(BroadbandSpec { level_db: bb_level, f_peak: bb_peak, rng: self.rng.fork() });
        let src = MovingSource { traj, tones, broadband: None, excess_db: excess, grazing_db: grazing, t_start: t0, t_end: t1, ramp };
        self.jobs.push(Job::Moving { src, bb, aircraft: true });
    }

    /// Road traffic along one straight road `road_offset` m from the microphone.
    fn vehicle(&mut self, road_heading: f64, road_offset: f64) {
        let rng = &mut self.rng;
        let kinds = ["car", "truck", "motorbike", "tractor"];
        let kind = kinds[rng.weighted(&[0.6, 0.2, 0.12, 0.08])];
        let (v, f_eng, l_eng, l_tyre, h, n) = match kind {
            "car" => (rng.range(12.0, 35.0), rng.range(45.0, 120.0), rng.range(78.0, 88.0), 88.0, 0.4, 3),
            "truck" => (rng.range(10.0, 25.0), rng.range(30.0, 75.0), rng.range(90.0, 100.0), 95.0, 1.0, 5),
            "motorbike" => (rng.range(12.0, 35.0), rng.range(60.0, 150.0), rng.range(88.0, 98.0), 84.0, 0.6, 4),
            _ => (rng.range(8.0, 12.0), rng.range(30.0, 55.0), rng.range(90.0, 100.0), 85.0, 1.5, 5),
        };
        let l_tyre = l_tyre + 30.0 * (v / 20.0).log10();
        let dir = road_heading + if rng.chance(0.5) { 0.0 } else { std::f64::consts::PI };
        let tc = rng.range(10.0, self.dur - 10.0);
        let g = self.scene.ground_z;
        let foot = [-road_heading.sin() * road_offset, road_heading.cos() * road_offset, g + h];
        let d = norm(sub(foot, self.scene.mic));
        let vel = [v * dir.cos(), v * dir.sin(), 0.0];
        let t_ref = tc - d / self.scene.c;
        let traj = Trajectory::straight(foot, vel, t_ref, -300.0, self.dur + 60.0);
        let span = (-300.0, self.dur + 60.0);
        let slope = rng.range(3.0, 7.0);
        let lv = self.ladder(l_eng, n, slope, 3.0);
        let mut fam = self.family("engine", f_eng, &lv, 0.01, 2.0, span);
        if self.rng.chance(0.3) {
            // load change / gear shift near the pass
            let ts = t_ref + self.rng.range(-15.0, 15.0);
            let k = 1.0 + self.rng.sign() * self.rng.range(0.08, 0.3);
            let base = fam.table.clone();
            fam.table = PhaseTable::new(span.0, span.1, 0.05, |t| {
                let s = ((t - ts) / 1.5).clamp(0.0, 1.0);
                base.freq(t) * (1.0 + (k - 1.0) * s)
            });
        }
        let r_cut = self.audible_range(l_eng, f_eng, 0.0).clamp(2.0 * d, 3000.0);
        let ramp = 5.0;
        let Some((t0, t1)) = self.heard_window(&traj, r_cut, ramp) else { return };
        let c = self.scene.c;
        let f_star = fam.table.freq(t_ref);
        let mut bands: Vec<(f64, f64)> =
            (1..=n).map(|k| (0.7 * f_eng * k as f64 * c / (c + v), 1.35 * f_eng * k as f64 * c / (c - v))).collect();
        bands.push((300.0, 2000.0));
        self.sources.push(SourceTruth {
            id: self.next_id(),
            category: Category::Vehicle,
            kind: kind.into(),
            t_start: t0.max(0.0),
            t_end: t1.min(self.dur),
            bands,
            flyby: Some(Flyby { f0: f_star, v, d, tc }),
            note: format!("engine {f_eng:.1} Hz, road offset {road_offset:.0} m"),
        });
        let bb = Some(BroadbandSpec { level_db: l_tyre, f_peak: self.rng.range(700.0, 1000.0), rng: self.rng.fork() });
        let grazing_db = self.rng.range(0.0, 6.0);
        let src = MovingSource { traj, tones: vec![fam], broadband: None, excess_db: 0.0, grazing_db, t_start: t0, t_end: t1, ramp };
        self.jobs.push(Job::Moving { src, bb, aircraft: false });
    }

    fn push(&mut self, category: Category, kind: &str, t: (f64, f64), bands: Vec<(f64, f64)>, note: String, job: Job) {
        self.sources.push(SourceTruth {
            id: self.next_id(),
            category,
            kind: kind.into(),
            t_start: t.0.max(0.0),
            t_end: t.1.min(self.dur),
            bands,
            flyby: None,
            note,
        });
        self.jobs.push(job);
    }

    /// Poisson-distributed count of events at `per_hour`.
    fn count(&mut self, per_hour: f64) -> usize {
        let lambda = per_hour * self.dur / 3600.0;
        let (l, mut k, mut p) = ((-lambda).exp(), 0, 1.0);
        loop {
            p *= self.rng.f64();
            if p <= l {
                return k;
            }
            k += 1;
        }
    }

    fn interference(&mut self) {
        let dur = self.dur;
        let nyq = 0.45 * self.scene.fs;
        if self.rng.chance(self.p.p_hum) {
            let snr = self.rng.range(self.p.hum_snr.0, self.p.hum_snr.1);
            let l1 = floor_db(self.floor, 50.0) + 1.8 + snr;
            let n = self.rng.int(3, 8);
            let levels: Vec<f64> = (1..=n)
                .map(|k| l1 - (if k % 2 == 1 { 3.0 } else { 9.0 }) * (k as f64).log2() + 3.0 * self.rng.gaussian())
                .collect();
            let bands = (1..=n).map(|k| (50.0 * k as f64 - 0.3, 50.0 * k as f64 + 0.3)).collect();
            let h = Hum { mains_hz: 50.0, levels_db: levels, drift_hz: 0.02 };
            let rng = self.rng.fork();
            self.push(Category::Hum, "mains_hum", (0.0, dur), bands, format!("{n} harmonics"), Job::Hum(h, rng));
        }
        for _ in 0..self.rng.int(self.p.n_machines.0, self.p.n_machines.1) {
            let rng = &mut self.rng;
            let f = rng.log_range(25.0, 400.0);
            let on = rng.range(-60.0, dur * 0.8);
            let off = (on + rng.range(60.0, 900.0)).min(dur + 30.0);
            let coast = if rng.chance(0.5) { rng.range(3.0, 20.0) } else { 0.0 };
            let spin_up = rng.range(1.0, 6.0);
            let steps = (0..rng.int(0, 3)).map(|_| (rng.range(on, off), rng.range(0.95, 1.05))).collect();
            let n = rng.int(1, 3);
            let snr = rng.range(5.0, 25.0);
            let l1 = floor_db(self.floor, f) + 1.8 + snr;
            let levels: Vec<f64> = (1..=n).map(|k| l1 - 6.0 * (k as f64).log2() + 3.0 * self.rng.gaussian()).collect();
            let bands = (1..=n).map(|k| (0.13 * f * k as f64, 1.06 * f * k as f64)).collect();
            let m = Machine { f_hz: f, levels_db: levels, on, off, spin_up, coast, steps };
            let t = (on, m.end());
            let note = format!("{f:.1} Hz, coast {coast:.0} s");
            let rng = self.rng.fork();
            self.push(Category::Machinery, "machine", t, bands, note, Job::Machine(m, rng));
        }
        for _ in 0..self.count(self.p.gusts_per_hour) {
            let g = Gust {
                t0: self.rng.range(-5.0, dur),
                dur: self.rng.range(2.0, 15.0),
                level_db: self.rng.range(self.p.gust_db.0, self.p.gust_db.1),
                corner: self.rng.range(15.0, 60.0),
            };
            let t = (g.t0, g.t0 + g.dur);
            let rng = self.rng.fork();
            self.push(Category::Other, "wind", t, vec![(0.0, 400.0)], String::new(), Job::Gust(g, rng));
        }
        for _ in 0..self.count(self.p.talks_per_hour) {
            let rng = &mut self.rng;
            let t0 = rng.range(0.0, dur - 20.0);
            let len = rng.range(15.0, 150.0);
            let n = rng.int(1, 3);
            let pitches: Vec<f64> =
                (0..n).map(|_| if rng.chance(0.5) { rng.range(90.0, 140.0) } else { rng.range(160.0, 250.0) }).collect();
            let levels = (0..n).map(|_| rng.range(42.0, 65.0)).collect();
            let lo = pitches.iter().cloned().fold(f64::INFINITY, f64::min) * 0.7;
            let c = Conversation { t0, t1: t0 + len, pitches, levels_db: levels };
            let note = format!("{n} speakers");
            let rng = self.rng.fork();
            self.push(Category::Speech, "speech", (t0, t0 + len + 5.0), vec![(lo, nyq)], note, Job::Talk(c, rng));
        }
        for _ in 0..self.count(self.p.bird_bouts_per_hour) {
            let b = Birds {
                t0: self.rng.range(0.0, dur),
                dur: self.rng.range(3.0, 40.0),
                dove: self.rng.chance(0.25),
                level_db: self.rng.range(35.0, 58.0),
            };
            let bands = vec![if b.dove { (380.0, 2000.0) } else { (800.0, nyq) }];
            let kind = if b.dove { "dove" } else { "songbird" };
            let t = (b.t0, b.t0 + b.dur + 15.0);
            let rng = self.rng.fork();
            self.push(Category::Other, kind, t, bands, String::new(), Job::Birds(b, rng));
        }
        if self.rng.chance(self.p.p_rain) {
            let r = Rain {
                t0: self.rng.range(0.0, dur * 0.8),
                dur: self.rng.range(180.0, 600.0),
                level_db: self.rng.range(5.0, 20.0),
                drops_per_s: self.rng.range(20.0, 150.0),
            };
            let t = (r.t0, r.t0 + r.dur);
            let rng = self.rng.fork();
            self.push(Category::Other, "rain", t, vec![(0.0, nyq)], String::new(), Job::Rain(r, rng));
        }
        for _ in 0..self.count(self.p.dogs_per_hour) {
            let d = Dog {
                t0: self.rng.range(0.0, dur),
                barks: self.rng.int(2, 8),
                f0: self.rng.range(300.0, 700.0),
                level_db: self.rng.range(50.0, 72.0),
            };
            let t = (d.t0, d.t0 + d.barks as f64 * 1.3 + 1.0);
            let bands = vec![(d.f0 * 0.85, nyq)];
            let rng = self.rng.fork();
            self.push(Category::Other, "dog", t, bands, String::new(), Job::Dog(d, rng));
        }
        for _ in 0..self.count(self.p.clutter_per_hour) {
            let rng = &mut self.rng;
            let f = rng.log_range(30.0, 800.0);
            let dur_f = rng.log_range(1.0, 25.0);
            let slope = if rng.chance(0.6) { 0.0 } else { rng.sign() * rng.log_range(0.05, 1.5) };
            let t0 = rng.range(-5.0, dur);
            let n = rng.int(1, 3);
            let snr = rng.range(0.0, 15.0);
            let l1 = floor_db(self.floor, f) + 1.8 + snr;
            let levels = (1..=n).map(|k| l1 - 6.0 * (k as f64).log2()).collect();
            let f_end = f + slope * dur_f;
            let bands = (1..=n).map(|k| (f.min(f_end) * k as f64 - 1.0, f.max(f_end) * k as f64 + 1.0)).collect();
            let c = Fragment { t0, dur: dur_f, f, slope, levels_db: levels };
            let rng = self.rng.fork();
            self.push(Category::Other, "clutter", (t0, t0 + dur_f), bands, format!("{f:.1} Hz"), Job::Fragment(c, rng));
        }
    }
}

fn render_job(job: Job, fs: f64, n: usize, scene: &Scene) -> Part {
    match job {
        Job::Moving { mut src, bb, .. } => {
            if let Some(mut spec) = bb {
                // broadband emission over the emission-time span of the window
                let r0 = propagate::solve(&src.traj, scene.mic, scene.c, src.t_start, None);
                let t0 = r0.tau - 1.0;
                let t1 = src.t_end + 1.0;
                let len = ((t1 - t0) * fs) as usize;
                let total = pa(spec.level_db).powi(2);
                let p = total / (spec.f_peak * std::f64::consts::FRAC_PI_4);
                let fp = spec.f_peak;
                let x = shaped_noise(len, fs, |f| {
                    let x = f / fp;
                    p * x * x / (1.0 + x * x).powi(2)
                }, &mut spec.rng);
                src.broadband = Some(Broadband { t0, fs, x });
            }
            let (i0, x) = source::render(&src, scene);
            Part { i0, x }
        }
        Job::Floor(f, mut r) => ambient::floor(n, fs, &f, &mut r),
        Job::Hum(h, mut r) => ambient::hum(n, fs, &h, &mut r),
        Job::Machine(m, mut r) => ambient::machine(fs, &m, &mut r),
        Job::Gust(g, mut r) => ambient::gust(fs, &g, &mut r),
        Job::Talk(c, mut r) => ambient::conversation(fs, &c, &mut r),
        Job::Birds(b, mut r) => ambient::birds(fs, &b, &mut r),
        Job::Rain(x, mut r) => ambient::rain(fs, &x, &mut r),
        Job::Dog(d, mut r) => ambient::dog(fs, &d, &mut r),
        Job::Fragment(c, mut r) => ambient::fragment(fs, &c, &mut r),
    }
}

pub struct Generated {
    /// Final digital samples in [-1, 1].
    pub samples: Vec<f32>,
    pub truth: Truth,
}

/// Plan, render and post-process one scenario.
pub fn generate(cfg: &ScenarioConfig, name: &str) -> Result<Generated> {
    let started = std::time::Instant::now();
    let mut rng = Rng::new(cfg.seed);
    let p = params(cfg.difficulty);
    let dur = cfg.duration.unwrap_or_else(|| rng.range(p.dur.0, p.dur.1).round());
    let fs = cfg.sample_rate as f64;
    let n = (dur * fs) as usize;
    let ground = rng.range(50.0, 600.0);
    let mic_height = rng.range(0.5, 2.0);
    let reflection = rng.range(0.4, 0.9);
    let scene = Scene { fs, c: DEFAULT_C, mic: [0.0, 0.0, ground + mic_height], ground_z: ground, reflection };
    let floor = Floor { pink_db_100: rng.range(p.pink.0, p.pink.1), rumble_db: rng.range(p.rumble.0, p.rumble.1) };
    let full_scale_db = rng.range(p.full_scale.0, p.full_scale.1);
    let agc = rng.chance(p.p_agc).then(|| Agc {
        target_dbfs: rng.range(-30.0, -20.0),
        max_gain_db: rng.range(15.0, 30.0),
        min_gain_db: -20.0,
        attack: 0.05,
        release: rng.range(1.0, 4.0),
    });
    let codec_draw = [Codec::None, Codec::Mp3, Codec::Opus, Codec::Phone][rng.weighted(&p.codecs)];
    let codec = cfg.codec.unwrap_or(codec_draw);
    let codec = if codec != Codec::None && !chain::ffmpeg_available() { Codec::None } else { codec };

    let mut pl = Planner {
        rng: rng.fork(),
        p,
        dur,
        scene,
        floor: &floor,
        jobs: Vec::new(),
        sources: Vec::new(),
        aircraft: Vec::new(),
    };
    let floor_rng = pl.rng.fork();
    pl.jobs.push(Job::Floor(floor.clone(), floor_rng));
    let n_ac = pl.rng.int(pl.p.n_aircraft.0, pl.p.n_aircraft.1);
    let mut tcs: Vec<f64> = Vec::new();
    for _ in 0..n_ac {
        let kind = [AircraftKind::Piston, AircraftKind::Turboprop, AircraftKind::Helicopter, AircraftKind::Jet]
            [pl.rng.weighted(&pl.p.kinds)];
        // hard scenes sometimes overlap two passes within a minute
        let hint = (cfg.difficulty == Difficulty::Hard && !tcs.is_empty() && pl.rng.chance(0.3))
            .then(|| (tcs[0] + pl.rng.range(-60.0, 60.0)).clamp(30.0, dur - 30.0));
        pl.aircraft(kind, hint);
        tcs.extend(pl.aircraft.last().map(|a| a.flyby.tc));
    }
    let n_veh = pl.rng.int(pl.p.n_vehicles.0, pl.p.n_vehicles.1);
    let road = (pl.rng.range(0.0, std::f64::consts::PI), pl.rng.log_range(5.0, 150.0));
    for _ in 0..n_veh {
        pl.vehicle(road.0, road.1);
    }
    pl.interference();
    let Planner { jobs, sources, mut aircraft, .. } = pl;

    // render jobs in parallel; each has its own RNG so the result is schedule-independent
    let is_aircraft: Vec<bool> = jobs.iter().map(|j| matches!(j, Job::Moving { aircraft: true, .. })).collect();
    let n_jobs = jobs.len();
    let queue = Mutex::new(jobs.into_iter().enumerate().collect::<Vec<_>>());
    let parts: Mutex<Vec<Option<Part>>> = Mutex::new((0..n_jobs).map(|_| None).collect());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(n_jobs.max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let Some((i, job)) = queue.lock().unwrap().pop() else { break };
                    let part = render_job(job, fs, n, &scene);
                    parts.lock().unwrap()[i] = Some(part);
                }
            });
        }
    });
    let mut bg = vec![0.0f32; n];
    let mut ac = vec![0.0f32; n];
    for (i, part) in parts.into_inner().unwrap().into_iter().enumerate() {
        let part = part.expect("rendered");
        let dst = if is_aircraft[i] { &mut ac } else { &mut bg };
        for (j, v) in part.x.iter().enumerate() {
            if let Some(o) = dst.get_mut(part.i0 + j) {
                *o += v;
            }
        }
    }

    // detectability from the measured background around each CPA
    let det = DetectConfig::default();
    let n_fft = ((fs * 1.024) as usize).next_power_of_two();
    let t_win = n_fft as f64 / fs;
    let enbw = 1.5 / t_win;
    for a in aircraft.iter_mut() {
        let fb = a.flyby;
        let i0 = ((fb.tc - 4.0) * fs).max(0.0) as usize;
        let i1 = (((fb.tc + 4.0) * fs) as usize).min(n);
        let psd = if i1 > i0 + n_fft { welch_psd(&bg[i0..i1], fs, n_fft) } else { Vec::new() };
        let noise_at = |f: f64| -> f64 {
            let df = fs / n_fft as f64;
            let (lo, hi) = (((f - 5.0) / df).floor().max(0.0) as usize, ((f + 5.0) / df).ceil() as usize);
            let mut v: Vec<f64> = psd.get(lo..=hi.min(psd.len().saturating_sub(1))).map_or(Vec::new(), |s| s.to_vec());
            if v.is_empty() {
                return pa2_per_hz(floor_db(&floor, f));
            }
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let spread = (1.0 + reflection * reflection).sqrt();
        let graze = source::grazing_loss(a.grazing_db, a.elevation_deg.to_radians().sin(), fb.d);
        let mut lines = Vec::new();
        for (name, f1, lv) in &a.families {
            for (k, l) in lv.iter().enumerate() {
                let f = f1 * (k + 1) as f64;
                if f < det.f_min || f > det.f_max.min(0.45 * fs) {
                    continue;
                }
                let amp = pa(*l) * 10f64.powf(-(absorption_db_per_m(f) * fb.d + a.excess_db + graze) / 20.0) / fb.d * spread;
                let slope = f * fb.v * fb.v / (DEFAULT_C * fb.d);
                let noise = noise_at(f);
                let snr = to_db(amp * amp / (noise * enbw.max(slope * t_win)));
                lines.push(LineTruth {
                    family: name.clone(),
                    harmonic: k + 1,
                    f_rest: f,
                    f_heard: (f * DEFAULT_C / (DEFAULT_C + fb.v), f * DEFAULT_C / (DEFAULT_C - fb.v)),
                    level_db: to_db(amp * amp / pa2_per_hz(0.0)),
                    noise_db_hz: to_db(noise / pa2_per_hz(0.0)),
                    snr_db: snr,
                });
            }
        }
        a.snr_db = lines.iter().map(|l| l.snr_db).fold(-99.0, f64::max);
        a.lines = lines;
        a.expected_detectable =
            a.snr_db >= SNR_THRESHOLD_DB && fb.v >= det.v_min && fb.tc >= 30.0 && fb.tc <= dur - 30.0;
    }

    // microphone → digital: calibration, AGC, clipping, codec
    let fs_amp = (pa(full_scale_db) * std::f64::consts::SQRT_2) as f32;
    let mut x: Vec<f32> = bg.iter().zip(&ac).map(|(b, a)| (b + a) / fs_amp).collect();
    drop(bg);
    drop(ac);
    if let Some(a) = &agc {
        chain::agc(&mut x, fs, a);
    }
    let clipped = chain::clip(&mut x, 1.0);
    let x = chain::codec(&x, cfg.sample_rate, codec)?;

    let truth = Truth {
        version: GENERATOR_VERSION,
        name: name.to_string(),
        seed: cfg.seed,
        difficulty: cfg.difficulty,
        duration: dur,
        sample_rate: cfg.sample_rate,
        c: DEFAULT_C,
        ground_elevation: ground,
        mic_height,
        reflection,
        full_scale_db,
        agc,
        clipped_samples: clipped,
        codec,
        snr_threshold_db: SNR_THRESHOLD_DB,
        aircraft,
        sources,
        synth_seconds: started.elapsed().as_secs_f64(),
    };
    Ok(Generated { samples: x, truth })
}

/// Write `<dir>/<name>.wav` and `<dir>/<name>.truth.json`; returns the WAV path.
pub fn write(dir: &std::path::Path, g: &Generated) -> Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let wav = dir.join(format!("{}.wav", g.truth.name));
    crate::dsp::write_wav(&wav, &g.samples, g.truth.sample_rate)?;
    std::fs::write(dir.join(format!("{}.truth.json", g.truth.name)), serde_json::to_string_pretty(&g.truth)?)?;
    Ok(wav)
}
