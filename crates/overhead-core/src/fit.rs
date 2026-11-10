//! Fitting the Doppler model to a spectrogram region or to traced points.
//!
//! Pipeline for a region:
//! 1. Convert the whitened spectrogram to a clipped z-score map.
//! 2. Track the brightest non-increasing ridge with dynamic programming.
//! 3. Initial guess from the ridge's asymptotes and CPA slope.
//! 4. Robust least squares of the model against the ridge points.
//! 5. Refine by maximising mean brightness under the model curve.
//! 6. Profile `v`, `d` and `tc` to get plausible ranges.

use serde::{Deserialize, Serialize};

use crate::audio::Audio;
use crate::model::Flyby;
use crate::spectrogram::{self, SpecRequest, Spectrogram, median};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FitConfig {
    /// Speed of sound (m/s).
    pub c: f64,
    /// Frequency multipliers of the fitted line that also count when scoring,
    /// e.g. `[1.0, 2.0]` if the second harmonic is visible too.
    pub harmonics: Vec<f64>,
    /// Steepest frequency drop the ridge tracker allows (Hz/s).
    pub max_slope_hz_s: f64,
    pub v_bounds: (f64, f64),
    pub d_bounds: (f64, f64),
    /// A parameter value is "plausible" when the profiled score keeps this
    /// fraction of the best score's height above the noise baseline.
    pub range_fraction: f64,
    /// Column spacing of the analysis spectrogram (s).
    pub dt: f64,
    pub n_fft: usize,
}

