//! Signal-processing helpers: filters, coloured noise with an exact PSD, smooth
//! random processes, air absorption, Welch PSD and WAV output.
//!
//! Internally every signal is sound pressure in pascals; levels are given in
//! dB SPL (re 20 µPa) and PSDs in dB re (20 µPa)²/Hz.

use std::path::Path;

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

use crate::rng::Rng;

pub const P_REF: f64 = 20e-6;

/// RMS pressure (Pa) of a level in dB SPL.
pub fn pa(db_spl: f64) -> f64 {
    P_REF * 10f64.powf(db_spl / 20.0)
}

/// One-sided PSD (Pa²/Hz) of a spectral density level in dB re (20 µPa)²/Hz.
pub fn pa2_per_hz(db: f64) -> f64 {
    P_REF * P_REF * 10f64.powf(db / 10.0)
}

pub fn to_db(power_ratio: f64) -> f64 {
    10.0 * power_ratio.max(1e-300).log10()
}

/// Atmospheric absorption (dB/m) at 20 °C, 70 % relative humidity.
///
/// Log-log interpolation of ISO 9613-2 Table 2 (0.1 dB/km at 63 Hz … 76.6 dB/km
/// at 8 kHz); below 63 Hz the classical ∝ f² law. Low tones carry for many
/// kilometres while broadband hiss of distant sources is strongly low-passed.
pub fn absorption_db_per_m(f: f64) -> f64 {
    const F: [f64; 8] = [63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];
    const A: [f64; 8] = [0.1, 0.3, 1.1, 2.8, 5.0, 9.0, 22.9, 76.6];
    let f = f.abs().max(1.0);
    let db_km = if f <= F[0] {
        A[0] * (f / F[0]).powi(2)
    } else if f >= F[7] {
        A[7] * (f / F[7]).powi(2)
    } else {
        let i = F.iter().rposition(|x| *x <= f).unwrap();
        let s = (f / F[i]).ln() / (F[i + 1] / F[i]).ln();
        (A[i].ln() + s * (A[i + 1].ln() - A[i].ln())).exp()
    };
    db_km / 1000.0
}

/// Frequency where absorption over `range` metres reaches 3 dB; the cutoff of
/// the one-pole low-pass used to approximate absorption on broadband noise.
pub fn absorption_cutoff(range: f64) -> f64 {
    let (mut lo, mut hi) = (1.0f64, 40_000.0f64);
    for _ in 0..40 {
        let mid = (lo * hi).sqrt();
        if absorption_db_per_m(mid) * range > 3.0 { hi = mid } else { lo = mid }
    }
    lo
}

