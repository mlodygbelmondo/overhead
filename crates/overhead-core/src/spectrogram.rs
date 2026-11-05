//! On-demand STFT spectrogram of an arbitrary time/frequency window.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};

use crate::audio::Audio;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpecRequest {
    pub t_start: f64,
    pub t_end: f64,
    pub f_min: f64,
    pub f_max: f64,
    /// Number of output time columns.
    pub columns: usize,
    /// FFT / Hann window length in samples.
    pub n_fft: usize,
    /// Subtract per-row and per-column medians (dB), which removes stationary
    /// hum lines and broadband bursts so faint moving tones stand out.
    pub whiten: bool,
}

/// Spectrogram in dB. Column `i` is centred at `t_start + (i + 0.5) * dt`,
/// row `j` is frequency `f_min + j * df`.
#[derive(Clone, Debug)]
pub struct Spectrogram {
    pub t_start: f64,
    pub dt: f64,
    pub f_min: f64,
    pub df: f64,
    pub cols: usize,
    pub rows: usize,
    /// Column-major: `db[col * rows + row]`.
    pub db: Vec<f32>,
    pub whitened: bool,
    pub n_fft: usize,
}

impl Spectrogram {
    pub fn t_end(&self) -> f64 {
        self.t_start + self.cols as f64 * self.dt
    }

    pub fn f_max(&self) -> f64 {
        self.f_min + (self.rows.saturating_sub(1)) as f64 * self.df
    }

    pub fn col_time(&self, col: usize) -> f64 {
        self.t_start + (col as f64 + 0.5) * self.dt
    }

    pub fn column(&self, col: usize) -> &[f32] {
        &self.db[col * self.rows..(col + 1) * self.rows]
    }

    /// Value at column `col`, frequency `f` (linear interpolation between bins).
    pub fn sample(&self, col: usize, f: f64) -> Option<f32> {
        let pos = (f - self.f_min) / self.df;
        if !(0.0..=(self.rows - 1) as f64).contains(&pos) {
            return None;
        }
        let i = pos as usize; // pos >= 0: same as floor (and much cheaper without SSE4.1)
        let frac = (pos - i as f64) as f32;
        let column = self.column(col);
        let a = column[i];
        let b = column[(i + 1).min(self.rows - 1)];
        Some(a + (b - a) * frac)
    }

    /// Low/high dB values at the given quantiles, for display contrast.
    pub fn quantiles(&self, lo: f64, hi: f64) -> (f32, f32) {
        if self.db.is_empty() {
            return (0.0, 1.0);
        }
        let stride = (self.db.len() / 200_000).max(1);
        let mut v: Vec<f32> = self.db.iter().step_by(stride).copied().collect();
        v.sort_by(f32::total_cmp);
        let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
        (q(lo), q(hi))
    }
}

