//! Recording chain after the microphone: automatic gain control, clipping and
//! lossy re-encoding through ffmpeg (what a phone or a video upload does).

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use realfft::RealFftPlanner;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    None,
    /// MP3, 64 kbit/s (libmp3lame).
    Mp3,
    /// Opus, 32 kbit/s.
    Opus,
    /// Phone-like: 150 Hz high-pass, FFT noise suppression, Opus 24 kbit/s VoIP.
    Phone,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Agc {
    /// Short-term RMS level the AGC steers towards (dBFS).
    pub target_dbfs: f64,
    pub max_gain_db: f64,
    pub min_gain_db: f64,
    /// Gain-reduction (attack) and recovery (release) time constants (s).
    pub attack: f64,
    pub release: f64,
}

/// Phone-style AGC: an RMS follower over 20 ms blocks drives a gain that drops
/// quickly on loud events and recovers slowly, so quiet stretches between them
/// "breathe".
pub fn agc(x: &mut [f32], fs: f64, a: &Agc) {
    let block = (0.02 * fs) as usize;
    let (ka, kr) = ((-0.02 / a.attack).exp(), (-0.02 / a.release).exp());
    let mut gain_db = 0.0f64;
    let mut prev = 0.0f64;
    for chunk in x.chunks_mut(block) {
        let rms = (chunk.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / chunk.len() as f64).sqrt();
        let level = 20.0 * rms.max(1e-9).log10();
        let want = (a.target_dbfs - level).clamp(a.min_gain_db, a.max_gain_db);
        let k = if want < gain_db { ka } else { kr };
        gain_db = want + (gain_db - want) * k;
        let (g0, g1) = (10f64.powf(prev / 20.0), 10f64.powf(gain_db / 20.0));
        let n = chunk.len() as f64;
        for (i, v) in chunk.iter_mut().enumerate() {
            *v = (*v as f64 * (g0 + (g1 - g0) * i as f64 / n)) as f32;
        }
        prev = gain_db;
    }
}

/// Hard clipping at ±`level`; returns the number of clipped samples.
pub fn clip(x: &mut [f32], level: f32) -> usize {
    let mut n = 0;
    for v in x.iter_mut() {
        if v.abs() > level {
            *v = v.signum() * level;
            n += 1;
        }
    }
    n
}

pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
}

/// Encode and decode `x` through ffmpeg, returning a signal of the same length,
/// time-aligned to the input (codec delay removed by cross-correlation).
pub fn codec(x: &[f32], fs: u32, codec: Codec) -> Result<Vec<f32>> {
    if codec == Codec::None {
        return Ok(x.to_vec());
    }
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("overhead-bench-{}-{id}", std::process::id()));
    let raw = tmp.with_extension("f32");
    let (enc, args): (PathBuf, Vec<&str>) = match codec {
        Codec::Mp3 => (tmp.with_extension("mp3"), vec!["-c:a", "libmp3lame", "-b:a", "64k"]),
        Codec::Opus => (tmp.with_extension("opus"), vec!["-c:a", "libopus", "-b:a", "32k"]),
        Codec::Phone => (
            tmp.with_extension("opus"),
            vec!["-af", "highpass=f=150:p=2,afftdn=nr=12:nf=-50", "-c:a", "libopus", "-b:a", "24k", "-application", "voip"],
        ),
        Codec::None => unreachable!(),
    };
    let bytes: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(&raw, bytes)?;
    let rate = fs.to_string();
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y", "-f", "f32le", "-ar", &rate, "-ac", "1", "-i"])
        .arg(&raw)
        .args(&args)
        .arg(&enc)
        .status()
        .context("run ffmpeg")?;
    let _ = std::fs::remove_file(&raw);
    if !status.success() {
        bail!("ffmpeg encode ({codec:?}) failed");
    }
    let mut child = Command::new("ffmpeg")
        .args(["-v", "fatal", "-nostdin", "-i"])
        .arg(&enc)
        .args(["-f", "f32le", "-ac", "1", "-ar", &rate, "-"])
        .stdout(Stdio::piped())
        .spawn()
        .context("run ffmpeg")?;
    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out)?;
    let ok = child.wait()?.success();
    let _ = std::fs::remove_file(&enc);
    if !ok {
        bail!("ffmpeg decode ({codec:?}) failed");
    }
    let y: Vec<f32> = out.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    let lag = estimate_lag(x, &y, (fs / 4) as usize);
    Ok((0..x.len())
        .map(|i| {
            let j = i as isize + lag;
            if j >= 0 && (j as usize) < y.len() { y[j as usize] } else { 0.0 }
        })
        .collect())
}

/// Lag `L` (samples) maximising `Σ x[i] y[i + L]` over `|L| ≤ max_lag`, from a
/// window in the middle of the signal.
fn estimate_lag(x: &[f32], y: &[f32], max_lag: usize) -> isize {
    let n = 1usize << 18;
    let len = x.len().min(y.len());
    if len < 4 * max_lag {
        return 0;
    }
    let w = (n / 2).min(len - 2 * max_lag);
    let start = (len - w) / 2;
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);
    let ifft = planner.plan_fft_inverse(n);
    let mut a = vec![0.0; n];
    let mut b = vec![0.0; n];
    for i in 0..w {
        a[i] = x[start + i] as f64;
    }
    for i in 0..(w + 2 * max_lag).min(n) {
        if let Some(v) = y.get(start - max_lag + i) {
            b[i] = *v as f64;
        }
    }
    let (mut fa, mut fb) = (fft.make_output_vec(), fft.make_output_vec());
    fft.process(&mut a, &mut fa).unwrap();
    fft.process(&mut b, &mut fb).unwrap();
    let mut prod: Vec<_> = fa.iter().zip(&fb).map(|(p, q)| p.conj() * q).collect();
    prod[0].im = 0.0;
    prod[n / 2].im = 0.0;
    let mut corr = ifft.make_output_vec();
    ifft.process(&mut prod, &mut corr).unwrap();
    // corr[k] = Σ a[i] b[i + k], b[0] corresponds to y[start - max_lag]
    let best = (0..=2 * max_lag).max_by(|&p, &q| corr[p].total_cmp(&corr[q])).unwrap();
    best as isize - max_lag as isize
}