/// Transposed direct-form-II biquad (RBJ cookbook designs).
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn from(b: [f64; 3], a: [f64; 3]) -> Self {
        Self { b0: b[0] / a[0], b1: b[1] / a[0], b2: b[2] / a[0], a1: a[1] / a[0], a2: a[2] / a[0], z1: 0.0, z2: 0.0 }
    }

    fn wq(fs: f64, f: f64, q: f64) -> (f64, f64) {
        let w = std::f64::consts::TAU * (f / fs).clamp(1e-5, 0.49);
        (w.cos(), w.sin() / (2.0 * q))
    }

    pub fn lowpass(fs: f64, f: f64, q: f64) -> Self {
        let (c, al) = Self::wq(fs, f, q);
        Self::from([(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0], [1.0 + al, -2.0 * c, 1.0 - al])
    }

    pub fn highpass(fs: f64, f: f64, q: f64) -> Self {
        let (c, al) = Self::wq(fs, f, q);
        Self::from([(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0], [1.0 + al, -2.0 * c, 1.0 - al])
    }

    /// Band-pass with 0 dB peak gain.
    pub fn bandpass(fs: f64, f: f64, q: f64) -> Self {
        let (c, al) = Self::wq(fs, f, q);
        Self::from([al, 0.0, -al], [1.0 + al, -2.0 * c, 1.0 - al])
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// One-pole low-pass whose cutoff may change every sample.
#[derive(Clone, Copy, Debug, Default)]
pub struct OnePole {
    y: f64,
}

impl OnePole {
    pub fn coeff(fs: f64, fc: f64) -> f64 {
        1.0 - (-std::f64::consts::TAU * fc.min(fs * 0.45) / fs).exp()
    }

    #[inline]
    pub fn process(&mut self, x: f64, a: f64) -> f64 {
        self.y += a * (x - self.y);
        self.y
    }
}

/// Stationary Gaussian noise of `n` samples whose one-sided PSD is `psd(f)` Pa²/Hz.
///
/// Blocks with random complex-Gaussian spectra of the requested magnitude are
/// inverse-FFT'd, sine-windowed and overlap-added at 50 %; sin² + cos² = 1 keeps
/// the variance (∫ psd df) constant across block joins.
pub fn shaped_noise(n: usize, fs: f64, psd: impl Fn(f64) -> f64, rng: &mut Rng) -> Vec<f32> {
    const N: usize = 16384;
    let hop = N / 2;
    let mut planner = RealFftPlanner::<f64>::new();
    let ifft = planner.plan_fft_inverse(N);
    let df = fs / N as f64;
    let amp: Vec<f64> = (0..=N / 2)
        .map(|k| if k == 0 || k == N / 2 { 0.0 } else { (psd(k as f64 * df) * df).max(0.0).sqrt() / 2.0 })
        .collect();
    let window: Vec<f64> = (0..N).map(|i| (std::f64::consts::PI * (i as f64 + 0.5) / N as f64).sin()).collect();
    let mut out = vec![0.0f32; n + N];
    let mut spec = ifft.make_input_vec();
    let mut buf = ifft.make_output_vec();
    let mut start = 0;
    while start < n + hop {
        for (k, s) in spec.iter_mut().enumerate() {
            let (g1, g2) = rng.gaussian2();
            *s = Complex::new(amp[k] * g1, amp[k] * g2);
        }
        ifft.process(&mut spec, &mut buf).expect("ifft");
        for i in 0..N {
            if let Some(o) = out.get_mut(start + i) {
                *o += (buf[i] * window[i]) as f32;
            }
        }
        start += hop;
    }
    // the first half-block only has one window; skip it
    out.drain(..hop);
    out.truncate(n);
    out
}

/// Smooth unit-variance random process: Gaussian knots every `h` seconds,
/// Catmull–Rom interpolated. Used for slow jitter, drift and fading.
#[derive(Clone, Debug)]
pub struct Smooth {
    t0: f64,
    h: f64,
    v: Vec<f64>,
}

impl Smooth {
    pub fn new(rng: &mut Rng, t0: f64, t1: f64, h: f64) -> Self {
        let n = ((t1 - t0) / h).ceil().max(0.0) as usize + 4;
        // Catmull-Rom of iid knots has ~0.75 of the knot variance between knots
        let v = (0..n).map(|_| rng.gaussian() * 1.15).collect();
        Self { t0: t0 - h, h, v }
    }

    pub fn at(&self, t: f64) -> f64 {
        let x = ((t - self.t0) / self.h).clamp(1.0, (self.v.len() - 3) as f64);
        let i = x.floor() as usize;
        let u = x - i as f64;
        let (p0, p1, p2, p3) = (self.v[i - 1], self.v[i], self.v[i + 1], self.v[i + 2]);
        0.5 * (2.0 * p1
            + (p2 - p0) * u
            + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * u * u
            + (3.0 * p1 - p0 - 3.0 * p2 + p3) * u * u * u)
    }
}

/// Raised-cosine fade weight for `t` inside `[t0, t1]` with ramps of `ramp` s.
pub fn fade(t: f64, t0: f64, t1: f64, ramp: f64) -> f64 {
    let e = ((t - t0) / ramp).min((t1 - t) / ramp);
    if e <= 0.0 {
        0.0
    } else if e >= 1.0 {
        1.0
    } else {
        0.5 - 0.5 * (std::f64::consts::PI * e).cos()
    }
}

/// One-sided Welch PSD (units²/Hz) of `x` with Hann windows of `n_fft`, hop n/2.
/// Bin `k` is frequency `k · fs / n_fft`.
pub fn welch_psd(x: &[f32], fs: f64, n_fft: usize) -> Vec<f64> {
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n_fft);
    let w: Vec<f64> = (0..n_fft)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n_fft as f64).cos())
        .collect();
    let w2: f64 = w.iter().map(|v| v * v).sum();
    let mut acc = vec![0.0; n_fft / 2 + 1];
    let mut buf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut count = 0;
    let mut start = 0;
    while start + n_fft <= x.len() {
        for i in 0..n_fft {
            buf[i] = x[start + i] as f64 * w[i];
        }
        fft.process(&mut buf, &mut spec).expect("fft");
        for (a, s) in acc.iter_mut().zip(&spec) {
            *a += s.norm_sqr();
        }
        count += 1;
        start += n_fft / 2;
    }
    let scale = 2.0 / (fs * w2 * count.max(1) as f64);
    acc.iter_mut().for_each(|a| *a *= scale);
    acc
}

/// Write 16-bit PCM mono WAV; samples are clamped to [-1, 1].
pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, bytes)
}
