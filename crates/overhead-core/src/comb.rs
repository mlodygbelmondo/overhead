//! Lines of one fly-by found by their shared Doppler shape (its harmonic comb).
//!
//! Every line of one aircraft is heard as `f · k(t)` with the same Doppler
//! factor `k(t)`: only the rest frequency `f` differs. So once one line is
//! fitted, the others can be found by scanning `f` and taking the mean
//! brightness along `f · k(t)` near the closest approach ([`scan_lines`]).
//! Lines of the engine and propeller are integer multiples of a fundamental
//! (shaft or firing rate) that may be weak or not heard at all, and the
//! strongest line is often a higher multiple; [`fundamental`] picks the
//! fundamental that explains the lines found best (a harmonic sieve: many
//! lines, few missing low multiples, each line within a fraction of a bin of
//! its multiple), and of equals the largest.
//!
//! A stationary tone (mains hum, machinery) does not follow the Doppler
//! shape: the scanned curve only crosses it, on one side of the closest
//! approach, so it fails the per-side contrast test.

use crate::audio::Audio;
use crate::fit::{self, FitConfig, Region, ScoreMap};
use crate::model::Flyby;

/// Half-width of the scanned window around the heard CPA: this many
/// `d / v` (the steep part, where the Doppler shape tells lines of this
/// aircraft from anything else), within the bounds (s).
const HALF_DV: f64 = 1.5;
const HALF_MIN_S: f64 = 15.0;
const HALF_MAX_S: f64 = 45.0;
/// FFT window of the scan map (s): fine enough in frequency to place lines
/// within a fraction of a percent at ~100 Hz.
const WINDOW_S: f64 = 1.0;
/// Time step of the scan map (s).
const SCAN_DT: f64 = 0.25;
/// A pass whose steep part is brief (`d / v` below this, s) is scanned with
/// a time step of at most `STEEP_DT_DV · d / v`, so each side of its steep
/// part (about ±1.2 `d / v`) has the columns `fit::side_contrast` needs (10;
/// at 0.25 s that takes `d / v` of 2 s). Vlissingen at 2408 s: 50 m at
/// 36 m/s, steep for ±1.7 s, too few columns to judge at 0.25 s. The window
/// stays: a shorter one has coarser bins, and its wider copy margin cuts off
/// the lowest lines (there: 75 Hz).
const STEEP_DV: f64 = 2.0;
const STEEP_DT_DV: f64 = 0.1;
/// A scanned rest frequency is a candidate line when it is the brightest
/// within ±`PEAK_REL` (or ±`PEAK_BINS`) and stands `SCAN_SIGMAS` robust
/// sigmas above the scan's median.
pub const PEAK_REL: f64 = 0.015;
pub const PEAK_BINS: f64 = 1.5;
const SCAN_SIGMAS: f64 = 4.0;
/// Candidates tested per fly-by, brightest first.
const MAX_CANDIDATES: usize = 24;
/// A candidate is a line of this fly-by when it stands out from frequency
/// shifted copies of itself (`fit::contrast`) and does so on both sides of
/// the closest approach (`fit::side_contrast`).
pub const MIN_CONTRAST: f64 = 5.0;
pub const MIN_SIDE_CONTRAST: f64 = 2.5;
/// A candidate is a stationary tone the curve crosses (mains hum, a
/// machine) when a level line within the curve's frequency span is this
/// bright, relative to the curve, on the curve's steep part. Lines of real
/// passes: 0.0-0.7 (one at 1.3); drifting mains hum crossed by a mis-fit in
/// the benchmark: 3.3 and more.
pub const MAX_STATIONARY: f64 = 1.0;
/// Fundamental search: highest multiple, lowest fundamental (Hz). Low
/// enough for a helicopter's main rotor (whose lines run to its 30th
/// multiple and beyond); [`COMPLETENESS_WEIGHT`] keeps the many low
/// candidates this allows from winning by chance.
pub const MAX_MULTIPLE: u32 = 32;
pub const MIN_FUNDAMENTAL: f64 = 10.0;
/// A line is the multiple `n · f1` when it lies within max(`TOL_REL` of its
/// frequency, `TOL_BINS` scan bins) of it, also after `f1` is refined on all
/// the lines it explains. Lines of one pass keep their ratios to about 0.1 %
/// in the scan (Vlissingen 87.5 × 3, 4, 7: within 0.3 Hz at 2.7 Hz bins), so
/// a line further off is no multiple, and placing it at one would move it.
pub const TOL_REL: f64 = 0.003;
pub const TOL_BINS: f64 = 0.4;
/// A fundamental is scored by the lines it explains, less this weight times
/// the log of the share of multiples up to its highest one that are heard
/// (a harmonic sieve): with lines at multiples up to 16 and a tolerance of a
/// fraction of a bin, two or three unrelated lines nearly always have some
/// common sub-fundamental, which then explains them only at a few scattered
/// high multiples. 105 × 1, 2 and an unrelated 290 Hz: 105 Hz scores 2, its
/// quarter (× 4, 8, 11) 3 − 2.6. Vlissingen 87.5 × 3, 4, 7, 10 scores 2.2.
pub const COMPLETENESS_WEIGHT: f64 = 2.0;

