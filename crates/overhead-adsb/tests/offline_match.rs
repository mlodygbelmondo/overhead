//! Offline matcher test: two straight synthetic flights, detections computed
//! from the exact geometry, no network (flat terrain, model-curve scoring).

use overhead_adsb::dem::Dem;
use overhead_adsb::flight::{self, Flight, FlightOptions};
use overhead_adsb::geo::{self, BBox, ecef, norm, sub};
use overhead_adsb::matcher::{MatchConfig, Search};
use overhead_adsb::score::Observed;
use overhead_adsb::trace::{Point, Track};
use overhead_adsb::types::AircraftClass;
use overhead_core::detect::Detection;
use overhead_core::fit::{Ranges, Region};
use overhead_core::model::Flyby;

const T0: f64 = 1_786_780_800.0; // 2026-08-15T00:00Z + some
const GROUND: f64 = 100.0;

fn track(hex: &str, lat0: f64, lon0: f64, heading_deg: f64, speed: f64, alt: f64, t0: f64, dur: f64) -> Track {
    let m_lat = norm(sub(ecef(lat0 + 0.001, lon0, alt), ecef(lat0, lon0, alt))) / 0.001;
    let m_lon = norm(sub(ecef(lat0, lon0 + 0.001, alt), ecef(lat0, lon0, alt))) / 0.001;
    let (s, c) = heading_deg.to_radians().sin_cos();
    let points = (0..=(dur / 10.0) as usize)
        .map(|i| {
            let dt = i as f64 * 10.0;
            Point {
                t: t0 + dt,
                lat: lat0 + speed * c * dt / m_lat,
                lon: lon0 + speed * s * dt / m_lon,
                alt_baro: None,
                alt_geom: Some(alt),
                ground: false,
                gs: Some(speed),
                track: Some(heading_deg),
                vrate: Some(0.0),
                stale: false,
                new_leg: false,
                mlat: false,
            }
        })
        .collect();
    Track {
        hex: hex.into(),
        reg: None,
        type_code: Some("C172".into()),
        desc: None,
        class: AircraftClass::Piston,
        callsigns: vec![],
        points,
    }
}

fn detection(f: &Flight, o_lat: f64, o_lon: f64, start: f64) -> Detection {
    let o = ecef(o_lat, o_lon, GROUND + 1.5);
    let seg = &f.segments[0];
    let (tc, d) = flight::closest_approach(seg, o, seg.t0(), seg.t1());
    let v = norm(seg.state(tc).1);
    let heard = tc + d / 343.0 - start;
    let fb = Flyby { f0: 110.0, v, d, tc: heard };
    Detection {
        flyby: fb,
        harmonics: vec![1.0],
        ranges: Ranges { v: (v * 0.95, v * 1.05), d: (d * 0.9, d * 1.1), tc: (heard - 1.0, heard + 1.0), clipped: [false; 3] },
        score: 2.0,
        contrast: 10.0,
        quality: Default::default(),
        likely_ground_vehicle: false,
        confident: true,
        other_lines: vec![],
        region: Region { t_start: heard - 90.0, t_end: heard + 90.0, f_min: 80.0, f_max: 140.0 },
        trace: None,
        origin: Default::default(),
    }
}

#[test]
fn two_flights_locate_observer() {
    let opt = FlightOptions::default();
    let a = Flight::from_track(&track("aaaaaa", 52.10, 20.80, 90.0, 55.0, 700.0, T0, 2400.0), &opt).unwrap();
    let b = Flight::from_track(&track("bbbbbb", 51.95, 21.00, 10.0, 45.0, 500.0, T0 + 600.0, 2400.0), &opt).unwrap();
    // decoy flight far away
    let c = Flight::from_track(&track("cccccc", 52.30, 20.80, 90.0, 60.0, 600.0, T0, 2400.0), &opt).unwrap();
    let (o_lat, o_lon) = (52.105, 21.03);
    let start = T0 + 200.0;
    let flights = vec![a, b, c];
    let dets = vec![detection(&flights[0], o_lat, o_lon, start), detection(&flights[1], o_lat, o_lon, start)];
    for d in &dets {
        assert!(d.flyby.tc > 60.0 && d.flyby.tc < 3000.0, "{:?}", d.flyby);
    }
    let observed: Vec<Observed> = dets.iter().map(Observed::from_detection).collect();
    let dem = Dem::flat(GROUND);
    let search = Search {
        dets: &dets,
        observed: &observed,
        flights: &flights,
        dem: &dem,
        duration: 3200.0,
        start_windows: vec![(start - 600.0, start + 600.0)],
        region: BBox::around(52.1, 21.0, 30_000.0),
        cfg: MatchConfig { refine: 60, refine_full: 20, top: 3, ..MatchConfig::default() },
        truth: None,
        cuts: Default::default(),
        collect: Default::default(),
        background: None,
    };
    let (cands, stats) = search.run();
    assert!(!cands.is_empty(), "{stats:?}");
    let best = &cands[0];
    let err = geo::ground_distance(best.lat, best.lon, o_lat, o_lon);
    assert!(err < 150.0, "error {err:.0} m, best {best:?}");
    assert_eq!(best.matched, 2);
    assert!((best.start_unix - start).abs() < 3.0);
    // exact stage-1 speeds and distances: the soft evidence takes nothing off
    for m in &best.flights {
        assert!(m.vd_evidence > 0.999, "{m:?}");
    }
}

