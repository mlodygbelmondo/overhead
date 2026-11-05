//! Retarded-time propagation from a moving source to a fixed receiver.
//!
//! Sound heard at time `t` left the source at the emission time `τ` solving
//! `g(τ) = τ + |p(τ) − o| / c − t = 0`. Since `g'(τ) = 1 + v_r/c` with radial
//! velocity `|v_r| ≤ |v| < c`, `g` is strictly increasing and Newton's method
//! converges from any nearby guess. The heard signal is the emitted one
//! evaluated at `τ(t)`, so a tone of phase `φ(τ)` is heard with frequency
//! `f · dτ/dt = f · c / (c + v_r)`: the Doppler shift falls out of the
//! geometry instead of being imposed.

use crate::trajectory::{Trajectory, V3, dot, norm, sub};

#[derive(Clone, Copy, Debug)]
pub struct Ray {
    /// Emission time (s).
    pub tau: f64,
    /// Source–receiver range at emission (m).
    pub range: f64,
    /// Radial velocity at emission (m/s, + = receding).
    pub v_radial: f64,
    /// Sine of the source's elevation angle seen from the receiver.
    pub sin_elev: f64,
}

impl Ray {
    /// dτ/dt: how fast emission time advances per second of hearing time.
    pub fn dtau_dt(&self, c: f64) -> f64 {
        c / (c + self.v_radial)
    }
}

/// Solve the retarded time for hearing time `t`, starting from `guess` if given.
pub fn solve(traj: &Trajectory, o: V3, c: f64, t: f64, guess: Option<f64>) -> Ray {
    let mut tau = match guess {
        Some(g) => g,
        None => {
            // two fixed-point steps from the instantaneous position
            let mut tau = t - norm(sub(traj.state(t).0, o)) / c;
            for _ in 0..2 {
                tau = t - norm(sub(traj.state(tau).0, o)) / c;
            }
            tau
        }
    };
    let mut ray = Ray { tau, range: 0.0, v_radial: 0.0, sin_elev: 0.0 };
    for _ in 0..30 {
        let (p, v) = traj.state(tau);
        let r_vec = sub(p, o);
        let range = norm(r_vec).max(1e-9);
        let v_radial = dot(r_vec, v) / range;
        let g = tau + range / c - t;
        let step = g / (1.0 + v_radial / c);
        ray = Ray { tau, range, v_radial, sin_elev: r_vec[2] / range };
        tau -= step;
        if step.abs() < 1e-11 {
            break;
        }
    }
    ray
}
