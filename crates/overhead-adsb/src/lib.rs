//! Stage 2 of Overhead: geolocating a recording by matching its Doppler
//! fly-by detections against historical ADS-B tracks from adsb.lol.
//!
//! * [`archive`] / [`cache`]: fetch one day of adsb.lol `globe_history`,
//!   keep only propeller/rotor (and unknown-type) aircraft, query by region.
//! * [`trace`], [`types`]: readsb trace files and aircraft classes.
//! * [`geo`], [`geoid`], [`dem`], [`region`]: geodesy, terrain height, user inputs.
//! * [`flight`]: an ADS-B track as a smooth 3D trajectory, with the heard
//!   (retarded-time) Doppler curve for any ground point.
//! * [`matcher`]: candidate ground points per detection, combination of
//!   several detections by their time offsets, the silence filter, and full
//!   Doppler-curve scoring of the best hypotheses.
//! * [`repeat`]: repeated passes of one flight (circuits) count less unless independent.
//! * [`silence`]: how likely an aircraft passing a place was to be heard, and what silence costs.
//! * [`ambiguity`]: relative probabilities of the places that fit, and the plausible area.
//! * [`locate`]: the whole pipeline, including recordings with cuts.
//! * [`explain`]: what a finished search can show besides its answer: score
//!   maps, per-fly-by scores by flight assignment, modelled curves of any
//!   flights, silence (flights the place would have heard), map links.
//! * [`links`]: Google Maps, Street View and OpenStreetMap URLs for a place.
//! * [`e2e`]: synthetic end-to-end validation on real ADS-B days.

pub mod ambiguity;
pub mod archive;
pub mod cache;
pub mod dem;
pub mod e2e;
pub mod explain;
pub mod flight;
pub mod geo;
pub mod geoid;
pub mod http;
pub mod links;
pub mod locate;
pub mod matcher;
pub mod region;
pub mod repeat;
pub mod score;
pub mod silence;
pub mod trace;
pub mod types;
