//! Suggesting where a recording was cut (material removed and the rest
//! joined), so the user can accept or remove each suggestion before the
//! location search: a cut breaks the time offsets between the fly-bys on
//! either side of it (the reference video names cuts as the one edit that
//! defeats its timing filter).
//!
//! Three kinds of evidence, each found on its own:
//!
//! * **Dropout**: a run of digital silence (exact zeros or a constant value)
//!   inside the recording, at least [`CutConfig::dropout_s`] long.
//! * **Discontinuity**: a jump of the waveform: one sample of the second
//!   difference far above its local scale (robust, ±2 s), with the
//!   neighbourhood beyond ±50 ms quiet again (a splice click, not a sound
//!   that rings on). Its strength is weighted by how much the jump is one
//!   lasting step of the waveform level (a splice) rather than a swing that
//!   returns (a natural click), so splices rank above most clicks.
//! * **Change**: an abrupt change of the background spectrum: the mean log
//!   power in 20 bands over the `window_s` before and after a point differs
//!   by many standard errors, and much more than it does a full window
//!   either side (a step, not a slow drift such as an
//!   approaching aircraft or wind building up).
//!
//! These are suggestions: a door slam, a gust on the microphone or a car
//! starting can look like a cut, and a cut inside a steady background may
//! leave no trace at all.

use realfft::RealFftPlanner;
use serde::{Deserialize, Serialize};

use crate::audio::Audio;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CutConfig {
    /// Shortest digital silence reported (s).
    pub dropout_s: f64,
    /// Discontinuity: peak of the second difference over its local robust
    /// scale, and over the loudest sample beyond ±50 ms within ±250 ms.
    pub jump_ratio: f64,
    pub jump_isolation: f64,
    /// Change: window either side (s), threshold (difference of the window
    /// means over its standard error, RMS over bands) and sharpness (over the
    /// value a full window away).
    pub window_s: f64,
    pub change_threshold: f64,
    pub change_sharpness: f64,
    /// A change must persist: the window this many windows beyond the
    /// following (preceding) one still differs from the preceding (following)
    /// one by half the threshold.
    pub persist_windows: usize,
    /// Ignore the first and last this many seconds.
    pub edge_s: f64,
    /// Suggestions closer than this are one (s); at most `max` are returned.
    pub merge_s: f64,
    pub max: usize,
    /// Suggestions weaker than this are dropped (a waveform jump that swings
    /// back like a natural click gets a weight of 0.2, see [`CutSuggestion::strength`]).
    #[serde(default)]
    pub min_strength: f64,
}

/// The defaults are set on the 14 unedited-looking test recordings: wind,
/// handling noise and birds give waveform jumps up to ~175× the local scale
/// and background changes up to ~20, so the defaults (60 and 8) still flag a
/// few places in some of them, and a splice inside one steady background
/// (whose jump is typically 15–40×) is found only with [`CutConfig::sensitive`].
impl Default for CutConfig {
    fn default() -> Self {
        Self {
            dropout_s: 0.05,
            jump_ratio: 60.0,
            jump_isolation: 4.0,
            window_s: 2.0,
            change_threshold: 8.0,
            change_sharpness: 2.5,
            persist_windows: 2,
            edge_s: 1.0,
            merge_s: 1.0,
            max: 20,
            min_strength: 0.5,
        }
    }
}

