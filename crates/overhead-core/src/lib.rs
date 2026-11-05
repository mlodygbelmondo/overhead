//! Core of Overhead: audio decoding, spectrograms, the retarded-time
//! Doppler model of an aircraft fly-by, and fitting that model to recordings.
//! Independent of any UI so the later ADS-B geolocation stage can reuse it.
//! Also: suggesting where a recording was cut ([`cuts`]) and listening to it
//! with a band boost ([`listen`]).

pub mod audio;
pub mod background;
pub mod colormap;
pub mod comb;
pub mod cuts;
pub mod detect;
pub mod edit;
pub mod fit;
pub mod listen;
pub mod model;
pub mod spectrogram;
pub mod synth;

pub use audio::Audio;
pub use fit::{FitConfig, FitResult, Region};
pub use model::Flyby;
