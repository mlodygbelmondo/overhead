//! How well a modelled heard-frequency curve matches one detection.
//!
//! With the recording at hand the curve is laid on a whitened z-score
//! spectrogram of the detection's neighbourhood and we take the mean
//! brightness under it (and its harmonics), sweeping the rest frequency a
//! little because `f0` is itself a measurement. The result is expressed
//! relative to the stage-1 straight-line fit on the same map: 1.0 means "as
//! good as the best constant-velocity curve", and the real flight geometry
//! can do better than that when the aircraft turned, climbed or changed speed.
//!
//! Without audio, the detection's fitted curve stands in for the spectrogram
//! (a Gaussian kernel in log-frequency, [`MODEL_SIGMA`]).

use overhead_core::audio::Audio;
use overhead_core::detect::Detection;
use overhead_core::fit::{self, FitConfig, Region, ScoreMap};
use overhead_core::model::{DEFAULT_C, Flyby};

/// Rest-frequency sweep: ±1.2 % in 0.05 % steps.
const F0_SWEEP: f64 = 0.012;
const F0_STEP: f64 = 0.0005;
/// Width (log-frequency) of the kernel around a detection's fitted curve when
/// there is no audio. The fitted curve is a straight constant-speed path at
/// 343 m/s, so the real geometry rarely stays within a few tenths of a
/// percent of it; 0.4 % matched almost nothing on the development cases,
/// 1 % recovered 2 of 10 (1.6 % no better).
const MODEL_SIGMA: f64 = 0.01;
/// Width (log-frequency) of the distance to points traced by hand, and its
/// time scale (s): the distance is taken across the curve in (t / tau,
/// ln f / sigma), so a point on the steep part near the closest approach is
/// not judged by its frequency alone. A person follows the real line, but
/// the matcher's ADS-B geometry and speed of sound are not exact. Chosen on
/// the synthetic development cases with traces read off the rendered line
/// (README, "Tracing a curve by hand"): 0.4–1.2 % and 0.5–3 s were tried;
/// a narrower kernel matched fewer of the true passes, a longer time scale
/// matched more but placed the answer less precisely.
pub const TRACE_SIGMA: f64 = 0.012;
pub const TRACE_TAU: f64 = 1.5;
/// Model samples are taken this far beyond the first and last traced point
/// (s), so the heard closest approach is found even near the trace's ends.
const TRACE_PAD: f64 = 5.0;
/// A detection's line counts as seen in a column when the map under its
/// stage-1 fit (the brightest of its lines, within two bins) reaches this.
const SUPPORT_Z: f32 = 1.5;
/// The line's support is smoothed over this (s) either side: it begins and
/// ends where at least `SUPPORT_MIN_FILL` of such a stretch is lit.
const SUPPORT_SMOOTH_S: f64 = 3.0;
const SUPPORT_MIN_FILL: f64 = 0.3;
/// The support grows from the line seen within `SUPPORT_SEED_S` of the
/// fitted closest approach, over breaks of at most `SUPPORT_GAP_S`.
const SUPPORT_SEED_S: f64 = 30.0;
const SUPPORT_GAP_S: f64 = 10.0;
/// Columns this far (s) beyond the line's support still count.
const SUPPORT_MARGIN_S: f64 = 20.0;

/// `OVERHEAD_NO_SUPPORT`: every column of a detection's map counts (the old behaviour, for comparison).
fn no_support() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("OVERHEAD_NO_SUPPORT").is_some())
}

/// Number of rest-frequency sweep values, and that padded to whole 8-lane
/// vectors (the padding has `f0·m = NaN`, which never falls on the map).
const NM: usize = (2.0 * F0_SWEEP / F0_STEP + 0.5) as usize + 1;
const NMP: usize = NM.div_ceil(8) * 8;

/// `acc[j] += Σ_h column(f0·m_j · k · h)` over the harmonics in order, for
/// every sweep value, where `column(f)` is [`Spectrogram::sample`]
/// (linear between bins, 0 outside the map).
///
/// [`Spectrogram::sample`]: overhead_core::spectrogram::Spectrogram::sample
type SweepKernel = fn(&[f32], f64, f64, &[f64; NMP], f64, &[f64], &mut [f64; NMP]);

fn sweep_kernel(rows: usize) -> SweepKernel {
    #[cfg(target_arch = "x86_64")]
    if rows >= 1 && rows < i32::MAX as usize {
        static AVX512: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *AVX512.get_or_init(|| {
            std::arch::is_x86_feature_detected!("avx512f")
                && std::arch::is_x86_feature_detected!("avx512vl")
                && std::arch::is_x86_feature_detected!("avx512dq")
                && std::env::var_os("OVERHEAD_NO_AVX512").is_none()
        }) {
            return sweep_avx512;
        }
        if std::arch::is_x86_feature_detected!("avx2") {
            return sweep_avx2;
        }
    }
    let _ = rows;
    sweep_scalar
}

fn sweep_scalar(column: &[f32], f_min: f64, df: f64, fm: &[f64; NMP], kk: f64, harmonics: &[f64], acc: &mut [f64; NMP]) {
    let rows = column.len();
    let top = rows.wrapping_sub(1) as f64;
    for h in harmonics {
        for j in 0..NM {
            let f = fm[j] * kk;
            let pos = (f * h - f_min) / df;
            if (0.0..=top).contains(&pos) {
                let r = pos as usize; // = floor for pos >= 0, without a libm call
                let frac = (pos - r as f64) as f32;
                let a = column[r];
                let b = column[(r + 1).min(rows - 1)];
                acc[j] += (a + (b - a) * frac) as f64;
            } else {
                acc[j] += 0.0f32 as f64;
            }
        }
    }
}

