//! Doppler model of a source moving in a straight line at constant speed.
//!
//! The frequency heard at time `t` depends on the source's radial velocity at
//! the moment it *emitted* the sound that arrives at `t` (retarded time), not on
//! where the source is at `t`. Using the instantaneous position produces a
//! visibly different curve, not merely a delayed one.
//!
//! Geometry: the source passes the observer at minimum (slant) distance `d`.
//! With `u` = emission time relative to the emission-time CPA, the range is
//! `R(u) = sqrt(d² + v²u²)` and the radial velocity is `v_r = v²u / R`
//! (positive = receding). Hearing time `t` and emission time satisfy
//! `t - t_e = u + R(u)/c`, a quadratic in `u` with a closed-form root.

use serde::{Deserialize, Serialize};

/// Speed of sound in dry air at `temp_c` degrees Celsius (m/s).
pub fn speed_of_sound(temp_c: f64) -> f64 {
    331.3 * (1.0 + temp_c / 273.15).sqrt()
}

/// Default speed of sound (≈ 20 °C).
pub const DEFAULT_C: f64 = 343.0;

/// Parameters of one fly-by as seen in a recording.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Flyby {
    /// Rest (source) frequency of the tracked line (Hz).
    pub f0: f64,
    /// Source speed (m/s).
    pub v: f64,
    /// Slant distance at closest point of approach (m).
    pub d: f64,
    /// Recording time (s) at which sound emitted at CPA is heard ("delayed CPA").
    pub tc: f64,
}

impl Flyby {
    /// Emission time (recording timeline) of the CPA.
    pub fn t_emit_cpa(&self, c: f64) -> f64 {
        self.tc - self.d / c
    }

    /// Emission-time offset `u` (s, relative to CPA emission) of the sound heard at `t`.
    pub fn emission_offset(&self, t: f64, c: f64) -> f64 {
        let big_t = t - self.t_emit_cpa(c);
        let (c2, v2) = (c * c, self.v * self.v);
        let disc = c2 * v2 * big_t * big_t + self.d * self.d * (c2 - v2);
        (c2 * big_t - disc.max(0.0).sqrt()) / (c2 - v2)
    }

    /// Radial velocity (m/s, + receding) of the source when it emitted the sound heard at `t`.
    pub fn radial_velocity(&self, t: f64, c: f64) -> f64 {
        let u = self.emission_offset(t, c);
        let r = (self.d * self.d + self.v * self.v * u * u).sqrt();
        if r == 0.0 { 0.0 } else { self.v * self.v * u / r }
    }

    /// Range (m) between observer and source at emission of the sound heard at `t`.
    pub fn emission_range(&self, t: f64, c: f64) -> f64 {
        let u = self.emission_offset(t, c);
        (self.d * self.d + self.v * self.v * u * u).sqrt()
    }

    /// Frequency (Hz) heard at recording time `t` (retarded-time model).
    pub fn freq_at(&self, t: f64, c: f64) -> f64 {
        self.f0 * c / (c + self.radial_velocity(t, c))
    }

    /// The naive model that uses the source position at hearing time. Wrong;
    /// kept to show the difference.
    pub fn naive_freq_at(&self, t: f64, c: f64) -> f64 {
        let u = t - self.tc;
        let r = (self.d * self.d + self.v * self.v * u * u).sqrt();
        let vr = if r == 0.0 { 0.0 } else { self.v * self.v * u / r };
        self.f0 * c / (c + vr)
    }

    /// Asymptotic frequency while approaching from far away.
    pub fn f_approach(&self, c: f64) -> f64 {
        self.f0 * c / (c - self.v)
    }

    /// Asymptotic frequency while receding far away.
    pub fn f_recede(&self, c: f64) -> f64 {
        self.f0 * c / (c + self.v)
    }

    /// Frequency slope at `tc` (Hz/s, negative). This is the steepest point.
    pub fn slope_at_cpa(&self, c: f64) -> f64 {
        -self.f0 * self.v * self.v / (c * self.d)
    }

    /// Recover `(f0, v)` from the two asymptotic frequencies.
    pub fn from_asymptotes(f_approach: f64, f_recede: f64, c: f64) -> (f64, f64) {
        let v = c * (f_approach - f_recede) / (f_approach + f_recede);
        let f0 = 2.0 * f_approach * f_recede / (f_approach + f_recede);
        (f0, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: f64 = 343.0;

    fn p() -> Flyby {
        Flyby { f0: 120.0, v: 80.0, d: 1500.0, tc: 100.0 }
    }

    #[test]
    fn heard_cpa_is_rest_frequency() {
        let p = p();
        assert!((p.freq_at(p.tc, C) - p.f0).abs() < 1e-9);
        assert!(p.emission_offset(p.tc, C).abs() < 1e-9);
    }

    #[test]
    fn emission_time_satisfies_propagation_equation() {
        let p = p();
        for t in [0.0, 50.0, 95.0, 100.0, 104.0, 180.0, 400.0] {
            let u = p.emission_offset(t, C);
            let r = p.emission_range(t, C);
            let heard = p.t_emit_cpa(C) + u + r / C;
            assert!((heard - t).abs() < 1e-6, "t={t} heard={heard}");
        }
    }

    #[test]
    fn asymptotes_and_roundtrip() {
        let p = p();
        assert!((p.freq_at(-1e6, C) - p.f_approach(C)).abs() < 1e-3);
        assert!((p.freq_at(1e6, C) - p.f_recede(C)).abs() < 1e-3);
        let (f0, v) = Flyby::from_asymptotes(p.f_approach(C), p.f_recede(C), C);
        assert!((f0 - p.f0).abs() < 1e-9 && (v - p.v).abs() < 1e-9);
    }

    #[test]
    fn slope_matches_numeric_derivative() {
        let p = p();
        let h = 1e-3;
        let numeric = (p.freq_at(p.tc + h, C) - p.freq_at(p.tc - h, C)) / (2.0 * h);
        assert!((numeric - p.slope_at_cpa(C)).abs() < 1e-4, "{numeric}");
    }

    #[test]
    fn naive_model_differs_from_retarded() {
        let p = p();
        let max_diff = (0..200)
            .map(|i| {
                let t = i as f64;
                (p.freq_at(t, C) - p.naive_freq_at(t, C)).abs()
            })
            .fold(0.0, f64::max);
        assert!(max_diff > 1.0, "{max_diff}");
    }
}
