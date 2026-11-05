//! Listening to a recording with a frequency boost, as the reference video
//! does to make a faint propeller audible: an optional band boost (and
//! attenuation of everything else), an overall gain and a soft limiter,
//! then resampling to the output device's rate. Pure DSP, no audio device:
//! the app's player (`overhead-app/src/audio.rs`) drives a [`Renderer`]
//! from its audio callback, and `overhead-cli listen` renders to a WAV file.

use serde::{Deserialize, Serialize};

use crate::audio::Audio;

/// What to hear.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListenSettings {
    /// Band to boost (Hz, low and high edge); `None` = the recording as it is.
    pub band: Option<(f64, f64)>,
    /// Level of the band (dB, e.g. +18).
    pub band_gain_db: f64,
    /// Level of everything outside the band (dB, e.g. −12; 0 keeps it).
    pub rest_gain_db: f64,
    /// Overall gain (dB).
    pub gain_db: f64,
    /// Soft limiter above −1 dBFS instead of hard clipping.
    pub limiter: bool,
}

impl Default for ListenSettings {
    fn default() -> Self {
        Self { band: None, band_gain_db: 18.0, rest_gain_db: -12.0, gain_db: 0.0, limiter: true }
    }
}

impl ListenSettings {
    /// Boost `lo..hi` Hz (the lines of a fly-by and their harmonics).
    pub fn boost(lo: f64, hi: f64) -> Self {
        Self { band: Some((lo.min(hi), lo.max(hi))), ..Self::default() }
    }
}

/// `(lo, hi)` in order and inside `1 Hz .. 0.49 · sr`, or `None` when that
/// leaves no band (or the edges are not numbers).
pub fn normalized_band(band: (f64, f64), sample_rate: f64) -> Option<(f64, f64)> {
    let (a, b) = band;
    if !a.is_finite() || !b.is_finite() {
        return None;
    }
    let top = 0.49 * sample_rate;
    let (lo, hi) = (a.min(b).clamp(1.0, top), a.max(b).clamp(1.0, top));
    (hi - lo >= 1.0).then_some((lo, hi))
}

fn db(x: f64) -> f64 {
    10f64.powf(x / 20.0)
}

/// RBJ biquad (direct form I), 2nd-order Butterworth sections.
#[derive(Clone, Copy, Debug)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

/// Q of the two sections of a 4th-order Butterworth filter.
const BUTTER4_Q: [f64; 2] = [0.541_196_1, 1.306_563];

impl Biquad {
    fn new(kind: u8, f: f64, q: f64, sr: f64) -> Self {
        let f = f.clamp(1.0, 0.49 * sr);
        let w = std::f64::consts::TAU * f / sr;
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = if kind == 0 { ((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0) } else { ((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0) };
        Self { b: [b0 / a0, b1 / a0, b2 / a0], a: [-2.0 * c / a0, (1.0 - alpha) / a0], x: [0.0; 2], y: [0.0; 2] }
    }
    fn low(f: f64, q: f64, sr: f64) -> Self {
        Self::new(0, f, q, sr)
    }
    fn high(f: f64, q: f64, sr: f64) -> Self {
        Self::new(1, f, q, sr)
    }
    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1] - self.a[0] * self.y[0] - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
    fn reset(&mut self) {
        self.x = [0.0; 2];
        self.y = [0.0; 2];
    }
}

/// The boost and gain at the recording's own sample rate (stateful, sample by sample).
#[derive(Clone, Debug)]
pub struct Processor {
    settings: ListenSettings,
    sr: f64,
    /// Band-pass: a 4th-order Butterworth high-pass at the low edge, then a 4th-order low-pass at the high edge (two sections each).
    sections: Vec<Biquad>,
    band: f64,
    rest: f64,
    gain: f64,
}

impl Processor {
    pub fn new(settings: ListenSettings, sample_rate: f64) -> Self {
        let mut p = Self { settings, sr: sample_rate, sections: Vec::new(), band: 1.0, rest: 1.0, gain: 1.0 };
        p.set(settings);
        p
    }

    pub fn settings(&self) -> ListenSettings {
        self.settings
    }

