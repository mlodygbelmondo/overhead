//! Interference recorded alongside the fly-bys. Each generator renders pressure
//! (Pa) at the microphone into [`Part`]s placed on the recording timeline.
//!
//! These are the things an automatic detector must not mistake for aircraft:
//! stationary lines (mains hum, machinery; machinery also spins up and coasts
//! down, giving rising/falling tones), broadband bursts (wind, rain), and
//! harmonic sounds with gliding pitch (speech, dogs, birds).

use serde::{Deserialize, Serialize};

use crate::dsp::{Biquad, Smooth, fade, pa, pa2_per_hz, shaped_noise};
use crate::rng::Rng;

const TAU: f64 = std::f64::consts::TAU;

/// A rendered snippet starting at sample `i0`.
pub struct Part {
    pub i0: usize,
    pub x: Vec<f32>,
}

impl Part {
    fn new(t0: f64, t1: f64, fs: f64) -> Self {
        let i0 = (t0.max(0.0) * fs).floor() as usize;
        let i1 = (t1.max(0.0) * fs).ceil() as usize;
        Self { i0, x: vec![0.0; i1.saturating_sub(i0)] }
    }

    fn t(&self, j: usize, fs: f64) -> f64 {
        (self.i0 + j) as f64 / fs
    }
}

/// Sum of harmonics `Σ a_k sin(k·2πφ)` with `z^k` rotation (one sincos per sample).
#[inline]
fn harmonics(cycles: f64, amps: &[f64]) -> f64 {
    let (s, c) = (TAU * cycles.fract()).sin_cos();
    let (mut zr, mut zi) = (1.0, 0.0);
    let mut acc = 0.0;
    for a in amps {
        (zr, zi) = (zr * c - zi * s, zr * s + zi * c);
        acc += a * zi;
    }
    acc
}

/// Ambient floor: pink-ish rural background plus low-frequency rumble of distant
/// traffic, with a slow ±2 dB level wander. Levels are PSD in dB re (20 µPa)²/Hz.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Floor {
    /// PSD at 100 Hz of the pink component.
    pub pink_db_100: f64,
    /// PSD below ~40 Hz of the rumble component.
    pub rumble_db: f64,
}

pub fn floor(n: usize, fs: f64, p: &Floor, rng: &mut Rng) -> Part {
    let (pink, rumble) = (pa2_per_hz(p.pink_db_100), pa2_per_hz(p.rumble_db));
    let psd = |f: f64| {
        let f = f.max(5.0);
        pink * (100.0 / f) / (1.0 + (f / 3500.0).powi(6)) + rumble / (1.0 + (f / 40.0).powi(2)).powi(2)
    };
    let mut x = shaped_noise(n, fs, psd, rng);
    let wander = Smooth::new(rng, 0.0, n as f64 / fs, 90.0);
    for (b, chunk) in x.chunks_mut(256).enumerate() {
        let g = 10f64.powf(wander.at((b * 256) as f64 / fs) * 2.0 / 20.0) as f32;
        chunk.iter_mut().for_each(|v| *v *= g);
    }
    Part { i0: 0, x }
}

/// Mains hum (electrical pickup): harmonics of the grid frequency, which drifts
/// by a few tens of mHz, with slowly wandering harmonic levels.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hum {
    pub mains_hz: f64,
    /// RMS level (Pa-equivalent, dB SPL) of each harmonic.
    pub levels_db: Vec<f64>,
    pub drift_hz: f64,
}

pub fn hum(n: usize, fs: f64, h: &Hum, rng: &mut Rng) -> Part {
    let dur = n as f64 / fs;
    let drift = Smooth::new(rng, 0.0, dur, 40.0);
    let wander: Vec<Smooth> = h.levels_db.iter().map(|_| Smooth::new(rng, 0.0, dur, 150.0)).collect();
    let base: Vec<f64> = h.levels_db.iter().map(|l| pa(*l) * std::f64::consts::SQRT_2).collect();
    let mut amps = base.clone();
    let mut x = vec![0.0f32; n];
    let mut cyc = rng.f64();
    for (i, v) in x.iter_mut().enumerate() {
        let t = i as f64 / fs;
        if i % 512 == 0 {
            for k in 0..amps.len() {
                amps[k] = base[k] * 10f64.powf(1.5 * wander[k].at(t) / 20.0);
            }
        }
        cyc += (h.mains_hz + h.drift_hz * drift.at(t)) / fs;
        *v = harmonics(cyc, &amps) as f32;
    }
    Part { i0: 0, x }
}