impl Default for FitConfig {
    fn default() -> Self {
        Self {
            c: crate::model::DEFAULT_C,
            harmonics: vec![1.0],
            max_slope_hz_s: 25.0,
            v_bounds: (5.0, 300.0),
            d_bounds: (30.0, 30_000.0),
            range_fraction: 0.85,
            dt: 0.25,
            n_fft: 8192,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub t_start: f64,
    pub t_end: f64,
    pub f_min: f64,
    pub f_max: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RidgePoint {
    pub t: f64,
    pub f: f64,
    pub weight: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ranges {
    pub v: (f64, f64),
    pub d: (f64, f64),
    pub tc: (f64, f64),
    /// `[v, d, tc]`: the range still reached the edge of the widest scan, so
    /// the true bound may lie further out.
    pub clipped: [bool; 3],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FitResult {
    pub flyby: Flyby,
    /// Mean z-score under the curve (higher = brighter match).
    pub score: f64,
    /// Expected score of a random curve (noise floor).
    pub baseline: f64,
    /// Sigmas above frequency-shifted copies; below ~6 the "fit" is likely noise.
    pub contrast: f64,
    pub ranges: Ranges,
    /// RMS frequency residual against the tracked ridge (Hz).
    pub rms_hz: f64,
    pub ridge: Vec<RidgePoint>,
}

/// Clipped z-score map used for scoring, blurred slightly in frequency.
pub struct ScoreMap {
    pub spec: Spectrogram,
    /// Expected score of a curve through pure noise.
    pub baseline: f64,
    /// Cells set to the baseline because a level tone ran through them
    /// ([`region_score_map`]); 0 for [`ScoreMap::new`].
    pub level_cells: usize,
}

/// Robust noise sigma of a whitened map (dB) from the median absolute
/// deviation. Digital silence whitens to exact zeros; it must not shrink it.
fn noise_sigma(spec: &Spectrogram) -> f32 {
    let mut abs: Vec<f32> =
        spec.db.iter().step_by((spec.db.len() / 100_000).max(1)).map(|v| v.abs()).filter(|v| *v > 1e-6).collect();
    (median(&mut abs) * 1.4826).max(1e-3)
}

/// Level tones of a whitened map (not clipped) as `(column, frequency Hz)`
/// cells, from its peaks of at least `peak_min`: see [`region_score_map`].
///
/// Spectral peaks are linked from column to column into lines (a line may
/// wander: each peak within `LEVEL_LINK_HZ` of the mean of the line's last
/// few). A stretch of a line is level where, over a window of `LEVEL_WIN_S`
/// around it, the line's frequency changes by less than
/// `LEVEL_MAX_REL_SLOPE` of itself per second
/// (the median of the window's last third against that of its first third,
/// so a tone that wobbles by a few tenths of a hertz is still level, and a
/// line that falls steadily is not).
fn level_tones(spec: &Spectrogram, peak_min: f32) -> Vec<(usize, f64)> {
    struct Line {
        /// `(column, frequency)`, columns rising.
        pts: Vec<(usize, f64)>,
        /// Mean frequency of the last `LEVEL_RECENT` points.
        recent: f64,
    }
    let (rows, cols) = (spec.rows, spec.cols);
    let (df, dt, f_min) = (spec.df, spec.dt, spec.f_min);
    let half = ((0.5 * LEVEL_WIN_S / dt).round() as usize).max(2);
    if rows < 5 || cols < 2 * half + 1 {
        return Vec::new();
    }
    let link = LEVEL_LINK_HZ.max(LEVEL_LINK_BINS * df);
    let gap_cols = ((LEVEL_GAP_S / dt).round() as usize).max(1);
    let mut active: Vec<Line> = Vec::new();
    let mut done: Vec<Line> = Vec::new();
    let mut peaks: Vec<(f64, bool)> = Vec::new();
    for col in 0..cols {
        let column = spec.column(col);
        peaks.clear();
        for r in 0..rows {
            let v = column[r];
            let below = if r > 0 { column[r - 1] } else { f32::NEG_INFINITY };
            let above = if r + 1 < rows { column[r + 1] } else { f32::NEG_INFINITY };
            if v >= peak_min && v >= below && v > above {
                let offset = if below.is_finite() && above.is_finite() {
                    let (a, b, c) = (below as f64, v as f64, above as f64);
                    let denom = a - 2.0 * b + c;
                    if denom.abs() > 1e-9 { (0.5 * (a - c) / denom).clamp(-0.5, 0.5) } else { 0.0 }
                } else {
                    0.0
                };
                peaks.push((f_min + (r as f64 + offset) * df, false));
            }
        }
        let mut k = 0;
        while k < active.len() {
            let line = &mut active[k];
            let nearest = peaks
                .iter_mut()
                .filter(|p| !p.1 && (p.0 - line.recent).abs() <= link)
                .min_by(|a, b| (a.0 - line.recent).abs().total_cmp(&(b.0 - line.recent).abs()));
            if let Some(p) = nearest {
                p.1 = true;
                line.pts.push((col, p.0));
                let tail = &line.pts[line.pts.len().saturating_sub(LEVEL_RECENT)..];
                line.recent = tail.iter().map(|q| q.1).sum::<f64>() / tail.len() as f64;
                k += 1;
            } else if col - line.pts[line.pts.len() - 1].0 > gap_cols {
                done.push(active.swap_remove(k));
            } else {
                k += 1;
            }
        }
        for p in peaks.iter().filter(|p| !p.1) {
            active.push(Line { pts: vec![(col, p.0)], recent: p.0 });
        }
    }
    done.extend(active);
    let debug = std::env::var_os("OVERHEAD_DEBUG_LEVEL").is_some();
    let mut out: Vec<(usize, f64)> = Vec::new();
    // level stretches: (frequency, first column, last column)
    let mut runs: Vec<(f64, usize, usize)> = Vec::new();
    let mut buf: Vec<f64> = Vec::new();
    let median_of = |v: &mut Vec<f64>| -> f64 {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    // the window's first and last thirds are compared; their centres lie this far apart
    let third = (2 * half + 1) / 3;
    let lever_s = (2 * half + 1 - third) as f64 * dt;
    for line in &done {
        let (first, last) = (line.pts[0].0, line.pts[line.pts.len() - 1].0);
        if last - first < 2 * half {
            continue;
        }
        let mut level: Vec<Option<f64>> = vec![None; last - first + 1];
        // the window as indices into `pts` (both ends move forward with the centre)
        let (mut lo, mut hi) = (0usize, 0usize);
        // (a level window marks all its columns, so the centres need not be
        // every column: every eighth of a window, and the last one)
        let stride = (half / 4).max(1);
        let centres = (first + half..=last - half).step_by(stride).chain(std::iter::once(last - half));
        for centre in centres {
            let (w0, w1) = (centre - half, centre + half);
            while line.pts[lo].0 < w0 {
                lo += 1;
            }
            while hi < line.pts.len() && line.pts[hi].0 <= w1 {
                hi += 1;
            }
            let window = &line.pts[lo..hi];
            if (window.len() as f64) < LEVEL_MIN_FILL * (2 * half + 1) as f64 {
                continue;
            }
            buf.clear();
            buf.extend(window.iter().filter(|q| q.0 < w0 + third).map(|q| q.1));
            if buf.len() < 3 {
                continue;
            }
            let early = median_of(&mut buf);
            buf.clear();
            buf.extend(window.iter().filter(|q| q.0 > w1 - third).map(|q| q.1));
            if buf.len() < 3 {
                continue;
            }
            let late = median_of(&mut buf);
            if (late - early).abs() > LEVEL_MAX_REL_SLOPE * 0.5 * (late + early) * lever_s {
                continue;
            }
            buf.clear();
            buf.extend(window.iter().map(|q| q.1));
            let f = median_of(&mut buf);
            for slot in &mut level[w0 - first..=w1 - first] {
                slot.get_or_insert(f);
            }
        }
        if debug {
            let n = level.iter().filter(|l| l.is_some()).count();
            if n > 0 {
                let f = level.iter().flatten().sum::<f64>() / n as f64;
                eprintln!("[level] {f:.2} Hz: {:.1} s level of a line {:.1}..{:.1} s", n as f64 * dt, spec.col_time(first), spec.col_time(last));
            }
        }
        // (runs of level columns, for the pieces that belong to them)
        let mut i = 0;
        while i < level.len() {
            let Some(f) = level[i] else {
                i += 1;
                continue;
            };
            let start = i;
            while i < level.len() && level[i].is_some() {
                i += 1;
            }
            runs.push((f, first + start, first + i - 1));
        }
        out.extend(level.iter().enumerate().filter_map(|(i, l)| l.map(|f| (first + i, f))));
    }
    // A tone is often tracked in pieces (it flickers, or wobbles past the
    // linking tolerance), and a piece shorter than the window cannot be
    // judged on its own: a piece that stays at a level tone's frequency and
    // begins or ends within a window's length of it is part of that tone.
    let reach = 2 * half;
    for line in &done {
        let (first, last) = (line.pts[0].0, line.pts[line.pts.len() - 1].0);
        let tone = runs.iter().find(|&&(f, a, b)| {
            first <= b + reach && last + reach >= a && line.pts.iter().all(|q| (q.1 - f).abs() <= 1.5 * link)
        });
        if let Some(&(f, ..)) = tone {
            out.extend((first..=last).map(|col| (col, f)));
        }
    }
    out
}

const Z_MAX: f32 = 6.0;
/// Level tones ([`level_tones`]): peaks of at least `LEVEL_PEAK_Z` noise
/// sigmas, linked within `LEVEL_LINK_HZ` (or `LEVEL_LINK_BINS` bins, if
/// wider) of the mean of the line's last `LEVEL_RECENT` points, over gaps of
/// at most `LEVEL_GAP_S`; level where the line changes by less than
/// `LEVEL_MAX_REL_SLOPE` of its frequency per second (0.04 Hz/s at 100 Hz)
/// over a window of `LEVEL_WIN_S` that has a peak in at least
/// `LEVEL_MIN_FILL` of its columns. The limit is relative, as the Doppler
/// shift is: every line of one fly-by then leaves the tones' reach at the
/// same time, the fundamental with its harmonics.
const LEVEL_PEAK_Z: f32 = 2.0;
const LEVEL_LINK_HZ: f64 = 0.75;
const LEVEL_LINK_BINS: f64 = 0.6;
const LEVEL_RECENT: usize = 8;
const LEVEL_GAP_S: f64 = 2.0;
const LEVEL_WIN_S: f64 = 20.0;
const LEVEL_MAX_REL_SLOPE: f64 = 4e-4;
const LEVEL_MIN_FILL: f64 = 0.5;
/// Fraction of the largest peak offsets ignored by `transition_quality`.
const RMS_TRIM: f64 = 0.15;
/// Cost (in z units) of a one-bin frequency rise in the ridge tracker.
const RISE_PENALTY: f64 = 1.0;

impl ScoreMap {
    pub fn new(spec: Spectrogram) -> Self {
        Self::build(spec, &[])
    }

    /// `tones`: `(column, frequency)` cells of level tones, set to the baseline.
    fn build(mut spec: Spectrogram, tones: &[(usize, f64)]) -> Self {
        if !spec.whitened {
            spectrogram::whiten(&mut spec);
        }
        let sigma = noise_sigma(&spec);
        for v in spec.db.iter_mut() {
            *v /= sigma;
        }
        for v in spec.db.iter_mut() {
            *v = v.clamp(0.0, Z_MAX);
        }
        let rows = spec.rows;
        let mut tmp = vec![0.0f32; rows];
        for col in spec.db.chunks_mut(rows) {
            for r in 0..rows {
                let a = col[r.saturating_sub(1)];
                let b = col[(r + 1).min(rows - 1)];
                tmp[r] = 0.25 * a + 0.5 * col[r] + 0.25 * b;
            }
            col.copy_from_slice(&tmp);
        }
        let mut masked: Vec<bool> = Vec::new();
        let mut level_cells = 0usize;
        if !tones.is_empty() {
            masked = vec![false; spec.db.len()];
            // the tone's wobble, its main lobe and the blur
            let half = (LEVEL_LINK_HZ.max(LEVEL_LINK_BINS * spec.df) / spec.df).ceil() as usize + 2;
            for &(col, f) in tones {
                let centre = ((f - spec.f_min) / spec.df).round().max(0.0) as usize;
                let (r0, r1) = (centre.saturating_sub(half), (centre + half).min(rows - 1));
                masked[col * rows + r0..=col * rows + r1].fill(true);
            }
            level_cells = masked.iter().filter(|m| **m).count();
        }
        let kept = spec.db.len() - level_cells;
        let baseline = if level_cells == 0 || kept == 0 {
            spec.db.iter().map(|v| *v as f64).sum::<f64>() / spec.db.len().max(1) as f64
        } else {
            // a masked cell says nothing either way: it holds the baseline of the rest
            let baseline = spec.db.iter().zip(&masked).filter(|(_, m)| !**m).map(|(v, _)| *v as f64).sum::<f64>() / kept as f64;
            for (v, m) in spec.db.iter_mut().zip(&masked) {
                if *m {
                    *v = baseline as f32;
                }
            }
            baseline
        };
        Self { spec, baseline, level_cells: if kept == 0 { 0 } else { level_cells } }
    }

    /// Mean brightness under the model curve (and its harmonics).
    pub fn score(&self, p: &Flyby, c: f64, harmonics: &[f64]) -> f64 {
        self.score_strided(p, c, harmonics, 1)
    }

    /// `score` using every `stride`-th column, for cheap coarse searches.
    pub fn score_strided(&self, p: &Flyby, c: f64, harmonics: &[f64], stride: usize) -> f64 {
        let spec = &self.spec;
        let mut total = 0.0;
        let mut n = 0usize;
        for col in (0..spec.cols).step_by(stride.max(1)) {
            n += 1;
            let f = p.freq_at(spec.col_time(col), c);
            let mut acc = 0.0;
            for h in harmonics {
                acc += spec.sample(col, f * h).unwrap_or(0.0) as f64;
            }
            total += acc / harmonics.len() as f64;
        }
        total / n.max(1) as f64
    }
}

/// Compute the analysis spectrogram for a region. Whitening medians come from a
/// wider time context so a long flat tail inside the box is not removed.
pub fn region_spectrogram(audio: &Audio, region: &Region, cfg: &FitConfig) -> Spectrogram {
    let (mut wide, first, last) = wide_spectrogram(audio, region, cfg);
    crop_columns(&mut wide, first, last);
    wide
}

/// The whitened spectrogram of the region with its time context (as long as
/// the region on either side, at least 60 s), and the region's columns in it
/// (`first..last`).
fn wide_spectrogram(audio: &Audio, region: &Region, cfg: &FitConfig) -> (Spectrogram, usize, usize) {
    let len = region.t_end - region.t_start;
    let pad = len.max(60.0);
    let t0 = (region.t_start - pad).max(0.0);
    let t1 = (region.t_end + pad).min(audio.duration());
    let cols = ((t1 - t0) / cfg.dt).round().max(1.0) as usize;
    let wide = spectrogram::compute(
        audio,
        &SpecRequest {
            t_start: t0,
            t_end: t1,
            f_min: region.f_min,
            f_max: region.f_max,
            columns: cols,
            n_fft: cfg.n_fft,
            whiten: true,
        },
    );
    let first = (((region.t_start - t0) / wide.dt).floor().max(0.0) as usize).min(wide.cols - 1);
    let last = (((region.t_end - t0) / wide.dt).ceil() as usize).clamp(first + 1, wide.cols);
    (wide, first, last)
}

fn crop_columns(spec: &mut Spectrogram, first: usize, last: usize) {
    spec.db = spec.db[first * spec.rows..last * spec.rows].to_vec();
    spec.t_start += first as f64 * spec.dt;
    spec.cols = last - first;
}

/// The map the ADS-B matcher scores the candidates of a fly-by in `region`
/// on: [`region_spectrogram`] as a [`ScoreMap`] with the level tones set to
/// the noise baseline. A level tone is a spectral line whose frequency
/// changes by less than [`LEVEL_MAX_REL_SLOPE`] of itself per second for
/// [`LEVEL_WIN_S`] or longer: mains hum, a fan, a compressor. A curve is
/// scored by the brightness under it, so with such a tone in the map the
/// flat end of a Doppler curve laid along it counts as if the aircraft
/// were heard there: the stage-1 fit that every candidate is measured
/// against looks brighter than the fly-by's line is, and any slow, distant
/// pass, whose curve is almost level, scores on it. The line of a fly-by
/// moves through the frequencies, so only its far ends, where it changes
/// that slowly, can go with the tones; those ends could not be told from a
/// tone anyway. The tones are looked for in the region's whole time
/// context, so a tone that only reaches a few seconds into the region is
/// still known for what it is. `OVERHEAD_KEEP_LEVEL_TONES=1` leaves the
/// tones in (for comparison).
pub fn region_score_map(audio: &Audio, region: &Region, cfg: &FitConfig) -> ScoreMap {
    let (mut wide, first, last) = wide_spectrogram(audio, region, cfg);
    let tones: Vec<(usize, f64)> = if keep_level_tones() {
        Vec::new()
    } else {
        level_tones(&wide, LEVEL_PEAK_Z * noise_sigma(&wide))
            .into_iter()
            .filter(|(col, _)| (first..last).contains(col))
            .map(|(col, f)| (col - first, f))
            .collect()
    };
    crop_columns(&mut wide, first, last);
    ScoreMap::build(wide, &tones)
}

/// `OVERHEAD_KEEP_LEVEL_TONES=1`: leave level tones in the maps (for comparison).
fn keep_level_tones() -> bool {
    std::env::var_os("OVERHEAD_KEEP_LEVEL_TONES").is_some()
}

/// Fit a fly-by inside a time/frequency box of the recording.
pub fn fit_region(audio: &Audio, region: &Region, cfg: &FitConfig) -> Option<FitResult> {
    let spec = region_spectrogram(audio, region, cfg);
    fit_spectrogram(spec, cfg)
}

pub fn fit_spectrogram(spec: Spectrogram, cfg: &FitConfig) -> Option<FitResult> {
    if spec.cols < 8 || spec.rows < 4 {
        return None;
    }
    let map = ScoreMap::new(spec);
    let ridge = track_ridge(&map.spec, cfg.max_slope_hz_s);
    let strong = strong_points(&ridge);
    let (flyby, score) = fit_seeded(&map, &strong, cfg)?;
    let ranges = profile_ranges(&map, &flyby, score, cfg);
    let rms_hz = rms_residual(&strong, &flyby, cfg.c);
    let contrast = contrast(&map, &flyby, cfg);
    Some(FitResult { flyby, score, baseline: map.baseline, contrast, ranges, rms_hz, ridge })
}

/// Fit starting from `seed` points that lie on (part of) one Doppler line:
/// robust least squares against the points, then refinement on brightness.
/// Returns the fly-by and its mean brightness score.
pub fn fit_seeded(map: &ScoreMap, seed: &[RidgePoint], cfg: &FitConfig) -> Option<(Flyby, f64)> {
    let guess = initial_guess(seed, cfg)?;
    let mut best = fit_from(map, seed, guess, cfg)?;
    // Seeds often cover only the bright steep part. Collect points along the
    // whole fitted curve (picking up faint tails) and refit, a few times.
    for _ in 0..3 {
        let guided = guided_points(map, &best.0, cfg.c);
        if guided.len() < 8 {
            break;
        }
        match fit_from(map, &guided, best.0, cfg) {
            Some(next) if next.1 > best.1 + 1e-6 => best = next,
            _ => break,
        }
    }
    Some(best)
}

/// Brightest point within ±4 bins of the model curve in every column, kept
/// where it stands above the noise.
pub fn guided_points(map: &ScoreMap, p: &Flyby, c: f64) -> Vec<RidgePoint> {
    const HALF: i64 = 4;
    let spec = &map.spec;
    let mut out = Vec::new();
    for col in 0..spec.cols {
        let t = spec.col_time(col);
        let centre = ((p.freq_at(t, c) - spec.f_min) / spec.df).round() as i64;
        let column = spec.column(col);
        let lo = (centre - HALF).max(1);
        let hi = (centre + HALF).min(spec.rows as i64 - 2);
        if lo > hi {
            continue;
        }
        let (r, v) = (lo..=hi)
            .map(|r| (r as usize, column[r as usize]))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .expect("non-empty range");
        if v < 1.5 {
            continue;
        }
        let (a, b, cc) = (column[r - 1] as f64, v as f64, column[r + 1] as f64);
        let denom = a - 2.0 * b + cc;
        let offset = if denom.abs() > 1e-9 { (0.5 * (a - cc) / denom).clamp(-0.5, 0.5) } else { 0.0 };
        out.push(RidgePoint { t, f: spec.f_min + (r as f64 + offset) * spec.df, weight: v as f64 });
    }
    out
}

/// How well the data follows the model where the curve is steep (|df/dt| ≥ 25%
/// of its CPA maximum), the part that distinguishes a fly-by from a level or
/// stair-stepping tone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TransitionQuality {
    /// Fraction of transition columns with a bright peak within ±3 bins of the model.
    pub coverage: f64,
    /// RMS offset of those peaks from the model, in bins.
    pub rms_bins: f64,
    /// Number of spectrogram columns in the transition.
    pub columns: usize,
    /// How many bins the line moves within one FFT window at CPA.
    pub smear_bins: f64,
    /// Coverage before and after the heard CPA. A fly-by leaves a trace on
    /// both sides; the abrupt edge of a machine tone only on one.
    pub coverage_sides: (f64, f64),
}

pub fn transition_quality(map: &ScoreMap, p: &Flyby, c: f64) -> TransitionQuality {
    let spec = &map.spec;
    let max_slope = p.slope_at_cpa(c).abs();
    // window length in seconds is n_fft / sample_rate = 1 / df
    let window = 1.0 / spec.df;
    let smear_bins = max_slope * window / spec.df;
    let half = 3 + (0.5 * smear_bins).ceil() as i64;
    let h = 0.5 * spec.dt;
    let (mut columns, mut hits) = (0usize, 0usize);
    let mut offsets: Vec<f64> = Vec::new();
    let mut side_cols = [0usize; 2];
    let mut side_hits = [0usize; 2];
    for col in 0..spec.cols {
        let t = spec.col_time(col);
        let slope = (p.freq_at(t + h, c) - p.freq_at(t - h, c)) / (2.0 * h);
        if slope.abs() < 0.25 * max_slope {
            continue;
        }
        columns += 1;
        let side = usize::from(t >= p.tc);
        side_cols[side] += 1;
        let pos = (p.freq_at(t, c) - spec.f_min) / spec.df;
        let centre = pos.round() as i64;
        let lo = (centre - half).max(1);
        let hi = (centre + half).min(spec.rows as i64 - 2);
        if lo > hi {
            continue;
        }
        let column = spec.column(col);
        let (r, v) = (lo..=hi)
            .map(|r| (r as usize, column[r as usize]))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .expect("non-empty range");
        if v < 2.0 {
            continue;
        }
        let (a, b, cc) = (column[r - 1] as f64, v as f64, column[r + 1] as f64);
        let denom = a - 2.0 * b + cc;
        let offset = if denom.abs() > 1e-9 { (0.5 * (a - cc) / denom).clamp(-0.5, 0.5) } else { 0.0 };
        hits += 1;
        side_hits[side] += 1;
        offsets.push((r as f64 + offset - pos).abs());
    }
    TransitionQuality {
        coverage: if columns > 0 { hits as f64 / columns as f64 } else { 0.0 },
        rms_bins: trimmed_rms(&mut offsets, RMS_TRIM),
        columns,
        smear_bins,
        coverage_sides: (
            side_hits[0] as f64 / side_cols[0].max(1) as f64,
            side_hits[1] as f64 / side_cols[1].max(1) as f64,
        ),
    }
}

/// RMS of `x` after dropping the largest `trim` fraction: a bird chirp or a
/// word crossing the line adds a few big offsets that should not decide.
pub fn trimmed_rms(x: &mut [f64], trim: f64) -> f64 {
    if x.is_empty() {
        return f64::INFINITY;
    }
    x.sort_by(f64::total_cmp);
    let keep = ((x.len() as f64 * (1.0 - trim)).ceil() as usize).clamp(1, x.len());
    (x[..keep].iter().map(|v| v * v).sum::<f64>() / keep as f64).sqrt()
}

/// Least squares against `seed` from `guess` (multi-start), then brightness refinement.
pub fn fit_from(map: &ScoreMap, seed: &[RidgePoint], guess: Flyby, cfg: &FitConfig) -> Option<(Flyby, f64)> {
    if !(guess.f0.is_finite() && guess.v.is_finite() && guess.d.is_finite() && guess.tc.is_finite()) {
        return None;
    }
    let sigma = 1.5 * map.spec.df;
    let neg_score = |x: &[f64]| -> f64 {
        let p = unpack(x);
        if !in_bounds(&p, cfg) {
            return 1e9;
        }
        -map.score(&p, cfg.c, &cfg.harmonics)
    };
    // Several starts (distance is the least certain guess). Each goes through
    // least squares on the seed and then brightness refinement; the brightest
    // result wins, so a misleading seed cannot pin the answer.
    let mut best: Option<(Flyby, f64)> = None;
    let mut consider = |candidate: (Flyby, f64)| {
        if candidate.1 > -1e8 && best.as_ref().is_none_or(|b| candidate.1 > b.1) {
            best = Some(candidate);
        }
    };
    // brightest points of a coarse grid around the guess, refined directly
    for start in coarse_starts(map, &guess, cfg, 3) {
        let (xr, fr) = nelder_mead(neg_score, &pack(&start), &[map.spec.df, 2.0, 0.15, 1.0], 600);
        consider((unpack(&xr), -fr));
    }
    for (dv, dd) in [(1.0, 1.0), (1.0, 0.5), (1.0, 2.0), (1.15, 1.0), (0.85, 1.0)] {
        let start = Flyby { v: guess.v * dv, d: guess.d * dd, ..guess };
        let ls = if seed.len() >= 4 {
            let (x, _) = nelder_mead(
                |x| ls_cost(seed, &unpack(x), cfg, sigma),
                &pack(&start),
                &[map.spec.df, 3.0, 0.3, 2.0],
                400,
            );
            x
        } else {
            pack(&start)
        };
        let ls_score = -neg_score(&ls);
        let (xr, fr) = nelder_mead(neg_score, &ls, &[map.spec.df, 2.0, 0.15, 1.0], 600);
        consider(if -fr > ls_score { (unpack(&xr), -fr) } else { (unpack(&ls), ls_score) });
    }
    best
}

/// The `n` brightest fly-bys on a coarse grid around `guess` (tc ±10 s,
/// v ×0.8–1.25, d ×0.5–2, f0 ±4 bins).
fn coarse_starts(map: &ScoreMap, guess: &Flyby, cfg: &FitConfig, n: usize) -> Vec<Flyby> {
    let stride = (map.spec.cols / 300).max(1);
    let mut scored = Vec::new();
    for dt in (-10..=10).map(|k| k as f64) {
        for fv in [0.8, 0.9, 1.0, 1.1, 1.25] {
            for fd in [0.5, 0.7, 1.0, 1.4, 2.0] {
                for kf in -4..=4 {
                    let p = Flyby {
                        f0: guess.f0 + kf as f64 * map.spec.df,
                        v: guess.v * fv,
                        d: guess.d * fd,
                        tc: guess.tc + dt,
                    };
                    if in_bounds(&p, cfg) {
                        scored.push((map.score_strided(&p, cfg.c, &cfg.harmonics, stride), p));
                    }
                }
            }
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().take(n).map(|(_, p)| p).collect()
}

/// How many standard deviations the curve's score stands above copies of the
/// same curve shifted in frequency by 4–20 bins. A real line is bright only
/// along its own track; noise or a broadband burst lights the copies too.
pub fn contrast(map: &ScoreMap, p: &Flyby, cfg: &FitConfig) -> f64 {
    contrast_stats(map, p, cfg).map_or(0.0, |s| s.contrast)
}

pub struct ContrastStats {
    pub contrast: f64,
    /// Spread of the shifted copies' scores, rescaled to the full-curve
    /// score of `ScoreMap::score` (which averages over all columns).
    pub sigma_full: f64,
}

pub fn contrast_stats(map: &ScoreMap, p: &Flyby, cfg: &FitConfig) -> Option<ContrastStats> {
    contrast_on(map, p, cfg, |_| true)
}

/// Contrast computed separately on the steep part before and after the heard
/// CPA (|df/dt| ≥ 25% of the maximum). A fly-by is coherent on both sides;
/// the abrupt edge of a machine tone or a one-sided chirp is not.
pub fn side_contrast(map: &ScoreMap, p: &Flyby, cfg: &FitConfig) -> (f64, f64) {
    let max_slope = p.slope_at_cpa(cfg.c).abs();
    let h = 0.5 * map.spec.dt;
    let steep = |t: f64| ((p.freq_at(t + h, cfg.c) - p.freq_at(t - h, cfg.c)) / (2.0 * h)).abs() >= 0.25 * max_slope;
    let before = contrast_on(map, p, cfg, |t| t < p.tc && steep(t)).map_or(0.0, |s| s.contrast);
    let after = contrast_on(map, p, cfg, |t| t >= p.tc && steep(t)).map_or(0.0, |s| s.contrast);
    (before, after)
}

/// `contrast_stats` restricted to columns whose centre time satisfies `keep`.
pub fn contrast_on(map: &ScoreMap, p: &Flyby, cfg: &FitConfig, keep: impl Fn(f64) -> bool) -> Option<ContrastStats> {
    contrast_impl(map, p, cfg, keep, true, false)
}

/// `absolute`: shift every harmonic by the same number of bins instead of
/// shifting the fundamental (which moves harmonic k by k times as much).
///
/// The lowest line of `cfg.harmonics` is taken as the fundamental: `p.f0`
/// itself need not be a line heard (a propeller heard at its 3rd, 4th and
/// 7th multiples), and copies shifted by 20 bins there would move the
/// higher lines far out of a map sized for the lines heard.
fn contrast_impl(
    map: &ScoreMap,
    p: &Flyby,
    cfg: &FitConfig,
    keep: impl Fn(f64) -> bool,
    exclude: bool,
    absolute: bool,
) -> Option<ContrastStats> {
    let base = cfg.harmonics.iter().copied().filter(|h| *h > 0.0).fold(f64::INFINITY, f64::min);
    if base.is_finite() && (base - 1.0).abs() > 1e-12 {
        let p = Flyby { f0: p.f0 * base, ..*p };
        let cfg = FitConfig { harmonics: cfg.harmonics.iter().map(|h| h / base).collect(), ..cfg.clone() };
        return contrast_impl(map, &p, &cfg, keep, exclude, absolute);
    }
    const MAX_SHIFT: i32 = 20;
    let spec = &map.spec;
    let h = cfg.harmonics.iter().copied().fold(1.0, f64::max);
    let lo_h = cfg.harmonics.iter().copied().fold(1.0, f64::min);
    let margin = MAX_SHIFT as f64 * spec.df;
    // Compare on the columns where the curve and every shifted copy (and
    // their harmonics) stay inside the analysed band, so the curve leaving the
    // box does not count as darkness.
    let cols: Vec<(usize, f64)> = (0..spec.cols)
        .filter_map(|col| {
            let t = spec.col_time(col);
            if !keep(t) {
                return None;
            }
            let f = p.freq_at(t, cfg.c);
            let ok = (f - margin) * lo_h >= spec.f_min && (f + margin) * h <= spec.f_max();
            ok.then_some((col, f))
        })
        .collect();
    if cols.len() < 10 {
        return None;
    }
    let score_at = |shift: f64| -> f64 {
        let mut total = 0.0;
        for &(col, f) in &cols {
            let mut acc = 0.0;
            for k in &cfg.harmonics {
                let at = if absolute { f * k + shift } else { (f + shift) * k };
                acc += spec.sample(col, at).unwrap_or(0.0) as f64;
            }
            total += acc / cfg.harmonics.len() as f64;
        }
        total / cols.len() as f64
    };
    let score = score_at(0.0);
    // The curve scaled by `ratio` on its steep part: where another line of
    // the same source would run. Flat tails cannot tell a scaled curve from
    // a constant shift, so only the steep part counts.
    let max_slope = p.slope_at_cpa(cfg.c).abs();
    let h_dt = 0.5 * spec.dt;
    let steep: Vec<(usize, f64)> = cols
        .iter()
        .copied()
        .filter(|&(col, _)| {
            let t = spec.col_time(col);
            ((p.freq_at(t + h_dt, cfg.c) - p.freq_at(t - h_dt, cfg.c)) / (2.0 * h_dt)).abs() >= 0.25 * max_slope
        })
        .collect();
    // Median over the steep columns, so a line must run along most of the
    // steep part: a curve that merely crosses the lines of a dense comb (a
    // mis-fit) is bright only at the crossings.
    let scaled_at = |ratio: f64| -> f64 {
        let mut vals: Vec<f64> = steep
            .iter()
            .map(|&(col, f)| {
                let acc: f64 = cfg.harmonics.iter().map(|k| spec.sample(col, f * ratio * k).unwrap_or(0.0) as f64).sum();
                acc / cfg.harmonics.len() as f64
            })
            .collect();
        median_f64(&mut vals)
    };
    // A line of the same shape within ±30 % of `shift` from the curve. Only a
    // curve that itself traces a ridge on its steep part may claim
    // neighbouring lines as its own source's: a mis-fit that cuts across a
    // dense rotor comb is bright where it crosses the lines and would
    // otherwise discount them all.
    let traces_ridge = std::cell::OnceCell::new();
    let same_source_line = |shift: f64, limit: f64| -> bool {
        steep.len() >= 10
            && *traces_ridge.get_or_init(|| {
                let q = transition_quality(map, p, cfg.c);
                q.rms_bins <= SAME_SOURCE_MAX_RMS_BINS
                    && (q.coverage >= SAME_SOURCE_MIN_COVERAGE || q.rms_bins <= SAME_SOURCE_FAINT_RMS_BINS)
            })
            && (-6..=6).any(|j| scaled_at(1.0 + shift * (1.0 + 0.05 * j as f64) / p.f0) > limit)
    };
    let copies: Vec<(i32, f64, f64)> = (4..=MAX_SHIFT)
        .flat_map(|k| [-1.0, 1.0].map(|sign| sign * k as f64 * spec.df))
        .map(|shift| ((shift.abs() / spec.df).round() as i32, shift, score_at(shift)))
        .collect();
    let (mean, sigma) = copy_noise(&copies, |s, l| exclude && same_source_line(s, l));
    let sigma = sigma.max(1e-3);
    let contrast = (score - mean) / sigma;
    contrast.is_finite().then_some(ContrastStats {
        contrast,
        sigma_full: sigma * cols.len() as f64 / spec.cols as f64,
    })
}

/// Copies shifted by at least this many bins may be left out of the noise
/// estimate when they are outliers; nearer copies overlap the line's own
/// skirts (window leakage, smear) and always count.
const FAR_COPY_BINS: i32 = 7;
/// How closely the curve must trace a ridge on its steep part (fraction of
/// columns with a peak near it, and their RMS offset in bins) before bright
/// copies may be put down to other lines of its source: peaks within
/// `SAME_SOURCE_MAX_RMS_BINS`, and either on `SAME_SOURCE_MIN_COVERAGE` of
/// the columns or, for a faint line with few peaks, within
/// `SAME_SOURCE_FAINT_RMS_BINS`. A dense rotor comb has a peak within a few
/// bins of any curve, so neither number alone tells. Clear real passes:
/// coverage 0.43-0.94 at RMS 0.22-0.44 bins (Jersey City S-76, Kampina
/// DR400, Edgemere comb lines); a faint synthetic line: 0.17 at 0.17;
/// mis-fits across the Edgemere comb: 0.13 at 0.50, or 0.5-0.9 at 0.81.
const SAME_SOURCE_MIN_COVERAGE: f64 = 0.3;
const SAME_SOURCE_FAINT_RMS_BINS: f64 = 0.3;
const SAME_SOURCE_MAX_RMS_BINS: f64 = 0.6;
/// A far copy brighter than this many robust sigmas above the far copies'
/// median runs along another line.
const COPY_OUTLIER_SIGMAS: f64 = 3.5;

/// Mean and standard deviation of the shifted copies' scores `(shift bins,
/// shift Hz, score)`, leaving out far copies that are much brighter than the
/// other far copies *and* lie next to another line of the same source:
/// `same_source(shift, limit)` says whether the curve scaled to about that
/// offset (same Doppler shape) is that bright on its steep part. A harmonic comb (helicopter rotor lines ~20 Hz apart, a second engine
/// or propeller line ~10 % away) lights just the copies whose shift matches
/// its spacing; counted in, those few copies inflated the noise estimate and
/// hid clear passes. Stationary lines (mains hum, machinery) next to a curve
/// do not follow its Doppler shape and still count, as do noise and
/// broadband bursts, which light most copies.
fn copy_noise(copies: &[(i32, f64, f64)], same_source: impl Fn(f64, f64) -> bool) -> (f64, f64) {
    let med = |s: &mut Vec<f64>| median_f64(s);
    let mut far: Vec<f64> = copies.iter().filter(|c| c.0 >= FAR_COPY_BINS).map(|c| c.2).collect();
    let limit = if far.len() >= 8 {
        let m = med(&mut far);
        let mut dev: Vec<f64> = far.iter().map(|s| (s - m).abs()).collect();
        m + COPY_OUTLIER_SIGMAS * 1.4826 * med(&mut dev)
    } else {
        f64::INFINITY
    };
    let kept: Vec<f64> = copies
        .iter()
        .filter(|c| c.0 < FAR_COPY_BINS || c.2 <= limit || !same_source(c.1, limit))
        .map(|c| c.2)
        .collect();
    let mean = kept.iter().sum::<f64>() / kept.len() as f64;
    let var = kept.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / (kept.len() - 1).max(1) as f64;
    (mean, var.sqrt())
}

fn median_f64(s: &mut [f64]) -> f64 {
    s.sort_by(f64::total_cmp);
    let n = s.len();
    match n {
        0 => 0.0,
        _ if n % 2 == 1 => s[n / 2],
        _ => 0.5 * (s[n / 2 - 1] + s[n / 2]),
    }
}

/// Least-squares fit to manually traced `(t, f)` points.
pub fn fit_points(points: &[(f64, f64)], cfg: &FitConfig) -> Option<(Flyby, f64)> {
    let pts: Vec<RidgePoint> = points.iter().map(|&(t, f)| RidgePoint { t, f, weight: 1.0 }).collect();
    let mut sorted = pts.clone();
    sorted.sort_by(|a, b| a.t.total_cmp(&b.t));
    let guess = initial_guess(&sorted, cfg)?;
    let mut best: Option<(Vec<f64>, f64)> = None;
    for dd in [1.0, 0.5, 2.0] {
        let start = Flyby { d: guess.d * dd, ..guess };
        let (x, fx) = nelder_mead(|x| ls_cost(&sorted, &unpack(x), cfg, 1.0), &pack(&start), &[1.0, 3.0, 0.3, 2.0], 800);
        if best.as_ref().is_none_or(|(_, b)| fx < *b) {
            best = Some((x, fx));
        }
    }
    let flyby = unpack(&best?.0);
    Some((flyby, rms_residual(&sorted, &flyby, cfg.c)))
}

/// Brightest path whose frequency never rises by more than one bin per column
/// and never drops faster than `max_slope_hz_s`.
pub fn track_ridge(spec: &Spectrogram, max_slope_hz_s: f64) -> Vec<RidgePoint> {
    let (rows, cols) = (spec.rows, spec.cols);
    let max_drop = ((max_slope_hz_s * spec.dt / spec.df).ceil() as usize).max(1);
    let up = 1usize;
    let mut dp = spec.column(0).iter().map(|v| *v as f64).collect::<Vec<_>>();
    let mut back = vec![0u32; rows * cols];
    let mut next = vec![0.0f64; rows];
    for col in 1..cols {
        let column = spec.column(col);
        for r in 0..rows {
            // previous row r' with r' - up <= r <= r' + max_drop  (frequency non-increasing)
            let lo = r.saturating_sub(up);
            let hi = (r + max_drop).min(rows - 1);
            let mut best_r = lo;
            let mut best_v = f64::NEG_INFINITY;
            for (rp, v) in dp.iter().enumerate().take(hi + 1).skip(lo) {
                // rising steps are allowed for noise tolerance but cost, so the
                // path cannot climb freely through noise
                let v = if rp < r { *v - RISE_PENALTY } else { *v };
                if v > best_v {
                    best_v = v;
                    best_r = rp;
                }
            }
            next[r] = best_v + column[r] as f64;
            back[col * rows + r] = best_r as u32;
        }
        std::mem::swap(&mut dp, &mut next);
    }
    let mut r = dp.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map_or(0, |(i, _)| i);
    let mut path = vec![0usize; cols];
    for col in (0..cols).rev() {
        path[col] = r;
        r = back[col * rows + r] as usize;
    }
    path.iter()
        .enumerate()
        .map(|(col, &r)| RidgePoint {
            t: spec.col_time(col),
            f: spec.f_min + r as f64 * spec.df,
            weight: spec.column(col)[r] as f64,
        })
        .collect()
}

/// Ridge points whose locally averaged brightness stands out from noise.
fn strong_points(ridge: &[RidgePoint]) -> Vec<RidgePoint> {
    const HALF: usize = 4;
    let smoothed: Vec<f64> = (0..ridge.len())
        .map(|i| {
            let lo = i.saturating_sub(HALF);
            let hi = (i + HALF + 1).min(ridge.len());
            ridge[lo..hi].iter().map(|p| p.weight).sum::<f64>() / (hi - lo) as f64
        })
        .collect();
    let mut sorted = smoothed.clone();
    sorted.sort_by(f64::total_cmp);
    // a pure-noise path still collects the per-column maximum, so threshold
    // relative to the path's own distribution
    let floor = sorted[sorted.len() / 4];
    let peak = sorted[sorted.len() * 19 / 20];
    let thresh = floor + 0.35 * (peak - floor);
    let strong: Vec<RidgePoint> = ridge
        .iter()
        .zip(&smoothed)
        .filter(|(_, s)| **s >= thresh)
        .map(|(p, s)| RidgePoint { weight: *s, ..*p })
        .collect();
    if strong.len() >= 10 { strong } else { ridge.to_vec() }
}

pub(crate) fn initial_guess(points: &[RidgePoint], cfg: &FitConfig) -> Option<Flyby> {
    if points.len() < 4 {
        return None;
    }
    let n = points.len();
    let k = (n / 7).max(2);
    let mut head: Vec<f32> = points[..k].iter().map(|p| p.f as f32).collect();
    let mut tail: Vec<f32> = points[n - k..].iter().map(|p| p.f as f32).collect();
    let f_pre = median(&mut head) as f64;
    let f_post = median(&mut tail) as f64;
    let (f0, v) = if f_pre > f_post {
        Flyby::from_asymptotes(f_pre, f_post, cfg.c)
    } else {
        (0.5 * (f_pre + f_post), cfg.v_bounds.0.max(20.0))
    };
    let v = v.clamp(cfg.v_bounds.0, cfg.v_bounds.1);
    // CPA: where the ridge crosses f0
    let tc = points
        .windows(2)
        .find(|w| w[0].f >= f0 && w[1].f <= f0)
        .map(|w| {
            let span = w[0].f - w[1].f;
            if span > 0.0 { w[0].t + (w[0].f - f0) / span * (w[1].t - w[0].t) } else { w[0].t }
        })
        .unwrap_or_else(|| 0.5 * (points[0].t + points[n - 1].t));
    // slope near CPA: regression over points in the middle half of the drop
    let band = 0.25 * (f_pre - f_post).abs().max(1.0);
    let mid: Vec<&RidgePoint> = points.iter().filter(|p| (p.f - f0).abs() <= band).collect();
    let slope = if mid.len() >= 3 {
        let mt = mid.iter().map(|p| p.t).sum::<f64>() / mid.len() as f64;
        let mf = mid.iter().map(|p| p.f).sum::<f64>() / mid.len() as f64;
        let cov: f64 = mid.iter().map(|p| (p.t - mt) * (p.f - mf)).sum();
        let var: f64 = mid.iter().map(|p| (p.t - mt).powi(2)).sum();
        if var > 0.0 { cov / var } else { 0.0 }
    } else {
        0.0
    };
    let d = if slope < -1e-6 { f0 * v * v / (cfg.c * -slope) } else { 1000.0 };
    let d = d.clamp(cfg.d_bounds.0, cfg.d_bounds.1);
    Some(Flyby { f0, v, d, tc })
}

pub(crate) fn pack(p: &Flyby) -> Vec<f64> {
    vec![p.f0, p.v, p.d.ln(), p.tc]
}

pub(crate) fn unpack(x: &[f64]) -> Flyby {
    Flyby { f0: x[0], v: x[1], d: x[2].exp(), tc: x[3] }
}

pub(crate) fn in_bounds(p: &Flyby, cfg: &FitConfig) -> bool {
    p.f0 > 0.0
        && (cfg.v_bounds.0..=cfg.v_bounds.1.min(cfg.c - 10.0)).contains(&p.v)
        && (cfg.d_bounds.0..=cfg.d_bounds.1).contains(&p.d)
}

pub(crate) fn ls_cost(points: &[RidgePoint], p: &Flyby, cfg: &FitConfig, sigma: f64) -> f64 {
    if !in_bounds(p, cfg) {
        return 1e12;
    }
    // Cauchy loss: robust to ridge points that jumped onto noise
    points
        .iter()
        .map(|pt| {
            let r = (p.freq_at(pt.t, cfg.c) - pt.f) / sigma;
            pt.weight * (1.0 + r * r).ln()
        })
        .sum()
}

pub fn rms_residual(points: &[RidgePoint], p: &Flyby, c: f64) -> f64 {
    let (mut sw, mut s) = (0.0, 0.0);
    for pt in points {
        let r = p.freq_at(pt.t, c) - pt.f;
        s += pt.weight * r * r;
        sw += pt.weight;
    }
    if sw > 0.0 { (s / sw).sqrt() } else { 0.0 }
}

/// For `v`, `d` and `tc`: scan a grid centred on the best fit, re-optimise the
/// other parameters at each value, and report where the profiled score crosses
/// the threshold (linearly interpolated between grid points). The threshold is
/// the more permissive of a fraction of the peak height above the noise
/// baseline and `RANGE_SIGMAS` noise sigmas below the peak, so ranges never
/// collapse on bright lines. Grids widen when the range reaches their edge; a
/// range still touching the edge is reported as clipped.
pub fn profile_ranges(map: &ScoreMap, best: &Flyby, best_score: f64, cfg: &FitConfig) -> Ranges {
    const RANGE_SIGMAS: f64 = 3.0;
    const HALF_STEPS: usize = 20;
    // Noise of the score near the fit, from its shifted copies. With several
    // harmonics, copies made by shifting the fundamental move harmonic k by k
    // times the shift, and on a dense comb (a helicopter's rotor lines) some
    // of them land on the source's own lines. Copies shifted by the same bins
    // on every line stay between the lines when the comb is wider than 20
    // bins (and hit them when it is narrower, where the first family may
    // not). A comb lights one family more than the other, so the smaller
    // spread is the noise. One line: both are the same.
    let sigma_of = |absolute: bool| contrast_impl(map, best, cfg, |_| true, true, absolute).map_or(0.0, |s| s.sigma_full);
    let sigma = if cfg.harmonics.len() > 1 { sigma_of(false).min(sigma_of(true)) } else { sigma_of(false) };
    let thresh = (map.baseline + cfg.range_fraction * (best_score - map.baseline)).min(best_score - RANGE_SIGMAS * sigma);
    let steps = [map.spec.df, 2.0, 0.15, 1.0];
    let base = pack(best);
    let bounds = [
        (0.0, f64::INFINITY),
        (cfg.v_bounds.0, cfg.v_bounds.1.min(cfg.c - 10.0)),
        (cfg.d_bounds.0.ln(), cfg.d_bounds.1.ln()),
        (f64::NEG_INFINITY, f64::INFINITY),
    ];
    let profiled_score = |idx: usize, val: f64, warm: &mut Vec<f64>| -> f64 {
        let free: Vec<usize> = (0..4).filter(|i| *i != idx).collect();
        let sub_steps: Vec<f64> = free.iter().map(|i| steps[*i]).collect();
        let full_at = |s: &[f64]| -> Vec<f64> {
            let mut full = base.clone();
            full[idx] = val;
            for (k, i) in free.iter().enumerate() {
                full[*i] = s[k];
            }
            full
        };
        let (sub, fx) = nelder_mead(
            |s| {
                let p = unpack(&full_at(s));
                if !in_bounds(&p, cfg) { 1e9 } else { -map.score(&p, cfg.c, &cfg.harmonics) }
            },
            warm,
            &sub_steps,
            150,
        );
        *warm = sub;
        -fx
    };
    // Returns (lo, hi, clipped) for one parameter with half-width `span`.
    let profile = |idx: usize, span: f64| -> (f64, f64, bool) {
        let centre = base[idx];
        let step = span / HALF_STEPS as f64;
        let free: Vec<usize> = (0..4).filter(|i| *i != idx).collect();
        let start: Vec<f64> = free.iter().map(|i| base[*i]).collect();
        // walk outwards from the optimum, warm-starting each step from the last
        let walk = |dir: f64| -> (f64, bool) {
            let mut warm = start.clone();
            let mut prev_val = centre;
            let mut prev_score = best_score;
            for k in 1..=HALF_STEPS {
                let val = centre + dir * step * k as f64;
                if val < bounds[idx].0 || val > bounds[idx].1 {
                    return (prev_val, true);
                }
                let s = profiled_score(idx, val, &mut warm);
                if s < thresh {
                    let frac = if prev_score > s { ((prev_score - thresh) / (prev_score - s)).clamp(0.0, 1.0) } else { 0.5 };
                    return (prev_val + frac * (val - prev_val), false);
                }
                prev_val = val;
                prev_score = s;
            }
            (prev_val, true)
        };
        let (lo, lo_clip) = walk(-1.0);
        let (hi, hi_clip) = walk(1.0);
        // never narrower than one grid step either side
        (lo.min(centre - step), hi.max(centre + step), lo_clip || hi_clip)
    };
    let adaptive = |idx: usize, mut span: f64| -> (f64, f64, bool) {
        let mut out = profile(idx, span);
        for _ in 0..2 {
            if !out.2 {
                break;
            }
            span *= 2.0;
            out = profile(idx, span);
        }
        out
    };
    let v = adaptive(1, (0.25 * best.v).max(15.0));
    let d = adaptive(2, 2.0_f64.ln());
    let (tc_lo, tc_hi, tc_clip) = adaptive(3, 10.0);
    let tc_floor = map.spec.dt.max(0.25);
    Ranges {
        v: (v.0, v.1),
        d: (d.0.exp(), d.1.exp()),
        tc: (tc_lo.min(best.tc - tc_floor), tc_hi.max(best.tc + tc_floor)),
        clipped: [v.2, d.2, tc_clip],
    }
}

/// Plain Nelder–Mead minimiser.
pub fn nelder_mead(mut f: impl FnMut(&[f64]) -> f64, x0: &[f64], step: &[f64], max_iter: usize) -> (Vec<f64>, f64) {
    let n = x0.len();
    let mut simplex: Vec<Vec<f64>> = vec![x0.to_vec()];
    for i in 0..n {
        let mut x = x0.to_vec();
        x[i] += step[i];
        simplex.push(x);
    }
    let mut values: Vec<f64> = simplex.iter().map(|x| f(x)).collect();
    for _ in 0..max_iter {
        let mut order: Vec<usize> = (0..=n).collect();
        order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
        simplex = order.iter().map(|i| simplex[*i].clone()).collect();
        values = order.iter().map(|i| values[*i]).collect();
        if (values[n] - values[0]).abs() <= 1e-10 * (values[0].abs() + 1e-10) {
            break;
        }
        let centroid: Vec<f64> = (0..n).map(|j| simplex[..n].iter().map(|x| x[j]).sum::<f64>() / n as f64).collect();
        let along = |t: f64| -> Vec<f64> { (0..n).map(|j| centroid[j] + t * (simplex[n][j] - centroid[j])).collect() };
        let xr = along(-1.0);
        let fr = f(&xr);
        if fr < values[0] {
            let xe = along(-2.0);
            let fe = f(&xe);
            if fe < fr {
                simplex[n] = xe;
                values[n] = fe;
            } else {
                simplex[n] = xr;
                values[n] = fr;
            }
        } else if fr < values[n - 1] {
            simplex[n] = xr;
            values[n] = fr;
        } else {
            let (xc, fc) = if fr < values[n] {
                let x = along(-0.5);
                let v = f(&x);
                (x, v)
            } else {
                let x = along(0.5);
                let v = f(&x);
                (x, v)
            };
            if fc < values[n].min(fr) {
                simplex[n] = xc;
                values[n] = fc;
            } else {
                let best = simplex[0].clone();
                for i in 1..=n {
                    for j in 0..n {
                        simplex[i][j] = best[j] + 0.5 * (simplex[i][j] - best[j]);
                    }
                    values[i] = f(&simplex[i]);
                }
            }
        }
    }
    let (i, _) = values.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1)).expect("non-empty");
    (simplex[i].clone(), values[i])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{SynthFlyby, synthesize};

    #[test]
    fn nelder_mead_finds_rosenbrock_minimum() {
        let (x, fx) = nelder_mead(
            |x| (1.0 - x[0]).powi(2) + 100.0 * (x[1] - x[0] * x[0]).powi(2),
            &[-1.2, 1.0],
            &[0.5, 0.5],
            2000,
        );
        assert!(fx < 1e-6, "{x:?} {fx}");
    }

    /// A fly-by whose `f0` is a fundamental nobody hears (lines at its 3rd,
    /// 4th and 7th multiples), on a map sized for the lines heard: contrast
    /// and its noise are measured as for the same lines on the lowest one.
    #[test]
    fn contrast_of_lines_above_an_unheard_fundamental() {
        let truth = Flyby { f0: 87.5, v: 55.0, d: 900.0, tc: 100.0 };
        let mut amps = vec![0.0; 7];
        (amps[2], amps[3], amps[6]) = (0.4, 0.4, 0.6);
        let audio = synthesize(8000.0, 200.0, &[SynthFlyby { flyby: truth, harmonics: amps, amplitude: 0.05 }], 0.3, &[], 3);
        let region = Region { t_start: 40.0, t_end: 160.0, f_min: 180.0, f_max: 800.0 };
        let cfg = FitConfig { harmonics: vec![3.0, 4.0, 7.0], ..FitConfig::default() };
        let map = ScoreMap::new(region_spectrogram(&audio, &region, &cfg));
        let stats = contrast_stats(&map, &truth, &cfg).expect("columns inside the map");
        let on_lowest = FitConfig { harmonics: vec![1.0, 4.0 / 3.0, 7.0 / 3.0], ..cfg.clone() };
        let lowest = contrast_stats(&map, &Flyby { f0: 3.0 * truth.f0, ..truth }, &on_lowest).expect("lowest");
        assert!(stats.contrast > 10.0 && stats.sigma_full > 0.0, "{} {}", stats.contrast, stats.sigma_full);
        assert!((stats.contrast - lowest.contrast).abs() < 1e-9 && (stats.sigma_full - lowest.sigma_full).abs() < 1e-12);
    }

    #[test]
    fn recovers_synthetic_flyby_buried_in_noise() {
        let truth = Flyby { f0: 112.0, v: 72.0, d: 1100.0, tc: 150.0 };
        let audio = synthesize(
            8000.0,
            300.0,
            &[SynthFlyby { flyby: truth, harmonics: vec![1.0, 0.5, 0.3], amplitude: 0.02 }],
            0.3,
            &[(60.0, 0.05), (120.0, 0.03)],
            7,
        );
        let cfg = FitConfig::default();
        let region = Region { t_start: 60.0, t_end: 250.0, f_min: 80.0, f_max: 160.0 };
        let fit = fit_region(&audio, &region, &cfg).expect("fit");
        let p = fit.flyby;
        eprintln!("{fit:#?}");
        assert!((p.f0 - truth.f0).abs() < 0.6, "f0 {}", p.f0);
        assert!((p.v - truth.v).abs() < 3.0, "v {}", p.v);
        assert!((p.d / truth.d - 1.0).abs() < 0.15, "d {}", p.d);
        assert!((p.tc - truth.tc).abs() < 1.0, "tc {}", p.tc);
        let r = fit.ranges;
        assert!(r.v.0 <= truth.v && truth.v <= r.v.1, "v range {:?}", r.v);
        assert!(r.d.0 <= truth.d && truth.d <= r.d.1, "d range {:?}", r.d);
        assert!(r.tc.0 <= truth.tc && truth.tc <= r.tc.1, "tc range {:?}", r.tc);
    }

    #[test]
    fn parallel_line_of_same_source_does_not_hide_the_line() {
        // a second engine/propeller line 12 % above the first (fs858591, a
        // DR400): the copies 11-14 bins up run along it
        let truth = Flyby { f0: 105.0, v: 58.0, d: 1200.0, tc: 150.0 };
        let audio = synthesize(
            8000.0,
            300.0,
            &[
                SynthFlyby { flyby: truth, harmonics: vec![1.0], amplitude: 0.03 },
                SynthFlyby { flyby: Flyby { f0: 1.12 * truth.f0, ..truth }, harmonics: vec![1.0], amplitude: 0.03 },
            ],
            0.3,
            &[],
            5,
        );
        let cfg = FitConfig { n_fft: 8192, dt: 0.25, ..FitConfig::default() };
        let region = Region { t_start: 40.0, t_end: 260.0, f_min: 50.0, f_max: 200.0 };
        let map = ScoreMap::new(region_spectrogram(&audio, &region, &cfg));
        let c = contrast(&map, &truth, &cfg);
        assert!(c >= 15.0, "contrast {c}");
        // a stationary tone next to a curve is not part of its source
        let hum = synthesize(8000.0, 300.0, &[SynthFlyby { flyby: truth, harmonics: vec![1.0], amplitude: 0.03 }], 0.3, &[(105.0 * 1.12 + 2.0, 0.03)], 5);
        let map = ScoreMap::new(region_spectrogram(&hum, &region, &cfg));
        let c_hum = contrast(&map, &truth, &cfg);
        assert!(c_hum < c, "contrast with a hum line {c_hum} vs {c}");
    }

    #[test]
    fn rotor_comb_neither_loosens_joint_ranges_nor_helps_misfits() {
        // a main-rotor comb like Edgemere's: lines every 17.5 Hz, fitted
        // jointly on every second line (base 35 Hz, multiples 1-3 and 5-7)
        let spacing = 17.5;
        let truth = Flyby { f0: spacing, v: 70.0, d: 950.0, tc: 150.0 };
        let mut amps = vec![0.0];
        amps.extend(std::iter::repeat_n(1.0, 13));
        let audio = synthesize(8000.0, 300.0, &[SynthFlyby { flyby: truth, harmonics: amps, amplitude: 0.012 }], 0.3, &[], 9);
        // 16384 points: 20 bins (9.8 Hz) stay inside one comb spacing
        let cfg = FitConfig { n_fft: 16384, dt: 0.25, ..FitConfig::default() };
        let region = Region { t_start: 60.0, t_end: 240.0, f_min: 10.0, f_max: 600.0 };
        let map = ScoreMap::new(region_spectrogram(&audio, &region, &cfg));
        let joint = FitConfig { harmonics: vec![1.0, 2.0, 3.0, 5.0, 6.0, 7.0], ..cfg.clone() };
        let base = Flyby { f0: 2.0 * spacing, ..truth };
        // shifting the base by half the spacing moves the even multiples onto
        // the comb: copies scaled with the harmonic number are lit by the
        // source itself, copies shifted by the same bins on every line not
        let scaled = contrast_stats(&map, &base, &joint).expect("stats").sigma_full;
        let absolute = contrast_impl(&map, &base, &joint, |_| true, true, true).expect("stats").sigma_full;
        assert!(absolute < 0.8 * scaled, "sigma absolute {absolute} vs scaled {scaled}");
        let best = map.score(&base, joint.c, &joint.harmonics);
        let r = profile_ranges(&map, &base, best, &joint);
        assert!(r.v.0 <= truth.v && truth.v <= r.v.1, "v range {:?}", r.v);
        // 66-73 m/s; 66-77 with the scaled copies' spread alone
        assert!(r.v.1 / r.v.0 < 1.15, "v range {:?}", r.v);
        assert!(!r.clipped.iter().any(|c| *c), "clipped {:?}", r.clipped);
        // a slow, close mis-fit cutting across the comb gains nothing from
        // the copies on comb lines being discounted (a guard only: these
        // synthetic mis-fits are not lifted even without the ridge test,
        // which was needed on the real Edgemere comb)
        let cfg1 = FitConfig { harmonics: vec![1.0], ..cfg };
        for misfit in [
            Flyby { f0: 8.5 * spacing, v: 32.0, d: 330.0, tc: 141.0 },
            Flyby { f0: 6.5 * spacing, v: 45.0, d: 460.0, tc: 144.0 },
        ] {
            let with = contrast(&map, &misfit, &cfg1);
            let without = contrast_impl(&map, &misfit, &cfg1, |_| true, false, false).map_or(0.0, |s| s.contrast);
            assert!(with <= without + 1e-9, "mis-fit {misfit:?}: contrast {with} vs {without} counting every copy");
        }
        // while a real comb line still may
        let line = Flyby { f0: 6.0 * spacing, ..truth };
        let c_line = contrast(&map, &line, &cfg1);
        assert!(c_line >= 6.0, "comb line contrast {c_line}");
    }

    #[test]
    fn fits_traced_points() {
        let truth = Flyby { f0: 140.0, v: 95.0, d: 2400.0, tc: 40.0 };
        let pts: Vec<(f64, f64)> = (0..60).map(|i| {
            let t = i as f64 * 1.5;
            (t, truth.freq_at(t, 343.0) + if i % 2 == 0 { 0.4 } else { -0.4 })
        }).collect();
        let (p, rms) = fit_points(&pts, &FitConfig::default()).unwrap();
        assert!(rms < 0.6, "rms {rms}");
        assert!((p.v - truth.v).abs() < 3.0 && (p.d / truth.d - 1.0).abs() < 0.15 && (p.tc - truth.tc).abs() < 1.0, "{p:?}");
    }

    /// A machine tone that runs for part of the recording (so whitening over
    /// time leaves it in) just under a fly-by's approach frequency: it is
    /// taken out of the map the fly-by's candidates are scored on, also
    /// where it only reaches a few seconds into the region, and the
    /// fly-by's own line stays.
    #[test]
    fn level_tone_is_taken_out_and_the_line_stays() {
        let truth = Flyby { f0: 112.0, v: 72.0, d: 1100.0, tc: 150.0 };
        let sr = 8000.0;
        let clean = synthesize(sr, 300.0, &[SynthFlyby { flyby: truth, harmonics: vec![1.0], amplitude: 0.02 }], 0.3, &[], 7);
        let mut audio = synthesize(sr, 300.0, &[SynthFlyby { flyby: truth, harmonics: vec![1.0], amplitude: 0.02 }], 0.3, &[], 7);
        // 139.5 Hz from 20 s to 110 s, wobbling by a few tenths of a hertz
        let tone_hz = 139.5;
        let mut phase = 0.0f64;
        for (i, x) in audio.samples.iter_mut().enumerate() {
            let t = i as f64 / sr;
            if (20.0..110.0).contains(&t) {
                let f = tone_hz + 0.25 * (2.0 * std::f64::consts::PI * t / 7.0).sin();
                phase += 2.0 * std::f64::consts::PI * f / sr;
                *x += (0.1 * phase.sin()) as f32;
            }
        }
        let cfg = FitConfig::default();
        // the region starts at 90 s: 20 s of the tone lie inside it
        let region = Region { t_start: 90.0, t_end: 250.0, f_min: 80.0, f_max: 160.0 };
        let with = ScoreMap::new(region_spectrogram(&audio, &region, &cfg));
        let without = region_score_map(&audio, &region, &cfg);
        assert_eq!(with.level_cells, 0);
        assert!(without.level_cells > 0, "no level tone found");
        let along_tone = |map: &ScoreMap| -> f64 {
            let cols: Vec<usize> = (0..map.spec.cols).filter(|c| (92.0..106.0).contains(&map.spec.col_time(*c))).collect();
            cols.iter().map(|c| map.spec.sample(*c, tone_hz).unwrap_or(0.0) as f64).sum::<f64>() / cols.len() as f64
        };
        assert!(along_tone(&with) > 2.5, "tone not in the plain map: {}", along_tone(&with));
        assert!((along_tone(&without) - without.baseline).abs() < 0.2, "tone left in: {} vs baseline {}", along_tone(&without), without.baseline);
        // the fly-by's line is as bright as in the recording without the tone
        // (in the plain map its flat approach end, 2 Hz above the tone, gains from it)
        let s_clean = region_score_map(&clean, &region, &cfg).score(&truth, cfg.c, &cfg.harmonics);
        let (s_with, s_without) = (with.score(&truth, cfg.c, &cfg.harmonics), without.score(&truth, cfg.c, &cfg.harmonics));
        assert!(s_without > 0.9 * s_clean && s_without < 1.1 * s_clean, "line with the tone taken out {s_without}, without the tone {s_clean}");
        assert!(s_with > 1.2 * s_clean, "the tone did not add to the curve in the plain map: {s_with} vs {s_clean}");
    }

    /// A line that keeps falling is not a level tone, however slowly it goes
    /// through its far end; a short tone is not one either.
    #[test]
    fn slow_fly_by_and_short_tone_are_not_level_tones() {
        // 2.6 km away at 50 m/s: 0.2 Hz/s at the closest approach
        let truth = Flyby { f0: 77.0, v: 50.0, d: 2600.0, tc: 200.0 };
        let sr = 8000.0;
        let mut audio = synthesize(sr, 400.0, &[SynthFlyby { flyby: truth, harmonics: vec![1.0], amplitude: 0.03 }], 0.3, &[], 11);
        // 8 s of a tone
        for (i, x) in audio.samples.iter_mut().enumerate() {
            let t = i as f64 / sr;
            if (300.0..308.0).contains(&t) {
                *x += (0.1 * (2.0 * std::f64::consts::PI * 95.0 * t).sin()) as f32;
            }
        }
        let cfg = FitConfig::default();
        let region = Region { t_start: 100.0, t_end: 330.0, f_min: 55.0, f_max: 110.0 };
        let with = ScoreMap::new(region_spectrogram(&audio, &region, &cfg));
        let without = region_score_map(&audio, &region, &cfg);
        // within 80 s of the closest approach the line moves by more than 0.04 Hz/s
        let near = |map: &ScoreMap| -> f64 {
            let cols: Vec<usize> = (0..map.spec.cols).filter(|c| (map.spec.col_time(*c) - truth.tc).abs() < 80.0).collect();
            cols.iter().map(|c| map.spec.sample(*c, truth.freq_at(map.spec.col_time(*c), cfg.c)).unwrap_or(0.0) as f64).sum::<f64>() / cols.len() as f64
        };
        assert!(near(&without) > 0.95 * near(&with), "the fly-by's line was taken for a level tone: {} vs {}", near(&without), near(&with));
        let col = (0..without.spec.cols).find(|c| without.spec.col_time(*c) > 304.0).unwrap();
        let (tone_with, tone_without) = (with.spec.sample(col, 95.0).unwrap(), without.spec.sample(col, 95.0).unwrap());
        assert!(tone_with > 2.5 && tone_without == tone_with, "an 8 s tone was taken out: {tone_without} vs {tone_with}");
    }
}