    /// New settings; the filter state is kept when the band is unchanged (no
    /// click). A band given high edge first is swapped; a band without width
    /// or outside 0..Nyquist (after clamping) plays the recording unfiltered.
    pub fn set(&mut self, mut s: ListenSettings) {
        s.band = s.band.and_then(|b| normalized_band(b, self.sr));
        let same_band = s.band == self.settings.band && !self.sections.is_empty();
        self.settings = s;
        if !same_band {
            self.sections = match s.band {
                Some((lo, hi)) => BUTTER4_Q
                    .iter()
                    .map(|&q| Biquad::high(lo, q, self.sr))
                    .chain(BUTTER4_Q.iter().map(|&q| Biquad::low(hi, q, self.sr)))
                    .collect(),
                None => Vec::new(),
            };
        }
        self.band = db(s.band_gain_db);
        self.rest = db(s.rest_gain_db);
        self.gain = db(s.gain_db);
    }

    pub fn reset(&mut self) {
        self.sections.iter_mut().for_each(Biquad::reset);
    }

    /// One sample in, one out.
    #[inline]
    pub fn run(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let y = if self.settings.band.is_some() {
            let mut b = x;
            for s in &mut self.sections {
                b = s.run(b);
            }
            // inside the band: `band`; outside: `rest`
            self.rest * x + (self.band - self.rest) * b
        } else {
            x
        };
        let y = y * self.gain;
        (if self.settings.limiter { soft_limit(y) } else { y.clamp(-1.0, 1.0) }) as f32
    }
}

/// Linear below 0.89 (−1 dBFS), then a smooth knee that never exceeds 1.
#[inline]
pub fn soft_limit(y: f64) -> f64 {
    const K: f64 = 0.891;
    let a = y.abs();
    if a <= K { y } else { y.signum() * (K + (1.0 - K) * ((a - K) / (1.0 - K)).tanh()) }
}

/// Plays a recording from a position at an output rate: the [`Processor`]
/// at the recording's rate, then cubic (Catmull–Rom) interpolation to the
/// output rate. Feed it from an audio callback with [`Renderer::render`].
#[derive(Clone, Debug)]
pub struct Renderer {
    proc: Processor,
    in_rate: f64,
    out_rate: f64,
    /// Next source sample to process, and the last four processed samples.
    next: usize,
    hist: [f32; 4],
    /// Fractional position between hist[1] and hist[2].
    frac: f64,
    /// Stop at this source sample (exclusive).
    end: usize,
}

/// Seconds of audio run through the filters before a seek target, so the
/// band-pass has settled when playback starts.
pub const PREROLL_S: f64 = 0.15;

impl Renderer {
    pub fn new(settings: ListenSettings, in_rate: f64, out_rate: f64) -> Self {
        Self { proc: Processor::new(settings, in_rate), in_rate, out_rate, next: 0, hist: [0.0; 4], frac: 0.0, end: usize::MAX }
    }

    pub fn settings(&self) -> ListenSettings {
        self.proc.settings()
    }

    pub fn set_settings(&mut self, s: ListenSettings) {
        self.proc.set(s);
    }

    /// Recording time (s) of the sample about to be played.
    pub fn position(&self) -> f64 {
        // hist[1] is the sample at `next - 3`
        ((self.next as f64 - 3.0) + self.frac).max(0.0) / self.in_rate
    }

    /// Stop at `t` s (`None`: at the end of the recording).
    pub fn set_end(&mut self, t: Option<f64>, samples: &[f32]) {
        self.end = t.map_or(usize::MAX, |t| (t.max(0.0) * self.in_rate).round() as usize).min(samples.len());
    }

    pub fn at_end(&self, samples: &[f32]) -> bool {
        // hist[1], the sample being played, is `next - 3`
        self.next.saturating_sub(3) >= self.end.min(samples.len())
    }

    /// Jump to `t` s: filters are reset and run over [`PREROLL_S`] before it.
    pub fn seek(&mut self, t: f64, samples: &[f32]) {
        let target = ((t.max(0.0) * self.in_rate).round() as usize).min(samples.len());
        let pre = (PREROLL_S * self.in_rate) as usize;
        self.proc.reset();
        let before = target.saturating_sub(1);
        for &x in &samples[before.saturating_sub(pre)..before] {
            self.proc.run(x);
        }
        // the sample at `target` becomes hist[1]: the one before it, then it and two more
        let h0 = self.proc.run(if target > 0 { samples[target - 1] } else { 0.0 });
        self.hist = [h0, 0.0, 0.0, 0.0];
        self.next = target;
        self.frac = 0.0;
        for k in 1..4 {
            self.hist[k] = self.pull(samples);
        }
    }

    fn pull(&mut self, samples: &[f32]) -> f32 {
        let x = if self.next < samples.len() && self.next < self.end { samples[self.next] } else { 0.0 };
        self.next += 1;
        self.proc.run(x)
    }