/// Stationary machine (pump, fan, generator) at fixed distance: harmonic tones
/// with small wander, load steps, spin-up on start and exponential coast-down
/// after switching off.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Machine {
    pub f_hz: f64,
    /// RMS level at the microphone of each harmonic (dB SPL).
    pub levels_db: Vec<f64>,
    pub on: f64,
    pub off: f64,
    pub spin_up: f64,
    /// Coast-down time constant (s); 0 = stops instantly.
    pub coast: f64,
    /// Load steps `(time, frequency factor)`.
    pub steps: Vec<(f64, f64)>,
}

impl Machine {
    pub fn freq(&self, t: f64, wander: f64) -> f64 {
        let mut f = self.f_hz * (1.0 + 0.003 * wander);
        for &(ts, k) in &self.steps {
            let s = ((t - ts) / 0.8).clamp(0.0, 1.0);
            f *= 1.0 + (k - 1.0) * s * s * (3.0 - 2.0 * s);
        }
        if t < self.on + self.spin_up {
            let s = ((t - self.on) / self.spin_up).clamp(0.0, 1.0);
            f *= 0.25 + 0.75 * s.sqrt();
        }
        if t > self.off && self.coast > 0.0 {
            f *= (-(t - self.off) / self.coast).exp();
        }
        f
    }

    pub fn end(&self) -> f64 {
        self.off + 2.0 * self.coast
    }
}

pub fn machine(fs: f64, m: &Machine, rng: &mut Rng) -> Part {
    let mut part = Part::new(m.on, m.end(), fs);
    let wander = Smooth::new(rng, m.on, m.end() + 1.0, 6.0);
    let base: Vec<f64> = m.levels_db.iter().map(|l| pa(*l) * std::f64::consts::SQRT_2).collect();
    let mut amps = base.clone();
    let mut cyc = rng.f64();
    let nyq = 0.45 * fs;
    for j in 0..part.x.len() {
        let t = part.t(j, fs);
        let f = m.freq(t, wander.at(t));
        if j % 64 == 0 {
            // radiated level falls with rotation speed while coasting
            let level = if t > m.off { (f / m.f_hz).powi(2) } else { 1.0 };
            for (k, a) in amps.iter_mut().enumerate() {
                *a = if f * (k + 1) as f64 > nyq { 0.0 } else { base[k] * level };
            }
        }
        cyc += f / fs;
        let env = fade(t, m.on, m.end(), 0.3);
        part.x[j] = (env * harmonics(cyc, &amps)) as f32;
    }
    part
}

/// Wind gust on the microphone: very low-frequency broadband noise with a
/// fluttering envelope.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Gust {
    pub t0: f64,
    pub dur: f64,
    /// PSD level at 20 Hz (dB re (20 µPa)²/Hz) at the gust peak.
    pub level_db: f64,
    /// Corner frequency of the spectrum (Hz).
    pub corner: f64,
}

pub fn gust(fs: f64, g: &Gust, rng: &mut Rng) -> Part {
    let mut part = Part::new(g.t0, g.t0 + g.dur, fs);
    let p = pa2_per_hz(g.level_db) * (1.0 + (20.0 / g.corner).powi(2)).powf(1.5);
    let noise = shaped_noise(part.x.len(), fs, |f| p / (1.0 + (f.max(3.0) / g.corner).powi(2)).powf(1.5), rng);
    let flutter = Smooth::new(rng, g.t0, g.t0 + g.dur, 0.4);
    for (j, v) in noise.iter().enumerate() {
        let t = part.t(j, fs);
        let env = fade(t, g.t0, g.t0 + g.dur, g.dur * 0.35) * 10f64.powf(4.0 * flutter.at(t) / 20.0);
        part.x[j] = (*v as f64 * env) as f32;
    }
    part
}

