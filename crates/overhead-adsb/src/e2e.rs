//! Synthetic end-to-end validation on real ADS-B.
//!
//! For a real adsb.lol day: pick a real ground point under real propeller /
//! rotor traffic, render what a microphone there would have recorded from
//! every real candidate flight around it (overhead-bench's retarded-time
//! propagation with spreading, air absorption, ground reflection, ambient
//! noise), run the stage-1 detector on that audio, then run the stage-2
//! search with only the inputs a user would have (day, optionally hours or
//! start time, region) and measure how far the answer is from the truth.
//!
//! To keep the test honest, the rendered "truth" differs from what the
//! matcher sees: positions between reports use a different interpolant
//! (Catmull–Rom tangents instead of the reported velocity), each flight gets
//! an altitude bias (σ 20 m, 40 m for pressure-only altitudes) and a slow
//! horizontal wander (15 m, plus 60 m where the reports are multilateration),
//! timestamps are jittered,
//! and the air temperature (speed of sound) is random while the detector
//! assumes 343 m/s.

use std::path::PathBuf;

use anyhow::{Result, bail};
use chrono::{NaiveDate, Timelike};
use overhead_bench::ambient::{self, Floor};
use overhead_bench::dsp::{Smooth, pa};
use overhead_bench::rng::Rng;
use overhead_bench::source::{self, MovingSource, PhaseTable, Scene, ToneFamily};
use overhead_bench::trajectory::Trajectory;
use overhead_core::audio::Audio;
use overhead_core::detect::{self, DetectConfig, Detection};
use overhead_core::model::speed_of_sound;
use serde::{Deserialize, Serialize};

use crate::cache::Day;
use crate::dem::Dem;
use crate::flight::{self, Flight, FlightOptions, Segment};
use crate::geo::{self, BBox, Enu, M_PER_DEG_LAT, V3, ecef, enu_axes, norm, scale, sub};
use crate::locate::{self, LocateInput, LocateOutput};
use crate::matcher::{MatchConfig, utc};
use crate::region::{Region, TimeSpec};
use crate::types::AircraftClass;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum TimeMode {
    /// Only the (local) day is known.
    Day,
    /// The day and a 4-hour local window (whole hours) containing the start.
    Hours,
    /// The local start time, off by up to ±1 minute (searched ±2 minutes).
    Start,
}