/// A detection whose stage-1 distance is far too short for its speed (its
/// `v^2/d` 40 times the true pass's, as after a slow-and-close mis-fit) still
/// matches its flight, but the soft speed/distance evidence discounts that
/// match: it is reported, and the score drops by what it takes off. (The
/// curves scored are the exact ones in both runs: only stage 1's numbers
/// differ.)
#[test]
fn far_off_ridge_match_is_discounted() {
    let opt = FlightOptions::default();
    let a = Flight::from_track(&track("aaaaaa", 52.10, 20.80, 90.0, 55.0, 700.0, T0, 2400.0), &opt).unwrap();
    let b = Flight::from_track(&track("bbbbbb", 51.95, 21.00, 10.0, 45.0, 500.0, T0 + 600.0, 2400.0), &opt).unwrap();
    let (o_lat, o_lon) = (52.105, 21.03);
    let start = T0 + 200.0;
    let flights = vec![a, b];
    let exact = vec![detection(&flights[0], o_lat, o_lon, start), detection(&flights[1], o_lat, o_lon, start)];
    let mut off = exact.clone();
    let d0 = off[0].flyby.d;
    off[0].flyby.d = d0 / 40.0;
    // a range wide enough that the search's outer gate still admits the pass
    off[0].ranges.d = (d0 / 80.0, d0 * 1.2);
    let dem = Dem::flat(GROUND);
    let observed: Vec<Observed> = exact.iter().map(Observed::from_detection).collect();
    let run = |dets: &[Detection]| {
        let search = Search {
            dets,
            observed: &observed,
            flights: &flights,
            dem: &dem,
            duration: 3200.0,
            start_windows: vec![(start - 600.0, start + 600.0)],
            region: BBox::around(52.1, 21.0, 30_000.0),
            cfg: MatchConfig { refine: 60, refine_full: 20, top: 3, ..MatchConfig::default() },
            truth: None,
            cuts: Default::default(),
            collect: Default::default(),
            background: None,
        };
        search.run().0
    };
    let (good, bad) = (run(&exact), run(&off));
    let (g, b) = (&good[0], &bad[0]);
    assert!(geo::ground_distance(b.lat, b.lon, o_lat, o_lon) < 150.0, "{b:?}");
    let m0 = b.flights.iter().find(|m| m.detection == 0).expect("detection 0 still matched");
    assert_eq!(m0.hex, "aaaaaa");
    // ln 40 = 3.7 below v^2/d, 1.2 beyond a confident detection's core: the floor
    assert!((m0.vd_evidence - overhead_adsb::matcher::VD_FLOOR).abs() < 1e-9, "{m0:?}");
    let m1 = b.flights.iter().find(|m| m.detection == 1).unwrap();
    assert!(m1.vd_evidence > 0.999);
    assert!(b.score < g.score - 0.5, "{} vs {}", b.score, g.score);
}

/// Without a cut in the recording, freeing a start between two fly-bys that
/// already fit together adds nothing: no cut is suggested.
#[test]
fn no_cut_is_found_where_the_timing_holds() {
    use overhead_adsb::matcher::Cuts;
    let opt = FlightOptions::default();
    let flights = vec![
        Flight::from_track(&track("aaaaaa", 52.10, 20.80, 90.0, 55.0, 700.0, T0, 2400.0), &opt).unwrap(),
        Flight::from_track(&track("bbbbbb", 51.95, 21.00, 10.0, 45.0, 500.0, T0 + 600.0, 2400.0), &opt).unwrap(),
        Flight::from_track(&track("cccccc", 52.30, 20.80, 90.0, 60.0, 600.0, T0, 2400.0), &opt).unwrap(),
    ];
    let (o_lat, o_lon) = (52.105, 21.03);
    let start = T0 + 200.0;
    let dets = vec![detection(&flights[0], o_lat, o_lon, start), detection(&flights[1], o_lat, o_lon, start)];
    let observed: Vec<Observed> = dets.iter().map(Observed::from_detection).collect();
    let dem = Dem::flat(GROUND);
    let found = overhead_adsb::locate::find_cuts_with(&dets, &[], None, 2, |cs| {
        let cuts = Cuts { at: cs.iter().map(|c| c.at).collect(), windows: cs.iter().map(|_| vec![(start - 600.0, start + 2400.0)]).collect() };
        let (c, _) = Search {
            dets: &dets,
            observed: &observed,
            flights: &flights,
            dem: &dem,
            duration: 3200.0,
            start_windows: vec![(start - 600.0, start + 600.0)],
            region: BBox::around(52.1, 21.0, 30_000.0),
            cfg: MatchConfig { refine: 60, refine_full: 20, top: 3, ..MatchConfig::default() },
            truth: None,
            cuts,
            collect: Default::default(),
            background: None,
        }
        .run();
        Ok(c.first().map_or((0.0, 0, f64::NAN, f64::NAN), |b| (b.score, b.matched, b.lat, b.lon)))
    })
    .unwrap();
    assert_eq!(found.baseline_matched, 2, "{found:?}");
    assert_eq!(found.tests.len(), 1);
    assert!(found.suggested.is_empty(), "{found:?}");
}