impl CutConfig {
    /// Finds weaker splices (waveform jumps from 15× the local scale,
    /// background changes from 4), at the cost of many natural clicks and
    /// gusts being suggested too.
    pub fn sensitive() -> Self {
        Self { jump_ratio: 15.0, change_threshold: 4.0, max: 50, min_strength: 0.0, ..Self::default() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutKind {
    Dropout,
    Discontinuity,
    Change,
}

impl CutKind {
    pub fn label(&self) -> &'static str {
        match self {
            CutKind::Dropout => "digital silence",
            CutKind::Discontinuity => "waveform jump",
            CutKind::Change => "background change",
        }
    }
}

/// One suggested cut.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CutSuggestion {
    /// Recording time (s).
    pub at: f64,
    pub kind: CutKind,
    /// How strong the evidence is, in units of the kind's threshold
    /// (dropouts: their length over `dropout_s`; waveform jumps: weighted
    /// 0.2–4× by how step-like they are, so this can be below 1). Only for
    /// ranking: what is reported is decided by the thresholds.
    pub strength: f64,
    /// Every kind found at this place (several kinds together are stronger evidence).
    pub kinds: Vec<CutKind>,
}

/// Suggested cuts, strongest first (at most `cfg.max`).
pub fn suggest(audio: &Audio, cfg: &CutConfig) -> Vec<CutSuggestion> {
    let sr = audio.sample_rate;
    let x = &audio.samples;
    let dur = audio.duration();
    let mut raw: Vec<(f64, CutKind, f64)> = Vec::new();
    raw.extend(dropouts(x, sr, cfg).into_iter().map(|(t, s)| (t, CutKind::Dropout, s)));
    raw.extend(jumps(x, sr, cfg).into_iter().map(|(t, s)| (t, CutKind::Discontinuity, s)));
    raw.extend(changes(x, sr, cfg).into_iter().map(|(t, s)| (t, CutKind::Change, s)));
    raw.retain(|r| r.0 >= cfg.edge_s && r.0 <= dur - cfg.edge_s);
    // strongest first; weaker evidence within merge_s joins it
    raw.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut out: Vec<CutSuggestion> = Vec::new();
    for (t, k, s) in raw {
        match out.iter_mut().find(|o| (o.at - t).abs() < cfg.merge_s) {
            Some(o) => {
                if !o.kinds.contains(&k) {
                    o.kinds.push(k);
                    // independent kinds of evidence at one place add up
                    o.strength += 0.5 * s;
                }
                // a waveform jump or dropout gives the exact place
                if o.kind == CutKind::Change && k != CutKind::Change {
                    o.at = t;
                }
            }
            None => out.push(CutSuggestion { at: t, kind: k, strength: s, kinds: vec![k] }),
        }
    }
    out.retain(|o| o.strength >= cfg.min_strength);
    out.sort_by(|a, b| b.strength.total_cmp(&a.strength));
    out.truncate(cfg.max);
    out
}

/// Runs of digital silence: (centre s, length / dropout_s).
fn dropouts(x: &[f32], sr: f64, cfg: &CutConfig) -> Vec<(f64, f64)> {
    let min_len = (cfg.dropout_s * sr).ceil().max(2.0) as usize;
    let mut out = Vec::new();
    let mut i = 0;
    while i < x.len() {
        let v = x[i];
        let mut j = i + 1;
        while j < x.len() && x[j] == v {
            j += 1;
        }
        if j - i >= min_len && i > 0 && j < x.len() {
            let len = (j - i) as f64 / sr;
            out.push(((i + j) as f64 * 0.5 / sr, len / cfg.dropout_s));
        }
        i = j;
    }
    out
}

/// Waveform jumps: (time s, ratio / jump_ratio).
fn jumps(x: &[f32], sr: f64, cfg: &CutConfig) -> Vec<(f64, f64)> {
    if x.len() < 16 {
        return Vec::new();
    }
    let d2: Vec<f32> = (0..x.len()).map(|n| if n < 2 { 0.0 } else { x[n] - 2.0 * x[n - 1] + x[n - 2] }).collect();
    // robust scale per 0.25 s block (1.4826 · median |d2|), then the median over ±2 s
    let block = ((0.25 * sr) as usize).max(8);
    let nb = d2.len().div_ceil(block);
    let mut bscale = Vec::with_capacity(nb);
    let mut tmp: Vec<f32> = Vec::with_capacity(block);
    for b in 0..nb {
        tmp.clear();
        tmp.extend(d2[b * block..((b + 1) * block).min(d2.len())].iter().map(|v| v.abs()));
        let m = tmp.len() / 2;
        let med = if tmp.is_empty() { 0.0 } else { *tmp.select_nth_unstable_by(m, f32::total_cmp).1 };
        bscale.push(1.4826 * med);
    }
    let reach = 8usize;
    let smooth = |bs: &[f32]| -> Vec<f32> {
        (0..nb)
            .map(|b| {
                let mut v: Vec<f32> = bs[b.saturating_sub(reach)..(b + reach + 1).min(nb)].to_vec();
                let m = v.len() / 2;
                *v.select_nth_unstable_by(m, f32::total_cmp).1
            })
            .collect()
    };
    let scale = smooth(&bscale);
    // level change across a jump: x[n + w] − x[n − w − 1] (about ±0.5 ms),
    // with its own robust scale per block
    let w = ((0.0005 * sr).round() as usize).max(2);
    let lag = 2 * w + 1;
    let mut lscale = Vec::with_capacity(nb);
    for b in 0..nb {
        tmp.clear();
        tmp.extend((b * block..((b + 1) * block).min(x.len())).filter(|&n| n >= lag).map(|n| (x[n] - x[n - lag]).abs()));
        let m = tmp.len() / 2;
        let med = if tmp.is_empty() { 0.0 } else { *tmp.select_nth_unstable_by(m, f32::total_cmp).1 };
        lscale.push(1.4826 * med);
    }
    let lscale = smooth(&lscale);
    let near = (0.05 * sr) as usize;
    let far = (0.25 * sr) as usize;
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut n = 2;
    while n < d2.len() {
        let s = scale[n / block].max(1e-12);
        let r = d2[n].abs() / s;
        if r >= cfg.jump_ratio as f32 {
            // the peak of this event
            let end = (n + near).min(d2.len());
            let (pk, pv) = (n..end).map(|k| (k, d2[k].abs())).fold((n, 0.0f32), |a, b| if b.1 > a.1 { b } else { a });
            // isolated: beyond ±50 ms (within ±250 ms) the signal is ordinary again
            let lo = pk.saturating_sub(far);
            let hi = (pk + far).min(d2.len());
            let mut rest: Vec<f32> =
                (lo..pk.saturating_sub(near)).chain((pk + near).min(hi)..hi).map(|k| d2[k].abs()).collect();
            let iso = if rest.is_empty() {
                f32::INFINITY
            } else {
                let q = ((rest.len() - 1) as f64 * 0.99) as usize;
                pv / rest.select_nth_unstable_by(q, f32::total_cmp).1.max(1e-12)
            };
            if iso >= cfg.jump_isolation as f32 {
                // a splice moves the waveform to another level and leaves it
                // there; a natural click (wind, handling, a bird's tick) swings
                // back. `frac`: how much of the movement around the jump is
                // one net step; `stepz`: that step over its usual size.
                let (a, b) = (pk.saturating_sub(w + 1), (pk + w).min(x.len() - 1));
                let net = (x[b] - x[a]).abs() as f64;
                let tot: f64 = (a + 1..=b).map(|k| (x[k] - x[k - 1]).abs() as f64).sum();
                let frac = net / tot.max(1e-12);
                let stepz = net / (lscale[pk / block] as f64).max(1e-12);
                let step_weight = (frac * stepz / 2.5).clamp(0.2, 4.0);
                out.push((pk as f64 / sr, (pv / s) as f64 / cfg.jump_ratio * step_weight));
            }
            n = pk + near;
            continue;
        }
        n += 1;
    }
    out
}

/// Number of bands and their range for the change detector.
const BANDS: usize = 20;
const BAND_LO: f64 = 50.0;

/// Abrupt background changes: (time s, statistic / change_threshold).
fn changes(x: &[f32], sr: f64, cfg: &CutConfig) -> Vec<(f64, f64)> {
    let n_fft = 512usize;
    let hop = 256usize;
    if x.len() < 4 * n_fft {
        return Vec::new();
    }
    let frames = (x.len() - n_fft) / hop + 1;
    let band_hi = (0.475 * sr).max(BAND_LO * 2.0);
    let df = sr / n_fft as f64;
    // band of every FFT bin (log-spaced edges)
    let edges: Vec<f64> = (0..=BANDS).map(|i| BAND_LO * (band_hi / BAND_LO).powf(i as f64 / BANDS as f64)).collect();
    let bin_band: Vec<Option<usize>> = (0..=n_fft / 2)
        .map(|k| {
            let f = k as f64 * df;
            (f >= BAND_LO && f < band_hi).then(|| edges.partition_point(|e| *e <= f).saturating_sub(1).min(BANDS - 1))
        })
        .collect();
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n_fft);
    let window: Vec<f32> = (0..n_fft).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n_fft as f32).cos()).collect();
    let mut input = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut lp = vec![0.0f64; frames * BANDS];
    for f in 0..frames {
        for (i, v) in input.iter_mut().enumerate() {
            *v = x[f * hop + i] * window[i];
        }
        fft.process(&mut input, &mut spec).expect("fft sizes");
        let row = &mut lp[f * BANDS..(f + 1) * BANDS];
        let mut acc = [0.0f64; BANDS];
        let mut cnt = [0usize; BANDS];
        for (k, c) in spec.iter().enumerate() {
            if let Some(b) = bin_band[k] {
                acc[b] += c.norm_sqr() as f64;
                cnt[b] += 1;
            }
        }
        for b in 0..BANDS {
            row[b] = 10.0 * (acc[b] / cnt[b].max(1) as f64 + 1e-20).log10();
        }
    }
    // prefix sums per band of x and x²
    let mut s1 = vec![0.0f64; (frames + 1) * BANDS];
    let mut s2 = vec![0.0f64; (frames + 1) * BANDS];
    for f in 0..frames {
        for b in 0..BANDS {
            let v = lp[f * BANDS + b];
            s1[(f + 1) * BANDS + b] = s1[f * BANDS + b] + v;
            s2[(f + 1) * BANDS + b] = s2[f * BANDS + b] + v * v;
        }
    }
    let w = ((cfg.window_s * sr / hop as f64).round() as usize).max(4);
    // statistic at boundary k (between frames k-1 and k)
    // statistic between the window ending at frame `l` and the one starting at `r`
    let between = |l: usize, r: usize| -> f64 {
        if l < w || r + w > frames {
            return 0.0;
        }
        let mut sum = 0.0;
        for b in 0..BANDS {
            let m = |a: usize, z: usize| {
                let n = (z - a) as f64;
                let mean = (s1[z * BANDS + b] - s1[a * BANDS + b]) / n;
                let var = ((s2[z * BANDS + b] - s2[a * BANDS + b]) / n - mean * mean).max(0.0);
                (mean, var)
            };
            let (ml, vl) = m(l - w, l);
            let (mr, vr) = m(r, r + w);
            // difference of the window means over its standard error (frame
            // spread at least 1 dB; log-power of noise frames scatters ≈ 5 dB)
            let sd = (0.5 * (vl + vr)).sqrt().max(1.0);
            let z = (mr - ml) / (sd * (2.0 / w as f64).sqrt());
            sum += z * z;
        }
        (sum / BANDS as f64).sqrt()
    };
    // statistic at boundary k (between frames k-1 and k)
    let stat = |k: usize| between(k, k);
    let st: Vec<f64> = (0..=frames).map(stat).collect();
    let mut out = Vec::new();
    for k in w..=frames.saturating_sub(w) {
        let v = st[k];
        if v < cfg.change_threshold {
            continue;
        }
        // local maximum over ±w
        let (a, b) = (k.saturating_sub(w), (k + w).min(frames));
        if (a..=b).any(|j| st[j] > v || (st[j] == v && j < k)) {
            continue;
        }
        // a full window away the two windows no longer straddle a step: there the
        // statistic is back to the noise, while a drift or a passing sound keeps it up
        let side = st[k.saturating_sub(w)].max(st[(k + w).min(frames)]);
        if v < cfg.change_sharpness * side {
            continue;
        }
        // a new background persists: the windows two windows further out on
        // either side still differ (a bird call, a voice or a gust that starts
        // abruptly is over within a few seconds)
        let reach = cfg.persist_windows * w;
        if k >= reach + w && k + reach + w <= frames {
            let p = between(k, k + reach).min(between(k - reach, k));
            if p < 0.5 * cfg.change_threshold {
                continue;
            }
        }
        // boundary k is at the start of frame k's centre region
        let t = (k * hop + n_fft / 2) as f64 / sr - 0.5 * hop as f64 / sr;
        out.push((t, v / cfg.change_threshold));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Low-passed noise (most outdoor ambience is low-frequency heavy).
    fn ambience(n: usize, seed: u64, level: f32, smooth: f32) -> Vec<f32> {
        let mut s = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
        let mut y = 0.0f32;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let u = (s >> 11) as f32 / (1u64 << 53) as f32 - 0.5;
                y = smooth * y + (1.0 - smooth) * u;
                level * y
            })
            .collect()
    }

    fn audio(x: Vec<f32>) -> Audio {
        Audio::from_samples(x, 8000.0)
    }

    #[test]
    fn steady_ambience_and_a_flyby_give_no_cut() {
        let sr = 8000.0;
        let n = (120.0 * sr) as usize;
        let mut x = ambience(n, 1, 0.3, 0.9);
        // a slowly falling tone (Doppler-like) that rises and falls in level over a minute
        let mut ph = 0.0f64;
        for (i, v) in x.iter_mut().enumerate() {
            let t = i as f64 / sr;
            let f = 120.0 - 20.0 * ((t - 60.0) / 8.0).tanh();
            ph += std::f64::consts::TAU * f / sr;
            let a = 0.2 * (-((t - 60.0) / 25.0).powi(2)).exp();
            *v += (a * ph.sin()) as f32;
        }
        let s = suggest(&audio(x), &CutConfig::default());
        assert!(s.is_empty(), "{s:?}");
    }

    #[test]
    fn a_splice_between_two_backgrounds_is_found() {
        let sr = 8000.0;
        let a = ambience((40.0 * sr) as usize, 2, 0.3, 0.9);
        let b = ambience((40.0 * sr) as usize, 3, 0.08, 0.6);
        let x: Vec<f32> = a.into_iter().chain(b).collect();
        let s = suggest(&audio(x), &CutConfig::default());
        assert!(!s.is_empty());
        assert!((s[0].at - 40.0).abs() < 0.1, "{s:?}");
        assert!(s[0].kinds.contains(&CutKind::Change), "{s:?}");
        assert_eq!(s.len(), 1, "{s:?}");
    }

    /// Material removed from one steady, low-frequency-heavy background: only
    /// the waveform jump at the splice shows it, and only when the two joined
    /// samples differ enough (a splice between two similar values is
    /// invisible). With the sensitive settings every splice whose jump is over
    /// 3 standard deviations of the background is found at the exact place, and
    /// nothing else is suggested; the defaults find none of them (their jumps
    /// are as large as natural clicks).
    #[test]
    fn material_removed_from_one_background_leaves_a_jump() {
        let sr = 8000.0;
        let full = ambience((60.0 * sr) as usize, 4, 0.3, 0.995);
        let sens = CutConfig::sensitive();
        assert!(suggest(&audio(full.clone()), &sens).is_empty());
        let sd = (full.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / full.len() as f64).sqrt();
        let (mut found, mut big, mut big_found, mut wrong, mut by_default) = (0, 0, 0, 0, 0);
        for k in 0..30 {
            let cut = ((15.0 + k as f64 * 0.37) * sr) as usize;
            // resume where the joined samples differ most within 1 s (a cut
            // without a crossfade between unlike values)
            let lo = cut + ((5.0 + k as f64 * 0.71) * sr) as usize;
            let resume = if k % 2 == 0 {
                (lo..lo + 8000).max_by(|a, b| (full[*a] - full[cut - 1]).abs().total_cmp(&(full[*b] - full[cut - 1]).abs())).unwrap()
            } else {
                lo
            };
            let x: Vec<f32> = full[..cut].iter().chain(&full[resume..]).copied().collect();
            let jump = (full[resume] - full[cut - 1]).abs() as f64 / sd;
            let a = audio(x);
            let s = suggest(&a, &sens);
            let at = cut as f64 / sr;
            let hit = s.iter().any(|c| (c.at - at).abs() < 0.01 && c.kind == CutKind::Discontinuity);
            wrong += s.iter().filter(|c| (c.at - at).abs() >= 0.01).count();
            found += hit as usize;
            if jump > 3.0 {
                big += 1;
                big_found += hit as usize;
            }
            by_default += suggest(&a, &CutConfig::default()).len();
        }
        assert_eq!(wrong, 0);
        assert_eq!(by_default, 0);
        assert!(big >= 10 && big_found == big, "found {found}/30; jumps over 3 sd: {big_found}/{big}");
    }

    /// A splice leaves the waveform at a new level; a click of the same size
    /// swings back. The splice ranks first even when the click is sharper.
    #[test]
    fn a_step_ranks_above_a_click_that_swings_back() {
        let sr = 8000.0;
        let full = ambience((80.0 * sr) as usize, 6, 0.3, 0.995);
        let cut = (20.0 * sr) as usize;
        let lo = cut + (7.0 * sr) as usize;
        let resume = (lo..lo + 8000).max_by(|a, b| (full[*a] - full[cut - 1]).abs().total_cmp(&(full[*b] - full[cut - 1]).abs())).unwrap();
        let mut x: Vec<f32> = full[..cut].iter().chain(&full[resume..]).copied().collect();
        let step = (full[resume] - full[cut - 1]).abs();
        // a click twice the size of the step, over in two samples
        let c = (45.0 * sr) as usize;
        x[c] += 2.0 * step;
        x[c + 1] -= 2.0 * step;
        let s = suggest(&audio(x), &CutConfig::sensitive());
        let at = |t: f64| s.iter().find(|c| (c.at - t).abs() < 0.01).map(|c| c.strength);
        let (sp, cl) = (at(cut as f64 / sr).expect("splice found"), at(c as f64 / sr).expect("click found"));
        assert!(sp > 2.0 * cl, "splice {sp:.2}, click {cl:.2}: {s:?}");
    }

    #[test]
    fn digital_silence_inside_is_a_dropout() {
        let sr = 8000.0;
        let mut x = ambience((30.0 * sr) as usize, 5, 0.3, 0.9);
        let at = (12.0 * sr) as usize;
        for v in &mut x[at..at + 800] {
            *v = 0.0;
        }
        let s = suggest(&audio(x), &CutConfig::default());
        assert!(s.iter().any(|c| c.kinds.contains(&CutKind::Dropout) && (c.at - 12.05).abs() < 0.1), "{s:?}");
    }
}