pub fn compute(audio: &Audio, req: &SpecRequest) -> Spectrogram {
    let sr = audio.sample_rate;
    let n_fft = req.n_fft.max(16);
    let df = sr / n_fft as f64;
    let nyquist_bin = n_fft / 2;
    let bin_lo = ((req.f_min / df).floor().max(0.0) as usize).min(nyquist_bin);
    let bin_hi = ((req.f_max / df).ceil().max(0.0) as usize).clamp(bin_lo, nyquist_bin);
    let rows = bin_hi - bin_lo + 1;
    let cols = req.columns.max(1);
    let t_start = req.t_start;
    let dt = ((req.t_end - req.t_start) / cols as f64).max(1.0 / sr);

    // When a column is wider than half a window, average several frames inside
    // it so short events are not skipped.
    let half_window = n_fft as f64 / 2.0 / sr;
    let frames_per_col = ((dt / half_window).floor() as usize).max(1);

    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n_fft);
    let window: Vec<f32> = (0..n_fft)
        .map(|i| {
            let x = std::f32::consts::PI * 2.0 * i as f32 / n_fft as f32;
            0.5 - 0.5 * x.cos()
        })
        .collect();
    let wsum: f32 = window.iter().sum();
    let norm = 1.0 / (wsum * wsum);

    let mut db = vec![0.0f32; cols * rows];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    let chunk_cols = cols.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (chunk_idx, chunk) in db.chunks_mut(chunk_cols * rows).enumerate() {
            let fft: Arc<dyn RealToComplex<f32>> = fft.clone();
            let window = &window;
            let samples = &audio.samples;
            scope.spawn(move || {
                let mut input = fft.make_input_vec();
                let mut spectrum = fft.make_output_vec();
                let mut scratch = fft.make_scratch_vec();
                let mut power = vec![0.0f32; rows];
                for (k, out) in chunk.chunks_mut(rows).enumerate() {
                    let col = chunk_idx * chunk_cols + k;
                    let col_start = t_start + col as f64 * dt;
                    power.iter_mut().for_each(|p| *p = 0.0);
                    for frame in 0..frames_per_col {
                        let centre = col_start + (frame as f64 + 0.5) * dt / frames_per_col as f64;
                        let first = (centre * sr).round() as i64 - n_fft as i64 / 2;
                        for (i, x) in input.iter_mut().enumerate() {
                            let idx = first + i as i64;
                            let s = if idx >= 0 && (idx as usize) < samples.len() {
                                samples[idx as usize]
                            } else {
                                0.0
                            };
                            *x = s * window[i];
                        }
                        fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch)
                            .expect("fft buffer sizes");
                        for (p, c) in power.iter_mut().zip(&spectrum[bin_lo..=bin_hi]) {
                            *p += c.norm_sqr();
                        }
                    }
                    let scale = norm / frames_per_col as f32;
                    for (o, p) in out.iter_mut().zip(&power) {
                        *o = 10.0 * (p * scale + 1e-20).log10();
                    }
                }
            });
        }
    });

    let mut spec = Spectrogram {
        t_start,
        dt,
        f_min: bin_lo as f64 * df,
        df,
        cols,
        rows,
        db,
        whitened: false,
        n_fft,
    };
    if req.whiten {
        whiten(&mut spec);
    }
    spec
}

/// Subtract per-row then per-column medians (in dB).
pub fn whiten(spec: &mut Spectrogram) {
    let (rows, cols) = (spec.rows, spec.cols);
    let mut buf = Vec::with_capacity(cols.max(rows));
    if cols >= 8 {
        for r in 0..rows {
            buf.clear();
            buf.extend((0..cols).map(|c| spec.db[c * rows + r]));
            let m = median(&mut buf);
            for c in 0..cols {
                spec.db[c * rows + r] -= m;
            }
        }
    }
    if rows >= 8 {
        for c in 0..cols {
            let column = &mut spec.db[c * rows..(c + 1) * rows];
            buf.clear();
            buf.extend_from_slice(column);
            let m = median(&mut buf);
            column.iter_mut().for_each(|v| *v -= m);
        }
    }
    spec.whitened = true;
}

pub(crate) fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mid = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable_by(mid, f32::total_cmp);
    *m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_peaks_in_right_bin() {
        let sr = 8000.0;
        let samples: Vec<f32> = (0..(sr as usize * 10))
            .map(|i| (2.0 * std::f64::consts::PI * 150.0 * i as f64 / sr).sin() as f32)
            .collect();
        let audio = Audio::from_samples(samples, sr);
        let spec = compute(
            &audio,
            &SpecRequest {
                t_start: 2.0,
                t_end: 8.0,
                f_min: 50.0,
                f_max: 400.0,
                columns: 20,
                n_fft: 8192,
                whiten: false,
            },
        );
        for col in 0..spec.cols {
            let column = spec.column(col);
            let (best, _) = column
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap();
            let f = spec.f_min + best as f64 * spec.df;
            assert!((f - 150.0).abs() <= spec.df, "col {col}: {f}");
        }
    }
}