/// A hairpin: east along y = +900 m, a 300 m radius turn, west along y = +300 m
/// (local metres around (lat0, lon0)), so one aircraft passes the origin twice
/// about 35 s apart.
fn hairpin(hex: &str, lat0: f64, lon0: f64, alt: f64, t0: f64) -> Track {
    let m_lat = norm(sub(ecef(lat0 + 0.001, lon0, alt), ecef(lat0, lon0, alt))) / 0.001;
    let m_lon = norm(sub(ecef(lat0, lon0 + 0.001, alt), ecef(lat0, lon0, alt))) / 0.001;
    let v = 50.0;
    let (r, x_turn, x_start) = (300.0, 400.0, -6000.0);
    let t_leg1 = (x_turn - x_start) / v;
    let t_turn = std::f64::consts::PI * r / v;
    let pos = |t: f64| -> (f64, f64) {
        if t < t_leg1 {
            (x_start + v * t, 900.0)
        } else if t < t_leg1 + t_turn {
            let a = (t - t_leg1) / t_turn * std::f64::consts::PI;
            (x_turn + r * a.sin(), 600.0 + r * a.cos())
        } else {
            (x_turn - v * (t - t_leg1 - t_turn), 300.0)
        }
    };
    let points = (0..=300)
        .map(|i| {
            let dt = i as f64 * 2.0;
            let (x, y) = pos(dt);
            let (x2, y2) = pos(dt + 0.1);
            let heading = (x2 - x).atan2(y2 - y).to_degrees().rem_euclid(360.0);
            Point {
                t: t0 + dt,
                lat: lat0 + y / m_lat,
                lon: lon0 + x / m_lon,
                alt_baro: None,
                alt_geom: Some(alt),
                ground: false,
                gs: Some(v),
                track: Some(heading),
                vrate: Some(0.0),
                stale: false,
                new_leg: false,
                mlat: false,
            }
        })
        .collect();
    Track {
        hex: hex.into(),
        reg: None,
        type_code: Some("C152".into()),
        desc: None,
        class: AircraftClass::Piston,
        callsigns: vec![],
        points,
    }
}

/// Detection of the closest approach within an emission-time window.
fn detection_in(f: &Flight, o_lat: f64, o_lon: f64, start: f64, t_lo: f64, t_hi: f64) -> Detection {
    let o = ecef(o_lat, o_lon, GROUND + 1.5);
    let seg = &f.segments[0];
    let (tc, d) = flight::closest_approach(seg, o, t_lo, t_hi);
    let v = norm(seg.state(tc).1);
    let heard = tc + d / 343.0 - start;
    let fb = Flyby { f0: 95.0, v, d, tc: heard };
    Detection {
        flyby: fb,
        harmonics: vec![1.0],
        ranges: Ranges { v: (v * 0.95, v * 1.05), d: (d * 0.9, d * 1.1), tc: (heard - 1.0, heard + 1.0), clipped: [false; 3] },
        score: 2.0,
        contrast: 10.0,
        quality: Default::default(),
        likely_ground_vehicle: false,
        confident: true,
        other_lines: vec![],
        region: Region { t_start: heard - 25.0, t_end: heard + 25.0, f_min: 70.0, f_max: 120.0 },
        trace: None,
        origin: Default::default(),
    }
}

/// The reported matched flights must describe the pass that was scored (the
/// one heard at the detection's tc), not a closer neighbouring pass of the
/// same aircraft.
#[test]
fn hairpin_reports_the_scored_pass() {
    let opt = FlightOptions::default();
    let (o_lat, o_lon) = (52.105, 21.03);
    let start = T0 + 100.0;
    let h = Flight::from_track(&hairpin("dddddd", o_lat, o_lon, 500.0, T0), &opt).unwrap();
    let b = Flight::from_track(&track("bbbbbb", 51.95, 21.00, 10.0, 45.0, 500.0, T0 - 200.0, 2400.0), &opt).unwrap();
    let flights = vec![h, b];
    let t_pass1 = T0 + 6000.0 / 50.0; // first pass over x = 0
    let dets = vec![
        detection_in(&flights[0], o_lat, o_lon, start, t_pass1 - 10.0, t_pass1 + 10.0),
        detection_in(&flights[0], o_lat, o_lon, start, t_pass1 + 20.0, t_pass1 + 50.0),
        detection(&flights[1], o_lat, o_lon, start),
    ];
    assert!((dets[1].flyby.tc - dets[0].flyby.tc - 35.0).abs() < 5.0, "{:?} {:?}", dets[0].flyby, dets[1].flyby);
    let observed: Vec<Observed> = dets.iter().map(Observed::from_detection).collect();
    let dem = Dem::flat(GROUND);
    let search = Search {
        dets: &dets,
        observed: &observed,
        flights: &flights,
        dem: &dem,
        duration: 2400.0,
        start_windows: vec![(start - 120.0, start + 120.0)],
        region: BBox::around(52.1, 21.0, 20_000.0),
        cfg: MatchConfig { refine: 60, refine_full: 20, top: 3, ..MatchConfig::default() },
        truth: None,
        cuts: Default::default(),
        collect: Default::default(),
        background: None,
    };
    let (cands, stats) = search.run();
    assert!(!cands.is_empty(), "{stats:?}");
    let best = &cands[0];
    let err = geo::ground_distance(best.lat, best.lon, o_lat, o_lon);
    assert!(err < 300.0, "error {err:.0} m, best {best:?}");
    for m in &best.flights {
        let det = &dets[m.detection];
        assert!((m.heard_rec - det.flyby.tc).abs() < 3.0, "det {} tc {:.1}: reported heard {:.1}", m.detection, det.flyby.tc, m.heard_rec);
        assert!((m.slant_m - det.flyby.d).abs() < 0.15 * det.flyby.d + 30.0, "det {}: slant {:.0} vs {:.0}", m.detection, m.slant_m, det.flyby.d);
    }
    assert!(best.flights.iter().filter(|m| m.hex == "dddddd").count() == 2, "{:?}", best.flights);
    // the two passes are 985 and 500 m off (y = 900 and 300 m, 400 m up):
    // clearly different distances, so the second is hardly discounted as a repeat
    for m in &best.flights {
        assert!(m.repeat.is_none_or(|r| r.factor > 0.9), "{m:?}");
    }
}

