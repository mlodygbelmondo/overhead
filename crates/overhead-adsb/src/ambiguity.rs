//! How sure a location is: the distinct places that fit about as well as
//! the best one, their relative probabilities, and a coarse "plausible area".
//!
//! The matcher refines a few hundred hypotheses; every one that matches at
//! least one detection is an [`Alternative`]. They are grouped into distinct
//! places (greedy, best first: an alternative within `distinct_m` of a
//! better place belongs to it) and each place gets the probability
//!
//! ```text
//! p_i = exp((s_i - s_best) / T) / Z,   Z = sum over places + exp((s_else - s_best) / T)
//! ```
//!
//! i.e. the score is read as a log-likelihood at temperature `T` (a softmax).
//! `s_else` is the score of "somewhere the search did not refine": a chance
//! place elsewhere in the region. It matters when the best score is low (one
//! fly-by, ≈0.5–1.3) and vanishes for several matched fly-bys (score 3+).
//! `T` and `s_else` were fitted on the synthetic end-to-end runs and the real
//! final runs (see the README section "How sure is the answer"); the
//! calibration rests on a few dozen runs, so read the numbers as rough.
//!
//! Nothing here changes the ranking: #1, the scores and margins stay as the
//! matcher computed them.

use serde::{Deserialize, Serialize};

use crate::geo::{self, BBox, M_PER_DEG_LAT};
use crate::matcher::Candidate;

/// One refined hypothesis that matches at least one detection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Alternative {
    pub lat: f64,
    pub lon: f64,
    /// Curve score minus silence penalties and the repeat discount (the
    /// same number candidates are ranked by).
    pub score: f64,
    /// The same without the soft speed/distance evidence and the soft
    /// silence cost, with the repeat discount (the matcher's margin is the
    /// smallest of the margins); `None` = as `score`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plain_score: Option<f64>,
    /// What the repeat discount ([`crate::repeat`]) took off `score` and
    /// `plain_score` (the margin is also measured without it); `None` = nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_loss: Option<[f64; 2]>,
    pub start_unix: f64,
    /// Matched flights, "registration type" (or hex).
    pub flights: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AmbiguityConfig {
    /// Score temperature of the probability transform.
    pub temperature: f64,
    /// Score of an unrefined chance place elsewhere (`None` = no "elsewhere" mass).
    pub elsewhere_score: Option<f64>,
    /// Places closer than this are one place (m).
    pub distinct_m: f64,
    /// Places reported (the rest only count in the probabilities).
    pub max_places: usize,
    /// Margin of #1 from which the answer is called confident (the matcher's own rule).
    pub confident_margin: f64,
    /// "Fits similarly": score within this of the best (same scale as the margin).
    pub similar_delta: f64,
    /// Plausible area: grid cells with a place whose probability is at least
    /// this fraction of the best place's.
    pub area_rel: f64,
    /// Grid cell size (m); `None` = from the search box (1/40 of its larger side, 2–10 km).
    pub area_cell_m: Option<f64>,
}

impl Default for AmbiguityConfig {
    fn default() -> Self {
        Self {
            temperature: DEFAULT_TEMPERATURE,
            elsewhere_score: Some(DEFAULT_ELSEWHERE_SCORE),
            distinct_m: 2000.0,
            max_places: 30,
            confident_margin: 1.0,
            similar_delta: 1.0,
            area_rel: 0.05,
            area_cell_m: None,
        }
    }
}

/// Fitted on the synthetic e2e runs and the real final runs (README, "How sure is the answer").
pub const DEFAULT_TEMPERATURE: f64 = 0.30;
pub const DEFAULT_ELSEWHERE_SCORE: f64 = 2.0;

impl Alternative {
    /// Score without the soft speed/distance evidence.
    pub fn plain(&self) -> f64 {
        self.plain_score.unwrap_or(self.score)
    }

    /// The same without the repeat discount: (score, plain score).
    pub fn undiscounted(&self) -> Alternative {
        let [a, b] = self.repeat_loss.unwrap_or([0.0; 2]);
        Alternative { score: self.score + a, plain_score: Some(self.plain() + b), repeat_loss: None, ..self.clone() }
    }
}