/// A conversation: utterances of voiced syllables with gliding pitch, synthesised
/// as harmonic series shaped by vowel formant resonances.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    pub t0: f64,
    pub t1: f64,
    /// Base pitch (Hz) of each speaker.
    pub pitches: Vec<f64>,
    /// Level at the microphone (dB SPL, voiced speech RMS) of each speaker.
    pub levels_db: Vec<f64>,
}

const VOWELS: [[f64; 3]; 6] = [
    [730.0, 1090.0, 2440.0],
    [530.0, 1840.0, 2480.0],
    [270.0, 2290.0, 3010.0],
    [570.0, 840.0, 2410.0],
    [300.0, 870.0, 2240.0],
    [500.0, 1500.0, 2500.0],
];

fn formant_gain(f: f64, formants: &[f64; 3]) -> f64 {
    const BW: [f64; 3] = [80.0, 110.0, 160.0];
    let mut g = 1.0;
    for (fj, bj) in formants.iter().zip(BW) {
        let r = f / fj;
        g /= ((1.0 - r * r).powi(2) + (f * bj / (fj * fj)).powi(2)).sqrt();
    }
    g
}

pub fn conversation(fs: f64, c: &Conversation, rng: &mut Rng) -> Part {
    let mut part = Part::new(c.t0, c.t1 + 5.0, fs);
    let nyq = (0.45 * fs).min(3800.0);
    let block = (fs / 100.0) as usize;
    let mut t = c.t0;
    while t < c.t1 {
        let who = rng.int(0, c.pitches.len() - 1);
        let base = c.pitches[who];
        let level = pa(c.levels_db[who]) * std::f64::consts::SQRT_2;
        let female = base > 150.0;
        let utter_len = rng.range(0.8, 4.5);
        let mut s0 = t;
        let mut pitch = base * rng.range(1.0, 1.25);
        while s0 < t + utter_len {
            let syl = rng.range(0.12, 0.32);
            let p_end = (pitch * rng.range(0.78, 1.18)).clamp(base * 0.7, base * 1.6);
            let vowel = VOWELS[rng.int(0, VOWELS.len() - 1)];
            let formants = if female { vowel.map(|f| f * 1.15) } else { vowel };
            let fric = rng.chance(0.3);
            // syllable: optional unvoiced fricative, then voiced nucleus
            if fric {
                let fl = rng.range(0.04, 0.1);
                let fc = (3000.0f64).min(nyq * 0.9);
                let mut bp = Biquad::bandpass(fs, fc, 1.5);
                let a = level * 0.4;
                let (i_a, i_b) = (((s0 - part.t(0, fs)) * fs) as isize, ((s0 + fl - part.t(0, fs)) * fs) as isize);
                for i in i_a.max(0)..i_b.min(part.x.len() as isize) {
                    let tt = part.t(i as usize, fs);
                    part.x[i as usize] += (a * fade(tt, s0, s0 + fl, fl * 0.4) * bp.process(rng.white())) as f32;
                }
                s0 += fl;
            }
            let i_a = ((s0 - part.t(0, fs)) * fs).max(0.0) as usize;
            let i_b = (((s0 + syl - part.t(0, fs)) * fs) as usize).min(part.x.len());
            let mut cyc = rng.f64();
            let mut amps: Vec<f64> = Vec::new();
            for i in i_a..i_b {
                let u = (i - i_a) as f64 / (i_b - i_a).max(1) as f64;
                let f0 = pitch + (p_end - pitch) * u;
                if (i - i_a).is_multiple_of(block) {
                    let n_h = (nyq / f0).floor().max(1.0) as usize;
                    amps.clear();
                    let mut norm = 0.0;
                    for k in 1..=n_h {
                        let a = formant_gain(k as f64 * f0, &formants) / k as f64;
                        norm += a * a;
                        amps.push(a);
                    }
                    let s = level / norm.sqrt().max(1e-12);
                    amps.iter_mut().for_each(|a| *a *= s);
                }
                cyc += f0 / fs;
                let env = (std::f64::consts::PI * u).sin().powf(0.6);
                part.x[i] += (env * harmonics(cyc, &amps)) as f32;
            }
            s0 += syl + rng.range(0.0, 0.08);
            pitch = p_end * (1.0 - 0.03 * rng.f64());
        }
        t = s0 + rng.range(0.2, 2.5);
    }
    part
}