/// Laps of a circuit: the same east leg (y = +600 m, 500 m up) flown again
/// `lap` s later, as one aircraft (a new segment after the gap).
fn laps(hex: &str, lat0: f64, lon0: f64, t0: f64, lap: f64) -> Track {
    let m_lat = norm(sub(ecef(lat0 + 0.001, lon0, 500.0), ecef(lat0, lon0, 500.0))) / 0.001;
    let m_lon = norm(sub(ecef(lat0, lon0 + 0.001, 500.0), ecef(lat0, lon0, 500.0))) / 0.001;
    let v = 50.0;
    let points = (0..2)
        .flat_map(|k| (0..=60).map(move |i| (k, i as f64 * 4.0)))
        .map(|(k, dt)| Point {
            t: t0 + k as f64 * lap + dt,
            lat: lat0 + 600.0 / m_lat,
            lon: lon0 + (-6000.0 + v * dt) / m_lon,
            alt_baro: None,
            alt_geom: Some(500.0),
            ground: false,
            gs: Some(v),
            track: Some(90.0),
            vrate: Some(0.0),
            stale: false,
            new_leg: false,
            mlat: false,
        })
        .collect();
    Track {
        hex: hex.into(),
        reg: None,
        type_code: Some("C152".into()),
        desc: None,
        class: AircraftClass::Piston,
        callsigns: vec![],
        points,
    }
}

/// Detection of the closest approach of whichever segment flies within an emission-time window.
fn detection_any(f: &Flight, o_lat: f64, o_lon: f64, start: f64, t_lo: f64, t_hi: f64) -> Detection {
    let o = ecef(o_lat, o_lon, GROUND + 1.5);
    let seg = f.segments.iter().find(|s| s.t0() <= t_hi && s.t1() >= t_lo).expect("segment");
    let (tc, d) = flight::closest_approach(seg, o, t_lo, t_hi);
    let mut det = detection_in(f, o_lat, o_lon, start, t_lo, t_hi);
    let v = norm(seg.state(tc).1);
    let heard = tc + d / 343.0 - start;
    det.flyby = Flyby { f0: 95.0, v, d, tc: heard };
    det.ranges = Ranges { v: (v * 0.95, v * 1.05), d: (d * 0.9, d * 1.1), tc: (heard - 1.0, heard + 1.0), clipped: [false; 3] };
    det.region = Region { t_start: heard - 25.0, t_end: heard + 25.0, f_min: 70.0, f_max: 120.0 };
    det
}

/// A circuit lap repeats its first pass's geometry: the weaker of the two
/// matches of the circuit flight counts `1 - repeat_weight` of its share, so
/// the place scores that much less than without the discount; a third,
/// independent flight is not touched.
#[test]
fn a_repeated_circuit_pass_counts_less() {
    let opt = FlightOptions::default();
    let (o_lat, o_lon) = (52.105, 21.03);
    let start = T0 + 100.0;
    let lap = 600.0;
    let c = Flight::from_track(&laps("eeeeee", o_lat, o_lon, T0, lap), &opt).unwrap();
    assert_eq!(c.segments.len(), 2);
    let b = Flight::from_track(&track("bbbbbb", 51.95, 21.00, 10.0, 45.0, 500.0, T0 - 200.0, 2400.0), &opt).unwrap();
    let flights = vec![c, b];
    let t_pass = T0 + 120.0; // over x = 0
    let dets = vec![
        detection_any(&flights[0], o_lat, o_lon, start, t_pass - 20.0, t_pass + 20.0),
        detection_any(&flights[0], o_lat, o_lon, start, t_pass + lap - 20.0, t_pass + lap + 20.0),
        detection(&flights[1], o_lat, o_lon, start),
    ];
    let observed: Vec<Observed> = dets.iter().map(Observed::from_detection).collect();
    let dem = Dem::flat(GROUND);
    let run = |w: f64| {
        let search = Search {
            dets: &dets,
            observed: &observed,
            flights: &flights,
            dem: &dem,
            duration: 2400.0,
            start_windows: vec![(start - 120.0, start + 120.0)],
            region: BBox::around(52.1, 21.0, 20_000.0),
            cfg: MatchConfig { refine: 60, refine_full: 20, top: 3, repeat_weight: w, ..MatchConfig::default() },
            truth: None,
            cuts: Default::default(),
            collect: Default::default(),
            background: None,
        };
        search.run().0
    };
    let (with, without) = (run(0.6), run(0.0));
    let (best, best0) = (&with[0], &without[0]);
    assert!(geo::ground_distance(best.lat, best.lon, o_lat, o_lon) < 300.0, "{best:?}");
    assert!(geo::ground_distance(best0.lat, best0.lon, o_lat, o_lon) < 300.0, "{best0:?}");
    assert_eq!(best.flights.iter().filter(|m| m.hex == "eeeeee").count(), 2, "{:?}", best.flights);
    let reps: Vec<_> = best.flights.iter().filter_map(|m| m.repeat.map(|r| (m, r))).collect();
    assert_eq!(reps.len(), 1, "{:?}", best.flights);
    let (m, r) = reps[0];
    assert_eq!(m.hex, "eeeeee");
    let other = best.flights.iter().find(|x| x.hex == "eeeeee" && x.detection != m.detection).unwrap();
    assert_eq!(r.of_detection, other.detection);
    assert!(r.dependence > 0.9 && (r.factor - (1.0 - 0.6 * r.dependence)).abs() < 1e-9, "{r:?}");
    assert!(best0.flights.iter().all(|x| x.repeat.is_none()));
    // the score drops by what the discount takes off the weaker match (share = capped curve above 0.45)
    let share = (m.curve_score.min(3.0) - 0.45).max(0.0) / 0.55 * m.vd_evidence;
    let drop = best0.score - best.score;
    assert!((drop - (1.0 - r.factor) * share).abs() < 0.05, "drop {drop:.3}, expected {:.3}", (1.0 - r.factor) * share);
}