/// A distinct place (more than `distinct_m` from every better one).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Place {
    pub lat: f64,
    pub lon: f64,
    /// Best score of the alternatives in this place.
    pub score: f64,
    pub probability: f64,
    /// Refined hypotheses that fell into this place.
    pub hypotheses: usize,
    pub start_unix: f64,
    pub flights: Vec<String>,
    /// Rank of the first reported candidate in this place, if any.
    pub candidate_rank: Option<usize>,
}

/// One cell of the plausible-area grid.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AreaCell {
    pub lat_min: f64,
    pub lon_min: f64,
    pub lat_max: f64,
    pub lon_max: f64,
    /// Probability-weighted centre of the places in the cell.
    pub lat: f64,
    pub lon: f64,
    pub best_score: f64,
    /// Total probability of the places in the cell.
    pub probability: f64,
    pub places: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ambiguity {
    /// "confident" (margin of #1 ≥ `confident_margin`) or "ambiguous".
    pub verdict: String,
    pub confident: bool,
    /// One line for people, e.g. "ambiguous: 14 places fit similarly".
    pub summary: String,
    pub margin: f64,
    pub best_score: f64,
    /// Distinct places with a score within `similar_delta` of the best (#1 included).
    pub similar_places: usize,
    /// exp(entropy) of the place probabilities: how many places the mass is really spread over.
    pub effective_places: f64,
    /// All distinct places found (only `max_places` are listed in `places`).
    pub total_places: usize,
    /// Probability of the best k places together, for k = 1, 3, 5, 10.
    pub top_mass: Vec<(usize, f64)>,
    /// Probability that the answer is none of the places found.
    pub elsewhere: f64,
    pub places: Vec<Place>,
    /// Plausible area: cells holding a place with at least `area_rel` of the best place's probability.
    pub area: Vec<AreaCell>,
    pub area_cell_m: f64,
    /// What would narrow the answer down (empty when confident).
    pub hints: Vec<String>,
    pub config: AmbiguityConfig,
}

/// Softmax of `scores / t`, with an optional extra "elsewhere" logit; returns
/// (probabilities of the scores, probability of elsewhere). Stable for any scale.
pub fn probabilities(scores: &[f64], t: f64, elsewhere: Option<f64>) -> (Vec<f64>, f64) {
    if scores.is_empty() {
        return (Vec::new(), if elsewhere.is_some() { 1.0 } else { 0.0 });
    }
    let t = t.max(1e-9);
    let top = scores.iter().copied().chain(elsewhere).fold(f64::NEG_INFINITY, f64::max);
    let w: Vec<f64> = scores.iter().map(|s| ((s - top) / t).exp()).collect();
    let we = elsewhere.map_or(0.0, |s| ((s - top) / t).exp());
    let z = w.iter().sum::<f64>() + we;
    (w.iter().map(|x| x / z).collect(), we / z)
}

/// Group alternatives (any order) into distinct places, best first: each
/// alternative joins the best place within `distinct_m`, else starts one.
/// Returns (place seeds as indices into `alts`, members per place).
pub fn cluster(alts: &[Alternative], distinct_m: f64) -> Vec<(usize, Vec<usize>)> {
    let mut order: Vec<usize> = (0..alts.len()).collect();
    order.sort_by(|a, b| alts[*b].score.total_cmp(&alts[*a].score));
    let mut places: Vec<(usize, Vec<usize>)> = Vec::new();
    for i in order {
        let a = &alts[i];
        match places.iter_mut().find(|(s, _)| geo::ground_distance(alts[*s].lat, alts[*s].lon, a.lat, a.lon) <= distinct_m) {
            Some((_, m)) => m.push(i),
            None => places.push((i, vec![i])),
        }
    }
    places
}

/// Index of the place (best first) a point belongs to, by the same rule as [`cluster`].
pub fn place_of(places: &[Place], lat: f64, lon: f64, distinct_m: f64) -> Option<usize> {
    places.iter().position(|p| geo::ground_distance(p.lat, p.lon, lat, lon) <= distinct_m)
}