/// Bird activity: bouts of short frequency sweeps (songbirds, 1.5–3.8 kHz), or
/// low "coo" phrases (doves, ~0.5 kHz) which fall inside the aircraft band.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Birds {
    pub t0: f64,
    pub dur: f64,
    pub dove: bool,
    pub level_db: f64,
}

pub fn birds(fs: f64, b: &Birds, rng: &mut Rng) -> Part {
    let mut part = Part::new(b.t0, b.t0 + b.dur + 1.0, fs);
    let nyq = 0.45 * fs;
    let amp = pa(b.level_db) * std::f64::consts::SQRT_2;
    let mut t = b.t0;
    while t < b.t0 + b.dur {
        let (len, fa, fb, h2) = if b.dove {
            let f = rng.range(420.0, 650.0);
            (rng.range(0.35, 0.8), f * 1.04, f * 0.93, 0.25)
        } else {
            let f = rng.range(1500.0, 3800.0).min(nyq * 0.9);
            (rng.range(0.04, 0.2), f, (f + rng.sign() * rng.range(300.0, 1500.0)).clamp(800.0, nyq * 0.9), 0.15)
        };
        let i_a = ((t - part.t(0, fs)) * fs).max(0.0) as usize;
        let i_b = (i_a + (len * fs) as usize).min(part.x.len());
        let mut cyc = 0.0;
        let a = amp * rng.range(0.4, 1.0);
        for i in i_a..i_b {
            let u = (i - i_a) as f64 / (i_b - i_a).max(1) as f64;
            let f = fa + (fb - fa) * u;
            cyc += f / fs;
            let amps = [1.0, if 2.0 * f < nyq { h2 } else { 0.0 }, if b.dove { 0.1 } else { 0.0 }];
            part.x[i] += (a * (std::f64::consts::PI * u).sin().powi(2) * harmonics(cyc, &amps)) as f32;
        }
        t += len + if b.dove { rng.range(0.3, 1.5) } else { rng.range(0.05, 0.4) };
        if rng.chance(0.1) {
            t += rng.range(2.0, 15.0);
        }
    }
    part
}

/// Rain shower: broadband hiss plus individual drop impacts (short decaying tones).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rain {
    pub t0: f64,
    pub dur: f64,
    /// Hiss PSD at 1 kHz (dB re (20 µPa)²/Hz).
    pub level_db: f64,
    pub drops_per_s: f64,
}

pub fn rain(fs: f64, r: &Rain, rng: &mut Rng) -> Part {
    let mut part = Part::new(r.t0, r.t0 + r.dur, fs);
    let p = pa2_per_hz(r.level_db);
    let hiss = shaped_noise(part.x.len(), fs, |f| p * (f.max(20.0) / 1000.0).sqrt() / (1.0 + (f / 5000.0).powi(2)), rng);
    let ramp = (r.dur * 0.2).min(60.0);
    let n = part.x.len();
    for (j, v) in hiss.iter().enumerate() {
        part.x[j] = (*v as f64 * fade(part.t(j, fs), r.t0, r.t0 + r.dur, ramp)) as f32;
    }
    let drops = (r.drops_per_s * r.dur) as usize;
    let drop_amp = (p * 2000.0).sqrt() * 3.0;
    for _ in 0..drops {
        let j0 = rng.int(0, n.saturating_sub(1));
        let f = rng.range(1000.0, 4000.0).min(0.45 * fs);
        let decay = rng.range(0.002, 0.008);
        let a = drop_amp * (rng.gaussian() * 0.7).exp() * fade(part.t(j0, fs), r.t0, r.t0 + r.dur, ramp);
        let len = ((decay * 5.0) * fs) as usize;
        for k in 0..len.min(n - j0) {
            let tt = k as f64 / fs;
            part.x[j0 + k] += (a * (-tt / decay).exp() * (TAU * f * tt).sin()) as f32;
        }
    }
    part
}