/// Traced detections: the observer is found from the distance of each
/// candidate's modelled curve to the traced points (no audio, no brightness).
#[test]
fn traced_detections_locate_observer() {
    let opt = FlightOptions::default();
    let a = Flight::from_track(&track("aaaaaa", 52.10, 20.80, 90.0, 55.0, 700.0, T0, 2400.0), &opt).unwrap();
    let b = Flight::from_track(&track("bbbbbb", 51.95, 21.00, 10.0, 45.0, 500.0, T0 + 600.0, 2400.0), &opt).unwrap();
    let c = Flight::from_track(&track("cccccc", 52.30, 20.80, 90.0, 60.0, 600.0, T0, 2400.0), &opt).unwrap();
    let (o_lat, o_lon) = (52.105, 21.03);
    let start = T0 + 200.0;
    let flights = vec![a, b, c];
    let dets: Vec<Detection> = [&flights[0], &flights[1]]
        .iter()
        .map(|f| {
            let fb = detection(f, o_lat, o_lon, start).flyby;
            // eight hand-like points, four either side of the closest approach
            let pts: Vec<[f64; 2]> = [-40.0, -25.0, -14.0, -6.0, 5.0, 13.0, 24.0, 40.0]
                .iter()
                .map(|dt| [fb.tc + dt, fb.freq_at(fb.tc + dt, 343.0)])
                .collect();
            overhead_core::edit::trace_detection(&pts, 343.0).unwrap()
        })
        .collect();
    let observed: Vec<Observed> = dets.iter().map(|d| Observed::for_detection(None, d)).collect();
    assert!(observed.iter().all(|o| matches!(o, Observed::Trace { .. })));
    let dem = Dem::flat(GROUND);
    let search = Search {
        dets: &dets,
        observed: &observed,
        flights: &flights,
        dem: &dem,
        duration: 3200.0,
        start_windows: vec![(start - 600.0, start + 600.0)],
        region: BBox::around(52.1, 21.0, 30_000.0),
        cfg: MatchConfig { refine: 60, refine_full: 20, top: 3, ..MatchConfig::default() },
        truth: None,
        cuts: Default::default(),
        collect: Default::default(),
        background: None,
    };
    let (cands, stats) = search.run();
    assert!(!cands.is_empty(), "{stats:?}");
    let best = &cands[0];
    let err = geo::ground_distance(best.lat, best.lon, o_lat, o_lon);
    assert!(err < 250.0, "error {err:.0} m, best {best:?}");
    assert_eq!(best.matched, 2);
}

/// A recording with material removed: `gap` seconds were cut out between the
/// two fly-bys, so the second one is heard `gap` s earlier in the file than
/// the flight geometry says. Without a cut no single start explains both;
/// with a cut between them each part gets its own start and both match.
#[test]
fn a_cut_between_two_flybys_gives_each_part_its_start() {
    use overhead_adsb::matcher::Cuts;
    let opt = FlightOptions::default();
    let flights = vec![
        Flight::from_track(&track("aaaaaa", 52.10, 20.80, 90.0, 55.0, 700.0, T0, 2400.0), &opt).unwrap(),
        Flight::from_track(&track("bbbbbb", 51.95, 21.00, 10.0, 45.0, 500.0, T0 + 600.0, 2400.0), &opt).unwrap(),
        Flight::from_track(&track("cccccc", 52.30, 20.80, 90.0, 60.0, 600.0, T0, 2400.0), &opt).unwrap(),
    ];
    let (o_lat, o_lon) = (52.105, 21.03);
    let start = T0 + 200.0;
    let gap = 300.0;
    let dets = vec![detection(&flights[0], o_lat, o_lon, start), detection(&flights[1], o_lat, o_lon, start + gap)];
    assert!(dets[1].flyby.tc - dets[0].flyby.tc > 200.0, "{:?} {:?}", dets[0].flyby, dets[1].flyby);
    let observed: Vec<Observed> = dets.iter().map(Observed::from_detection).collect();
    let dem = Dem::flat(GROUND);
    let cut_at = 0.5 * (dets[0].flyby.tc + dets[1].flyby.tc);
    let run = |cuts: Cuts| {
        Search {
            dets: &dets,
            observed: &observed,
            flights: &flights,
            dem: &dem,
            duration: 3200.0 - gap,
            start_windows: vec![(start - 600.0, start + 600.0)],
            region: BBox::around(52.1, 21.0, 30_000.0),
            cfg: MatchConfig { refine: 60, refine_full: 20, top: 3, ..MatchConfig::default() },
            truth: None,
            cuts,
            collect: Default::default(),
            background: None,
        }
        .run()
    };
    // no cut: the locked timing cannot hold both fly-bys at the true place
    let (plain, _) = run(Cuts::default());
    let ok_plain = plain.first().is_some_and(|b| b.matched == 2 && geo::ground_distance(b.lat, b.lon, o_lat, o_lon) < 150.0);
    assert!(!ok_plain, "{:?}", plain.first());
    assert!(plain.iter().all(|c| c.part_starts_unix.is_none()));
    // a cut between them, the later part starting anywhere in the next 40 min
    let cuts = Cuts { at: vec![cut_at], windows: vec![vec![(start - 600.0, start + 2400.0)]] };
    let (cands, stats) = run(cuts);
    assert!(!cands.is_empty(), "{stats:?}");
    let best = &cands[0];
    let err = geo::ground_distance(best.lat, best.lon, o_lat, o_lon);
    assert!(err < 150.0, "error {err:.0} m, best {best:?}");
    assert_eq!(best.matched, 2, "{best:?}");
    let parts = best.part_starts_unix.as_ref().expect("part starts with cuts");
    assert_eq!(parts.len(), 2);
    let (s0, s1) = (parts[0].expect("part 0 start"), parts[1].expect("part 1 start"));
    assert!((s0 - start).abs() < 3.0, "part 0 start off by {:.1} s", s0 - start);
    assert!((s1 - (start + gap)).abs() < 3.0, "part 1 start off by {:.1} s", s1 - (start + gap));
    assert!((best.start_unix - start).abs() < 3.0);
    assert_eq!(best.start_part, Some(0));
    assert!(plain.iter().all(|c| c.start_part.is_none()));

    // finding the cut from the fly-bys: the one gap is tried and taken
    let found = overhead_adsb::locate::find_cuts_with(&dets, &[], None, 2, |cs| {
        let cuts = Cuts { at: cs.iter().map(|c| c.at).collect(), windows: cs.iter().map(|_| vec![(start - 600.0, start + 2400.0)]).collect() };
        let (c, _) = run(cuts);
        Ok(c.first().map_or((0.0, 0, f64::NAN, f64::NAN), |b| (b.score, b.matched, b.lat, b.lon)))
    })
    .unwrap();
    assert_eq!(found.tests.len(), 1, "{found:?}");
    assert_eq!(found.suggested.len(), 1, "{found:?}");
    let f = &found.suggested[0];
    assert!(f.range.0 >= dets[0].flyby.tc && f.range.1 <= dets[1].flyby.tc && f.at > f.range.0 && f.at < f.range.1, "{f:?}");
    assert!(f.matched_gain >= 1 && f.score_gain >= overhead_adsb::locate::CUT_SCORE_GAIN_MIN, "{f:?}");
    assert!(found.tests[0].accepted);
}