/// [`sweep_scalar`] four sweep values at a time. Every operation is the same
/// IEEE operation as in the scalar code (no fused multiply-add, the same
/// truncation and rounding), so the result is bit for bit identical.
#[cfg(target_arch = "x86_64")]
fn sweep_avx2(column: &[f32], f_min: f64, df: f64, fm: &[f64; NMP], kk: f64, harmonics: &[f64], acc: &mut [f64; NMP]) {
    // SAFETY: only selected by `sweep_kernel` when AVX2 is available and the column is not empty
    unsafe { sweep_avx2_inner(column, f_min, df, fm, kk, harmonics, acc) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn sweep_avx2_inner(column: &[f32], f_min: f64, df: f64, fm: &[f64; NMP], kk: f64, harmonics: &[f64], acc: &mut [f64; NMP]) {
    use std::arch::x86_64::*;
    let rows = column.len();
    debug_assert!(rows >= 1 && rows < i32::MAX as usize);
    let top = _mm256_set1_pd((rows - 1) as f64);
    let zero = _mm256_setzero_pd();
    let one = _mm_set1_epi32(1);
    let last = _mm_set1_epi32((rows - 1) as i32);
    let (kk_v, fmin_v, df_v) = (_mm256_set1_pd(kk), _mm256_set1_pd(f_min), _mm256_set1_pd(df));
    let base = column.as_ptr();
    for &h in harmonics {
        let h_v = _mm256_set1_pd(h);
        for c in 0..NMP / 4 {
            // SAFETY: c * 4 + 3 < NMP
            let fm_v = unsafe { _mm256_loadu_pd(fm.as_ptr().add(c * 4)) };
            let f = _mm256_mul_pd(fm_v, kk_v);
            let pos = _mm256_div_pd(_mm256_sub_pd(_mm256_mul_pd(f, h_v), fmin_v), df_v);
            // 0 <= pos <= rows - 1 (false for NaN)
            let ok = _mm256_and_pd(_mm256_cmp_pd::<_CMP_GE_OQ>(pos, zero), _mm256_cmp_pd::<_CMP_LE_OQ>(pos, top));
            let posc = _mm256_and_pd(pos, ok);
            let r = _mm256_cvttpd_epi32(posc);
            let frac = _mm256_cvtpd_ps(_mm256_sub_pd(posc, _mm256_cvtepi32_pd(r)));
            let r1 = _mm_min_epi32(_mm_add_epi32(r, one), last);
            // SAFETY: 0 <= r, r1 <= rows - 1 in every lane (out-of-map lanes use index 0)
            let (a, b) = unsafe { (_mm_i32gather_ps::<4>(base, r), _mm_i32gather_ps::<4>(base, r1)) };
            let v = _mm_add_ps(a, _mm_mul_ps(_mm_sub_ps(b, a), frac));
            let v = _mm256_and_pd(_mm256_cvtps_pd(v), ok);
            // SAFETY: c * 4 + 3 < NMP
            unsafe {
                let p = acc.as_mut_ptr().add(c * 4);
                _mm256_storeu_pd(p, _mm256_add_pd(_mm256_loadu_pd(p), v));
            }
        }
    }
}

/// `x / d` correctly rounded, from `inv = RN(1 / d)`, without a division:
/// two residual corrections with fused multiply-adds (Markstein). The first
/// brings the product `x·inv` (within 1.5 ulp) to within half an ulp plus a
/// tiny excess, so it is faithful; from a faithful quotient the residual
/// `x - d·q` is exact and the second correction rounds to `RN(x / d)`
/// (Markstein's theorem, no underflow or overflow at these magnitudes).
/// NaN stays NaN. Checked bit for bit against the division in the tests.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
fn div_exact_512(x: std::arch::x86_64::__m512d, d: std::arch::x86_64::__m512d, inv: std::arch::x86_64::__m512d) -> std::arch::x86_64::__m512d {
    use std::arch::x86_64::*;
    let q0 = _mm512_mul_pd(x, inv);
    let q1 = _mm512_fmadd_pd(_mm512_fnmadd_pd(q0, d, x), inv, q0);
    _mm512_fmadd_pd(_mm512_fnmadd_pd(q1, d, x), inv, q1)
}

/// [`sweep_scalar`] eight sweep values at a time, bit for bit identical
/// (checked against it in the tests): the division by the bin width is the
/// correctly rounded quotient ([`div_exact_512`]), and the two map values of
/// each lane are taken from the map rows the whole sweep of one harmonic
/// touches, loaded once (the sweep spans ±1.2 % of the line, a few bins),
/// instead of being gathered lane by lane; a harmonic whose sweep spans more
/// than 32 rows falls back to (masked) gathers. Out-of-map lanes add 0, as
/// in the scalar code.
#[cfg(target_arch = "x86_64")]
fn sweep_avx512(column: &[f32], f_min: f64, df: f64, fm: &[f64; NMP], kk: f64, harmonics: &[f64], acc: &mut [f64; NMP]) {
    // SAFETY: only selected by `sweep_kernel` when AVX-512F/VL/DQ are available and the column is not empty
    unsafe { sweep_avx512_inner(column, f_min, df, fm, kk, harmonics, acc) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f,avx512vl,avx512dq")]
unsafe fn sweep_avx512_inner(column: &[f32], f_min: f64, df: f64, fm: &[f64; NMP], kk: f64, harmonics: &[f64], acc: &mut [f64; NMP]) {
    // the sweep in groups of chunks small enough to stay in registers
    // through all the harmonics; the last sweep value on its own, across
    // the harmonics (the rest of its chunk would only be padding)
    const { assert!(NM == 49 && NMP == 56) };
    // SAFETY: AVX-512F/VL/DQ enabled here; chunks 0..3 and 3..6 of NMP / 8
    unsafe {
        sweep_avx512_group::<3>(column, f_min, df, fm, kk, harmonics, acc, 0);
        sweep_avx512_group::<3>(column, f_min, df, fm, kk, harmonics, acc, 3);
    }
    // SAFETY: AVX-512F/VL/DQ enabled here
    unsafe { sweep_last(column, f_min, df, fm[48], kk, harmonics, &mut acc[48]) };
}

/// [`sweep_scalar`] for one sweep value (`fm`), eight harmonics at a time,
/// bit for bit: the eight map values are found together and then added in
/// the harmonics' order.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f,avx512vl,avx512dq")]
unsafe fn sweep_last(column: &[f32], f_min: f64, df: f64, fm: f64, kk: f64, harmonics: &[f64], acc: &mut f64) {
    use std::arch::x86_64::*;
    let rows = column.len();
    debug_assert!(rows >= 1 && rows < i32::MAX as usize);
    let top = _mm512_set1_pd((rows - 1) as f64);
    let zero = _mm512_setzero_pd();
    let one = _mm256_set1_epi32(1);
    let last = _mm256_set1_epi32((rows - 1) as i32);
    let f = _mm512_set1_pd(fm * kk);
    let (fmin_v, df_v, inv_v) = (_mm512_set1_pd(f_min), _mm512_set1_pd(df), _mm512_set1_pd(1.0 / df));
    let base = column.as_ptr();
    let mut a = *acc;
    let mut vals = [0.0f64; 8];
    for hs in harmonics.chunks(8) {
        let m: __mmask8 = ((1u32 << hs.len()) - 1) as u8;
        // SAFETY: only the first hs.len() lanes are read
        let h_v = unsafe { _mm512_maskz_loadu_pd(m, hs.as_ptr()) };
        let p = div_exact_512(_mm512_sub_pd(_mm512_mul_pd(f, h_v), fmin_v), df_v, inv_v);
        let k = m & _mm512_cmp_pd_mask::<_CMP_GE_OQ>(p, zero) & _mm512_cmp_pd_mask::<_CMP_LE_OQ>(p, top);
        let pc = _mm512_maskz_mov_pd(k, p);
        let r = _mm512_cvttpd_epi32(pc);
        let r1 = _mm256_min_epi32(_mm256_add_epi32(r, one), last);
        // SAFETY: masked lanes are not read; valid lanes have 0 <= r, r1 <= rows - 1
        let (va, vb) =
            unsafe { (_mm256_mmask_i32gather_ps::<4>(_mm256_setzero_ps(), k, r, base), _mm256_mmask_i32gather_ps::<4>(_mm256_setzero_ps(), k, r1, base)) };
        let frac = _mm512_cvtpd_ps(_mm512_reduce_pd::<{ _MM_FROUND_TO_ZERO }>(pc));
        let v = _mm256_add_ps(va, _mm256_mul_ps(_mm256_sub_ps(vb, va), frac));
        // off-map harmonics add +0, as in the scalar code
        // SAFETY: 8 lanes into an [f64; 8]
        unsafe { _mm512_storeu_pd(vals.as_mut_ptr(), _mm512_maskz_mov_pd(k, _mm512_cvtps_pd(v))) };
        for &x in &vals[..hs.len()] {
            a += x;
        }
    }
    *acc = a;
}

/// Chunks `c0..c0 + G` (of 8 sweep values each) of [`sweep_avx512`].
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f,avx512vl,avx512dq")]
#[allow(clippy::too_many_arguments)]
unsafe fn sweep_avx512_group<const G: usize>(
    column: &[f32],
    f_min: f64,
    df: f64,
    fm: &[f64; NMP],
    kk: f64,
    harmonics: &[f64],
    acc: &mut [f64; NMP],
    c0: usize,
) {
    use std::arch::x86_64::*;
    assert!((c0 + G) * 8 <= NMP);
    let rows = column.len();
    debug_assert!(rows >= 1 && rows < i32::MAX as usize);
    let top = _mm512_set1_pd((rows - 1) as f64);
    let zero = _mm512_setzero_pd();
    let one = _mm256_set1_epi32(1);
    let last = _mm256_set1_epi32((rows - 1) as i32);
    let (kk_v, fmin_v, df_v) = (_mm512_set1_pd(kk), _mm512_set1_pd(f_min), _mm512_set1_pd(df));
    let inv_v = _mm512_set1_pd(1.0 / df);
    let base = column.as_ptr();
    // f0·m·k for every sweep value (as the scalar code: `fm[j] * kk`)
    // SAFETY (loads and stores below): (c0 + c) * 8 + 7 < NMP
    let f: [__m512d; G] = std::array::from_fn(|c| _mm512_mul_pd(unsafe { _mm512_loadu_pd(fm.as_ptr().add((c0 + c) * 8)) }, kk_v));
    let mut acc_v: [__m512d; G] = std::array::from_fn(|c| unsafe { _mm512_loadu_pd(acc.as_ptr().add((c0 + c) * 8)) });
    for &h in harmonics {
        let h_v = _mm512_set1_pd(h);
        let mut pos = [zero; G];
        let mut ok = [0 as __mmask8; G];
        let mut r = [_mm256_setzero_si256(); G];
        let mut r1 = [_mm256_setzero_si256(); G];
        let mut lo_v = _mm256_set1_epi32(i32::MAX);
        let mut hi_v = _mm256_set1_epi32(-1);
        for c in 0..G {
            let p = div_exact_512(_mm512_sub_pd(_mm512_mul_pd(f[c], h_v), fmin_v), df_v, inv_v);
            // 0 <= pos <= rows - 1 (false for NaN)
            let k = _mm512_cmp_pd_mask::<_CMP_GE_OQ>(p, zero) & _mm512_cmp_pd_mask::<_CMP_LE_OQ>(p, top);
            let pc = _mm512_maskz_mov_pd(k, p);
            let ri = _mm512_cvttpd_epi32(pc);
            let ri1 = _mm256_min_epi32(_mm256_add_epi32(ri, one), last);
            lo_v = _mm256_mask_min_epi32(lo_v, k, lo_v, ri);
            hi_v = _mm256_mask_max_epi32(hi_v, k, hi_v, ri1);
            (pos[c], ok[c], r[c], r1[c]) = (pc, k, ri, ri1);
        }
        if ok.iter().fold(0, |a, k| a | k) == 0 {
            // nothing of this harmonic on the map: every lane would add 0,
            // and the sums are never -0, so they stay as they are
            continue;
        }
        // the rows the valid lanes read (`r` and `r + 1`, capped at the last
        // row); with every lane on the map, from the first and the last lane
        // (the positions are monotonic in the sweep value: each step from
        // `fm` to `pos` is a monotonic rounded operation)
        let (lo, hi) = if ok.iter().fold(0xff, |a, k| a & k) == 0xff {
            let (r_a, r_b) = (_mm256_cvtsi256_si32(r[0]), _mm256_extract_epi32::<7>(r[G - 1]));
            let (r1_a, r1_b) = (_mm256_cvtsi256_si32(r1[0]), _mm256_extract_epi32::<7>(r1[G - 1]));
            (r_a.min(r_b), r1_a.max(r1_b))
        } else {
            (
                _mm512_mask_reduce_min_epi32(0xff, _mm512_zextsi256_si512(lo_v)).max(0),
                _mm512_mask_reduce_max_epi32(0xff, _mm512_zextsi256_si512(hi_v)),
            )
        };
        // a and b of every lane from the rows lo..lo + 32 held in two
        // registers, or gathered when the sweep spans more rows
        let table = hi - lo < 32;
        let (mut t0, mut t1) = (_mm512_setzero_ps(), _mm512_setzero_ps());
        if table {
            let avail = rows - lo as usize;
            let m0: __mmask16 = if avail >= 16 { 0xffff } else { ((1u32 << avail) - 1) as u16 };
            // SAFETY: masked loads read only rows lo..rows of the column (avail >= 1)
            unsafe {
                t0 = _mm512_maskz_loadu_ps(m0, base.add(lo as usize));
                if avail > 16 {
                    let m1: __mmask16 = if avail >= 32 { 0xffff } else { ((1u32 << (avail - 16)) - 1) as u16 };
                    t1 = _mm512_maskz_loadu_ps(m1, base.add(lo as usize + 16));
                }
            }
        }
        let lo_i = _mm512_set1_epi32(lo);
        for c in 0..G {
            let (a, b) = if table {
                // (invalid lanes pick some table value; they add 0 below)
                let ia = _mm512_sub_epi32(_mm512_zextsi256_si512(r[c]), lo_i);
                let ib = _mm512_sub_epi32(_mm512_zextsi256_si512(r1[c]), lo_i);
                (
                    _mm512_castps512_ps256(_mm512_permutex2var_ps(t0, ia, t1)),
                    _mm512_castps512_ps256(_mm512_permutex2var_ps(t0, ib, t1)),
                )
            } else {
                // SAFETY: masked lanes are not read; valid lanes have 0 <= r, r1 <= rows - 1
                unsafe {
                    (
                        _mm256_mmask_i32gather_ps::<4>(_mm256_setzero_ps(), ok[c], r[c], base),
                        _mm256_mmask_i32gather_ps::<4>(_mm256_setzero_ps(), ok[c], r1[c], base),
                    )
                }
            };
            // pos - trunc(pos), exact (= pos - r: 0 <= pos < 2^31)
            let frac = _mm512_cvtpd_ps(_mm512_reduce_pd::<{ _MM_FROUND_TO_ZERO }>(pos[c]));
            let v = _mm256_add_ps(a, _mm256_mul_ps(_mm256_sub_ps(b, a), frac));
            let v = _mm512_maskz_mov_pd(ok[c], _mm512_cvtps_pd(v));
            acc_v[c] = _mm512_add_pd(acc_v[c], v);
        }
    }
    for (c, a) in acc_v.iter().enumerate() {
        // SAFETY: (c0 + c) * 8 + 7 < NMP
        unsafe { _mm512_storeu_pd(acc.as_mut_ptr().add((c0 + c) * 8), *a) };
    }
}

/// `totals[j] += acc[j] / nh` for every sweep value (the column's mean over
/// the lines added to the curve's sums), bit for bit; eight at a time with
/// the division-free quotient when AVX-512 is there.
fn add_mean(totals: &mut [f64; NMP], acc: &[f64; NMP], nh: f64) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx512f") {
        // SAFETY: AVX-512F checked
        unsafe { add_mean_avx512(totals, acc, nh) };
        return;
    }
    for (t, a) in totals.iter_mut().zip(acc) {
        *t += a / nh;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn add_mean_avx512(totals: &mut [f64; NMP], acc: &[f64; NMP], nh: f64) {
    use std::arch::x86_64::*;
    let (d, inv) = (_mm512_set1_pd(nh), _mm512_set1_pd(1.0 / nh));
    for c in 0..NMP / 8 {
        // SAFETY: c * 8 + 7 < NMP
        unsafe {
            let p = totals.as_mut_ptr().add(c * 8);
            let q = div_exact_512(_mm512_loadu_pd(acc.as_ptr().add(c * 8)), d, inv);
            _mm512_storeu_pd(p, _mm512_add_pd(_mm512_loadu_pd(p), q));
        }
    }
}

/// First and last column of a detection's map in which candidates are
/// scored: the stretch around the fitted closest approach in which the
/// line under its stage-1 fit is seen, and [`SUPPORT_MARGIN_S`] beyond
/// either end (a
/// real flight's curve may follow the line further than the straight fit
/// does). The map reaches 30 s and more beyond the detection's box, and the
/// box itself is as long as the fitted pass would take, not as long as the
/// aircraft was heard: what sounds there (a tone under the flat end of a
/// curve, another aircraft) would count for every curve laid across it,
/// the stage-1 fit that sets the scale included. Unlike a weight by
/// distance, this holds whatever the range the aircraft is heard to: in a
/// quiet place a line is seen for minutes, and all of it counts.
fn line_support(map: &ScoreMap, fb: &Flyby, harmonics: &[f64], c: f64) -> (usize, usize) {
    let spec = &map.spec;
    let cols = spec.cols;
    if cols == 0 {
        return (0, 0);
    }
    let lit: Vec<bool> = (0..cols)
        .map(|col| {
            let f = fb.freq_at(spec.col_time(col), c);
            harmonics.iter().any(|h| {
                [-2.0, -1.0, 0.0, 1.0, 2.0].iter().any(|o| spec.sample(col, f * h + o * spec.df).is_some_and(|v| v >= SUPPORT_Z))
            })
        })
        .collect();
    let half = ((SUPPORT_SMOOTH_S / spec.dt).round() as usize).max(1);
    let mut prefix = vec![0usize; cols + 1];
    for (i, l) in lit.iter().enumerate() {
        prefix[i + 1] = prefix[i] + usize::from(*l);
    }
    let seen = |col: usize| -> bool {
        let (a, b) = (col.saturating_sub(half), (col + half + 1).min(cols));
        lit[col] && (prefix[b] - prefix[a]) as f64 >= SUPPORT_MIN_FILL * (b - a) as f64
    };
    // The stretch around the fitted closest approach in which the line is
    // seen without a break longer than `SUPPORT_GAP_S`: what is lit under
    // the fit further off, after a longer silence, is not this fly-by (a
    // tone that stops, another aircraft's line crossing the fit's flat end).
    let centre = (((fb.tc - spec.t_start) / spec.dt).floor().max(0.0) as usize).min(cols - 1);
    let reach = (SUPPORT_SEED_S / spec.dt).round() as usize;
    let seed = (0..=reach).flat_map(|k| [centre.checked_sub(k), (centre + k < cols).then_some(centre + k)]).flatten().find(|c| seen(*c));
    let Some(seed) = seed else {
        // (no line under the fit near its closest approach: nothing to go by, every column counts)
        return (0, cols - 1);
    };
    let gap = (SUPPORT_GAP_S / spec.dt).round() as usize;
    let (mut first, mut last) = (seed, seed);
    for col in (0..seed).rev() {
        if seen(col) {
            first = col;
        } else if first - col > gap {
            break;
        }
    }
    for col in seed + 1..cols {
        if seen(col) {
            last = col;
        } else if col - last > gap {
            break;
        }
    }
    let margin = (SUPPORT_MARGIN_S / spec.dt).round() as usize;
    let out = (first.saturating_sub(margin), (last + margin).min(cols - 1));
    if std::env::var_os("OVERHEAD_DEBUG_SUPPORT").is_some() {
        eprintln!(
            "[support] tc {:.0} s: line seen {:.0}..{:.0} s, scored {:.0}..{:.0} s of the map's {:.0}..{:.0} s",
            fb.tc,
            spec.col_time(first),
            spec.col_time(last),
            spec.col_time(out.0),
            spec.col_time(out.1),
            spec.col_time(0),
            spec.col_time(cols - 1)
        );
    }
    out
}

pub enum Observed {
    Spectrogram {
        map: ScoreMap,
        harmonics: Vec<f64>,
        /// First and last column that count ([`line_support`]).
        support: (usize, usize),
        /// Score of the stage-1 fit on this map.
        reference: f64,
        /// [`Observed::score_bound`].
        bound: f64,
    },
    Model {
        flyby: Flyby,
        t0: f64,
        t1: f64,
    },
    /// Points traced by hand (`Detection::trace`): the score is the mean,
    /// over the points, of a Gaussian of the distance between the point and
    /// the modelled curve ([`TRACE_SIGMA`], [`TRACE_TAU`]; 1 = through every
    /// point), as in the reference video for a line too faint to score by
    /// brightness.
    Trace {
        /// `(t, f)` sorted by time.
        points: Vec<(f64, f64)>,
        /// First model sample time (s); samples every 0.25·stride s.
        t0: f64,
        t1: f64,
    },
}

impl Observed {
    /// Spectrogram neighbourhood of a detection covering all its lines.
    pub fn from_audio(audio: &Audio, det: &Detection) -> Self {
        let fb = det.flyby;
        let c = DEFAULT_C;
        let hs: Vec<f64> = if det.harmonics.is_empty() { vec![1.0] } else { det.harmonics.clone() };
        let (hmin, hmax) = hs.iter().fold((f64::INFINITY, 0.0f64), |(a, b), h| (a.min(*h), b.max(*h)));
        let v = det.ranges.v.1.max(fb.v).min(0.8 * c);
        let f_lo = (fb.f0 * hmin * c / (c + v) * 0.97).max(10.0);
        let f_hi = (fb.f0 * hmax * c / (c - v) * 1.03).min(0.48 * audio.sample_rate);
        let t0 = (det.region.t_start - 30.0).max(0.0);
        let t1 = (det.region.t_end + 30.0).min(audio.duration());
        let region = Region { t_start: t0, t_end: t1, f_min: f_lo, f_max: f_hi };
        let cfg = FitConfig::default();
        let map = fit::region_score_map(audio, &region, &cfg);
        let support = if no_support() { (0, map.spec.cols.saturating_sub(1)) } else { line_support(&map, &fb, &hs, c) };
        let mut obs = Observed::Spectrogram { map, harmonics: hs, support, reference: 0.0, bound: f64::INFINITY };
        let reference = {
            let Observed::Spectrogram { map, .. } = &obs else { unreachable!() };
            let times: Vec<f64> = (0..map.spec.cols).map(|c| map.spec.col_time(c)).collect();
            let k: Vec<f64> = times.iter().map(|t| fb.freq_at(*t, c) / fb.f0).collect();
            obs.raw_score(fb.f0, &k, 1).0
        };
        if let Observed::Spectrogram { map, reference: r, bound, .. } = &mut obs {
            *r = reference;
            // the brightest map value (every column's mean over the lines is at
            // most that, up to the f32 rounding of the interpolation) relative
            // to the stage-1 fit, with some to spare
            let peak = map.spec.db.iter().fold(map.baseline.max(0.0), |a, &v| a.max(v as f64));
            let denom = (reference - map.baseline).max(0.05);
            *bound = ((peak * (1.0 + 1e-6) - map.baseline) / denom).max(0.0) * (1.0 + 1e-9) + 1e-9;
            if !(peak.is_finite() && reference.is_finite()) || map.spec.db.iter().any(|v| v.is_nan()) {
                // (a NaN score would count in full: no bound)
                *bound = f64::INFINITY;
            }
        }
        obs
    }

    pub fn from_detection(det: &Detection) -> Self {
        match Self::from_trace(det) {
            Some(o) => o,
            None => Observed::Model { flyby: det.flyby, t0: det.region.t_start, t1: det.region.t_end },
        }
    }

    /// The traced points of a detection, if it has at least two.
    pub fn from_trace(det: &Detection) -> Option<Self> {
        let mut points: Vec<(f64, f64)> =
            det.trace.as_ref()?.iter().filter(|p| p[0].is_finite() && p[1] > 0.0).map(|p| (p[0], p[1])).collect();
        if points.len() < 2 {
            return None;
        }
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (t0, t1) = (points[0].0 - TRACE_PAD, points[points.len() - 1].0 + TRACE_PAD);
        Some(Observed::Trace { points, t0, t1 })
    }

    /// What the matcher scores a detection against: its traced points when
    /// it has them, else the recording's spectrogram when given, else its
    /// own fitted curve.
    pub fn for_detection(audio: Option<&Audio>, det: &Detection) -> Self {
        if let Some(o) = Self::from_trace(det) {
            return o;
        }
        match audio {
            Some(a) => Self::from_audio(a, det),
            None => Self::from_detection(det),
        }
    }

    /// The point `(recording time, frequency)` of the detection's line that
    /// a modelled curve is laid through (its rest frequency is that
    /// frequency over the model's Doppler factor at that time): stage 1's
    /// `(tc, f0)` on the spectrogram, the traced point nearest `tc` for a
    /// traced line; none for the model-only score, which compares curve
    /// shapes and does not use a rest frequency.
    pub fn anchor_point(&self, det: &Detection) -> Option<(f64, f64)> {
        let tc = det.flyby.tc;
        match self {
            Observed::Spectrogram { .. } => Some((tc, det.flyby.f0)),
            Observed::Trace { points, .. } => points.iter().copied().min_by(|a, b| (a.0 - tc).abs().total_cmp(&(b.0 - tc).abs())),
            Observed::Model { .. } => None,
        }
    }

    /// Recording times at which the model curve must be evaluated.
    pub fn times(&self, stride: usize) -> Vec<f64> {
        match self {
            Observed::Spectrogram { map, .. } => {
                (0..map.spec.cols).step_by(stride.max(1)).map(|c| map.spec.col_time(c)).collect()
            }
            Observed::Model { t0, t1, .. } | Observed::Trace { t0, t1, .. } => {
                let dt = 0.25 * stride.max(1) as f64;
                let n = ((t1 - t0) / dt).ceil() as usize;
                (0..=n).map(|i| t0 + i as f64 * dt).collect()
            }
        }
    }

    /// Best `(score, f0 multiplier)` of the curve `f0·m·k(t)`; `k` sampled at
    /// `times(stride)`.
    fn raw_score(&self, f0: f64, k: &[f64], stride: usize) -> (f64, f64) {
        // Every sweep value `m` is accumulated over the columns in the same
        // order as a per-`m` loop would, so the sums are bit-identical; the
        // loops are only swapped so that each spectrogram column (and each
        // model point) is visited once for the whole sweep.
        let mut ms = [0.0f64; NM];
        for (j, m) in ms.iter_mut().enumerate() {
            *m = 1.0 - F0_SWEEP + j as f64 * F0_STEP;
        }
        // (padded: the lanes past NM are never read)
        let mut totals = [0.0f64; NMP];
        let denom = match self {
            Observed::Spectrogram { map, harmonics, support, .. } => {
                let spec = &map.spec;
                let mut cnt = 0usize;
                let nh = harmonics.len() as f64;
                // f0·m for every sweep value (the curve is f0·m·k, evaluated left to right)
                let mut fm = [f64::NAN; NMP];
                for j in 0..NM {
                    fm[j] = f0 * ms[j];
                }
                let kernel = sweep_kernel(spec.rows);
                for (i, col) in (0..spec.cols).step_by(stride.max(1)).enumerate() {
                    let Some(&kk) = k.get(i) else { break };
                    // No model there (beyond the ADS-B track), or the column
                    // lies outside the time the detection's line was seen in
                    // (see `support`): it counts at the noise floor, so a
                    // curve that is only known over part of the window cannot
                    // score as if its known part were all of it, and nothing
                    // that sounds long before or after the fly-by counts for
                    // any curve.
                    if !kk.is_finite() || col < support.0 || col > support.1 {
                        for t in totals.iter_mut() {
                            *t += map.baseline;
                        }
                        cnt += 1;
                        continue;
                    }
                    let mut acc = [0.0f64; NMP];
                    kernel(spec.column(col), spec.f_min, spec.df, &fm, kk, harmonics, &mut acc);
                    add_mean(&mut totals, &acc, nh);
                    cnt += 1;
                }
                cnt.max(1) as f64
            }
            Observed::Model { flyby, t0, .. } => {
                let dt = 0.25 * stride.max(1) as f64;
                let sigma = MODEL_SIGMA;
                for (i, kk) in k.iter().enumerate() {
                    if !kk.is_finite() {
                        // no model there: contributes nothing (the kernel's floor)
                        continue;
                    }
                    let t = t0 + i as f64 * dt;
                    let kd = flyby.freq_at(t, DEFAULT_C) / flyby.f0;
                    for (tot, &m) in totals.iter_mut().zip(&ms) {
                        let e = (m * kk / kd).ln() / sigma;
                        *tot += (-0.5 * e * e).exp();
                    }
                }
                k.len().max(1) as f64
            }
            Observed::Trace { points, t0, .. } => {
                let dt = 0.25 * stride.max(1) as f64;
                let (sigma, tau) = (TRACE_SIGMA, TRACE_TAU);
                let lm: Vec<f64> = ms.iter().map(|m| m.ln() / sigma).collect();
                let win = 4.0 * tau + dt;
                let mut d2 = [f64::INFINITY; NM];
                for &(t, f) in points {
                    // distance from the point to the model curve in (time / tau,
                    // ln f / sigma): the nearest segment between model samples
                    // within a few tau; a point with no model nearby (beyond the
                    // track) counts 0
                    let lo = (((t - win - t0) / dt).floor().max(0.0)) as usize;
                    let hi = (((t + win - t0) / dt).ceil().max(0.0)) as usize;
                    d2.fill(f64::INFINITY);
                    let lf = (f / f0).ln();
                    for i in lo..hi.min(k.len().saturating_sub(1)) {
                        let (a, b) = (k[i], k[i + 1]);
                        if !(a.is_finite() && b.is_finite() && a > 0.0 && b > 0.0) {
                            continue;
                        }
                        let ax = (t0 + i as f64 * dt - t) / tau;
                        let bx = ax + dt / tau;
                        let (ay, by) = ((a.ln() - lf) / sigma, (b.ln() - lf) / sigma);
                        let (ex, ey) = (bx - ax, by - ay);
                        let ee = ex * ex + ey * ey;
                        for (j, &o) in lm.iter().enumerate() {
                            // segment shifted by the sweep: A + (0, o) .. B + (0, o)
                            let (px, py) = (-ax, -(ay + o));
                            let u = if ee > 0.0 { ((px * ex + py * ey) / ee).clamp(0.0, 1.0) } else { 0.0 };
                            let (qx, qy) = (px - u * ex, py - u * ey);
                            let q = qx * qx + qy * qy;
                            if q < d2[j] {
                                d2[j] = q;
                            }
                        }
                    }
                    for (tot, q) in totals.iter_mut().zip(&d2) {
                        if q.is_finite() {
                            *tot += (-0.5 * q).exp();
                        }
                    }
                }
                points.len().max(1) as f64
            }
        };
        let mut best = (f64::NEG_INFINITY, 1.0);
        for (tot, &m) in totals[..NM].iter().zip(&ms) {
            let s = tot / denom;
            if s > best.0 {
                best = (s, m);
            }
        }
        best
    }

    /// Relative score (≈1 = as good as the stage-1 fit) and the best f0 multiplier.
    /// An upper bound on [`Self::score`] for any curve: the brightest map
    /// value (every column's mean over the lines is at most that) relative to
    /// the stage-1 fit; 1 for the model and the traced points (means of
    /// Gaussians). With a little to spare for rounding.
    pub fn score_bound(&self) -> f64 {
        match self {
            Observed::Spectrogram { bound, .. } => *bound,
            Observed::Model { .. } | Observed::Trace { .. } => 1.0 + 1e-9,
        }
    }

    pub fn score(&self, f0: f64, k: &[f64], stride: usize) -> (f64, f64) {
        let (s, m) = self.raw_score(f0, k, stride);
        match self {
            Observed::Spectrogram { map, reference, .. } => {
                let denom = (reference - map.baseline).max(0.05);
                ((s - map.baseline) / denom, m)
            }
            Observed::Model { .. } | Observed::Trace { .. } => (s, m),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kernel this machine can run (the scalar one first).
    fn kernels() -> Vec<(&'static str, SweepKernel)> {
        let mut out: Vec<(&'static str, SweepKernel)> = vec![("scalar", sweep_scalar)];
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("avx2") {
                out.push(("avx2", sweep_avx2));
            }
            if std::arch::is_x86_feature_detected!("avx512f")
                && std::arch::is_x86_feature_detected!("avx512vl")
                && std::arch::is_x86_feature_detected!("avx512dq")
            {
                out.push(("avx512", sweep_avx512));
            }
        }
        out
    }

    /// The vector kernels must give exactly the scalar kernel's bits: short
    /// sweeps read from registers, long ones (high lines on a fine map)
    /// gathered, curves running off either edge of the map, NaN in the map.
    #[test]
    fn sweep_kernels_agree() {
        let mut s = 0x2545f4914f6cdd1du64;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let ks = kernels();
        for case in 0..6000 {
            let big = case % 4 == 3;
            let rows = 1 + (rnd() * if big { 4000.0 } else { 300.0 }) as usize;
            let mut column: Vec<f32> = (0..rows).map(|_| (rnd() * 8.0 - 2.0) as f32).collect();
            if case % 50 == 7 {
                column[rows / 2] = f32::NAN;
            }
            let df = if case % 5 == 0 { 8000.0 / 8192.0 } else { 0.05 + 1.5 * rnd() };
            let f_min = 20.0 + rnd() * 50.0;
            // some curves run off either edge of the map
            let f0 = f_min + (rnd() * 1.2 - 0.1) * rows as f64 * df * 0.6;
            let mut fm = [f64::NAN; NMP];
            for j in 0..NM {
                fm[j] = f0 * (1.0 - F0_SWEEP + j as f64 * F0_STEP);
            }
            let kk = 0.8 + 0.4 * rnd();
            let harmonics: Vec<f64> = match case % 4 {
                0 => vec![1.0],
                1 => vec![1.0, 2.0, 3.0],
                2 => vec![1.0, 1.5, 2.0, 2.5, 4.0],
                _ => vec![0.5, 1.0, 2.0, 3.0, 5.0, 8.0, 13.0, 18.0],
            };
            let mut want = [0.0; NMP];
            sweep_scalar(&column, f_min, df, &fm, kk, &harmonics, &mut want);
            for (name, k) in &ks[1..] {
                let mut got = [0.0; NMP];
                k(&column, f_min, df, &fm, kk, &harmonics, &mut got);
                for j in 0..NM {
                    assert_eq!(want[j].to_bits(), got[j].to_bits(), "{name} case {case} j {j}: {} vs {}", want[j], got[j]);
                }
            }
        }
    }

    /// The mean over the lines is added exactly as `t += a / nh`.
    #[test]
    fn add_mean_is_bit_identical() {
        let mut s = 0x51_7c_c1_b7_27_22_0a_95u64;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        for case in 0..20_000 {
            let nh = (1 + case % 12) as f64;
            let acc: [f64; NMP] = std::array::from_fn(|_| rnd() * 6.0 * nh);
            let start: [f64; NMP] = std::array::from_fn(|_| rnd() * 3000.0);
            let mut want = start;
            for (t, a) in want.iter_mut().zip(&acc) {
                *t += a / nh;
            }
            let mut got = start;
            add_mean(&mut got, &acc, nh);
            for j in 0..NMP {
                assert_eq!(want[j].to_bits(), got[j].to_bits(), "case {case} j {j}");
            }
        }
    }

    /// The division-free quotient is the correctly rounded one.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn exact_division_without_dividing() {
        if !std::arch::is_x86_feature_detected!("avx512f") {
            return;
        }
        use std::arch::x86_64::*;
        let mut s = 0x9e3779b97f4a7c15u64;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let rates = [8000.0, 4000.0, 11025.0, 16000.0, 22050.0, 44100.0, 48000.0, 7999.0, 3000.0];
        for case in 0..400_000 {
            let d = if case % 2 == 0 { rates[case % rates.len()] / 8192.0 } else { 0.01 + 10.0 * rnd() };
            let mut x = [0.0f64; 8];
            for v in x.iter_mut() {
                *v = (rnd() * 2.0 - 0.1) * 5000.0 * d;
            }
            // SAFETY: avx512f checked above
            let q: [f64; 8] = unsafe {
                let r = div_exact_512(_mm512_loadu_pd(x.as_ptr()), _mm512_set1_pd(d), _mm512_set1_pd(1.0 / d));
                std::mem::transmute(r)
            };
            for (xi, qi) in x.iter().zip(&q) {
                assert_eq!((xi / d).to_bits(), qi.to_bits(), "{xi} / {d}");
            }
        }
    }

    /// A curve is laid through the traced point nearest the fitted `tc`;
    /// the model-only score has no such point.
    #[test]
    fn anchor_point_of_a_trace_and_of_the_model() {
        let fb = Flyby { f0: 110.0, v: 55.0, d: 800.0, tc: 300.0 };
        let det = traced(&fb);
        let (t, f) = Observed::for_detection(None, &det).anchor_point(&det).expect("a traced point");
        assert!((t - det.flyby.tc).abs() <= 3.0 && (f - fb.freq_at(t, DEFAULT_C)).abs() < 1e-9, "{t} {f}");
        let mut plain = det.clone();
        plain.trace = None;
        assert!(Observed::for_detection(None, &plain).anchor_point(&plain).is_none());
    }

    /// On the spectrogram: the fly-by's own curve scores about 1; a curve
    /// through the detection's point whose closest approach is a second or
    /// more late scores clearly less; a tone that sounds a minute before the
    /// line is seen, under the flat end of a curve, does not count.
    #[test]
    fn spectrogram_score_follows_timing_and_counts_where_the_line_is_seen() {
        use overhead_core::synth::{SynthFlyby, synthesize};
        let fb = Flyby { f0: 120.0, v: 70.0, d: 500.0, tc: 150.0 };
        let c = DEFAULT_C;
        let sr = 8000.0;
        let mut audio = synthesize(sr, 300.0, &[SynthFlyby { flyby: fb, harmonics: vec![1.0], amplitude: 0.06 }], 0.3, &[], 5);
        // a tone 2 Hz above the approach frequency, 60 to 110 s before the
        // closest approach, wandering too much to be taken for a level tone
        let tone = fb.f_approach(c) + 2.0;
        for (i, x) in audio.samples.iter_mut().enumerate() {
            let t = i as f64 / sr;
            if (40.0..90.0).contains(&t) {
                *x += (0.2 * (2.0 * std::f64::consts::PI * (tone + 0.9 * (t / 3.0).sin()) * t).sin()) as f32;
            }
        }
        let mut det = traced(&fb);
        det.trace = None;
        det.region = Region { t_start: 60.0, t_end: 240.0, f_min: 80.0, f_max: 170.0 };
        let obs = Observed::from_audio(&audio, &det);
        let (t_first, t_last) = match &obs {
            Observed::Spectrogram { map, support, .. } => (map.spec.col_time(support.0), map.spec.col_time(support.1)),
            _ => panic!("spectrogram"),
        };
        assert!(t_first > 95.0 && t_first < 140.0 && t_last > 160.0, "support {t_first}..{t_last}");
        let times = obs.times(1);
        let curve = |p: &Flyby| -> Vec<f64> { times.iter().map(|t| p.freq_at(*t, c) / p.f0).collect() };
        let k = curve(&fb);
        let (own, _) = obs.score(fb.f0, &k, 1);
        assert!((own - 1.0).abs() < 0.05, "own curve: {own}");
        let mut last = own;
        for late_s in [1.0, 2.0, 3.0] {
            let late = Flyby { tc: fb.tc + late_s, ..fb };
            // (the rest frequency of a late curve through the detection's point)
            let f0_late = fb.f0 / (late.freq_at(fb.tc, c) / late.f0);
            let (r, _) = obs.score(f0_late, &curve(&late), 1);
            assert!(r < last - 0.1, "{late_s} s late: {r} after {last}");
            last = r;
        }
        assert!(last < 0.2, "3 s late: {last}");
        // a slow, distant pass whose almost level curve lies along the tone:
        // nothing where the line is seen, so it scores nothing; with every
        // column counted the tone would make it a match
        let level = Flyby { f0: tone, v: 6.0, d: 6000.0, tc: fb.tc };
        let (on_tone, _) = obs.score(level.f0, &curve(&level), 1);
        assert!(on_tone < 0.05, "the tone before the line counted: {on_tone}");
        let mut all = obs;
        if let Observed::Spectrogram { map, support, reference, .. } = &mut all {
            *support = (0, map.spec.cols - 1);
            // (the scale stays the fit's score within the support: only what
            // the level curve collects changes)
            let _ = reference;
        }
        let (unwindowed, _) = all.score(level.f0, &curve(&level), 1);
        assert!(unwindowed > on_tone + 0.1, "the tone counts {unwindowed} with every column, {on_tone} within the support");
    }

    fn traced(fb: &Flyby) -> Detection {
        let pts: Vec<[f64; 2]> = (-5..=5).map(|k| fb.tc + 6.0 * k as f64).map(|t| [t, fb.freq_at(t, DEFAULT_C)]).collect();
        let mut d = overhead_core::edit::trace_detection(&pts, DEFAULT_C).unwrap();
        d.flyby.f0 = fb.f0;
        d
    }

    /// A model curve through the traced points scores about 1; a curve off by a
    /// few seconds or steeper scores far less; samples beyond the track count 0.
    #[test]
    fn trace_score_is_distance_to_the_points() {
        let fb = Flyby { f0: 110.0, v: 55.0, d: 800.0, tc: 300.0 };
        let det = traced(&fb);
        let obs = Observed::for_detection(None, &det);
        assert!(matches!(obs, Observed::Trace { .. }));
        let times = obs.times(1);
        let k_of = |p: &Flyby| -> Vec<f64> { times.iter().map(|t| p.freq_at(*t, DEFAULT_C) / p.f0).collect() };
        let (s_true, m) = obs.score(fb.f0, &k_of(&fb), 1);
        assert!(s_true > 0.97, "{s_true}");
        assert!((m - 1.0).abs() < 1e-3, "{m}");
        let late = Flyby { tc: fb.tc + 4.0, ..fb };
        let (s_late, _) = obs.score(fb.f0, &k_of(&late), 1);
        assert!(s_late < 0.75, "{s_late}");
        let steep = Flyby { d: 350.0, ..fb };
        let (s_steep, _) = obs.score(fb.f0, &k_of(&steep), 1);
        assert!(s_steep < 0.75, "{s_steep}");
        // a 0.8 % lower model rest frequency is absorbed by the f0 sweep
        let (s_f0, m_f0) = obs.score(fb.f0 / 1.008, &k_of(&fb), 1);
        assert!(s_f0 > 0.95 && (m_f0 - 1.008).abs() < 1e-3, "{s_f0} {m_f0}");
        // the model is unknown over the second half: those points count 0
        let mut k = k_of(&fb);
        for (kk, t) in k.iter_mut().zip(&times) {
            if *t > fb.tc + 1.0 {
                *kk = f64::NAN;
            }
        }
        let (s_half, _) = obs.score(fb.f0, &k, 1);
        assert!(s_half > 0.4 && s_half < 0.6, "{s_half}");
        // stride: still every point scored
        let t2 = obs.times(4);
        let k2: Vec<f64> = t2.iter().map(|t| fb.freq_at(*t, DEFAULT_C) / fb.f0).collect();
        assert!(obs.score(fb.f0, &k2, 4).0 > 0.95);
    }

    /// The distance is taken across the curve: points clicked a second late
    /// on the steep part near the closest approach still count as on the
    /// line, while the same points judged by frequency alone would not.
    #[test]
    fn trace_distance_is_across_the_curve() {
        let fb = Flyby { f0: 110.0, v: 60.0, d: 300.0, tc: 300.0 };
        // steep: about 3 %/s at the closest approach
        let slope = (fb.freq_at(fb.tc + 0.5, DEFAULT_C) - fb.freq_at(fb.tc - 0.5, DEFAULT_C)).abs() / fb.f0;
        assert!(slope > 0.02, "{slope}");
        let pts: Vec<[f64; 2]> = [-3.0, -2.0, -1.0, 0.0, 1.0, 2.0, 3.0].iter().map(|dt| [fb.tc + dt + 1.0, fb.freq_at(fb.tc + dt, DEFAULT_C)]).collect();
        let mut det = traced(&fb);
        det.trace = Some(pts);
        let obs = Observed::from_trace(&det).unwrap();
        let times = obs.times(1);
        let k: Vec<f64> = times.iter().map(|t| fb.freq_at(*t, DEFAULT_C) / fb.f0).collect();
        let (s, _) = obs.score(fb.f0, &k, 1);
        assert!(s > 0.75, "1 s late on the steep part: {s}");
        // vertically those points are several percent off the curve
        let off = (fb.freq_at(fb.tc + 1.0, DEFAULT_C) / fb.freq_at(fb.tc, DEFAULT_C)).ln().abs();
        assert!(off > 2.0 * TRACE_SIGMA, "{off}");
        // a curve far from the points scores nothing
        let far = Flyby { tc: fb.tc + 20.0, ..fb };
        let kf: Vec<f64> = times.iter().map(|t| far.freq_at(*t, DEFAULT_C) / far.f0).collect();
        assert!(obs.score(fb.f0, &kf, 1).0 < 0.1);
    }

    /// Without a trace nothing changes: model or spectrogram as before.
    #[test]
    fn no_trace_keeps_the_old_observation() {
        let fb = Flyby { f0: 110.0, v: 55.0, d: 800.0, tc: 300.0 };
        let mut det = traced(&fb);
        det.trace = None;
        assert!(matches!(Observed::for_detection(None, &det), Observed::Model { .. }));
        assert!(matches!(Observed::from_detection(&det), Observed::Model { .. }));
    }
}