    /// Fill `out` (interleaved, `channels` per frame, the same sample on
    /// each) and return the frames written before the end (the rest is silence).
    pub fn render(&mut self, samples: &[f32], out: &mut [f32], channels: usize) -> usize {
        let ch = channels.max(1);
        let step = self.in_rate / self.out_rate;
        let mut written = 0;
        for frame in out.chunks_mut(ch) {
            if self.at_end(samples) {
                frame.iter_mut().for_each(|v| *v = 0.0);
                continue;
            }
            let [p0, p1, p2, p3] = self.hist.map(|v| v as f64);
            let t = self.frac;
            // Catmull–Rom between p1 and p2
            let y = 0.5
                * (2.0 * p1 + (-p0 + p2) * t + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t * t * t);
            // the interpolation can overshoot between limited samples
            let y = y.clamp(-1.0, 1.0);
            frame.iter_mut().for_each(|v| *v = y as f32);
            written += 1;
            self.frac += step;
            while self.frac >= 1.0 {
                self.frac -= 1.0;
                let n = self.pull(samples);
                self.hist = [self.hist[1], self.hist[2], self.hist[3], n];
            }
        }
        written
    }
}

/// Render `t0..t1` s of a recording through `settings` at `out_rate` (mono).
pub fn render(audio: &Audio, t0: f64, t1: f64, settings: ListenSettings, out_rate: f64) -> Vec<f32> {
    let mut r = Renderer::new(settings, audio.sample_rate, out_rate);
    r.seek(t0, &audio.samples);
    r.set_end(Some(t1), &audio.samples);
    let n = ((t1 - t0).max(0.0) * out_rate).ceil() as usize;
    let mut out = vec![0.0f32; n];
    let w = r.render(&audio.samples, &mut out, 1);
    out.truncate(w);
    out
}

/// Gain (dB) that brings `t0..t1` through `settings` (without gain and
/// limiter) to an RMS of `target_dbfs` (e.g. −20), capped at +40 dB.
pub fn suggest_gain_db(audio: &Audio, t0: f64, t1: f64, settings: ListenSettings, target_dbfs: f64) -> f64 {
    let s = ListenSettings { gain_db: 0.0, limiter: false, ..settings };
    let mut p = Processor::new(s, audio.sample_rate);
    let a = ((t0.max(0.0) * audio.sample_rate) as usize).min(audio.samples.len());
    let b = ((t1.max(0.0) * audio.sample_rate) as usize).clamp(a, audio.samples.len());
    let mut acc = 0.0f64;
    // the processor clamps at full scale without the limiter; measure before that
    for &x in &audio.samples[a..b] {
        let y = p.run(x) as f64;
        acc += y * y;
    }
    let rms = (acc / (b - a).max(1) as f64).sqrt().max(1e-9);
    (target_dbfs - 20.0 * rms.log10()).min(40.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f64, amp: f64, sr: f64, secs: f64) -> Vec<f32> {
        (0..(sr * secs) as usize).map(|i| (amp * (std::f64::consts::TAU * f * i as f64 / sr).sin()) as f32).collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }

    fn level_db(f: f64, s: ListenSettings) -> f64 {
        let sr = 8000.0;
        let a = Audio::from_samples(tone(f, 0.01, sr, 3.0), sr);
        let y = render(&a, 0.5, 2.5, s, 48000.0);
        20.0 * (rms(&y) / (0.01 / 2f64.sqrt())).log10()
    }

    #[test]
    fn passthrough_keeps_the_signal() {
        let s = ListenSettings { band: None, ..Default::default() };
        for f in [60.0, 300.0, 1500.0] {
            let l = level_db(f, s);
            assert!(l.abs() < 0.3, "{f} Hz: {l:.2} dB");
        }
    }

    #[test]
    fn band_boost_lifts_the_band_and_cuts_the_rest() {
        let s = ListenSettings::boost(80.0, 250.0); // +18 dB in, −12 dB out
        let inside = level_db(150.0, s);
        assert!((inside - 18.0).abs() < 1.5, "inside {inside:.1} dB");
        for f in [20.0, 1200.0, 3000.0] {
            let out = level_db(f, s);
            assert!(out < -9.0 && out > -14.0, "{f} Hz: {out:.1} dB");
        }
        // only the band (rest muted): far outside is at least 40 dB down
        let iso = ListenSettings { rest_gain_db: -120.0, band_gain_db: 0.0, ..s };
        assert!(level_db(150.0, iso).abs() < 1.5);
        assert!(level_db(1500.0, iso) < -40.0, "{}", level_db(1500.0, iso));
    }

    #[test]
    fn a_reversed_or_empty_band_is_handled() {
        let fwd = ListenSettings::boost(80.0, 250.0);
        let rev = ListenSettings { band: Some((250.0, 80.0)), ..fwd };
        for f in [40.0, 150.0, 1200.0] {
            assert!((level_db(f, fwd) - level_db(f, rev)).abs() < 1e-9, "{f} Hz");
        }
        assert_eq!(Processor::new(rev, 8000.0).settings().band, Some((80.0, 250.0)));
        // no width, not numbers, or entirely above Nyquist: no band
        assert_eq!(normalized_band((100.0, 100.0), 8000.0), None);
        assert_eq!(normalized_band((f64::NAN, 100.0), 8000.0), None);
        assert_eq!(normalized_band((5000.0, 6000.0), 8000.0), None);
        assert_eq!(normalized_band((3000.0, 6000.0), 8000.0), Some((3000.0, 3920.0)));
    }

    #[test]
    fn gain_and_limiter() {
        let s = ListenSettings { gain_db: 6.0, ..Default::default() };
        assert!((level_db(300.0, s) - 6.0).abs() < 0.3);
        // a full-scale tone with +20 dB never exceeds 1 with the limiter
        let sr = 8000.0;
        let a = Audio::from_samples(tone(200.0, 0.9, sr, 1.0), sr);
        let y = render(&a, 0.0, 1.0, ListenSettings { gain_db: 20.0, ..Default::default() }, 44100.0);
        assert!(y.iter().all(|v| v.abs() <= 1.0));
        assert!(y.iter().any(|v| v.abs() > 0.95));
        // small signals stay linear
        assert_eq!(soft_limit(0.5), 0.5);
        assert!(soft_limit(10.0) <= 1.0 && soft_limit(-10.0) >= -1.0 && soft_limit(0.95) < 0.95);
    }

    #[test]
    fn resampling_keeps_pitch_and_timing() {
        let sr = 8000.0;
        let a = Audio::from_samples(tone(440.0, 0.5, sr, 2.0), sr);
        let out_rate = 44100.0;
        let y = render(&a, 0.0, 2.0, ListenSettings::default(), out_rate);
        assert!((y.len() as f64 - 2.0 * out_rate).abs() <= 2.0, "{}", y.len());
        // count zero crossings over 1 s: 880 for 440 Hz
        let seg = &y[(0.5 * out_rate) as usize..(1.5 * out_rate) as usize];
        let zc = seg.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
        assert!((zc as i64 - 880).abs() <= 2, "{zc}");
    }

    #[test]
    fn seek_position_and_end() {
        let sr = 8000.0;
        // a ramp: sample value = its time in seconds / 100
        let x: Vec<f32> = (0..(sr * 10.0) as usize).map(|i| (i as f64 / sr / 100.0) as f32).collect();
        let a = Audio::from_samples(x, sr);
        let mut r = Renderer::new(ListenSettings { limiter: false, ..Default::default() }, sr, 16000.0);
        r.seek(4.0, &a.samples);
        assert!((r.position() - 4.0).abs() < 1e-3, "{}", r.position());
        let mut out = vec![0.0f32; 1600 * 2];
        let w = r.render(&a.samples, &mut out, 2);
        assert_eq!(w, 1600);
        // first frame plays t = 4.0 s on both channels
        assert!((out[0] as f64 - 0.04).abs() < 1e-4 && out[0] == out[1], "{}", out[0]);
        assert!((r.position() - 4.1).abs() < 1e-3, "{}", r.position());
        // stops at the end point
        r.set_end(Some(4.2), &a.samples);
        let mut out = vec![0.0f32; 16000];
        let w = r.render(&a.samples, &mut out, 1);
        assert!((w as i64 - 1600).abs() <= 3, "{w}");
        assert!(r.at_end(&a.samples));
        assert!(out[w + 5..].iter().all(|v| *v == 0.0));
    }

    #[test]
    fn suggested_gain_reaches_the_target() {
        let sr = 8000.0;
        let a = Audio::from_samples(tone(150.0, 0.001, sr, 3.0), sr);
        let s = ListenSettings::boost(100.0, 200.0);
        let g = suggest_gain_db(&a, 0.0, 3.0, s, -20.0);
        let y = render(&a, 0.5, 2.5, ListenSettings { gain_db: g, ..s }, 8000.0);
        let l = 20.0 * rms(&y).log10();
        assert!((l + 20.0).abs() < 1.0, "{l:.1} dBFS with {g:.1} dB");
    }
}
