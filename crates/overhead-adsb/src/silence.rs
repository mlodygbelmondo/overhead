//! Silence: how likely stage 1 was to detect an aircraft that passed a
//! candidate place while the recording ran, and what not detecting it costs.
//!
//! A place where a propeller aircraft passed close by while nothing was heard
//! is less likely to be the recording's place, but how much less depends on
//! how close the pass was, what flew it and how loud the recording's
//! background was at the time: an unheard light single at 300 m is hard to
//! explain, one at 4 km is ordinary. [`Model::p_detect`] gives the
//! probability that stage 1 detects such a pass, and [`Model::cost`] what an
//! unheard pass costs a hypothesis: `min(cap, -weight · ln(1 - p))`, in the
//! units of the score. `weight` 0.15 is half the ambiguity temperature (which
//! reads score differences as log-likelihoods), and `cap` 0.2 holds one
//! unheard flight below a fifth of a matched detection: the model rests on
//! 22 real passes, takes passes as independent, and stage 1 also misses
//! close passes for reasons it does not know (the synthetic end-to-end
//! cases, whose renderer has its own level model, miss about one piston
//! pass in ten within 2 km), so silence must rank places that are otherwise
//! alike without overturning a place whose curves fit clearly better.
//!
//! **Calibration** (real recordings only, never the synthetic renderer,
//! whose level model is its own): every pass of a candidate aircraft within
//! 6 km of the true place of the 7 recordings with an ADS-B ground truth
//! (Oosterschelde, Amer, Vlissingen, both Kampina clips, Berlin, Moerdijk;
//! exact start, `examples/silence_cal.rs`), passes of one aircraft less than
//! 120 s apart taken as one, against "stage 1 has a detection within 30 s"
//! (what the matcher counts as explained). Engine aircraft (piston,
//! turboprop): 10 of 10 detected within 3 km, 3 of 4 at 3–4.5 km, 0 of 4 at
//! 5.2–5.9 km. A logistic in log distance with `p_max` 0.95 fits best with
//! `d50` ≈ 4.0 km for any slope `k` from 3 to 10 (log-likelihood −5.5 to
//! −4.2); `k` = 4 is taken (a ±6 dB spread of source level and propagation).
//! Twins and turboprops: twice the power, `d50` × 1.4 (two of two detected,
//! one at 4.4 km). Helicopters: 1 of 2 within 1.4 km (the main rotor's
//! fundamental is below the band stage 1 searches), so `p_max` 0.6 and half
//! the distance. Unknown types (microlights, gliders with ADS-B, ...): 0 of
//! 1 at 1.6 km, `p_max` 0.3. The background, measured as below, did not
//! predict detection on these recordings (all within −6.7…+2.5 dB of their median;
//! with a 20·log10 range scaling the fit got worse, −7.5 against −5.0), so
//! only a background clearly louder than usual (more than
//! [`Model::bg_grace_db`] above the recording's median) shortens the range.
//!
//! The background is measured on the recording ([`Background`]): per 5 s,
//! the spectrogram floor in 60–1000 Hz (the band of the propeller lines)
//! relative to the recording's own median floor, and for each window the
//! 20th percentile of that over 1–10 min before and after it, so that the
//! aircraft itself (it raises the floor by 10–20 dB while it passes) does not
//! count. Recordings are not calibrated, so only this relative level can be
//! compared between recordings.

use overhead_core::audio::Audio;
use overhead_core::spectrogram::{self, SpecRequest};
use serde::{Deserialize, Serialize};

use crate::types::{self, AircraftClass};

const F_LO: f64 = 60.0;
const F_HI: f64 = 1000.0;
/// Column width of the floor measurement (s) and window of one value (columns).
const COL_S: f64 = 0.5;
const WINDOW_COLS: usize = 10;
/// Background around a time: windows this far before and after it (s).
const CONTEXT_NEAR: f64 = 60.0;
const CONTEXT_FAR: f64 = 600.0;
const CONTEXT_Q: f64 = 0.2;

/// The spectrogram floor of a recording over time.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Background {
    /// Length of one window (s); window `i` covers `[i·step, (i+1)·step)`.
    pub step_s: f64,
    /// Per window: floor (median over 60–1000 Hz of each column, its 20th
    /// percentile over the window) minus the recording's median floor (dB).
    pub floor_rel_db: Vec<f32>,
    /// Per window: the background around it, the 20th percentile of
    /// `floor_rel_db` over the windows 60–600 s before and after it (dB).
    pub context_db: Vec<f32>,
}

