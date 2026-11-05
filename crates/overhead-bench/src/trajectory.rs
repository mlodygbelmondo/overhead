//! Source trajectories in a local east/north/up frame (metres, z above sea level).
//!
//! A trajectory is a kinematic state table on a uniform emission-time grid.
//! Between samples, position is the cubic Hermite interpolant of the sampled
//! positions and velocities, and velocity is that interpolant's derivative, so
//! the two are always consistent. Hermite is exact for constant-velocity
//! motion, so a straight fly-by needs only two samples.

use serde::{Deserialize, Serialize};

pub type V3 = [f64; 3];

pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}

#[derive(Clone, Debug)]
pub struct Trajectory {
    pub t0: f64,
    pub dt: f64,
    pub pos: Vec<V3>,
    pub vel: Vec<V3>,
}

impl Trajectory {
    /// Motion with constant velocity `vel`, at `p_ref` at time `t_ref`, over `[t0, t1]`.
    pub fn straight(p_ref: V3, vel: V3, t_ref: f64, t0: f64, t1: f64) -> Self {
        let at = |t: f64| add(p_ref, scale(vel, t - t_ref));
        Self { t0, dt: (t1 - t0).max(1e-9), pos: vec![at(t0), at(t1)], vel: vec![vel, vel] }
    }

    pub fn stationary(p: V3) -> Self {
        Self { t0: 0.0, dt: 1.0, pos: vec![p], vel: vec![[0.0; 3]] }
    }

    /// Integrate a flight plan over `[t0, t1]` with step `dt`, forward and
    /// backward from the plan's reference time. Altitude is kept above `z_floor`.
    pub fn from_plan(plan: &FlightPlan, t0: f64, t1: f64, dt: f64, z_floor: f64) -> Self {
        let n_back = ((plan.t_ref - t0) / dt).ceil().max(0.0) as usize;
        let n_fwd = ((t1 - plan.t_ref) / dt).ceil().max(0.0) as usize;
        let start = plan.t_ref - n_back as f64 * dt;
        let mut pos = vec![[0.0; 3]; n_back + n_fwd + 1];
        let mut vel = vec![[0.0; 3]; n_back + n_fwd + 1];
        for dir in [1.0f64, -1.0] {
            let (mut p, mut heading, mut speed) = (plan.p_ref, plan.heading, plan.speed);
            let steps = if dir > 0.0 { n_fwd } else { n_back };
            let mut t = plan.t_ref;
            for s in 0..=steps {
                let idx = if dir > 0.0 { n_back + s } else { n_back - s };
                let c = plan.controls(t);
                let vz = if p[2] <= z_floor && c.climb * dir < 0.0 { 0.0 } else { c.climb };
                let v = [speed * heading.cos(), speed * heading.sin(), vz];
                pos[idx] = p;
                vel[idx] = v;
                if s == steps {
                    break;
                }
                // midpoint step with piecewise-constant controls
                let h = dir * dt;
                let cm = plan.controls(t + 0.5 * h);
                let hm = heading + 0.5 * h * cm.turn_rate;
                let sm = (speed + 0.5 * h * cm.accel).max(5.0);
                let vzm = if p[2] <= z_floor && cm.climb * dir < 0.0 { 0.0 } else { cm.climb };
                p = add(p, scale([sm * hm.cos(), sm * hm.sin(), vzm], h));
                p[2] = p[2].max(z_floor);
                heading += h * cm.turn_rate;
                speed = (speed + h * cm.accel).max(5.0);
                t += h;
            }
        }
        Self { t0: start, dt, pos, vel }
    }

    pub fn span(&self) -> (f64, f64) {
        (self.t0, self.t0 + self.dt * (self.pos.len() - 1) as f64)
    }