/// The explain session of a search: coarse and fine score maps, per-event
/// scores, modelled curves, silence and links agree with the search result,
/// and a flight that would have been heard loudly but is not in the recording
/// shows up as silent against a hand-given hypothesis.
#[test]
fn explain_session_agrees_with_the_search() {
    use overhead_adsb::explain::{FlightChoice, Hypothesis, ScoreMapOptions, Session};
    use overhead_adsb::matcher::Collect;
    let opt = FlightOptions::default();
    let (o_lat, o_lon) = (52.105, 21.03);
    let start = T0 + 200.0;
    let mk = |with_silent: bool| {
        let mut v = vec![
            Flight::from_track(&track("aaaaaa", 52.10, 20.80, 90.0, 55.0, 700.0, T0, 2400.0), &opt).unwrap(),
            Flight::from_track(&track("bbbbbb", 51.95, 21.00, 10.0, 45.0, 500.0, T0 + 600.0, 2400.0), &opt).unwrap(),
            Flight::from_track(&track("cccccc", 52.30, 20.80, 90.0, 60.0, 600.0, T0, 2400.0), &opt).unwrap(),
        ];
        if with_silent {
            // straight over the place at 300 m, ~1350 s into the recording, never detected
            v.push(Flight::from_track(&track("dddddd", 52.08, 21.03, 0.0, 50.0, 300.0, T0 + 1500.0, 300.0), &opt).unwrap());
        }
        v
    };
    let flights = mk(false);
    let dets = vec![detection(&flights[0], o_lat, o_lon, start), detection(&flights[1], o_lat, o_lon, start)];
    let observed = || dets.iter().map(Observed::from_detection).collect::<Vec<_>>();
    let obs = observed();
    let dem = Dem::flat(GROUND);
    let region = BBox::around(52.1, 21.0, 30_000.0);
    let cfg = MatchConfig { refine: 60, refine_full: 20, top: 3, ..MatchConfig::default() };
    let windows = vec![(start - 600.0, start + 600.0)];
    let detailed = Search {
        dets: &dets,
        observed: &obs,
        flights: &flights,
        dem: &dem,
        duration: 3200.0,
        start_windows: windows.clone(),
        region,
        cfg: cfg.clone(),
        truth: None,
        cuts: Default::default(),
        collect: Collect { coarse_cell_m: Some(overhead_adsb::explain::coarse_cell_for(&region)) },
        background: None,
    }
    .run_detailed();
    assert!(!detailed.candidates.is_empty());
    assert!(!detailed.coarse.is_empty());
    let best = detailed.candidates[0].clone();
    let session = Session {
        dets: dets.clone(),
        observed: observed(),
        flights: mk(false),
        dem: Dem::flat(GROUND),
        duration: 3200.0,
        start_windows: windows.clone(),
        region,
        cfg: cfg.clone(),
        cuts: Default::default(),
        candidates: detailed.candidates.clone(),
        refined: detailed.refined,
        candidate_refined: detailed.candidate_refined,
        coarse: detailed.coarse,
        coarse_cell_m: detailed.coarse_cell_m,
        background: None,
    };
    let ex = session.explain(&ScoreMapOptions { fine_radius_m: 1000.0, fine_step_m: 100.0, ..Default::default() }, 2);
    // coarse map: its best cell is near the place
    let top = ex.score_maps.coarse.cells.iter().max_by(|a, b| a.combined.total_cmp(&b.combined)).unwrap();
    assert!(geo::ground_distance(top.lat, top.lon, o_lat, o_lon) < 2.0 * ex.score_maps.coarse.cell_m + 500.0, "{top:?}");
    assert_eq!(top.matched, 2);
    // fine map: the combined maximum is at the candidate, one layer per matched detection
    let fine = ex.score_maps.fine.as_ref().expect("fine map");
    assert_eq!(fine.rows * fine.cols, fine.combined.values.len());
    assert_eq!(fine.per_detection.len(), 2);
    assert!(geo::ground_distance(fine.combined.best_lat, fine.combined.best_lon, best.lat, best.lon) < 150.0);
    assert!((fine.combined.max - best.score).abs() < 0.15 * best.score.abs() + 0.1, "{} vs {}", fine.combined.max, best.score);
    // per-event scores: the best assignment is the candidate's, scored per detection
    let pt = &ex.event_scores.best_per_assignment[0];
    assert_eq!(pt.candidate_rank, Some(1));
    assert!(pt.per_detection.iter().all(|s| s.as_ref().is_some_and(|s| s.score > 0.5)), "{pt:?}");
    assert!((pt.total - best.score).abs() < 1e-6, "{} vs {}", pt.total, best.score);
    // curves: one per detection, heard closest approach at the detection's tc
    let c1 = &ex.candidates[0];
    assert_eq!(c1.curves.len(), 2);
    for c in &c1.curves {
        let tc = dets[c.detection].flyby.tc;
        assert!((c.heard_cpa_rec.unwrap() - tc).abs() < 3.0, "det {}: {:?} vs {tc}", c.detection, c.heard_cpa_rec);
        assert!(!c.lines.is_empty() && c.lines[0].pieces.iter().map(|p| p.len()).sum::<usize>() > 100);
        // the modelled line passes the detection's own model frequency at tc
        let f_tc = dets[c.detection].flyby.freq_at(tc, 343.0);
        let near = c.lines[0].pieces.iter().flatten().min_by(|a, b| (a[0] - tc).abs().total_cmp(&(b[0] - tc).abs())).unwrap();
        assert!((near[1] - f_tc).abs() < 3.0, "det {}: {near:?} vs {f_tc:.1} Hz", c.detection);
    }
    // silence: consistent with the matcher's violations
    assert_eq!(c1.silence.violations, best.violations.len());
    assert!(c1.links.street_view.contains("viewpoint="));

    // a hand-given hypothesis in a session that also knows a silent flight
    let session2 = Session { flights: mk(true), candidates: vec![], refined: vec![], candidate_refined: vec![], coarse: vec![], ..session };
    let h = Hypothesis {
        lat: o_lat,
        lon: o_lon,
        starts: vec![Some(start)],
        c0: 343.0,
        flights: vec![
            FlightChoice { detection: 0, flight: "aaaaaa".into(), t_cpa_unix: None },
            FlightChoice { detection: 1, flight: "BBBBBB".into(), t_cpa_unix: None },
        ],
    };
    let chk = session2.check(&h).expect("check");
    assert!(chk.curves.iter().all(|c| c.flight.is_some() && c.score > 0.8), "{:?}", chk.curves.iter().map(|c| c.score).collect::<Vec<_>>());
    let silent: Vec<_> = chk.silence.passes.iter().filter(|p| p.status == "silent").collect();
    assert_eq!(silent.len(), 1, "{:?}", chk.silence.passes);
    assert_eq!(silent[0].flight.hex, "dddddd");
    assert!(silent[0].counts_against && silent[0].slant_m < 400.0, "{:?}", silent[0]);
    assert!((silent[0].heard_rec - 1356.0).abs() < 20.0, "{:?}", silent[0]);
    assert_eq!(chk.silence.violations, 1);
    // within 3 km the two detected passes appear too, as matched
    let wide = session2.silence(&h, Some(3000.0));
    assert_eq!(wide.passes.iter().filter(|p| p.status == "matched").count(), 2, "{:?}", wide.passes);
    assert_eq!(wide.violations, 1);
    // an unknown flight is reported, not guessed
    let bad = Hypothesis { flights: vec![FlightChoice { detection: 0, flight: "zzzzzz".into(), t_cpa_unix: None }], ..h };
    assert!(session2.check(&bad).is_err());
}

