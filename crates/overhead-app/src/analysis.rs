//! Background work: decoding, spectrogram images, detection.

use std::path::Path;
use std::sync::Arc;

use overhead_core::audio::{self, Audio};
use overhead_core::detect::{self, DetectConfig, DetectStats, Detection};
use overhead_core::spectrogram::{self, SpecRequest};
use gpui::RenderImage;
use image::{Frame, ImageBuffer, Rgba};

/// A rendered spectrogram window and the time/frequency box it covers.
#[derive(Clone)]
pub struct SpecImage {
    pub image: Arc<RenderImage>,
    pub req: SpecRequest,
    /// Exact extent covered by the pixels.
    pub t0: f64,
    pub t1: f64,
    pub f0: f64,
    pub f1: f64,
}

/// Seconds of detection per second of audio on this machine (16 threads),
/// measured on a 1 h recording: ~96 s including the ffmpeg decode.
pub const DETECT_S_PER_AUDIO_S: f64 = 0.024;

/// Rough decode time (s) from the file size, for the progress bar only.
/// Measured here: 45.8 MB Opus/WebM through ffmpeg in 85 s; a 57.6 MB WAV
/// through symphonia in ~1 s.
pub fn decode_estimate_s(path: &Path, size_bytes: u64) -> f64 {
    let mb = size_bytes as f64 / 1e6;
    let native = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "wav" | "flac" | "mp3" | "ogg" | "oga" | "m4a" | "aac" | "alac" | "aif" | "aiff"));
    if native { (mb * 0.03).max(0.5) } else { (mb * 1.9).max(2.0) }
}

pub fn load(path: &Path) -> anyhow::Result<Audio> {
    audio::load(path, audio::DEFAULT_TARGET_RATE)
}

pub fn run_detection(audio: &Audio) -> (Vec<Detection>, DetectStats) {
    detect::detect_with_stats(audio, &DetectConfig::default())
}

/// Compute a spectrogram window and turn it into a GPU image (inferno map,
/// highest frequency on the top row).
pub fn render_spec(audio: &Audio, req: SpecRequest) -> SpecImage {
    let spec = spectrogram::compute(audio, &req);
    let (lo, hi) = if spec.whitened { spec.quantiles(0.55, 0.9992) } else { spec.quantiles(0.05, 0.999) };
    let span = (hi - lo).max(1e-3);
    let lut = overhead_core::colormap::lut();
    let (w, h) = (spec.cols as u32, spec.rows as u32);
    let mut buf: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(w, h);
    for col in 0..spec.cols {
        let column = spec.column(col);
        for row in 0..spec.rows {
            let x = ((column[row] - lo) / span).clamp(0.0, 1.0);
            // gentle gamma so faint lines show without washing out the floor
            let x = x.powf(0.9);
            let [r, g, b] = lut[(x * 255.0) as usize];
            // GPUI sprites are BGRA.
            buf.put_pixel(col as u32, h - 1 - row as u32, Rgba([b, g, r, 255]));
        }
    }
    let image = Arc::new(RenderImage::new(vec![Frame::new(buf)]));
    let f1 = spec.f_min + spec.rows as f64 * spec.df;
    SpecImage {
        image,
        req,
        t0: spec.t_start,
        t1: spec.t_end(),
        f0: spec.f_min - 0.5 * spec.df,
        f1: f1 - 0.5 * spec.df,
    }
}

/// Per-column loudness (dB, normalised 0..1) for the overview strip.
pub fn envelope(audio: &Audio, columns: usize) -> Vec<f32> {
    let n = audio.samples.len();
    if n == 0 || columns == 0 {
        return vec![];
    }
    let mut out: Vec<f32> = (0..columns)
        .map(|c| {
            let a = c * n / columns;
            let b = ((c + 1) * n / columns).max(a + 1).min(n);
            let s = &audio.samples[a..b];
            let rms = (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
            20.0 * rms.max(1e-7).log10()
        })
        .collect();
    let mut sorted = out.clone();
    sorted.sort_by(f32::total_cmp);
    let lo = sorted[(sorted.len() as f32 * 0.02) as usize];
    let hi = sorted[((sorted.len() - 1) as f32 * 0.998) as usize];
    for v in &mut out {
        *v = ((*v - lo) / (hi - lo).max(1e-3)).clamp(0.0, 1.0);
    }
    out
}