#[derive(Clone, Debug)]
pub struct E2eConfig {
    pub day: NaiveDate,
    /// Where observers are placed (and the default search region).
    pub area: BBox,
    /// Search region given to the matcher (default: `area`).
    pub search: Option<BBox>,
    pub cases: usize,
    pub seed: u64,
    pub duration: f64,
    pub min_close: usize,
    pub close_d: f64,
    pub time_mode: TimeMode,
    pub out: PathBuf,
    pub cache_root: PathBuf,
    pub streams: usize,
    pub sample_rate: u32,
    /// Ambient floor PSD at 100 Hz, dB re (20 µPa)²/Hz.
    pub noise_db: (f64, f64),
    /// UTC hours in which recording starts are drawn.
    pub hours_utc: (f64, f64),
    pub cfg: MatchConfig,
    /// Reuse audio, truth and detections already in `out` for the same case names.
    pub reuse: bool,
    /// Match from the detections alone (no spectrogram scoring), as `locate --detections`.
    pub detections_only: bool,
    /// Where result JSONs go (default `out`).
    pub results: PathBuf,
    /// Only render the audio and run stage 1 (for a later `--reuse`).
    pub render_only: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TruePass {
    pub hex: String,
    pub type_code: Option<String>,
    pub class: AircraftClass,
    pub heard_rec: f64,
    pub slant_m: f64,
    pub speed_ms: f64,
    pub height_agl_m: f64,
    pub f0: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaseTruth {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub ground_m: f64,
    pub start_unix: f64,
    pub start_utc: String,
    pub duration: f64,
    pub c_true: f64,
    pub temperature_c: f64,
    pub noise_db: f64,
    /// Every rendered pass within 5 km, heard during the recording.
    pub passes: Vec<TruePass>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaseResult {
    pub truth: CaseTruth,
    /// Detections: (tc, f0, v, d, v range, d range, confident, ground vehicle).
    pub detection_list: Vec<(f64, f64, f64, f64, (f64, f64), (f64, f64), bool, bool)>,
    pub detections: usize,
    pub detections_matching_truth: usize,
    pub error_m: Option<f64>,
    /// Rank of the first candidate within 500 m of the truth.
    pub rank_within_500m: Option<usize>,
    pub best_lat: Option<f64>,
    pub best_lon: Option<f64>,
    pub reported_radius_m: Option<f64>,
    pub start_error_s: Option<f64>,
    pub flights_correct: Option<String>,
    pub seconds_search: f64,
    /// Probability the ambiguity assessment gives to places within 2 km of
    /// the truth, and whether the truth is inside its plausible area.
    #[serde(default)]
    pub truth_mass_2km: Option<f64>,
    #[serde(default)]
    pub truth_in_area: Option<bool>,
    pub output: LocateOutput,
}

/// Local minima of range from a segment to `o` within `[t0, t1]`: `(t, d)`.
pub fn approaches(seg: &Segment, o: V3, t0: f64, t1: f64, d_max: f64) -> Vec<(f64, f64)> {
    let (a, b) = (seg.t0().max(t0), seg.t1().min(t1));
    let mut out = Vec::new();
    if b <= a {
        return out;
    }
    let step = 2.0;
    let r = |t: f64| norm(sub(seg.state(t).0, o));
    let mut t = a;
    let (mut r2, mut r1) = (f64::INFINITY, r(a));
    while t + step <= b {
        let r0 = r(t + step);
        if r1 < r0 && r1 <= r2 && r1 < d_max {
            out.push(flight::closest_approach(seg, o, t - step, t + step));
        }
        r2 = r1;
        r1 = r0;
        t += step;
    }
    out
}

/// "True" trajectory of a segment as heard: Catmull–Rom tangents, altitude
/// bias, smooth horizontal wander, timing jitter; in the observer's ENU frame
/// with recording time `t - start`.
fn truth_trajectory(seg: &Segment, enu: &Enu, start: f64, span: (f64, f64), alt_sigma: f64, rng: &mut Rng) -> Option<Trajectory> {
    let k = &seg.knots;
    let n = k.len();
    if n < 2 {
        return None;
    }
    // Catmull-Rom tangents (non-uniform): finite differences of neighbours
    let tang: Vec<V3> = (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(1), (i + 1).min(n - 1));
            scale(sub(k[b].p, k[a].p), 1.0 / (k[b].t - k[a].t).max(1e-3))
        })
        .collect();
    let dz = alt_sigma * rng.gaussian();
    let dt_shift = rng.range(-0.5, 0.5);
    let (wa, wb) = (span.0 - 100.0, span.1 + 100.0);
    let we = Smooth::new(rng, wa, wb, 25.0);
    let wn = Smooth::new(rng, wa, wb, 25.0);
    // multilateration positions are much worse than GNSS: extra wander where the reports are MLAT
    let me = Smooth::new(rng, wa, wb, 20.0);
    let mn = Smooth::new(rng, wa, wb, 20.0);
    let a = (k[0].t).max(span.0);
    let b = (k[n - 1].t).min(span.1);
    if b - a < 5.0 {
        return None;
    }
    let dt = 0.25;
    let m = ((b - a) / dt).ceil() as usize;
    let mut pos = Vec::with_capacity(m + 1);
    let mut vel = Vec::with_capacity(m + 1);
    let mut i = 0;
    for j in 0..=m {
        let t = (a + j as f64 * dt).min(b);
        let tq = t + dt_shift;
        while i + 2 < n && k[i + 1].t <= tq {
            i += 1;
        }
        let (p0, p1) = (&k[i], &k[i + 1]);
        let h = p1.t - p0.t;
        let u = ((tq - p0.t) / h).clamp(-0.5, 1.5);
        let (u2, u3) = (u * u, u * u * u);
        let (h00, h10, h01, h11) = (2.0 * u3 - 3.0 * u2 + 1.0, u3 - 2.0 * u2 + u, -2.0 * u3 + 3.0 * u2, u3 - u2);
        let (d00, d10, d01, d11) = (6.0 * u2 - 6.0 * u, 3.0 * u2 - 4.0 * u + 1.0, -6.0 * u2 + 6.0 * u, 3.0 * u2 - 2.0 * u);
        let mut p = [0.0; 3];
        let mut v = [0.0; 3];
        for c in 0..3 {
            p[c] = h00 * p0.p[c] + h10 * h * tang[i][c] + h01 * p1.p[c] + h11 * h * tang[i + 1][c];
            v[c] = (d00 * p0.p[c] + d01 * p1.p[c]) / h + d10 * tang[i][c] + d11 * tang[i + 1][c];
        }
        let mut l = enu.to_local(p);
        let ml = 60.0 * ((1.0 - u.clamp(0.0, 1.0)) * p0.mlat as u8 as f64 + u.clamp(0.0, 1.0) * p1.mlat as u8 as f64);
        l[0] += 15.0 * we.at(t) + ml * me.at(t);
        l[1] += 15.0 * wn.at(t) + ml * mn.at(t);
        l[2] += dz;
        pos.push(l);
        vel.push(enu.vec_to_local(v));
    }
    Some(Trajectory { t0: a - start, dt, pos, vel })
}

fn ladder(rng: &mut Rng, l1: f64, n: usize, slope: f64, scatter: f64) -> Vec<f64> {
    (1..=n).map(|k| l1 - slope * (k as f64).log2() + if k == 1 { 0.0 } else { scatter * rng.gaussian() }).collect()
}

fn family(rng: &mut Rng, name: &str, f: f64, levels: &[f64], span: (f64, f64)) -> ToneFamily {
    let (s1, s2) = (Smooth::new(rng, span.0, span.1, 5.0), Smooth::new(rng, span.0, span.1, 1.2));
    let table = PhaseTable::new(span.0, span.1, 0.05, |t| f * (1.0 + 0.002 * (0.8 * s1.at(t) + 0.6 * s2.at(t))));
    ToneFamily {
        name: name.to_string(),
        table,
        amps: levels.iter().map(|l| pa(*l)).collect(),
        phases: levels.iter().map(|_| rng.range(0.0, std::f64::consts::TAU)).collect(),
        fading: Some(Smooth::new(rng, span.0, span.1, 1.5)),
        fading_db: 1.5,
    }
}

/// Tone families of a class, as in overhead-bench's scenarios; returns families and the fundamental.
fn tones(class: AircraftClass, rng: &mut Rng, span: (f64, f64)) -> (Vec<ToneFamily>, f64) {
    match class {
        AircraftClass::Turboprop => {
            let f0 = rng.range(85.0, 250.0);
            let l1 = rng.range(128.0, 136.0);
            let (n, slope) = (rng.int(3, 6), rng.range(5.0, 9.0));
            let lv = ladder(rng, l1, n, slope, 2.5);
            let mut v = vec![family(rng, "propeller", f0, &lv, span)];
            if rng.chance(0.6) {
                let k = 1.0 + rng.sign() * rng.range(0.001, 0.008);
                let dl = rng.range(0.0, 3.0);
                let l2: Vec<f64> = lv.iter().map(|l| l - dl).collect();
                v.push(family(rng, "propeller_2", f0 * k, &l2, span));
            }
            (v, f0)
        }
        AircraftClass::Rotor => {
            let f0 = rng.range(14.0, 25.0);
            let l1 = rng.range(128.0, 134.0);
            let n = ((450.0 / f0) as usize).min(30);
            let slope = rng.range(4.0, 7.0);
            let lv = ladder(rng, l1, n, slope, 3.0);
            let ft = rng.range(60.0, 120.0);
            let lt = rng.range(118.0, 126.0);
            let (nt, st) = (rng.int(3, 6), rng.range(5.0, 8.0));
            let lvt = ladder(rng, lt, nt, st, 2.0);
            (vec![family(rng, "main_rotor", f0, &lv, span), family(rng, "tail_rotor", ft, &lvt, span)], ft)
        }
        _ => {
            let f0 = rng.range(60.0, 200.0);
            let l1 = rng.range(122.0, 130.0);
            let (n, slope) = (rng.int(3, 8), rng.range(4.0, 9.0));
            let lv = ladder(rng, l1, n, slope, 2.5);
            let mut v = vec![family(rng, "propeller", f0, &lv, span)];
            if rng.chance(0.5) {
                let el = l1 - rng.range(8.0, 16.0);
                let n = rng.int(2, 4);
                let lv2 = ladder(rng, el, n, 3.0, 3.0);
                v.push(family(rng, "engine", f0 / 2.0, &lv2, span));
            }
            (v, f0)
        }
    }
}

struct Picked {
    lat: f64,
    lon: f64,
    ground: f64,
    start: f64,
}

fn pick_case(flights: &[Flight], dem: &Dem, c: &E2eConfig, rng: &mut Rng) -> Option<Picked> {
    let synth: Vec<&Flight> = flights
        .iter()
        .filter(|f| matches!(f.class, AircraftClass::Piston | AircraftClass::Turboprop | AircraftClass::Rotor))
        .collect();
    if synth.is_empty() {
        return None;
    }
    let day0 = c.day.and_hms_opt(0, 0, 0)?.and_utc().timestamp() as f64;
    for _ in 0..4000 {
        let f = synth[rng.int(0, synth.len() - 1)];
        let seg = &f.segments[rng.int(0, f.segments.len() - 1)];
        if seg.t1() - seg.t0() < 120.0 {
            continue;
        }
        let t = rng.range(seg.t0() + 30.0, seg.t1() - 30.0);
        let hour = (t - day0) / 3600.0;
        if hour < c.hours_utc.0 || hour > c.hours_utc.1 {
            continue;
        }
        let (p, v, _) = seg.state(t);
        let (lat, lon, h) = geo::geodetic(p);
        if !c.area.contains(lat, lon) {
            continue;
        }
        let agl = h - dem.height(lat, lon);
        let speed = norm(v);
        if !(150.0..=2500.0).contains(&agl) || !(30.0..=140.0).contains(&speed) {
            continue;
        }
        let d = rng.log_range(350.0, 3000.0).max(agl * 1.05);
        let x = (d * d - agl * agl).sqrt();
        let (e, n, _) = enu_axes(lat, lon);
        let (ve, vn) = (geo::dot(v, e), geo::dot(v, n));
        let hs = (ve * ve + vn * vn).sqrt().max(1e-6);
        let side = rng.sign();
        let (oe, on) = (-vn / hs * x * side, ve / hs * x * side);
        let olat = lat + on / M_PER_DEG_LAT;
        let olon = lon + oe / (M_PER_DEG_LAT * lat.to_radians().cos());
        if !c.area.contains(olat, olon) {
            continue;
        }
        let ground = dem.height(olat, olon);
        let start = t + d / 343.0 - rng.range(0.15, 0.85) * c.duration;
        // count close passes during the recording
        let o = ecef(olat, olon, ground + 1.5);
        let mut close = 0;
        for g in &synth {
            for s in &g.segments {
                for (tc, dd) in approaches(s, o, start - 60.0, start + c.duration, c.close_d) {
                    let tr = tc + dd / 343.0 - start;
                    if tr > 60.0 && tr < c.duration - 60.0 {
                        close += 1;
                    }
                }
            }
        }
        if close >= c.min_close {
            return Some(Picked { lat: olat, lon: olon, ground, start });
        }
    }
    None
}

/// Render the recording heard at a ground point.
fn render(
    flights: &[Flight],
    p: &Picked,
    c: &E2eConfig,
    c_true: f64,
    noise_db: f64,
    rng: &mut Rng,
) -> (Vec<f32>, Vec<TruePass>) {
    let fs = c.sample_rate as f64;
    let n = (c.duration * fs) as usize;
    let enu = Enu::new(p.lat, p.lon, p.ground);
    let mic_h = 1.5;
    let scene = Scene { fs, c: c_true, mic: [0.0, 0.0, mic_h], ground_z: 0.0, reflection: rng.range(0.4, 0.8) };
    let o = ecef(p.lat, p.lon, p.ground + mic_h);
    let mut ac = vec![0.0f32; n];
    let mut passes = Vec::new();
    let r_cut = 15_000.0;
    let mut sources = Vec::new();
    for f in flights {
        if !matches!(f.class, AircraftClass::Piston | AircraftClass::Turboprop | AircraftClass::Rotor | AircraftClass::Unknown) {
            continue;
        }
        let mut frng = rng.fork();
        let span = (p.start - 300.0, p.start + c.duration + 60.0);
        let rel = (span.0 - p.start, span.1 - p.start);
        let (fams, f0) = tones(f.class, &mut frng, rel);
        let excess = frng.range(0.0, 6.0);
        let grazing = frng.range(0.0, 10.0);
        for seg in &f.segments {
            if seg.t1() < span.0 || seg.t0() > span.1 {
                continue;
            }
            // closest range in the window decides whether to render
            let near = approaches(seg, o, span.0, span.1, r_cut);
            let ends = [seg.t0().max(span.0), seg.t1().min(span.1)];
            let min_end = ends.iter().map(|t| norm(sub(seg.state(*t).0, o))).fold(f64::INFINITY, f64::min);
            if near.is_empty() && min_end > r_cut {
                continue;
            }
            for &(tc, d) in &near {
                let heard = tc + d / c_true - p.start;
                if heard > 0.0 && heard < c.duration && d < 5000.0 {
                    let (pp, v, _) = seg.state(tc);
                    let (_, _, hh) = geo::geodetic(pp);
                    passes.push(TruePass {
                        hex: f.hex.clone(),
                        type_code: f.type_code.clone(),
                        class: f.class,
                        heard_rec: heard,
                        slant_m: d,
                        speed_ms: norm(v),
                        height_agl_m: hh - p.ground,
                        f0,
                    });
                }
            }
            // pressure-only altitudes are less certain (QNH, temperature, altimeter error)
            let alt_sigma = if f.baro_only { 40.0 } else { 20.0 };
            let Some(traj) = truth_trajectory(seg, &enu, p.start, span, alt_sigma, &mut frng) else { continue };
            let (ta, tb) = traj.span();
            // heard window: emission span shifted by travel time
            let r_a = norm(traj.state(ta).0);
            let r_b = norm(traj.state(tb).0);
            let t_start = (ta + r_a / c_true).max(-5.0);
            let t_end = (tb + r_b / c_true).min(c.duration + 5.0);
            if t_end - t_start < 10.0 {
                continue;
            }
            sources.push(MovingSource {
                traj,
                tones: fams.clone(),
                broadband: None,
                excess_db: excess,
                grazing_db: grazing,
                t_start,
                t_end,
                ramp: 8.0,
            });
        }
    }
    let parts: Vec<(usize, Vec<f32>)> = {
        use rayon::prelude::*;
        sources.par_iter().map(|s| source::render(s, &scene)).collect()
    };
    for (i0, x) in parts {
        for (j, v) in x.iter().enumerate() {
            if let Some(o) = ac.get_mut(i0 + j) {
                *o += v;
            }
        }
    }
    let floor = Floor { pink_db_100: noise_db, rumble_db: noise_db + 8.0 };
    let bg = ambient::floor(n, fs, &floor, &mut rng.fork());
    let full_scale = (pa(100.0) * std::f64::consts::SQRT_2) as f32;
    let x: Vec<f32> = ac.iter().zip(&bg.x).map(|(a, b)| ((a + b) / full_scale).clamp(-1.0, 1.0)).collect();
    passes.sort_by(|a, b| a.heard_rec.total_cmp(&b.heard_rec));
    (x, passes)
}

/// Render (or reuse) a case and run stage 1 only; `None` result means `render_only`.
pub fn run_case(i: usize, c: &E2eConfig, flights: &[Flight], dem: &Dem) -> Result<Option<CaseResult>> {
    let mut rng = Rng::new(c.seed.wrapping_mul(1000) + i as u64);
    let Some(p) = pick_case(flights, dem, c, &mut rng) else { bail!("no suitable observer found in the area") };
    let temperature = rng.range(0.0, 30.0);
    let c_true = speed_of_sound(temperature);
    let noise_db = rng.range(c.noise_db.0, c.noise_db.1);
    let name = format!("e2e-{}-s{}-{i:02}", c.day, c.seed);
    eprintln!(
        "[{name}] observer {:.5},{:.5} ({:.0} m), start {}, T={temperature:.1} C (c={c_true:.1}), noise {noise_db:.1} dB",
        p.lat,
        p.lon,
        p.ground,
        utc(p.start)
    );
    let wav = c.out.join(format!("{name}.wav"));
    let truth_path = c.out.join(format!("{name}.truth.json"));
    let dets_path = c.out.join(format!("{name}.dets.json"));
    std::fs::create_dir_all(&c.out)?;
    let reused = if c.reuse { reuse_case(&wav, &truth_path, &dets_path, c.sample_rate) } else { None };
    let (truth, audio, dets) = match reused {
        Some(x) => {
            eprintln!("[{name}] reusing rendered audio, truth and detections");
            x
        }
        None => {
            let (x, passes) = render(flights, &p, c, c_true, noise_db, &mut rng);
            overhead_bench::dsp::write_wav(&wav, &x, c.sample_rate)?;
            let truth = CaseTruth {
                name: name.clone(),
                lat: p.lat,
                lon: p.lon,
                ground_m: p.ground,
                start_unix: p.start,
                start_utc: utc(p.start),
                duration: c.duration,
                c_true,
                temperature_c: temperature,
                noise_db,
                passes,
            };
            // detect on exactly what was written (16-bit), so a reused case is identical
            let audio = read_wav16(&wav, c.sample_rate)?;
            let dets: Vec<Detection> = detect::detect(&audio, &DetectConfig::default());
            std::fs::write(&truth_path, serde_json::to_vec_pretty(&truth)?)?;
            std::fs::write(&dets_path, serde_json::to_vec_pretty(&dets)?)?;
            (truth, audio, dets)
        }
    };
    if c.render_only {
        eprintln!("[{name}] rendered: {} true passes < 5 km, {} detections", truth.passes.len(), dets.len());
        return Ok(None);
    }
    // user inputs are drawn from their own stream so that reuse does not change them
    let mut rng = Rng::new(c.seed.wrapping_mul(1000) + i as u64 + 0x5eed);
    let matching = dets
        .iter()
        .filter(|d| truth.passes.iter().any(|tp| (tp.heard_rec - d.flyby.tc).abs() < 15.0))
        .count();
    eprintln!(
        "[{name}] {} true passes < 5 km, {} detections ({} near a true pass)",
        truth.passes.len(),
        dets.len(),
        matching
    );
    // inputs the user would give: the local clock of the true place (time
    // zone database), with no time zone given, so the matcher has to work the
    // offset out from the search region as it does for a real user
    let search = c.search.unwrap_or(c.area);
    let start_dt = chrono::DateTime::from_timestamp(truth.start_unix as i64, 0).unwrap();
    let true_offset = crate::region::zone_at(truth.lat, truth.lon)
        .map(|z| {
            use chrono::{Offset, TimeZone};
            z.offset_from_utc_datetime(&start_dt.naive_utc()).fix().local_minus_utc()
        })
        .unwrap_or_else(|| ((truth.lon / 15.0).round() as i32) * 3600);
    let local = start_dt.naive_utc() + chrono::Duration::seconds(true_offset as i64);
    let (start_s, hours_s) = match c.time_mode {
        TimeMode::Day => (None, None),
        TimeMode::Hours => {
            // a 4-hour window of whole hours containing the start, at a random place
            let k = rng.int(0, 3) as u32;
            let h0 = (local.hour() + 24 - k) % 24;
            (None, Some(format!("{h0:02}:00-{:02}:00", (h0 + 4) % 24)))
        }
        TimeMode::Start => {
            let t = local + chrono::Duration::seconds(rng.range(-60.0, 60.0) as i64);
            (Some(t.format("%H:%M:%S").to_string()), None)
        }
    };
    let time = TimeSpec::from_inputs(local.date(), None, start_s.as_deref(), 120.0, hours_s.as_deref(), None, Some(&search))?;
    let t0 = std::time::Instant::now();
    let out = locate::locate(LocateInput {
        detections: dets.clone(),
        audio: if c.detections_only { None } else { Some(&audio) },
        duration: c.duration,
        time,
        region: Some(Region { name: "e2e search box".into(), bbox: search }),
        cfg: c.cfg.clone(),
        cache_root: c.cache_root.clone(),
        streams: c.streams,
        flat_ground: None,
        truth: Some((truth.lat, truth.lon, truth.start_unix)),
        cuts: Vec::new(),
        cut_gap_max_s: None,
    })?;
    let secs = t0.elapsed().as_secs_f64();
    let best = out.candidates.first();
    let error = best.map(|b| geo::ground_distance(b.lat, b.lon, truth.lat, truth.lon));
    let rank = out.candidates.iter().position(|b| geo::ground_distance(b.lat, b.lon, truth.lat, truth.lon) < 500.0).map(|r| r + 1);
    let flights_correct = best.map(|b| {
        let ok = b
            .flights
            .iter()
            .filter(|m| truth.passes.iter().any(|tp| tp.hex == m.hex && (tp.heard_rec - m.heard_rec).abs() < 10.0))
            .count();
        format!("{ok}/{}", b.flights.len())
    });
    let res = CaseResult {
        detection_list: dets
            .iter()
            .map(|d| (d.flyby.tc, d.flyby.f0, d.flyby.v, d.flyby.d, d.ranges.v, d.ranges.d, d.confident, d.likely_ground_vehicle))
            .collect(),
        detections: dets.len(),
        detections_matching_truth: matching,
        error_m: error,
        rank_within_500m: rank,
        best_lat: best.map(|b| b.lat),
        best_lon: best.map(|b| b.lon),
        reported_radius_m: best.map(|b| b.uncertainty.radius_m),
        start_error_s: best.map(|b| b.start_unix - truth.start_unix),
        flights_correct,
        seconds_search: secs,
        truth_mass_2km: out.ambiguity.as_ref().map(|a| crate::ambiguity::truth_check(a, truth.lat, truth.lon, 2000.0).0),
        truth_in_area: out.ambiguity.as_ref().map(|a| crate::ambiguity::truth_check(a, truth.lat, truth.lon, 2000.0).1),
        truth,
        output: out,
    };
    std::fs::create_dir_all(&c.results)?;
    std::fs::write(c.results.join(format!("{name}.result.json")), serde_json::to_vec_pretty(&res)?)?;
    eprintln!(
        "[{name}] error {} m, rank of truth {:?}, flights {}, {:.0} s",
        res.error_m.map_or("-".into(), |e| format!("{e:.0}")),
        res.rank_within_500m,
        res.flights_correct.clone().unwrap_or_default(),
        secs
    );
    if let Some(a) = &res.output.ambiguity {
        eprintln!(
            "[{name}] {}; p(#1) {:.2}, mass within 2 km of the truth {:.2}, truth in plausible area: {}",
            a.summary,
            a.top_mass.first().map_or(0.0, |m| m.1),
            res.truth_mass_2km.unwrap_or(0.0),
            res.truth_in_area.unwrap_or(false)
        );
    }
    Ok(Some(res))
}

fn read_wav16(path: &std::path::Path, rate: u32) -> Result<Audio> {
    let b = std::fs::read(path)?;
    if b.len() < 44 || &b[..4] != b"RIFF" {
        bail!("{}: not a WAV file", path.display());
    }
    let x: Vec<f32> = b[44..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32767.0).collect();
    Ok(Audio::from_samples(x, rate as f64))
}

fn reuse_case(wav: &std::path::Path, truth: &std::path::Path, dets: &std::path::Path, rate: u32) -> Option<(CaseTruth, Audio, Vec<Detection>)> {
    let t: CaseTruth = serde_json::from_slice(&std::fs::read(truth).ok()?).ok()?;
    let d: Vec<Detection> = serde_json::from_slice(&std::fs::read(dets).ok()?).ok()?;
    let a = read_wav16(wav, rate).ok()?;
    Some((t, a, d))
}

pub fn load_area_flights(c: &E2eConfig) -> Result<Vec<Flight>> {
    let day = Day::ensure(&c.cache_root, c.day, c.streams)?;
    let t0 = c.day.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() as f64;
    let tracks = day.tracks(&c.area.expand(20_000.0), t0 - 3600.0, t0 + 90_000.0, 9000.0)?;
    // whole-day regional offset here; the matcher derives its own from its time window
    // raw reports (no matcher smoothing): the truth below gets its own smoother
    let opt = FlightOptions { baro_offset: flight::regional_baro_offset(&tracks), smooth_mlat: false, ..FlightOptions::default() };
    let mut flights: Vec<Flight> = tracks.iter().filter_map(|t| Flight::from_track(t, &opt)).collect();
    for f in &mut flights {
        for s in &mut f.segments {
            truth_smooth_mlat(&mut s.knots);
            s.finish();
        }
    }
    Ok(flights)
}

/// The synthetic truth of an MLAT-tracked aircraft. Its reports scatter by
/// ~100 m, so a path drawn through them would put that scatter into the
/// audio as real motion (speed swinging 30-80 m/s); the aircraft flew a
/// smooth path near them. The truth is a Gaussian-weighted (σ = 15 s) local
/// cubic through the positions, deliberately not the matcher's
/// [`flight::FlightOptions::smooth_mlat`] (quadratic, adaptive window, turn
/// gate), so that the matcher, which sees the raw reports from the archive,
/// is tested against a path it did not make. A cubic has no turn-rate bias
/// to first order; MLAT wander is still added in [`truth_trajectory`].
fn truth_smooth_mlat(k: &mut [flight::Knot]) {
    const SIGMA: f64 = 15.0;
    let n = k.len();
    if n < 5 || !k.iter().any(|x| x.mlat) {
        return;
    }
    let orig: Vec<(f64, V3)> = k.iter().map(|x| (x.t, x.p)).collect();
    for i in 0..n {
        if !k[i].mlat {
            continue;
        }
        let (ti, pi) = orig[i];
        // weighted normal equations of the basis 1, τ, τ², τ³ (τ in units of σ)
        let mut m = [[0.0f64; 4]; 4];
        let mut r = [[0.0f64; 3]; 4];
        let (mut before, mut after, mut cnt) = (0, 0, 0);
        for &(tj, pj) in &orig {
            let x = (tj - ti) / SIGMA;
            if x.abs() > 3.0 {
                continue;
            }
            let w = (-0.5 * x * x).exp();
            let b = [1.0, x, x * x, x * x * x];
            let y = sub(pj, pi);
            for a in 0..4 {
                for c in 0..4 {
                    m[a][c] += w * b[a] * b[c];
                }
                for ax in 0..3 {
                    r[a][ax] += w * b[a] * y[ax];
                }
            }
            before += (x < -0.05) as usize;
            after += (x > 0.05) as usize;
            cnt += 1;
        }
        if cnt < 6 || before < 2 || after < 2 {
            continue;
        }
        let Some(c) = solve4(m, r) else { continue };
        k[i].p = [pi[0] + c[0][0], pi[1] + c[0][1], pi[2] + c[0][2]];
        k[i].v = scale(c[1], 1.0 / SIGMA);
        k[i].h = geo::geodetic(k[i].p).2;
    }
}

/// Gaussian elimination with partial pivoting: `m x = r` for 3 right-hand sides.
fn solve4(mut m: [[f64; 4]; 4], mut r: [[f64; 3]; 4]) -> Option<[[f64; 3]; 4]> {
    for col in 0..4 {
        let piv = (col..4).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
        if m[piv][col].abs() < 1e-9 {
            return None;
        }
        m.swap(col, piv);
        r.swap(col, piv);
        for row in 0..4 {
            if row != col {
                let f = m[row][col] / m[col][col];
                for c in 0..4 {
                    m[row][c] -= f * m[col][c];
                }
                for ax in 0..3 {
                    r[row][ax] -= f * r[col][ax];
                }
            }
        }
    }
    let mut x = [[0.0; 3]; 4];
    for row in 0..4 {
        for ax in 0..3 {
            x[row][ax] = r[row][ax] / m[row][row];
        }
    }
    Some(x)
}

pub fn summary_markdown(results: &[CaseResult]) -> String {
    let mut s = String::from(
        "| case | truth (lat, lon) | start (UTC) | T °C | passes < 5 km | detections (true) | error | truth rank | reported radius | margin | Δstart | flights ok | search |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
    );
    for r in results {
        let t = &r.truth;
        s += &format!(
            "| {} | {:.5}, {:.5} | {} | {:.0} | {} | {} ({}) | {} | {} | {} | {} | {} | {} | {:.0} s |\n",
            t.name,
            t.lat,
            t.lon,
            t.start_utc,
            t.temperature_c,
            t.passes.len(),
            r.detections,
            r.detections_matching_truth,
            r.error_m.map_or("-".into(), |e| format!("{e:.0} m")),
            r.rank_within_500m.map_or("-".into(), |x| x.to_string()),
            r.reported_radius_m.map_or("-".into(), |e| format!("{e:.0} m")),
            r.output.candidates.first().map_or("-".into(), |c| format!("{:.2}", c.margin)),
            r.start_error_s.map_or("-".into(), |e| format!("{e:+.1} s")),
            r.flights_correct.clone().unwrap_or_default(),
            r.seconds_search,
        );
    }
    s
}
