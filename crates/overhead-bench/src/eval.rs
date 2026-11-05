//! Scoring detections against ground truth.
//!
//! Detections and truth aircraft are matched one-to-one by heard-CPA time
//! (`|Δtc| ≤ tol`, closest pairs first). Recall counts only aircraft flagged
//! `expected_detectable`; a detection matched to any aircraft (detectable or
//! not) is a true positive. Each unmatched detection is attributed to the
//! nearest source in time/frequency: cost = `Δt/5 s + Δf/5 Hz`, with `Δf` the
//! distance of a detected line from the source's bands (for moving sources the
//! Doppler range of each harmonic) and `Δt = |tc − tc_truth|` for moving
//! sources, else the distance of `tc` from the source's active interval. A
//! false positive attributed to an aircraft that already has a match is a
//! `duplicate` (typically a mis-fitted harmonic of the same pass).

use std::collections::BTreeMap;

use overhead_core::detect::Detection;
use overhead_core::model::Flyby;
use serde::{Deserialize, Serialize};

use crate::scenario::{AircraftKind, Category, Truth};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchRecord {
    pub truth_id: usize,
    pub kind: AircraftKind,
    pub expected_detectable: bool,
    pub snr_db: f64,
    pub maneuver_near_cpa: bool,
    pub truth: Flyby,
    pub det: Flyby,
    pub contrast: f64,
    pub v_rel_err: f64,
    pub d_rel_err: f64,
    pub tc_err: f64,
    /// Error of the detected rest frequency against the nearest truth line.
    pub f0_err_hz: f64,
    pub f0_line: String,
    pub in_v: bool,
    pub in_d: bool,
    pub in_tc: bool,
    /// Detector flagged it as a likely ground vehicle (slow and close).
    pub flagged_ground: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissRecord {
    pub truth_id: usize,
    pub kind: AircraftKind,
    pub expected_detectable: bool,
    pub snr_db: f64,
    pub maneuver_near_cpa: bool,
    pub truth: Flyby,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FalsePositive {
    pub det: Flyby,
    pub contrast: f64,
    pub cause: Category,
    pub source_id: Option<usize>,
    pub source_kind: String,
    /// Second detection of an aircraft that already has a match.
    pub duplicate: bool,
    /// Distance to the attributed source (s, Hz).
    pub dt: Option<f64>,
    pub df: Option<f64>,
    /// Detector flagged it as a likely ground vehicle (slow and close).
    pub flagged_ground: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub name: String,
    pub duration: f64,
    pub codec: String,
    pub n_aircraft: usize,
    pub n_detectable: usize,
    pub n_detections: usize,
    pub matches: Vec<MatchRecord>,
    pub misses: Vec<MissRecord>,
    pub false_positives: Vec<FalsePositive>,
    pub synth_seconds: f64,
    pub detect_seconds: f64,
    pub error: Option<String>,
}

/// Distance from `x` to the interval `[lo, hi]`.
fn gap(x: f64, lo: f64, hi: f64) -> f64 {
    (lo - x).max(x - hi).max(0.0)
}

pub fn evaluate(truth: &Truth, dets: &[Detection], tol_tc: f64) -> ScenarioResult {
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (i, d) in dets.iter().enumerate() {
        for (j, a) in truth.aircraft.iter().enumerate() {
            let dt = (d.flyby.tc - a.flyby.tc).abs();
            if dt <= tol_tc {
                pairs.push((dt, i, j));
            }
        }
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut det_used = vec![false; dets.len()];
    let mut truth_used = vec![false; truth.aircraft.len()];
    let mut matches = Vec::new();
    for (_, i, j) in pairs {
        if det_used[i] || truth_used[j] {
            continue;
        }
        det_used[i] = true;
        truth_used[j] = true;
        let (d, a) = (&dets[i], &truth.aircraft[j]);
        let (t, p) = (a.flyby, d.flyby);
        // nearest pair of a heard line and a truth line (`p.f0` is the
        // fundamental, which need not be heard)
        let (mut best, mut label) = (f64::INFINITY, String::new());
        for f in heard_lines(d) {
            for (name, f1, lv) in &a.families {
                for k in 1..=lv.len() {
                    let e = f - f1 * k as f64;
                    if e.abs() < best.abs() {
                        (best, label) = (e, format!("{name}×{k}"));
                    }
                }
            }
        }
        let r = &d.ranges;
        matches.push(MatchRecord {
            truth_id: a.id,
            kind: a.kind,
            expected_detectable: a.expected_detectable,
            snr_db: a.snr_db,
            maneuver_near_cpa: a.maneuver_near_cpa,
            truth: t,
            det: p,
            contrast: d.contrast,
            v_rel_err: (p.v - t.v) / t.v,
            d_rel_err: (p.d - t.d) / t.d,
            tc_err: p.tc - t.tc,
            f0_err_hz: best,
            f0_line: label,
            in_v: (r.v.0..=r.v.1).contains(&t.v),
            in_d: (r.d.0..=r.d.1).contains(&t.d),
            in_tc: (r.tc.0..=r.tc.1).contains(&t.tc),
            flagged_ground: d.likely_ground_vehicle,
        });
    }
    let misses = truth
        .aircraft
        .iter()
        .zip(&truth_used)
        .filter(|(_, used)| !**used)
        .map(|(a, _)| MissRecord {
            truth_id: a.id,
            kind: a.kind,
            expected_detectable: a.expected_detectable,
            snr_db: a.snr_db,
            maneuver_near_cpa: a.maneuver_near_cpa,
            truth: a.flyby,
        })
        .collect();
    let false_positives = dets
        .iter()
        .zip(&det_used)
        .filter(|(_, used)| !**used)
        .map(|(d, _)| {
            let mut fp = attribute(truth, d);
            fp.flagged_ground = d.likely_ground_vehicle;
            fp.duplicate = fp.cause == Category::Aircraft
                && truth.aircraft.iter().zip(&truth_used).any(|(a, u)| *u && Some(a.id) == fp.source_id);
            fp
        })
        .collect();
    ScenarioResult {
        name: truth.name.clone(),
        duration: truth.duration,
        codec: format!("{:?}", truth.codec).to_lowercase(),
        n_aircraft: truth.aircraft.len(),
        n_detectable: truth.aircraft.iter().filter(|a| a.expected_detectable).count(),
        n_detections: dets.len(),
        matches,
        misses,
        false_positives,
        synth_seconds: truth.synth_seconds,
        ..Default::default()
    }
}

/// Rest frequencies of the lines a detection heard: its `harmonics` times
/// `flyby.f0` (the fundamental, which need not be one of them).
fn heard_lines(d: &Detection) -> Vec<f64> {
    if d.harmonics.is_empty() { vec![d.flyby.f0] } else { d.harmonics.iter().map(|h| h * d.flyby.f0).collect() }
}

fn attribute(truth: &Truth, d: &Detection) -> FalsePositive {
    let tc = d.flyby.tc;
    let freqs = heard_lines(d);
    let mut best: Option<(f64, usize, f64, f64)> = None;
    for (i, s) in truth.sources.iter().enumerate() {
        let dt = match s.flyby {
            Some(fb) => (tc - fb.tc).abs(),
            None => gap(tc, s.t_start, s.t_end),
        };
        let df = s
            .bands
            .iter()
            .flat_map(|&(lo, hi)| freqs.iter().map(move |f| gap(*f, lo, hi)))
            .fold(f64::INFINITY, f64::min);
        // narrow (tonal) sources win ties against wide broadband ones
        let width = s.bands.iter().map(|b| b.1 - b.0).fold(0.0, f64::max);
        let cost = dt / 5.0 + df / 5.0 + 0.01 * (1.0 + width).ln();
        if best.is_none_or(|b| cost < b.0) {
            best = Some((cost, i, dt, df));
        }
    }
    match best {
        Some((cost, i, dt, df)) if cost < 6.0 => {
            let s = &truth.sources[i];
            FalsePositive {
                det: d.flyby,
                contrast: d.contrast,
                cause: s.category,
                source_id: Some(s.id),
                source_kind: s.kind.clone(),
                duplicate: false,
                dt: Some(dt),
                df: Some(df),
                flagged_ground: false,
            }
        }
        _ => FalsePositive {
            det: d.flyby,
            contrast: d.contrast,
            cause: Category::Other,
            source_id: None,
            source_kind: "unexplained".into(),
            duplicate: false,
            dt: None,
            df: None,
            flagged_ground: false,
        },
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Summary {
    pub scenarios: usize,
    pub errors: usize,
    pub hours: f64,
    pub aircraft: usize,
    pub detectable: usize,
    pub found_detectable: usize,
    pub found_any: usize,
    pub detections: usize,
    pub true_positives: usize,
    pub recall: Option<f64>,
    pub recall_all: Option<f64>,
    pub precision: Option<f64>,
    /// Counting only detections not flagged as likely ground vehicles.
    pub recall_aircraft_class: Option<f64>,
    pub precision_aircraft_class: Option<f64>,
    pub fp_per_hour_aircraft_class: f64,
    pub fp_per_hour: f64,
    pub fp_by_cause: BTreeMap<String, usize>,
    pub fp_by_kind: BTreeMap<String, usize>,
    /// Median / 90th percentile of |error| over matches.
    pub v_rel_err: Option<(f64, f64)>,
    pub d_rel_err: Option<(f64, f64)>,
    pub tc_err_s: Option<(f64, f64)>,
    pub f0_err_hz: Option<(f64, f64)>,
    /// Fraction of matches whose truth lies inside the reported ranges.
    pub in_range_v: Option<f64>,
    pub in_range_d: Option<f64>,
    pub in_range_tc: Option<f64>,
    /// (found, total) of all aircraft by CPA SNR bucket, by kind, and maneuvering.
    pub recall_by_snr: Vec<(String, usize, usize)>,
    pub recall_by_kind: BTreeMap<String, (usize, usize)>,
    pub recall_maneuver_near_cpa: (usize, usize),
    pub synth_seconds: f64,
    pub detect_seconds: f64,
}

fn med_p90(mut v: Vec<f64>) -> Option<(f64, f64)> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    Some((q(0.5), q(0.9)))
}

fn frac(n: usize, d: usize) -> Option<f64> {
    (d > 0).then(|| n as f64 / d as f64)
}

pub fn summarize(results: &[ScenarioResult]) -> Summary {
    let mut s = Summary { scenarios: results.len(), ..Default::default() };
    let all_m: Vec<&MatchRecord> = results.iter().flat_map(|r| &r.matches).collect();
    for r in results {
        s.errors += r.error.is_some() as usize;
        s.hours += r.duration / 3600.0;
        s.aircraft += r.n_aircraft;
        s.detectable += r.n_detectable;
        s.detections += r.n_detections;
        s.true_positives += r.matches.len();
        s.found_detectable += r.matches.iter().filter(|m| m.expected_detectable).count();
        s.synth_seconds += r.synth_seconds;
        s.detect_seconds += r.detect_seconds;
        for fp in &r.false_positives {
            let cause = if fp.duplicate { "duplicate".to_string() } else { format!("{:?}", fp.cause).to_lowercase() };
            *s.fp_by_cause.entry(cause).or_default() += 1;
            *s.fp_by_kind.entry(fp.source_kind.clone()).or_default() += 1;
        }
    }
    s.found_any = s.true_positives;
    s.recall = frac(s.found_detectable, s.detectable);
    s.recall_all = frac(s.found_any, s.aircraft);
    s.precision = frac(s.true_positives, s.detections);
    let tp_ac = all_m.iter().filter(|m| !m.flagged_ground).count();
    let tp_ac_det = all_m.iter().filter(|m| !m.flagged_ground && m.expected_detectable).count();
    let fp_ac: usize = results.iter().map(|r| r.false_positives.iter().filter(|f| !f.flagged_ground).count()).sum();
    s.recall_aircraft_class = frac(tp_ac_det, s.detectable);
    s.precision_aircraft_class = frac(tp_ac, tp_ac + fp_ac);
    s.fp_per_hour_aircraft_class = if s.hours > 0.0 { fp_ac as f64 / s.hours } else { 0.0 };
    let n_fp: usize = s.fp_by_cause.values().sum();
    s.fp_per_hour = if s.hours > 0.0 { n_fp as f64 / s.hours } else { 0.0 };
    s.v_rel_err = med_p90(all_m.iter().map(|m| m.v_rel_err.abs()).collect());
    s.d_rel_err = med_p90(all_m.iter().map(|m| m.d_rel_err.abs()).collect());
    s.tc_err_s = med_p90(all_m.iter().map(|m| m.tc_err.abs()).collect());
    s.f0_err_hz = med_p90(all_m.iter().map(|m| m.f0_err_hz.abs()).collect());
    s.in_range_v = frac(all_m.iter().filter(|m| m.in_v).count(), all_m.len());
    s.in_range_d = frac(all_m.iter().filter(|m| m.in_d).count(), all_m.len());
    s.in_range_tc = frac(all_m.iter().filter(|m| m.in_tc).count(), all_m.len());

    // every truth aircraft as (snr, kind, near-cpa maneuver, found)
    let mut all: Vec<(f64, AircraftKind, bool, bool)> = Vec::new();
    for r in results {
        all.extend(r.matches.iter().map(|m| (m.snr_db, m.kind, m.maneuver_near_cpa, true)));
        all.extend(r.misses.iter().map(|m| (m.snr_db, m.kind, m.maneuver_near_cpa, false)));
    }
    let buckets = [(f64::NEG_INFINITY, 0.0, "< 0 dB"), (0.0, 6.0, "0–6 dB"), (6.0, 12.0, "6–12 dB"), (12.0, 20.0, "12–20 dB"), (20.0, f64::INFINITY, "≥ 20 dB")];
    for (lo, hi, label) in buckets {
        let inb: Vec<_> = all.iter().filter(|a| a.0 >= lo && a.0 < hi).collect();
        s.recall_by_snr.push((label.into(), inb.iter().filter(|a| a.3).count(), inb.len()));
    }
    for a in &all {
        let e = s.recall_by_kind.entry(format!("{:?}", a.1).to_lowercase()).or_default();
        e.0 += a.3 as usize;
        e.1 += 1;
    }
    let near: Vec<_> = all.iter().filter(|a| a.2).collect();
    s.recall_maneuver_near_cpa = (near.iter().filter(|a| a.3).count(), near.len());
    s
}

fn pct(x: Option<f64>) -> String {
    x.map_or("n/a".into(), |v| format!("{:.0} %", 100.0 * v))
}

fn mp(x: Option<(f64, f64)>, scale: f64, unit: &str) -> String {
    x.map_or("n/a".into(), |(m, p)| format!("{:.2}{unit} / {:.2}{unit}", m * scale, p * scale))
}

pub fn markdown(title: &str, results: &[ScenarioResult], s: &Summary) -> String {
    let mut o = format!("# {title}\n\n");
    o += "| scenario | dur (min) | codec | aircraft | detectable | detections | TP | missed (det.) | FP | detect (s) | note |\n";
    o += "|---|---|---|---|---|---|---|---|---|---|---|\n";
    for r in results {
        let missed = r.misses.iter().filter(|m| m.expected_detectable).count();
        o += &format!(
            "| {} | {:.1} | {} | {} | {} | {} | {} | {} | {} | {:.1} | {} |\n",
            r.name,
            r.duration / 60.0,
            r.codec,
            r.n_aircraft,
            r.n_detectable,
            r.n_detections,
            r.matches.len(),
            missed,
            r.false_positives.len(),
            r.detect_seconds,
            r.error.as_deref().unwrap_or("")
        );
    }
    o += "\n## Summary\n\n| metric | value |\n|---|---|\n";
    let rows = [
        ("scenarios (errors)", format!("{} ({})", s.scenarios, s.errors)),
        ("audio hours", format!("{:.2}", s.hours)),
        ("aircraft / expected detectable", format!("{} / {}", s.aircraft, s.detectable)),
        ("**recall** (expected detectable)", format!("{} ({}/{})", pct(s.recall), s.found_detectable, s.detectable)),
        ("recall (all aircraft)", format!("{} ({}/{})", pct(s.recall_all), s.found_any, s.aircraft)),
        ("**precision**", format!("{} ({}/{})", pct(s.precision), s.true_positives, s.detections)),
        ("**recall, aircraft class only**", pct(s.recall_aircraft_class)),
        ("**precision, aircraft class only**", pct(s.precision_aircraft_class)),
        ("false positives / hour, aircraft class", format!("{:.2}", s.fp_per_hour_aircraft_class)),
        ("false positives / hour", format!("{:.2}", s.fp_per_hour)),
        ("FP by cause", format!("{:?}", s.fp_by_cause)),
        ("FP by source kind", format!("{:?}", s.fp_by_kind)),
        ("\\|v err\\| rel, median / p90", mp(s.v_rel_err, 100.0, " %")),
        ("\\|d err\\| rel, median / p90", mp(s.d_rel_err, 100.0, " %")),
        ("\\|tc err\\|, median / p90", mp(s.tc_err_s, 1.0, " s")),
        ("\\|f0 err\\| (nearest line), median / p90", mp(s.f0_err_hz, 1.0, " Hz")),
        ("truth inside ranges v / d / tc", format!("{} / {} / {}", pct(s.in_range_v), pct(s.in_range_d), pct(s.in_range_tc))),
        ("found / total, maneuvering near CPA", format!("{} / {}", s.recall_maneuver_near_cpa.0, s.recall_maneuver_near_cpa.1)),
        ("synth / detect time (s)", format!("{:.1} / {:.1}", s.synth_seconds, s.detect_seconds)),
    ];
    for (k, v) in rows {
        o += &format!("| {k} | {v} |\n");
    }
    o += "\n### Found / total by CPA SNR (all aircraft)\n\n| SNR | found | total |\n|---|---|---|\n";
    for (label, f, t) in &s.recall_by_snr {
        o += &format!("| {label} | {f} | {t} |\n");
    }
    o += "\n### Found / total by kind\n\n| kind | found | total |\n|---|---|---|\n";
    for (k, (f, t)) in &s.recall_by_kind {
        o += &format!("| {k} | {f} | {t} |\n");
    }
    o
}