/// One line found along the fly-by's Doppler shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CombLine {
    /// Rest frequency under the scanned geometry (Hz).
    pub f: f64,
    /// Mean z-score along the line in the scanned window.
    pub score: f64,
    /// `fit::contrast` of the line on the scan map.
    pub contrast: f64,
    /// `fit::side_contrast` of the line.
    pub sides: (f64, f64),
    /// [`stationary_ratio`] of the line: about 0 for a line that follows
    /// the shape, 1 or more for a level tone the curve crosses.
    pub level: f64,
}

impl CombLine {
    /// A line of the scanned fly-by: it stands out from shifted copies of
    /// itself, on both sides of the closest approach, and is no level tone.
    pub fn is_line(&self) -> bool {
        self.contrast >= MIN_CONTRAST && self.sides.0.min(self.sides.1) >= MIN_SIDE_CONTRAST && self.level < MAX_STATIONARY
    }
}

/// Result of [`scan_lines`].
pub struct Comb {
    /// Lines found, by rising frequency.
    pub lines: Vec<CombLine>,
    /// Frequency resolution of the scan map (Hz).
    pub df: f64,
    flyby: Flyby,
    c: f64,
    cfg: FitConfig,
    map: ScoreMap,
    /// Mean z along `f · k(t)` at `f = f_lo + i · step`.
    scan: Vec<f64>,
    f_lo: f64,
    step: f64,
}

impl Comb {
    fn freq(&self, i: f64) -> f64 {
        self.f_lo + i * self.step
    }

    /// The line at the scan's brightest rest frequency within ±2 % of `f`,
    /// measured as the scan measures its own (`None`: `f` is outside the
    /// scanned range). For a line found by its own fit, whose rest frequency
    /// under its own geometry is less precise.
    pub fn measure(&self, f: f64) -> Option<CombLine> {
        let n = self.scan.len();
        let at = |f: f64| (f - self.f_lo) / self.step;
        let (lo, hi) = (at(f * 0.98).ceil().max(0.0) as usize, (at(f * 1.02).floor().min(n as f64 - 1.0)) as usize);
        if n == 0 || at(f) < 0.0 || at(f) > (n - 1) as f64 || lo > hi {
            return None;
        }
        let i = (lo..=hi).max_by(|a, b| self.scan[*a].total_cmp(&self.scan[*b]))?;
        Some(self.line_at(i))
    }

    /// The candidate line at scan index `i` (refined between grid points).
    fn line_at(&self, i: usize) -> CombLine {
        let scan = &self.scan;
        let off = if i > 0 && i + 1 < scan.len() {
            let (a, b, cc) = (scan[i - 1], scan[i], scan[i + 1]);
            let den = a - 2.0 * b + cc;
            if den.abs() > 1e-12 { (0.5 * (a - cc) / den).clamp(-0.5, 0.5) } else { 0.0 }
        } else {
            0.0
        };
        let f = self.freq(i as f64 + off);
        let q = Flyby { f0: f, ..self.flyby };
        CombLine {
            f,
            score: scan[i],
            contrast: fit::contrast(&self.map, &q, &self.cfg),
            sides: fit::side_contrast(&self.map, &q, &self.cfg),
            level: stationary_ratio(&self.map, &q, self.c),
        }
    }
}