/// A dog barking: a few harmonic barks with a rising-then-falling pitch and a noisy component.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dog {
    pub t0: f64,
    pub barks: usize,
    pub f0: f64,
    pub level_db: f64,
}

pub fn dog(fs: f64, d: &Dog, rng: &mut Rng) -> Part {
    let mut part = Part::new(d.t0, d.t0 + d.barks as f64 * 1.3 + 1.0, fs);
    let nyq = (0.45 * fs).min(3800.0);
    let amp = pa(d.level_db) * std::f64::consts::SQRT_2;
    let mut t = d.t0;
    for _ in 0..d.barks {
        let len = rng.range(0.12, 0.3);
        let f0 = d.f0 * rng.range(0.9, 1.1);
        let n_h = (nyq / (f0 * 1.3)).floor().max(1.0) as usize;
        let amps: Vec<f64> = (1..=n_h).map(|k| formant_gain(k as f64 * f0, &[900.0, 1800.0, 3000.0]).min(4.0) / k as f64).collect();
        let norm = amps.iter().map(|a| a * a).sum::<f64>().sqrt();
        let amps: Vec<f64> = amps.iter().map(|a| a / norm).collect();
        let mut bp = Biquad::bandpass(fs, 1200.0f64.min(nyq), 0.8);
        let i_a = ((t - part.t(0, fs)) * fs).max(0.0) as usize;
        let i_b = (i_a + (len * fs) as usize).min(part.x.len());
        let mut cyc = 0.0;
        for i in i_a..i_b {
            let u = (i - i_a) as f64 / (i_b - i_a).max(1) as f64;
            let f = f0 * (1.0 + 0.25 * (std::f64::consts::PI * u.powf(0.7)).sin());
            cyc += f / fs;
            let env = (u / 0.08).min(1.0) * (1.0 - u).powf(1.5);
            let v = 0.8 * harmonics(cyc, &amps) + 0.5 * bp.process(rng.white());
            part.x[i] += (amp * env * v) as f32;
        }
        t += len + rng.range(0.25, 1.0);
    }
    part
}

/// Short faint tonal fragment (distant engine, machine, horn): a few seconds of
/// a harmonic tone with a slow linear glide. Real recordings are full of them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fragment {
    pub t0: f64,
    pub dur: f64,
    pub f: f64,
    /// Frequency glide (Hz/s).
    pub slope: f64,
    pub levels_db: Vec<f64>,
}

pub fn fragment(fs: f64, c: &Fragment, rng: &mut Rng) -> Part {
    let mut part = Part::new(c.t0, c.t0 + c.dur, fs);
    let wobble = Smooth::new(rng, c.t0, c.t0 + c.dur, 1.0);
    let nyq = 0.45 * fs;
    let mut cyc = rng.f64();
    let base: Vec<f64> = c.levels_db.iter().map(|l| pa(*l) * std::f64::consts::SQRT_2).collect();
    let mut amps = base.clone();
    for j in 0..part.x.len() {
        let t = part.t(j, fs);
        let f = (c.f + c.slope * (t - c.t0)) * (1.0 + 0.003 * wobble.at(t));
        if j % 64 == 0 {
            for (k, a) in amps.iter_mut().enumerate() {
                *a = if f * (k + 1) as f64 > nyq { 0.0 } else { base[k] };
            }
        }
        cyc += f / fs;
        part.x[j] = (fade(t, c.t0, c.t0 + c.dur, c.dur * 0.25) * harmonics(cyc, &amps)) as f32;
    }
    part
}