/// Grid cell size for a search box: 1/40 of its larger side, 2–10 km, in 500 m steps.
pub fn auto_cell_m(b: &BBox) -> f64 {
    let lat_c = 0.5 * (b.lat_min + b.lat_max);
    let h = (b.lat_max - b.lat_min) * M_PER_DEG_LAT;
    let w = (b.lon_max - b.lon_min) * M_PER_DEG_LAT * lat_c.to_radians().cos().max(0.01);
    ((h.max(w) / 40.0).clamp(2000.0, 10_000.0) / 500.0).round() * 500.0
}

/// Plausible-area grid: places binned into `cell_m` cells (on a grid anchored
/// at the equator / Greenwich, longitude step from `lat_ref`), keeping cells
/// with a place of at least `rel` of the best place's probability.
pub fn area_grid(places: &[Place], cell_m: f64, lat_ref: f64, rel: f64) -> Vec<AreaCell> {
    let dlat = cell_m / M_PER_DEG_LAT;
    let dlon = cell_m / (M_PER_DEG_LAT * lat_ref.to_radians().cos().max(0.01));
    let p_best = places.iter().map(|p| p.probability).fold(0.0, f64::max);
    let mut cells: Vec<((i64, i64), AreaCell, f64)> = Vec::new();
    for p in places {
        let key = ((p.lat / dlat).floor() as i64, (p.lon / dlon).floor() as i64);
        let c = match cells.iter_mut().find(|c| c.0 == key) {
            Some(c) => c,
            None => {
                cells.push((
                    key,
                    AreaCell {
                        lat_min: key.0 as f64 * dlat,
                        lon_min: key.1 as f64 * dlon,
                        lat_max: (key.0 + 1) as f64 * dlat,
                        lon_max: (key.1 + 1) as f64 * dlon,
                        lat: 0.0,
                        lon: 0.0,
                        best_score: f64::NEG_INFINITY,
                        probability: 0.0,
                        places: 0,
                    },
                    0.0,
                ));
                cells.last_mut().unwrap()
            }
        };
        c.1.lat += p.lat * p.probability;
        c.1.lon += p.lon * p.probability;
        c.1.probability += p.probability;
        c.1.best_score = c.1.best_score.max(p.score);
        c.1.places += 1;
        c.2 = c.2.max(p.probability);
    }
    let mut out: Vec<AreaCell> = cells
        .into_iter()
        .filter(|c| c.2 >= rel * p_best && c.2 > 0.0)
        .map(|(_, mut c, _)| {
            if c.probability > 0.0 {
                c.lat /= c.probability;
                c.lon /= c.probability;
            }
            c
        })
        .collect();
    out.sort_by(|a, b| b.probability.total_cmp(&a.probability));
    out
}