    /// Position and velocity at time `t`; constant-velocity extrapolation outside the table.
    #[inline]
    pub fn state(&self, t: f64) -> (V3, V3) {
        let n = self.pos.len();
        if n == 1 {
            return (add(self.pos[0], scale(self.vel[0], t - self.t0)), self.vel[0]);
        }
        let x = (t - self.t0) / self.dt;
        if x <= 0.0 {
            return (add(self.pos[0], scale(self.vel[0], t - self.t0)), self.vel[0]);
        }
        if x >= (n - 1) as f64 {
            let tl = self.t0 + self.dt * (n - 1) as f64;
            return (add(self.pos[n - 1], scale(self.vel[n - 1], t - tl)), self.vel[n - 1]);
        }
        let i = x.floor() as usize;
        let u = x - i as f64;
        let h = self.dt;
        let (u2, u3) = (u * u, u * u * u);
        let (h00, h10, h01, h11) = (2.0 * u3 - 3.0 * u2 + 1.0, u3 - 2.0 * u2 + u, -2.0 * u3 + 3.0 * u2, u3 - u2);
        let (d00, d10, d01, d11) = (6.0 * u2 - 6.0 * u, 3.0 * u2 - 4.0 * u + 1.0, -6.0 * u2 + 6.0 * u, 3.0 * u2 - 2.0 * u);
        let (p0, p1, v0, v1) = (self.pos[i], self.pos[i + 1], self.vel[i], self.vel[i + 1]);
        let mut p = [0.0; 3];
        let mut v = [0.0; 3];
        for k in 0..3 {
            p[k] = h00 * p0[k] + h10 * h * v0[k] + h01 * p1[k] + h11 * h * v1[k];
            v[k] = (d00 * p0[k] + d01 * p1[k]) / h + d10 * v0[k] + d11 * v1[k];
        }
        (p, v)
    }

    /// Emission time and range of the global closest approach to `o` within `[t0, t1]`.
    pub fn closest_approach(&self, o: V3, t0: f64, t1: f64) -> (f64, f64) {
        let range = |t: f64| norm(sub(self.state(t).0, o));
        let step = 0.25;
        let n = ((t1 - t0) / step).ceil().max(1.0) as usize;
        let (mut best_t, mut best_r) = (t0, f64::INFINITY);
        for i in 0..=n {
            let t = (t0 + i as f64 * step).min(t1);
            let r = range(t);
            if r < best_r {
                (best_t, best_r) = (t, r);
            }
        }
        // golden-section refinement around the coarse minimum
        let (mut a, mut b) = ((best_t - step).max(t0), (best_t + step).min(t1));
        let g = 0.5 * (5f64.sqrt() - 1.0);
        for _ in 0..80 {
            let (c, d) = (b - g * (b - a), a + g * (b - a));
            if range(c) < range(d) { b = d } else { a = c }
        }
        let t = 0.5 * (a + b);
        (t, range(t))
    }
}

/// Piecewise-constant flight controls active over `[t_start, t_end)` (absolute
/// emission time). Outside all maneuvers the source flies straight and level
/// at constant speed.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Maneuver {
    pub t_start: f64,
    pub t_end: f64,
    /// Heading change rate (rad/s, + = counter-clockwise seen from above).
    pub turn_rate: f64,
    /// Along-track acceleration (m/s²).
    pub accel: f64,
    /// Vertical speed (m/s, + = climbing).
    pub climb: f64,
}

/// A flight defined by its state at a reference time plus maneuvers before and after.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlightPlan {
    pub t_ref: f64,
    pub p_ref: V3,
    /// Heading at `t_ref` (rad, mathematical: 0 = east, π/2 = north).
    pub heading: f64,
    pub speed: f64,
    pub maneuvers: Vec<Maneuver>,
}

impl FlightPlan {
    fn controls(&self, t: f64) -> Maneuver {
        let mut c = Maneuver::default();
        for m in &self.maneuvers {
            if t >= m.t_start && t < m.t_end {
                c.turn_rate += m.turn_rate;
                c.accel += m.accel;
                c.climb += m.climb;
            }
        }
        c
    }
}