/// Every line that follows the Doppler shape of `p` (rest frequencies whose
/// curve `f · k(t)` stays inside `band` Hz), scanned over the steep part of
/// the pass. A steep pass (close, fast) is scanned at a finer time step, so
/// both sides of its brief steep part still have columns to judge.
pub fn scan_lines(audio: &Audio, p: &Flyby, band: (f64, f64), c: f64) -> Option<Comb> {
    if !(p.v > 0.0 && p.d > 0.0 && p.f0 > 0.0) {
        return None;
    }
    let half = (HALF_DV * p.d / p.v).clamp(HALF_MIN_S, HALF_MAX_S);
    let (t0, t1) = ((p.tc - half).max(0.0), (p.tc + half).min(audio.duration()));
    if t1 - t0 < HALF_MIN_S {
        return None;
    }
    let n_fft = ((WINDOW_S * audio.sample_rate).log2().round().exp2() as usize).clamp(2048, 16384);
    let mut dt = SCAN_DT;
    let dv = p.d / p.v;
    if dv < STEEP_DV {
        dt = dt.min(STEEP_DT_DV * dv).max(0.02);
    }
    let cfg = FitConfig { c, n_fft, dt, harmonics: vec![1.0], ..FitConfig::default() };
    let region = Region { t_start: t0, t_end: t1, f_min: band.0.max(1.0), f_max: band.1 };
    if region.f_max <= region.f_min {
        return None;
    }
    let map = ScoreMap::new(fit::region_spectrogram(audio, &region, &cfg));
    let spec = &map.spec;
    if spec.cols < 10 || spec.rows < 50 {
        return None;
    }
    let ks: Vec<f64> = (0..spec.cols).map(|col| p.freq_at(spec.col_time(col), c) / p.f0).collect();
    let (kmin, kmax) = ks.iter().fold((f64::INFINITY, 0.0f64), |(a, b), k| (a.min(*k), b.max(*k)));
    // room for the ±20-bin contrast copies
    let margin = 21.0 * spec.df;
    let f_lo = (spec.f_min + margin) / kmin;
    let f_hi = (spec.f_max() - margin) / kmax;
    let step = 0.25 * spec.df / kmax;
    let df = spec.df;
    let mut comb = Comb { lines: Vec::new(), df, flyby: *p, c, cfg, map, scan: Vec::new(), f_lo, step };
    if f_hi.is_nan() || f_hi <= f_lo {
        return Some(comb);
    }
    let n = ((f_hi - f_lo) / step) as usize + 1;
    let spec = &comb.map.spec;
    comb.scan = (0..n)
        .map(|i| {
            let f = f_lo + i as f64 * step;
            let total: f64 = ks.iter().enumerate().map(|(col, k)| spec.sample(col, f * k).unwrap_or(0.0) as f64).sum();
            total / spec.cols as f64
        })
        .collect();
    let scan = &comb.scan;
    let mut sorted = scan.clone();
    sorted.sort_by(f64::total_cmp);
    let med = sorted[n / 2];
    let mut dev: Vec<f64> = scan.iter().map(|s| (s - med).abs()).collect();
    dev.sort_by(f64::total_cmp);
    let sigma = (1.4826 * dev[n / 2]).max(1e-6);
    let mut peaks: Vec<usize> = (0..n)
        .filter(|&i| {
            let f = comb.freq(i as f64);
            let w = ((PEAK_REL * f).max(PEAK_BINS * df) / step).round() as usize;
            let (lo, hi) = (i.saturating_sub(w), (i + w + 1).min(n));
            scan[i] > med + SCAN_SIGMAS * sigma && scan[lo..hi].iter().all(|s| *s <= scan[i])
        })
        .collect();
    peaks.sort_by(|a, b| scan[*b].total_cmp(&scan[*a]));
    peaks.truncate(MAX_CANDIDATES);
    let debug = std::env::var_os("OVERHEAD_DEBUG").is_some();
    let mut lines = Vec::new();
    for i in peaks {
        let l = comb.line_at(i);
        if debug {
            eprintln!(
                "comb tc {:.1}: {:.1} Hz scan {:.2} ({:.1} sigma) contrast {:.1} sides {:.1}/{:.1} level {:.2}{}",
                p.tc,
                l.f,
                l.score,
                (l.score - med) / sigma,
                l.contrast,
                l.sides.0,
                l.sides.1,
                l.level,
                if l.is_line() { " line" } else { "" }
            );
        }
        if l.is_line() {
            lines.push(l);
        }
    }
    lines.sort_by(|a, b| a.f.total_cmp(&b.f));
    comb.lines = lines;
    Some(comb)
}

