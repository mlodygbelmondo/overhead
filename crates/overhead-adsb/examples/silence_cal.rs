//! Calibration data for the silence term (`overhead_adsb::silence`): every pass
//! of a candidate (propeller, rotor, unknown-type) aircraft at a recording's
//! true place during the recording, with its slant range, height, speed,
//! class, the recording's background around that time ([`Background::at`]),
//! the model's detection probability and cost, and whether stage 1 has a
//! detection within 30 s of its heard closest approach (what the matcher
//! counts as explained).
//!
//! usage: silence_cal AUDIO DETECTIONS.json LAT LON START_UNIX [RADIUS_M] > passes.jsonl

use anyhow::{Context, Result};
use overhead_adsb::geo::BBox;
use overhead_adsb::locate;
use overhead_adsb::matcher::{MatchConfig, Search};
use overhead_adsb::silence::{Background, Loudness};
use overhead_core::audio;

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let audio_path = std::path::PathBuf::from(&a[1]);
    let (dets, _) = locate::read_detections(&std::fs::read(&a[2])?).context("detections")?;
    let lat: f64 = a[3].parse()?;
    let lon: f64 = a[4].parse()?;
    let start: f64 = a[5].parse()?;
    let radius: f64 = a.get(6).map(|s| s.parse()).transpose()?.unwrap_or(6000.0);
    let au = audio::load(&audio_path, audio::DEFAULT_TARGET_RATE)?;
    let duration = au.duration();
    let bg = Background::measure(&au);
    if let Ok(p) = std::env::var("SILENCE_CAL_BG") {
        std::fs::write(p, serde_json::to_vec(&bg)?)?;
    }
    let root = overhead_adsb::cache::default_root();
    let t0 = start - 600.0;
    let t1 = start + duration + 600.0;
    let mut days = Vec::new();
    let mut d = chrono::DateTime::from_timestamp(t0 as i64, 0).unwrap().date_naive();
    let last = chrono::DateTime::from_timestamp(t1 as i64, 0).unwrap().date_naive();
    while d <= last {
        days.push(d);
        d += chrono::Duration::days(1);
    }
    let bbox = BBox::around(lat, lon, radius + 10_000.0);
    let flights = locate::load_flights(&root, &days, &bbox, t0, t1, 8)?;
    let dem = locate::dem_for(&root, None);
    let search = Search {
        dets: &dets,
        observed: &[],
        flights: &flights,
        dem: &dem,
        duration,
        start_windows: vec![(start, start)],
        region: bbox,
        cfg: MatchConfig::default(),
        truth: None,
        cuts: Default::default(),
        collect: Default::default(),
        background: Some(&bg),
    };
    let gh = dem.height(lat, lon);
    for p in search.loud_passes_at(lat, lon, &[start], &[], radius) {
        let f = &flights[p.flight as usize];
        let seg = f.segment_at(p.t_cpa).context("segment")?;
        let (pos, v, _) = seg.state(p.t_cpa);
        let (_, _, h) = overhead_adsb::geo::geodetic(pos);
        let near = dets
            .iter()
            .enumerate()
            .map(|(i, d)| (i, d.flyby.tc - p.heard_rec))
            .min_by(|x, y| x.1.abs().total_cmp(&y.1.abs()));
        let (p_detect, cost) = search.loud_pass_cost(&p, &[]);
        let line = serde_json::json!({
            "hex": f.hex, "reg": f.reg, "type": f.type_code, "class": f.class,
            "loudness": Loudness::of(f.class, f.type_code.as_deref()),
            "heard_rec": p.heard_rec, "slant_m": p.slant_m, "agl_m": h - gh, "agl_cpa": p.agl_m,
            "speed_ms": overhead_adsb::geo::norm(v), "inside": p.inside,
            "near_det": p.near_det, "nearest_det": near.map(|n| n.0), "nearest_dt": near.map(|n| n.1),
            "nearest_conf": near.map(|n| dets[n.0].confident), "nearest_contrast": near.map(|n| dets[n.0].contrast),
            "floor_rel_db": bg.at(p.heard_rec), "p_detect": p_detect, "cost": cost,
            "duration": duration,
        });
        println!("{line}");
    }
    Ok(())
}