/// Silence from detection probability: an unheard light single straight
/// overhead costs the place the cap, one passing 4 km away about half of
/// that, a helicopter overhead less than the single (stage 1 hears rotors
/// less reliably), an unheard pass on the ground nothing, and a flight the
/// hypothesis matches is not held against it for another pass. The flat
/// term (`quiet_d`) gives every unheard flight within `quiet_d` 0.75.
#[test]
fn silence_costs_by_detection_probability() {
    use overhead_adsb::explain::{FlightChoice, Hypothesis, Session};
    let opt = FlightOptions::default();
    let (o_lat, o_lon) = (52.105, 21.03);
    let start = T0 + 200.0;
    let m_lon = 111_320.0 * (o_lat as f64).to_radians().cos();
    let mut heli = track("gggggg", 52.08, 21.03, 0.0, 50.0, 300.0, T0 + 2550.0, 300.0);
    heli.type_code = Some("EC35".into());
    heli.class = AircraftClass::Rotor;
    // on the ground (taxiing at 8 m/s) 300 m from the place
    let mut taxi = track("hhhhhh", 52.105 - 0.0027, 21.03 - 0.002, 90.0, 8.0, GROUND, T0 + 1100.0, 120.0);
    taxi.points.iter_mut().for_each(|p| p.alt_geom = Some(GROUND));
    let flights: Vec<Flight> = [
        track("aaaaaa", 52.10, 20.80, 90.0, 55.0, 700.0, T0, 2400.0),
        track("bbbbbb", 51.95, 21.00, 10.0, 45.0, 500.0, T0 + 600.0, 2400.0),
        // overhead at 300 m, heard ~1350 s into the recording
        track("dddddd", 52.08, 21.03, 0.0, 50.0, 300.0, T0 + 1500.0, 300.0),
        // 4 km east at 600 m, heard ~1970 s
        track("eeeeee", 52.08, 21.03 + 4000.0 / m_lon, 0.0, 50.0, 600.0, T0 + 2100.0, 300.0),
        heli,
        taxi,
    ]
    .iter()
    .map(|t| Flight::from_track(t, &opt).unwrap())
    .collect();
    let dets = vec![detection(&flights[0], o_lat, o_lon, start), detection(&flights[1], o_lat, o_lon, start)];
    let session = |cfg: MatchConfig| Session {
        dets: dets.clone(),
        observed: dets.iter().map(Observed::from_detection).collect(),
        flights: flights.clone(),
        dem: Dem::flat(GROUND),
        duration: 3200.0,
        start_windows: vec![(start - 600.0, start + 600.0)],
        region: BBox::around(52.1, 21.0, 30_000.0),
        cfg,
        cuts: Default::default(),
        candidates: vec![],
        refined: vec![],
        candidate_refined: vec![],
        coarse: vec![],
        coarse_cell_m: 1000.0,
        background: None,
    };
    let h = Hypothesis {
        lat: o_lat,
        lon: o_lon,
        starts: vec![Some(start)],
        c0: 343.0,
        flights: vec![
            FlightChoice { detection: 0, flight: "aaaaaa".into(), t_cpa_unix: None },
            FlightChoice { detection: 1, flight: "bbbbbb".into(), t_cpa_unix: None },
        ],
    };
    let soft = session(MatchConfig::default()).check(&h).expect("check").silence;
    let cost = |hex: &str| {
        soft.passes.iter().filter(|p| p.flight.hex == hex && p.status == "silent").map(|p| p.cost).fold(0.0, f64::max)
    };
    let (d, e, g, t) = (cost("dddddd"), cost("eeeeee"), cost("gggggg"), cost("hhhhhh"));
    assert!((d - MatchConfig::default().silence.cap).abs() < 1e-9, "overhead piston {d}: {:?}", soft.passes);
    assert!(e > 0.05 && e < 0.6 * d, "4 km piston {e}: {:?}", soft.passes);
    assert!(g > 0.05 && g < d, "overhead helicopter {g}");
    assert_eq!(t, 0.0, "taxiing aircraft: {:?}", soft.passes.iter().filter(|p| p.flight.hex == "hhhhhh").collect::<Vec<_>>());
    assert_eq!(soft.violations, 3);
    assert!((soft.penalty - (d + e + g)).abs() < 1e-9);
    let p_d = soft.passes.iter().find(|p| p.flight.hex == "dddddd").unwrap();
    assert!(p_d.p_detect.unwrap() > 0.9);
    // a flight the hypothesis matches (here, wrongly, to detection 1) costs
    // nothing for its passes that no detection explains
    let h2 = Hypothesis {
        flights: vec![
            FlightChoice { detection: 0, flight: "aaaaaa".into(), t_cpa_unix: None },
            FlightChoice { detection: 1, flight: "dddddd".into(), t_cpa_unix: None },
        ],
        ..h.clone()
    };
    let s2 = session(MatchConfig::default()).check(&h2).expect("check").silence;
    assert!(s2.passes.iter().filter(|p| p.flight.hex == "dddddd").all(|p| p.cost == 0.0), "{:?}", s2.passes);
    // the flat term: within 1 km, 0.75 each, nothing for the 4 km pass
    let flat = session(MatchConfig { quiet_d: Some(1000.0), ..MatchConfig::default() }).check(&h).expect("check").silence;
    let bad: Vec<&str> = flat.passes.iter().filter(|p| p.counts_against).map(|p| p.flight.hex.as_str()).collect();
    assert!(bad.contains(&"dddddd") && bad.contains(&"gggggg") && !bad.contains(&"eeeeee"), "{bad:?}");
    assert!((flat.penalty - 0.75 * flat.violations as f64).abs() < 1e-9, "{flat:?}");
}