/// Brightest level line within the frequency span of `q`'s steep part,
/// relative to `q` itself, both averaged over the steep columns (z above the
/// map's baseline). About 0 for a Doppler line; 1 or more for a tone at a
/// constant frequency that `q` crosses (its brightness is all on the level
/// line, while `q` collects it only near the crossing).
fn stationary_ratio(map: &ScoreMap, q: &Flyby, c: f64) -> f64 {
    let spec = &map.spec;
    let max_slope = q.slope_at_cpa(c).abs();
    let h = 0.5 * spec.dt;
    let steep: Vec<usize> = (0..spec.cols)
        .filter(|&col| {
            let t = spec.col_time(col);
            ((q.freq_at(t + h, c) - q.freq_at(t - h, c)) / (2.0 * h)).abs() >= 0.25 * max_slope
        })
        .collect();
    if steep.len() < 4 {
        return 0.0;
    }
    let fs: Vec<f64> = steep.iter().map(|&col| q.freq_at(spec.col_time(col), c)).collect();
    let curve = steep.iter().zip(&fs).map(|(&col, &f)| spec.sample(col, f).unwrap_or(0.0) as f64).sum::<f64>()
        / steep.len() as f64
        - map.baseline;
    let (lo, hi) = fs.iter().fold((f64::INFINITY, 0.0f64), |(a, b), f| (a.min(*f), b.max(*f)));
    let r0 = (((lo - spec.f_min) / spec.df).floor().max(0.0)) as usize;
    let r1 = (((hi - spec.f_min) / spec.df).ceil() as usize).min(spec.rows.saturating_sub(1));
    let level = (r0..=r1)
        .map(|r| steep.iter().map(|&col| spec.column(col)[r] as f64).sum::<f64>() / steep.len() as f64 - map.baseline)
        .fold(0.0f64, f64::max);
    if curve <= 1e-6 { f64::INFINITY } else { level / curve }
}