/// What the matcher knows about near-equal places, from its alternatives and
/// the reported candidates. `None` without candidates.
pub fn assess(alts: &[Alternative], candidates: &[Candidate], search_box: &BBox, cfg: &AmbiguityConfig) -> Option<Ambiguity> {
    let best = candidates.first()?;
    let groups = cluster(alts, cfg.distinct_m);
    let scores: Vec<f64> = groups.iter().map(|g| alts[g.0].score).collect();
    let (probs, elsewhere) = probabilities(&scores, cfg.temperature, cfg.elsewhere_score);
    let mut places: Vec<Place> = groups
        .iter()
        .zip(&probs)
        .map(|((s, members), &p)| {
            let a = &alts[*s];
            Place {
                lat: a.lat,
                lon: a.lon,
                score: a.score,
                probability: p,
                hypotheses: members.len(),
                start_unix: a.start_unix,
                flights: a.flights.clone(),
                candidate_rank: None,
            }
        })
        .collect();
    for c in candidates {
        if let Some(i) = place_of(&places, c.lat, c.lon, cfg.distinct_m)
            && places[i].candidate_rank.is_none()
        {
            places[i].candidate_rank = Some(c.rank);
        }
    }
    let best_score = scores.first().copied().unwrap_or(best.score).max(best.score);
    let similar = scores.iter().filter(|s| **s >= best_score - cfg.similar_delta).count().max(1);
    let total: f64 = probs.iter().sum::<f64>() + elsewhere;
    let entropy: f64 = probs.iter().chain(std::iter::once(&elsewhere)).filter(|p| **p > 0.0).map(|p| -p * (p / total).ln()).sum();
    let top_mass = [1usize, 3, 5, 10].iter().map(|&k| (k, probs.iter().take(k).fold(0.0, |a, b| a + b))).collect();
    let cell_m = cfg.area_cell_m.unwrap_or_else(|| auto_cell_m(search_box));
    let area = area_grid(&places, cell_m, 0.5 * (search_box.lat_min + search_box.lat_max), cfg.area_rel);
    let confident = best.margin >= cfg.confident_margin;
    let summary = if confident {
        format!("confident: margin {:.2}, {:.0} % on #1", best.margin, 100.0 * probs.first().copied().unwrap_or(0.0))
    } else if similar > 1 {
        format!("ambiguous: {similar} places fit similarly (margin {:.2})", best.margin)
    } else {
        format!("ambiguous: weak evidence (margin {:.2})", best.margin)
    };
    let total_places = places.len();
    places.truncate(cfg.max_places);
    Some(Ambiguity {
        verdict: if confident { "confident" } else { "ambiguous" }.into(),
        confident,
        summary,
        margin: best.margin,
        best_score,
        similar_places: similar,
        effective_places: entropy.exp(),
        total_places,
        top_mass,
        elsewhere,
        places,
        area,
        area_cell_m: cell_m,
        hints: Vec::new(),
        config: cfg.clone(),
    })
}

/// Validation: probability given to places within `radius_m` of a known
/// point, and whether the point is in the plausible area: inside one of its
/// cells, or within `radius_m` of a place that put its cell there (a place
/// near a cell edge stands for ground on both sides of it).
pub fn truth_check(a: &Ambiguity, lat: f64, lon: f64, radius_m: f64) -> (f64, bool) {
    let near = |p: &Place| geo::ground_distance(p.lat, p.lon, lat, lon) <= radius_m;
    let mass = a.places.iter().filter(|p| near(p)).map(|p| p.probability).fold(0.0, |a, b| a + b);
    let p_best = a.places.iter().map(|p| p.probability).fold(0.0, f64::max);
    let inside = a.area.iter().any(|c| lat >= c.lat_min && lat < c.lat_max && lon >= c.lon_min && lon < c.lon_max)
        || a.places.iter().any(|p| near(p) && p.probability > 0.0 && p.probability >= a.config.area_rel * p_best);
    (mass, inside)
}