/// The search ranks by the curve score minus the silence cost: a candidate
/// place with an unheard flight overhead reports it with its detection
/// probability, and its score is the lower by that cost.
#[test]
fn search_subtracts_silence_cost() {
    let opt = FlightOptions::default();
    let (o_lat, o_lon) = (52.105, 21.03);
    let start = T0 + 200.0;
    let base = vec![
        Flight::from_track(&track("aaaaaa", 52.10, 20.80, 90.0, 55.0, 700.0, T0, 2400.0), &opt).unwrap(),
        Flight::from_track(&track("bbbbbb", 51.95, 21.00, 10.0, 45.0, 500.0, T0 + 600.0, 2400.0), &opt).unwrap(),
    ];
    let dets = vec![detection(&base[0], o_lat, o_lon, start), detection(&base[1], o_lat, o_lon, start)];
    let observed: Vec<Observed> = dets.iter().map(Observed::from_detection).collect();
    let dem = Dem::flat(GROUND);
    let run = |flights: &[Flight]| {
        Search {
            dets: &dets,
            observed: &observed,
            flights,
            dem: &dem,
            duration: 3200.0,
            start_windows: vec![(start - 600.0, start + 600.0)],
            region: BBox::around(52.1, 21.0, 30_000.0),
            cfg: MatchConfig { refine: 60, refine_full: 20, top: 3, ..MatchConfig::default() },
            truth: None,
            cuts: Default::default(),
            collect: Default::default(),
            background: None,
        }
        .run()
        .0
    };
    let quiet = run(&base);
    let mut with = base.clone();
    with.push(Flight::from_track(&track("dddddd", 52.08, 21.03, 0.0, 50.0, 300.0, T0 + 1500.0, 300.0), &opt).unwrap());
    let loud = run(&with);
    let (a, b) = (&quiet[0], &loud[0]);
    assert!(geo::ground_distance(b.lat, b.lon, o_lat, o_lon) < 150.0, "{b:?}");
    let v = b.violations.iter().find(|v| v.hex == "dddddd").expect("violation");
    assert!(v.p_detect.unwrap() > 0.9 && (v.cost - MatchConfig::default().silence.cap).abs() < 1e-9, "{v:?}");
    assert!((a.score - b.score - v.cost).abs() < 0.03, "{} - {} vs {}", a.score, b.score, v.cost);
}