impl Background {
    pub fn measure(audio: &Audio) -> Self {
        let duration = audio.duration();
        let f_hi = F_HI.min(audio.sample_rate * 0.45);
        let cols = (duration / COL_S).floor() as usize;
        if cols < WINDOW_COLS || f_hi <= F_LO {
            return Self::default();
        }
        let n_fft = ((0.128 * audio.sample_rate).log2().round().exp2() as usize).max(64);
        let spec = spectrogram::compute(
            audio,
            &SpecRequest { t_start: 0.0, t_end: cols as f64 * COL_S, f_min: F_LO, f_max: f_hi, columns: cols, n_fft, whiten: false },
        );
        let col_floor: Vec<f32> = (0..spec.cols)
            .map(|c| {
                let mut v = spec.column(c).to_vec();
                v.sort_by(f32::total_cmp);
                v[v.len() / 2]
            })
            .collect();
        let floor: Vec<f32> = col_floor
            .chunks(WINDOW_COLS)
            .map(|w| {
                let mut v = w.to_vec();
                quantile(&mut v, 0.2)
            })
            .collect();
        Self::from_floor(COL_S * WINDOW_COLS as f64, &floor)
    }

    /// From absolute per-window floors (dB).
    pub fn from_floor(step_s: f64, floor: &[f32]) -> Self {
        if floor.is_empty() {
            return Self::default();
        }
        let med = quantile(&mut floor.to_vec(), 0.5);
        let rel: Vec<f32> = floor.iter().map(|f| f - med).collect();
        let near = (CONTEXT_NEAR / step_s).round() as usize;
        let far = (CONTEXT_FAR / step_s).round() as usize;
        let mut buf = Vec::new();
        let context_db = (0..rel.len())
            .map(|i| {
                buf.clear();
                for j in i.saturating_sub(far)..(i + far + 1).min(rel.len()) {
                    if i.abs_diff(j) >= near {
                        buf.push(rel[j]);
                    }
                }
                if buf.is_empty() {
                    // a recording shorter than the context: the window itself
                    0.0
                } else {
                    quantile(&mut buf, CONTEXT_Q)
                }
            })
            .collect();
        Self { step_s, floor_rel_db: rel, context_db }
    }

    /// Background around recording time `t` (dB above the recording's
    /// median floor; `None` without a measurement).
    pub fn at(&self, t: f64) -> Option<f64> {
        if self.context_db.is_empty() || self.step_s <= 0.0 || !t.is_finite() {
            return None;
        }
        let i = ((t / self.step_s).floor().max(0.0) as usize).min(self.context_db.len() - 1);
        Some(self.context_db[i] as f64)
    }
}

fn quantile(v: &mut [f32], p: f64) -> f32 {
    v.sort_by(f32::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// What a pass's source is, as far as being heard goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Loudness {
    /// Single piston engine (most light aircraft).
    PistonSingle,
    /// Several piston engines.
    PistonTwin,
    Turboprop,
    Rotor,
    /// Electric, or no type known (microlights, gliders with ADS-B, ...).
    Other,
}

impl Loudness {
    pub fn of(class: AircraftClass, type_code: Option<&str>) -> Self {
        match class {
            AircraftClass::Piston => {
                let engines = type_code.and_then(types::description).and_then(|d| d.as_bytes().get(1).copied());
                match engines {
                    Some(b'2'..=b'8') => Self::PistonTwin,
                    _ => Self::PistonSingle,
                }
            }
            AircraftClass::Turboprop => Self::Turboprop,
            AircraftClass::Rotor => Self::Rotor,
            _ => Self::Other,
        }
    }

    /// Range factor on `d50` and the most a pass can be detected (`p_max`).
    fn params(self) -> (f64, f64) {
        match self {
            Self::PistonSingle => (1.0, 0.95),
            Self::PistonTwin | Self::Turboprop => (1.4, 0.95),
            Self::Rotor => (0.5, 0.6),
            Self::Other => (1.0, 0.3),
        }
    }
}

/// Detection probability of a pass and the cost of not hearing it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Model {
    /// Slant range (m) at which a single piston engine is detected with half
    /// its `p_max`, and the slope of the logistic in log distance.
    pub d50_m: f64,
    pub k: f64,
    /// Background more than this above the recording's median floor (dB)
    /// shortens the range by 20·log10.
    pub bg_grace_db: f64,
    /// Cost of an unheard pass = `min(cap, weight · −ln(1 − p))` (score units).
    pub weight: f64,
    pub cap: f64,
    /// Passes costing less than this are not counted (and set the radius searched).
    pub min_cost: f64,
}

impl Default for Model {
    fn default() -> Self {
        Self { d50_m: 4000.0, k: 4.0, bg_grace_db: 3.0, weight: 0.15, cap: 0.2, min_cost: 0.03 }
    }
}

impl Model {
    /// Probability that stage 1 detects a pass at slant range `slant_m` of a
    /// `loud` source, with the background `bg_db` (dB above the recording's
    /// median floor, [`Background::at`]).
    pub fn p_detect(&self, slant_m: f64, loud: Loudness, bg_db: Option<f64>) -> f64 {
        let (range, p_max) = loud.params();
        let excess = (bg_db.unwrap_or(0.0) - self.bg_grace_db).clamp(0.0, 30.0);
        let d50 = self.d50_m * range * 10f64.powf(-excess / 20.0);
        p_max / (1.0 + (slant_m.max(0.0) / d50).powf(self.k))
    }