/// The fundamental `f1` of lines `(f, weight)` and each line's multiple of
/// it (`None`: not a multiple; such lines are kept as measured ratios by the
/// caller). Candidates are `f / n` for every line `f` and `n` up to
/// [`MAX_MULTIPLE`], at least [`MIN_FUNDAMENTAL`], refined (least squares)
/// on the lines they explain. The best harmonic-sieve score wins (see
/// [`COMPLETENESS_WEIGHT`]), then one that explains line `primary`, then the
/// most weight, and of equals the largest. A candidate must explain two
/// lines and score above the primary alone; otherwise the primary is the
/// fundamental, with the lines at its integer multiples. `tol(f)`: how far
/// (Hz) a line at `f` may lie from its multiple.
pub fn fundamental(lines: &[(f64, f64)], primary: usize, tol: impl Fn(f64) -> f64) -> (f64, Vec<Option<u32>>) {
    let assign = |f1: f64| -> Vec<Option<u32>> {
        lines
            .iter()
            .map(|&(f, _)| {
                let k = (f / f1).round();
                ((1.0..=MAX_MULTIPLE as f64).contains(&k) && (f - k * f1).abs() <= tol(f)).then_some(k as u32)
            })
            .collect()
    };
    // weighted least squares f1 over the assigned lines
    let refine = |m: &[Option<u32>]| -> f64 {
        let (mut num, mut den) = (0.0, 0.0);
        for (&(f, w), k) in lines.iter().zip(m) {
            if let Some(k) = k {
                let (k, w) = (*k as f64, w.max(1e-3));
                num += w * k * f;
                den += w * k * k;
            }
        }
        num / den
    };
    let count = |m: &[Option<u32>]| m.iter().flatten().count();
    let sieve = |m: &[Option<u32>]| -> f64 {
        let n = count(m);
        let top = m.iter().flatten().copied().max().unwrap_or(1).max(1);
        n as f64 + COMPLETENESS_WEIGHT * (n as f64 / top as f64).ln()
    };
    struct Candidate {
        score: f64,
        with_primary: bool,
        weight: f64,
        f1: f64,
        m: Vec<Option<u32>>,
    }
    let mut best: Option<Candidate> = None;
    for &(f, _) in lines {
        for n in 1..=MAX_MULTIPLE {
            let guess = f / n as f64;
            if guess < MIN_FUNDAMENTAL {
                break;
            }
            let mut m = assign(guess);
            let mut f1 = refine(&m);
            let m2 = assign(f1);
            if count(&m2) >= count(&m) {
                m = m2;
                f1 = refine(&m);
            }
            // every line explained lies at its multiple of the refined f1
            for _ in 0..4 {
                let off: Vec<bool> =
                    lines.iter().zip(&m).map(|(l, k)| k.is_some_and(|k| (l.0 - k as f64 * f1).abs() > tol(l.0))).collect();
                if !off.contains(&true) {
                    break;
                }
                for (k, o) in m.iter_mut().zip(off) {
                    if o {
                        *k = None;
                    }
                }
                f1 = refine(&m);
            }
            if !(f1 >= MIN_FUNDAMENTAL) || count(&m) < 2 || lines.iter().zip(&m).any(|(l, k)| k.is_some_and(|k| (l.0 - k as f64 * f1).abs() > tol(l.0))) {
                continue;
            }
            let score = sieve(&m);
            let with_primary = m[primary].is_some();
            let weight: f64 = lines.iter().zip(&m).filter(|(_, k)| k.is_some()).map(|(l, _)| l.1).sum();
            let better = match &best {
                None => true,
                Some(b) => {
                    let by_score = if score > b.score + 1e-9 {
                        std::cmp::Ordering::Greater
                    } else if score < b.score - 1e-9 {
                        std::cmp::Ordering::Less
                    } else {
                        std::cmp::Ordering::Equal
                    };
                    by_score.then(with_primary.cmp(&b.with_primary)).then_with(|| {
                        if weight > b.weight * (1.0 + 1e-9) {
                            std::cmp::Ordering::Greater
                        } else if weight < b.weight * (1.0 - 1e-9) {
                            std::cmp::Ordering::Less
                        } else {
                            f1.total_cmp(&(b.f1 * (1.0 + 1e-6)))
                        }
                    }) == std::cmp::Ordering::Greater
                }
            };
            if better {
                best = Some(Candidate { score, with_primary, weight, f1, m });
            }
        }
    }
    match best {
        // the primary alone scores 1
        Some(b) if b.score > 1.0 + 1e-9 => (b.f1, b.m),
        _ => {
            // the primary as heard, and the lines at its integer multiples
            let f1 = lines[primary].0;
            let mut m = assign(f1);
            m[primary] = Some(1);
            (f1, m)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DEFAULT_C;
    use crate::synth::{SynthFlyby, synthesize};

    /// A machine tone at a constant frequency, on only around the pass (so
    /// whitening keeps it), is crossed by curves of the fly-by's shape but is
    /// no line of it.
    #[test]
    fn crossed_stationary_tone_is_no_line() {
        let p = Flyby { f0: 120.0, v: 60.0, d: 800.0, tc: 100.0 };
        let mut audio = synthesize(8000.0, 200.0, &[SynthFlyby { flyby: p, harmonics: vec![1.0], amplitude: 0.03 }], 0.3, &[], 5);
        let tone = 150.0;
        for (i, x) in audio.samples.iter_mut().enumerate() {
            let t = i as f64 / audio.sample_rate;
            if (70.0..130.0).contains(&t) {
                *x += (0.05 * (std::f64::consts::TAU * tone * t).sin()) as f32;
            }
        }
        let cfg = FitConfig { n_fft: 8192, dt: 0.25, ..FitConfig::default() };
        let region = Region { t_start: 60.0, t_end: 140.0, f_min: 40.0, f_max: 400.0 };
        let map = ScoreMap::new(fit::region_spectrogram(&audio, &region, &cfg));
        let on_line = stationary_ratio(&map, &p, DEFAULT_C);
        let crossing = stationary_ratio(&map, &Flyby { f0: tone, ..p }, DEFAULT_C);
        assert!(on_line < 0.5 && crossing > MAX_STATIONARY, "line {on_line}, crossing the tone {crossing}");
        let comb = scan_lines(&audio, &p, (10.0, 1000.0), DEFAULT_C).expect("scan");
        assert!(comb.lines.iter().any(|l| (l.f - p.f0).abs() < 1.0), "{:?}", comb.lines);
        assert!(!comb.lines.iter().any(|l| (l.f / tone - 1.0).abs() < 0.05), "{:?}", comb.lines);
    }

    /// A single line in noise, for many noise draws and geometries: the scan
    /// finds that line and nothing else (no comb of noise peaks).
    #[test]
    fn scan_adds_no_noise_lines() {
        let mut extra = Vec::new();
        for seed in 1..=12u64 {
            let p = Flyby {
                f0: 90.0 + 23.0 * seed as f64,
                v: 40.0 + 7.0 * (seed % 5) as f64,
                d: 500.0 + 230.0 * (seed % 7) as f64,
                tc: 100.0,
            };
            let audio = synthesize(8000.0, 200.0, &[SynthFlyby { flyby: p, harmonics: vec![1.0], amplitude: 0.03 }], 0.3, &[], seed);
            let comb = scan_lines(&audio, &p, (10.0, 1000.0), DEFAULT_C).expect("scan");
            assert!(comb.lines.iter().any(|l| (l.f - p.f0).abs() < 1.0), "seed {seed}: {:?}", comb.lines);
            extra.extend(comb.lines.iter().filter(|l| (l.f - p.f0).abs() >= 1.0).map(|l| (seed, *l)));
        }
        assert!(extra.is_empty(), "{extra:?}");
    }

    /// The scan's tolerance at 8 kHz (about 1 Hz bins).
    fn tol(f: f64) -> f64 {
        (TOL_REL * f).max(TOL_BINS * 0.98)
    }

    /// Every line given a multiple lies within the tolerance of it.
    fn placed_within_tol(lines: &[(f64, f64)], f1: f64, m: &[Option<u32>]) -> bool {
        lines.iter().zip(m).all(|(l, k)| k.is_none_or(|k| (l.0 - k as f64 * f1).abs() <= tol(l.0)))
    }

    #[test]
    fn unrelated_line_gets_no_common_sub_fundamental() {
        // 105 x 1, 2 and an unrelated 290 Hz: not 26.34 x 4, 8, 11, which
        // would move the lines by up to 0.7 Hz
        let lines = [(105.0, 9.0), (210.0, 6.0), (290.0, 5.0)];
        let (f1, m) = fundamental(&lines, 0, tol);
        assert!((f1 - 105.0).abs() < 0.01, "{f1}");
        assert_eq!(m, vec![Some(1), Some(2), None]);
        let lines = [(88.0, 9.0), (176.0, 6.0), (241.3, 5.0)];
        let (f1, m) = fundamental(&lines, 0, tol);
        assert!((f1 - 88.0).abs() < 0.01, "{f1}");
        assert_eq!(m, vec![Some(1), Some(2), None]);
        // two lines at 6 / 5 (Oosterschelde at 3500 s): no 20.7 Hz x 5, 6
        let lines = [(103.4, 9.0), (124.5, 6.0)];
        assert_eq!(fundamental(&lines, 0, tol), (103.4, vec![Some(1), None]));
    }

    #[test]
    fn helicopter_rotor_is_the_fundamental() {
        // a 14.16 Hz main rotor heard at scattered multiples up to its 30th
        let ks = [3, 9, 12, 15, 19, 22, 25, 27, 30];
        let lines: Vec<(f64, f64)> = ks.iter().map(|&k| (14.16 * k as f64, 5.0)).collect();
        let (f1, m) = fundamental(&lines, 1, tol);
        assert!((f1 - 14.16).abs() < 0.01, "{f1}");
        assert_eq!(m, ks.iter().map(|&k| Some(k)).collect::<Vec<_>>());
    }

    #[test]
    fn twin_lines_are_placed_where_measured() {
        // Oosterschelde at 5781 s (a DA62): two propellers at 105.7 and
        // 119.2 Hz and lines between them. Whatever fundamental explains
        // them, it places them within the tolerance of where they were
        // measured, and the lines it does not explain are left to the caller.
        let fs = [105.7, 119.2, 211.5, 238.4, 317.5, 344.1, 357.7, 423.1, 449.8, 476.6];
        let lines: Vec<(f64, f64)> = fs.iter().map(|&f| (f, 10.0)).collect();
        let (f1, m) = fundamental(&lines, 0, tol);
        assert!(placed_within_tol(&lines, f1, &m), "{f1} {m:?}");
        assert!(m[0].is_some(), "{f1} {m:?}");
        // and random sets of lines, whatever they get
        let mut x = 12345u64;
        for _ in 0..200 {
            let lines: Vec<(f64, f64)> = (0..5)
                .map(|_| {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    (60.0 + (x >> 33) as f64 / (1u64 << 31) as f64 * 600.0, 5.0)
                })
                .collect();
            let (f1, m) = fundamental(&lines, 0, tol);
            assert!(placed_within_tol(&lines, f1, &m), "{lines:?}: {f1} {m:?}");
        }
    }

    /// A close pass (50 m at 36 m/s) is steep for under 2 s either side:
    /// scanned at a finer step, both its lines are judged on both sides.
    #[test]
    fn close_pass_lines_are_found() {
        let p = Flyby { f0: 150.0, v: 36.0, d: 50.0, tc: 100.0 };
        let audio = synthesize(8000.0, 200.0, &[SynthFlyby { flyby: p, harmonics: vec![1.0, 0.8], amplitude: 0.05 }], 0.3, &[], 9);
        let comb = scan_lines(&audio, &p, (10.0, 1000.0), DEFAULT_C).expect("scan");
        for f in [150.0, 300.0] {
            assert!(comb.lines.iter().any(|l| (l.f - f).abs() < 1.5), "{f}: {:?}", comb.lines);
        }
    }

    #[test]
    fn fundamental_below_the_lines() {
        // Vlissingen det 1: 3rd, 4th and 7th multiples of an unheard 87.5 Hz
        let lines = [(262.6, 3.0), (349.6, 2.0), (612.3, 10.0)];
        let (f1, m) = fundamental(&lines, 2, tol);
        assert!((f1 - 87.5).abs() < 0.3, "{f1}");
        assert_eq!(m, vec![Some(3), Some(4), Some(7)]);
    }

    #[test]
    fn largest_fundamental_of_a_full_comb() {
        let lines: Vec<(f64, f64)> = (1..=5).map(|k| (77.8 * k as f64, 1.0)).collect();
        let (f1, m) = fundamental(&lines, 1, tol);
        assert!((f1 - 77.8).abs() < 0.1, "{f1}");
        assert_eq!(m, (1..=5).map(Some).collect::<Vec<_>>());
        // the even multiples alone: 155.6 explains them, not 77.8
        let even = [(155.6, 1.0), (311.2, 1.0), (466.8, 1.0)];
        let (f1, m) = fundamental(&even, 0, tol);
        assert!((f1 - 155.6).abs() < 0.1, "{f1}");
        assert_eq!(m, vec![Some(1), Some(2), Some(3)]);
    }

    #[test]
    fn comb_beside_the_primary() {
        // a propeller comb at 93.7 Hz and a stronger line at no integer
        // multiple of it (Amer's PC-7): the comb's fundamental, the primary
        // left out
        let lines = [(671.3, 24.0), (93.7, 34.0), (187.6, 7.0), (281.3, 5.0)];
        let (f1, m) = fundamental(&lines, 0, tol);
        assert!((f1 - 93.75).abs() < 0.2, "{f1}");
        assert_eq!(m, vec![None, Some(1), Some(2), Some(3)]);
    }

    #[test]
    fn unrelated_lines_keep_the_primary() {
        // one line, or a second line at no integer ratio (a twin's other propeller)
        let (f1, m) = fundamental(&[(231.0, 5.0)], 0, tol);
        assert_eq!((f1, m), (231.0, vec![Some(1)]));
        let (f1, m) = fundamental(&[(105.0, 5.0), (117.6, 3.0)], 0, tol);
        assert_eq!((f1, m), (105.0, vec![Some(1), None]));
    }
}