/// Hints on what would narrow an ambiguous answer down, from what was searched.
pub fn hints(start_span_s: f64, search_box: &BBox, matched_best: usize, detections: usize) -> Vec<String> {
    let mut h = Vec::new();
    if start_span_s > 15.0 * 60.0 {
        h.push("give the exact start time (to the minute)".to_string());
    }
    let lat_c = 0.5 * (search_box.lat_min + search_box.lat_max);
    let side = ((search_box.lat_max - search_box.lat_min) * M_PER_DEG_LAT)
        .max((search_box.lon_max - search_box.lon_min) * M_PER_DEG_LAT * lat_c.to_radians().cos().max(0.01));
    if side > 40_000.0 {
        h.push("give a smaller region (a town or a point with a radius)".to_string());
    }
    if matched_best < 3 {
        h.push(if detections < 3 {
            "a longer recording with more aircraft passing: each extra fly-by must fit the same place and start".to_string()
        } else {
            "only a few fly-bys could be matched; a longer recording with more aircraft would help".to_string()
        });
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alt(lat: f64, lon: f64, score: f64) -> Alternative {
        Alternative { lat, lon, score, plain_score: None, repeat_loss: None, start_unix: 0.0, flights: vec![] }
    }

    #[test]
    fn probabilities_are_a_softmax() {
        let (p, e) = probabilities(&[2.0, 1.0, 1.0], 0.5, None);
        assert_eq!(e, 0.0);
        assert!((p.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        // exp(2) : 1 : 1 at T = 0.5
        let r = (2.0f64).exp();
        assert!((p[0] - r / (r + 2.0)).abs() < 1e-12);
        assert!((p[1] - p[2]).abs() < 1e-15);
        // equal scores share equally; a hotter temperature flattens
        let (q, _) = probabilities(&[1.0, 1.0, 1.0, 1.0], 0.25, None);
        assert!(q.iter().all(|x| (x - 0.25).abs() < 1e-12));
        let (hot, _) = probabilities(&[2.0, 1.0], 10.0, None);
        let (cold, _) = probabilities(&[2.0, 1.0], 0.05, None);
        assert!(hot[0] < 0.6 && cold[0] > 0.999);
        // large scores do not overflow
        let (big, _) = probabilities(&[1000.0, 999.0], 0.01, None);
        assert!(big[0].is_finite() && (big[0] + big[1] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn elsewhere_takes_mass_from_weak_answers_only() {
        let (p, e) = probabilities(&[0.6], 0.25, Some(0.6));
        assert!((p[0] - 0.5).abs() < 1e-12 && (e - 0.5).abs() < 1e-12);
        let (p, e) = probabilities(&[4.5, 3.0], 0.25, Some(0.6));
        assert!(e < 1e-6 && (p[0] + p[1] + e - 1.0).abs() < 1e-12);
        let (p, e) = probabilities(&[], 0.25, Some(0.6));
        assert!(p.is_empty() && e == 1.0);
    }

    #[test]
    fn cluster_groups_within_distance_best_first() {
        // the best (1.2), one 600 m from it, one 10 km away, one 1.5 km from the best
        let a = vec![alt(52.0, 13.0, 1.0), alt(52.0054, 13.0, 1.2), alt(52.09, 13.0, 1.1), alt(52.0189, 13.0, 0.5)];
        let g = cluster(&a, 2000.0);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0], (1, vec![1, 0, 3]));
        assert_eq!(g[1], (2, vec![2]));
        // 2.1 km from the best is its own place
        let b = vec![alt(52.0, 13.0, 1.0), alt(52.0189, 13.0, 0.9)];
        assert_eq!(cluster(&b, 2000.0).len(), 2);
    }

    fn place(lat: f64, lon: f64, score: f64, p: f64) -> Place {
        Place { lat, lon, score, probability: p, hypotheses: 1, start_unix: 0.0, flights: vec![], candidate_rank: None }
    }

    #[test]
    fn area_grid_bins_and_thresholds() {
        let places = vec![
            place(52.020, 13.020, 1.0, 0.5),
            place(52.030, 13.030, 0.9, 0.3), // same 5 km cell as the first
            place(52.30, 13.30, 0.8, 0.15),
            place(52.60, 13.60, 0.1, 0.01), // below 5 % of the best place
        ];
        let cells = area_grid(&places, 5000.0, 52.0, 0.05);
        assert_eq!(cells.len(), 2);
        assert!((cells[0].probability - 0.8).abs() < 1e-12);
        assert_eq!(cells[0].places, 2);
        assert_eq!(cells[0].best_score, 1.0);
        // probability-weighted centre, inside the cell
        assert!(cells[0].lat > 52.020 && cells[0].lat < 52.030);
        assert!(cells[0].lat >= cells[0].lat_min && cells[0].lat <= cells[0].lat_max);
        assert!(cells[0].lon >= cells[0].lon_min && cells[0].lon <= cells[0].lon_max);
        assert!((cells[0].lat_max - cells[0].lat_min) * M_PER_DEG_LAT - 5000.0 < 1e-6);
        assert!((cells[1].probability - 0.15).abs() < 1e-12);
    }

    #[test]
    fn auto_cell_size() {
        let berlin = BBox { lat_min: 52.3, lat_max: 52.7, lon_min: 13.0, lon_max: 13.8 };
        assert_eq!(auto_cell_m(&berlin), 2000.0);
        let bavaria = BBox { lat_min: 47.2, lat_max: 50.6, lon_min: 8.9, lon_max: 13.9 };
        assert_eq!(auto_cell_m(&bavaria), 9500.0);
        assert_eq!(auto_cell_m(&BBox::WORLD), 10_000.0);
    }
}
