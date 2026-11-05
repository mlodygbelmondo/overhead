//! Synthetic benchmark for the automatic fly-by detector of overhead-core.
//!
//! * [`trajectory`] + [`propagate`]: 3D source motion and the retarded-time
//!   solution `t = τ + |p(τ) − o| / c` that turns it into what a microphone hears.
//! * [`source`]: moving sources (aircraft, vehicles) rendered through that
//!   propagation with spherical spreading, air absorption and a ground reflection.
//! * [`ambient`]: interference recorded alongside (hum, machinery, wind, speech,
//!   birds, rain, dogs, ambient floor).
//! * [`scenario`]: seeded scenes with ground truth; [`chain`]: AGC, clipping and
//!   lossy codecs; [`eval`]: matching detections to truth and reporting.

pub mod ambient;
pub mod chain;
pub mod dsp;
pub mod eval;
pub mod propagate;
pub mod rng;
pub mod scenario;
pub mod source;
pub mod trajectory;
