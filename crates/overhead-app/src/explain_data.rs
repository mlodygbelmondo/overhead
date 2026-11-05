//! What the analysis views show, computed from a finished search's
//! [`overhead_adsb::explain::Session`] in the background and kept with the
//! result: the per-fly-by scores of every refined place (the reference
//! video's scatter), the coarse and fine score maps (its heat maps), the
//! modelled curves of a candidate (its spectrogram overlays) and what a
//! place would have heard (its silence filter). Nothing here searches again.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use overhead_adsb::explain::{CoarseMap, EventScores, FineMap, FlightChoice, Hypothesis, ModelCurve, ScoreMapOptions, Session, Silence};

/// One piece of analysis. Candidates are 0-based indices into the result's
/// list (the session counts them from 1); `Probe` is a place the user picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Want {
    Events,
    Coarse,
    Fine(usize),
    Curves(usize),
    Silence(usize),
    Probe,
}

/// A place checked by hand with the flights of a candidate.
#[derive(Clone, Debug)]
pub struct Probe {
    pub lat: f64,
    pub lon: f64,
    /// The candidate whose flights and start it takes.
    pub from: usize,
    pub curves: Vec<ModelCurve>,
    pub silence: Option<Silence>,
    /// Sum of the curve scores (what the place would score without silence).
    pub score: f64,
    pub error: Option<String>,
}

pub enum Got {
    Events(EventScores),
    Coarse(CoarseMap),
    Fine(usize, Option<FineMap>),
    Curves(usize, Vec<ModelCurve>),
    Silence(usize, Option<Silence>),
    Probe(Probe),
}

pub struct Explain {
    pub session: Arc<Session>,
    /// Session detection `k` is the app's fly-by `det_index[k]`.
    pub det_index: Vec<usize>,
    pub events: Option<Arc<EventScores>>,
    pub coarse: Option<Arc<CoarseMap>>,
    pub fine: HashMap<usize, Option<Arc<FineMap>>>,
    pub curves: HashMap<usize, Arc<Vec<ModelCurve>>>,
    pub silence: HashMap<usize, Option<Arc<Silence>>>,
    pub probe: Option<Arc<Probe>>,
    /// Asked for a place: (lat, lon, from candidate).
    pub probe_at: Option<(f64, f64, usize)>,
    pub running: HashSet<Want>,
}

impl Explain {
    pub fn new(session: Arc<Session>, det_index: Vec<usize>) -> Self {
        Self {
            session,
            det_index,
            events: None,
            coarse: None,
            fine: HashMap::new(),
            curves: HashMap::new(),
            silence: HashMap::new(),
            probe: None,
            probe_at: None,
            running: HashSet::new(),
        }
    }

    /// Already there (or being computed)?
    pub fn has(&self, w: Want) -> bool {
        self.running.contains(&w)
            || match w {
                Want::Events => self.events.is_some(),
                Want::Coarse => self.coarse.is_some(),
                Want::Fine(k) => self.fine.contains_key(&k),
                Want::Curves(k) => self.curves.contains_key(&k),
                Want::Silence(k) => self.silence.contains_key(&k),
                Want::Probe => self.probe_at.is_none() || self.probe.as_ref().is_some_and(|p| Some((p.lat, p.lon, p.from)) == self.probe_at),
            }
    }

    pub fn store(&mut self, g: Got) {
        match g {
            Got::Events(e) => {
                self.running.remove(&Want::Events);
                self.events = Some(Arc::new(e));
            }
            Got::Coarse(c) => {
                self.running.remove(&Want::Coarse);
                self.coarse = Some(Arc::new(c));
            }
            Got::Fine(k, m) => {
                self.running.remove(&Want::Fine(k));
                self.fine.insert(k, m.map(Arc::new));
            }
            Got::Curves(k, c) => {
                self.running.remove(&Want::Curves(k));
                self.curves.insert(k, Arc::new(c));
            }
            Got::Silence(k, s) => {
                self.running.remove(&Want::Silence(k));
                self.silence.insert(k, s.map(Arc::new));
            }
            Got::Probe(p) => {
                self.running.remove(&Want::Probe);
                self.probe = Some(Arc::new(p));
            }
        }
    }

    /// The app's fly-by index of a session detection.
    pub fn app_det(&self, k: usize) -> usize {
        self.det_index.get(k).copied().unwrap_or(k)
    }
}

/// Radius of the silence check: twice the distance a light aircraft is
/// always heard within (as `locate --explain` uses).
fn silence_radius(s: &Session) -> f64 {
    2.0 * s.quiet_d()
}

/// Do one piece of work (slow ones take a second or two: run off the UI thread).
pub fn compute(s: &Session, w: Want, probe_at: Option<(f64, f64, usize)>) -> Got {
    match w {
        Want::Events => Got::Events(s.event_scores()),
        Want::Coarse => Got::Coarse(s.coarse_map()),
        Want::Fine(k) => Got::Fine(k, s.fine_map(&ScoreMapOptions { candidate: k + 1, ..Default::default() })),
        Want::Curves(k) => Got::Curves(k, s.candidate_curves(k + 1)),
        Want::Silence(k) => Got::Silence(k, s.candidate_silence(k + 1, Some(silence_radius(s)))),
        Want::Probe => {
            let (lat, lon, from) = probe_at.unwrap_or((0.0, 0.0, 0));
            Got::Probe(probe(s, lat, lon, from))
        }
    }
}

/// Check a place by hand: the flights of candidate `from` with their passes
/// found again at the new place, its start, and what the place would have heard.
pub fn probe(s: &Session, lat: f64, lon: f64, from: usize) -> Probe {
    let Some(h0) = s.candidate_hypothesis(from + 1) else {
        return Probe { lat, lon, from, curves: vec![], silence: None, score: 0.0, error: Some("no candidate to take the flights from".into()) };
    };
    let h = Hypothesis {
        lat,
        lon,
        starts: h0.starts.clone(),
        c0: h0.c0,
        // the pass heard nearest each fly-by at this place
        flights: h0.flights.iter().map(|f| FlightChoice { detection: f.detection, flight: f.flight.clone(), t_cpa_unix: None }).collect(),
    };
    match s.check(&h) {
        Ok(c) => {
            let score = c.curves.iter().map(|m| m.score).sum();
            Probe { lat, lon, from, curves: c.curves, silence: Some(c.silence), score, error: None }
        }
        Err(e) => Probe { lat, lon, from, curves: vec![], silence: None, score: 0.0, error: Some(format!("{e:#}")) },
    }
}
