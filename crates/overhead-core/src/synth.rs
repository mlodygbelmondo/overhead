//! Synthetic fly-by recordings with known ground truth, for tests and demos.

use crate::audio::Audio;
use crate::model::{DEFAULT_C, Flyby};

#[derive(Clone, Debug)]
pub struct SynthFlyby {
    pub flyby: Flyby,
    /// Relative amplitude of harmonic k (index 0 = fundamental `f0`).
    pub harmonics: Vec<f64>,
    /// Amplitude of the fundamental at CPA; falls off as `d / R`.
    pub amplitude: f64,
}

/// Mix fly-bys, stationary hum tones `(freq, amplitude)` and white noise of
/// RMS `noise_rms` into a mono signal.
pub fn synthesize(
    sample_rate: f64,
    duration: f64,
    flybys: &[SynthFlyby],
    noise_rms: f64,
    hums: &[(f64, f64)],
    seed: u64,
) -> Audio {
    let n = (sample_rate * duration) as usize;
    let mut out = vec![0.0f32; n];
    let tau = std::f64::consts::TAU;
    for s in flybys {
        let mut phase = 0.0f64;
        for (i, o) in out.iter_mut().enumerate() {
            let t = i as f64 / sample_rate;
            let f = s.flyby.freq_at(t, DEFAULT_C);
            let gain = s.amplitude * s.flyby.d / s.flyby.emission_range(t, DEFAULT_C);
            phase = (phase + tau * f / sample_rate) % (tau * 1e6);
            let mut v = 0.0;
            for (k, a) in s.harmonics.iter().enumerate() {
                v += a * ((k + 1) as f64 * phase).sin();
            }
            *o += (gain * v) as f32;
        }
    }
    for &(f, a) in hums {
        for (i, o) in out.iter_mut().enumerate() {
            *o += (a * (tau * f * i as f64 / sample_rate).sin()) as f32;
        }
    }
    let mut rng = XorShift(seed.max(1));
    for o in out.iter_mut() {
        *o += (noise_rms * rng.gaussian()) as f32;
    }
    Audio::from_samples(out, sample_rate)
}

/// Write 16-bit PCM mono WAV.
pub fn write_wav(path: &std::path::Path, audio: &Audio) -> std::io::Result<()> {
    let peak = audio.samples.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-9);
    let scale = if peak > 1.0 { 1.0 / peak } else { 1.0 };
    let data_len = (audio.samples.len() * 2) as u32;
    let sr = audio.sample_rate.round() as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&sr.to_le_bytes());
    bytes.extend_from_slice(&(sr * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in &audio.samples {
        let v = (s * scale * 32767.0).clamp(-32768.0, 32767.0) as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, bytes)
}

struct XorShift(u64);

impl XorShift {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    fn gaussian(&mut self) -> f64 {
        let u1 = self.next_f64().max(1e-300);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}
