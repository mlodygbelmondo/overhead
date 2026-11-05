//! How busy the background of a recording is in the aircraft band: talking,
//! music, clatter. Such sounds change level from one syllable or note to the
//! next, which a steady ambience (wind, distant traffic, a quiet room) does
//! not. They do not look like fly-bys, but they cover faint aircraft tones, so
//! a recording that is busy all the time only shows loud, close fly-bys.
//!
//! Measure: band level 100-1000 Hz every 50 ms; in each 5 s window the spread
//! between its 90th and 10th percentile. Measured spreads (median over the
//! recording): 2.9-7.2 dB on the 14 outdoor recordings of the test set, whose
//! busy windows (spread > 8 dB) are 1-32 % of the time; 16.3 dB and 99 % on an
//! indoor party with talking and music for 87 minutes (no fly-by found there;
//! the same party laid over the Oosterschelde recording hides all but its
//! loudest fly-by from 10 dB down).

use serde::{Deserialize, Serialize};

use crate::audio::Audio;
use crate::spectrogram::{self, SpecRequest};

const F_LO: f64 = 100.0;
const F_HI: f64 = 1000.0;
const FRAME_S: f64 = 0.05;
const WINDOW_FRAMES: usize = 100;
/// A window whose level spread (p90 - p10) exceeds this is busy (dB).
pub const BUSY_SPREAD_DB: f64 = 8.0;
/// Windows quieter than this (dB, the spectrogram's scale) are digital
/// silence and are not counted.
const SILENT_DB: f64 = -120.0;
/// Busy more than this fraction of the time: worth telling the user.
pub const MOSTLY_BUSY: f64 = 0.5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Background {
    /// Fraction of the 5 s windows that are busy (0..=1).
    pub busy_fraction: f64,
    /// Median level spread of the windows (dB).
    pub median_spread_db: f64,
    /// Windows measured (silent ones excluded).
    pub windows: usize,
}

impl Background {
    /// Busy for most of the recording, measured over at least 30 s.
    pub fn mostly_busy(&self) -> bool {
        self.windows >= 6 && self.busy_fraction > MOSTLY_BUSY
    }
}

pub fn measure(audio: &Audio) -> Background {
    let duration = audio.duration();
    let f_hi = F_HI.min(audio.sample_rate / 2.0 * 0.9);
    let columns = (duration / FRAME_S).floor() as usize;
    if columns < WINDOW_FRAMES || f_hi <= F_LO {
        return Background::default();
    }
    let n_fft = ((FRAME_S * 1.25 * audio.sample_rate).log2().round().exp2() as usize).max(64);
    let spec = spectrogram::compute(
        audio,
        &SpecRequest {
            t_start: 0.0,
            t_end: columns as f64 * FRAME_S,
            f_min: F_LO,
            f_max: f_hi,
            columns,
            n_fft,
            whiten: false,
        },
    );
    let level: Vec<f64> = (0..spec.cols)
        .map(|c| {
            let p: f64 = spec
                .column(c)
                .iter()
                .map(|db| 10f64.powf(*db as f64 / 10.0))
                .sum();
            10.0 * p.max(1e-30).log10()
        })
        .collect();
    let mut spreads: Vec<f64> = level
        .chunks_exact(WINDOW_FRAMES)
        .filter_map(|w| {
            let mut v = w.to_vec();
            v.sort_by(f64::total_cmp);
            let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
            (q(0.5) > SILENT_DB).then(|| q(0.9) - q(0.1))
        })
        .collect();
    if spreads.is_empty() {
        return Background::default();
    }
    let busy = spreads.iter().filter(|s| **s > BUSY_SPREAD_DB).count();
    spreads.sort_by(f64::total_cmp);
    Background {
        busy_fraction: busy as f64 / spreads.len() as f64,
        median_spread_db: spreads[spreads.len() / 2],
        windows: spreads.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(sr: f64, secs: f64, mut amp: impl FnMut(f64) -> f64) -> Audio {
        let n = (sr * secs) as usize;
        let mut seed = 1u32;
        let samples = (0..n)
            .map(|i| {
                let t = i as f64 / sr;
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 8) as f64 / (1u32 << 24) as f64 - 0.5;
                (0.01 * noise + amp(t) * (2.0 * std::f64::consts::PI * 300.0 * t).sin()) as f32
            })
            .collect();
        Audio::from_samples(samples, sr)
    }

    #[test]
    fn steady_background_is_not_busy() {
        let b = measure(&tone(8000.0, 120.0, |_| 0.05));
        assert!(b.windows >= 20, "{b:?}");
        assert!(b.busy_fraction < 0.1 && !b.mostly_busy(), "{b:?}");
    }

    #[test]
    fn syllable_like_bursts_are_busy() {
        // on 0.15 s, off 0.15 s, 30 dB apart: like talking
        let b = measure(&tone(8000.0, 120.0, |t| {
            if (t / 0.15) as u64 % 2 == 0 {
                0.3
            } else {
                0.01
            }
        }));
        assert!(b.busy_fraction > 0.9 && b.mostly_busy(), "{b:?}");
    }

    #[test]
    fn short_recordings_are_not_judged() {
        let b = measure(&tone(8000.0, 20.0, |t| {
            if (t / 0.15) as u64 % 2 == 0 {
                0.3
            } else {
                0.01
            }
        }));
        assert!(!b.mostly_busy(), "{b:?}");
    }
}