    /// What an unheard pass with detection probability `p` costs (score units).
    pub fn cost(&self, p: f64) -> f64 {
        let c = (-self.weight * (1.0 - p.clamp(0.0, 0.999)).ln()).min(self.cap);
        if c < self.min_cost { 0.0 } else { c }
    }

    /// Slant range beyond which no pass costs anything (any class, quiet background).
    pub fn radius_m(&self) -> f64 {
        [Loudness::PistonSingle, Loudness::PistonTwin, Loudness::Turboprop, Loudness::Rotor, Loudness::Other]
            .iter()
            .map(|l| {
                let (range, p_max) = l.params();
                // cost(p) = min_cost  <=>  p = 1 - exp(-min_cost / weight)
                let p = 1.0 - (-self.min_cost / self.weight).exp();
                if p >= p_max { 0.0 } else { self.d50_m * range * (p_max / p - 1.0).powf(1.0 / self.k) }
            })
            .fold(0.0, f64::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_unheard_pass_costs_much_far_one_little() {
        let m = Model::default();
        let p = |d: f64| m.p_detect(d, Loudness::PistonSingle, None);
        let (p300, p1600, p3000, p5000) = (p(300.0), p(1600.0), p(3000.0), p(5000.0));
        assert!(p300 > 0.94 && p1600 > 0.9 && p1600 < p300 && p3000 > 0.7 && p5000 < 0.35, "{p300} {p1600} {p3000} {p5000}");
        // close by: the cap; then falling with distance
        assert_eq!(m.cost(p300), m.cap);
        assert_eq!(m.cost(p1600), m.cap);
        let (c3, c4, c5) = (m.cost(p3000), m.cost(p(4000.0)), m.cost(p5000));
        assert!(c3 < m.cap && c3 > 0.15 && c4 < 0.6 * c3 && c5 < 0.6 * c4 && c5 > 0.0, "{c3} {c4} {c5}");
        // unknown types (microlights, gliders) are often unheard: little cost even close by
        let other = m.cost(m.p_detect(1600.0, Loudness::Other, None));
        assert!(other < 0.3 * m.cap, "{other}");
        // helicopters cost less than a piston single at the same range
        assert!(m.p_detect(1000.0, Loudness::Rotor, None) < m.p_detect(1000.0, Loudness::PistonSingle, None));
        // twins carry further
        assert!(m.p_detect(5000.0, Loudness::PistonTwin, None) > p5000);
        // nothing costs anything beyond the radius
        let r = m.radius_m();
        for l in [Loudness::PistonSingle, Loudness::PistonTwin, Loudness::Turboprop, Loudness::Rotor, Loudness::Other] {
            assert_eq!(m.cost(m.p_detect(r + 1.0, l, None)), 0.0, "{l:?}");
        }
        assert!(r > 5000.0 && r < 12_000.0, "{r}");
    }

    #[test]
    fn loud_background_shortens_the_range() {
        let m = Model::default();
        let quiet = m.p_detect(3000.0, Loudness::PistonSingle, Some(0.0));
        let usual = m.p_detect(3000.0, Loudness::PistonSingle, Some(2.5));
        let loud = m.p_detect(3000.0, Loudness::PistonSingle, Some(15.0));
        assert_eq!(quiet, usual);
        assert!(loud < 0.5 * quiet, "{loud} {quiet}");
    }

    #[test]
    fn classes() {
        assert_eq!(Loudness::of(AircraftClass::Piston, Some("C172")), Loudness::PistonSingle);
        assert_eq!(Loudness::of(AircraftClass::Piston, Some("DA62")), Loudness::PistonTwin);
        assert_eq!(Loudness::of(AircraftClass::Piston, None), Loudness::PistonSingle);
        assert_eq!(Loudness::of(AircraftClass::Turboprop, Some("PC12")), Loudness::Turboprop);
        assert_eq!(Loudness::of(AircraftClass::Rotor, Some("EC35")), Loudness::Rotor);
        assert_eq!(Loudness::of(AircraftClass::Unknown, None), Loudness::Other);
    }

    #[test]
    fn background_context_leaves_out_the_pass_itself() {
        // 5 s windows over 30 min: a steady floor, a 20 dB aircraft for 40 s
        // at 15 min, and a 12 dB louder last 10 min (a generator starts)
        let floor: Vec<f32> = (0..360)
            .map(|i| {
                let t = i as f64 * 5.0;
                let mut f = -60.0;
                if (880.0..920.0).contains(&t) {
                    f += 20.0;
                }
                if t >= 1200.0 {
                    f += 12.0;
                }
                f
            })
            .collect();
        let b = Background::from_floor(5.0, &floor);
        // during the aircraft: still the steady background
        assert!(b.at(900.0).unwrap().abs() < 0.5, "{:?}", b.at(900.0));
        // well inside the loud stretch: 12 dB up
        assert!((b.at(1750.0).unwrap() - 12.0).abs() < 0.5, "{:?}", b.at(1750.0));
        assert!(Background::default().at(10.0).is_none());
    }
}
