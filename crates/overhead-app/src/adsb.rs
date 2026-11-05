//! The real location search: the form's hints and the detections go through
//! overhead-adsb's public API (region lookup, time constraints, day archives,
//! `locate::locate`), and its ranked candidates come back as
//! [`LocationResults`] for the map, the fly-by table and the detail panel.
//!
//! Progress and cancellation: the stages before the matcher (region lookup,
//! fetching a day archive, which takes a minute or more the first time) report
//! progress and stop at once when cancelled. The matcher itself has no
//! cancellation hook; a cancelled search that is already matching finishes in
//! the background and its result is dropped.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use overhead_adsb::archive;
use overhead_adsb::cache::{self, Day};
use overhead_adsb::geo::BBox;
use overhead_adsb::locate::{self, LocateInput, LocateOutput};
use overhead_adsb::matcher::MatchConfig;
use overhead_adsb::region::{self, TimeSpec};
use overhead_core::detect::Detection;

use crate::location::{Ambiguity, LocateControl, LocateJob, LocationCandidate, LocationResults, LocationSource, MatchedTrack, PlausibleCell};
use crate::query::{Date, SearchQuery, TimeHint};

/// Expected size of the part of a day archive that is kept (propeller, rotor
/// and unknown-type traces), for the download progress bar: 0.87 GB for
/// 2026-08-15. Only an estimate; the bar never claims to be done before it is.
const KEPT_BYTES_ESTIMATE: f64 = 0.9e9;

/// Extra margin around a named region (km), as the CLI's default.
const REGION_MARGIN_KM: f64 = 5.0;

pub struct AdsbSource {
    pub cache_root: PathBuf,
    /// Parallel HTTP range streams when downloading a day.
    pub streams: usize,
}

impl AdsbSource {
    pub fn new() -> Self {
        Self { cache_root: cache::default_root(), streams: 8 }
    }
}

/// One download per day at a time, across all searches of this process: a
/// superseded search's download keeps running (it fills the cache), and a new
/// search for the same day waits for it instead of writing the same files.
fn fetching() -> &'static Mutex<HashSet<NaiveDate>> {
    static F: std::sync::OnceLock<Mutex<HashSet<NaiveDate>>> = std::sync::OnceLock::new();
    F.get_or_init(|| Mutex::new(HashSet::new()))
}

fn to_naive(d: Date) -> Result<NaiveDate> {
    NaiveDate::from_ymd_opt(d.y, d.m, d.d).context("invalid date")
}

fn clock(s: u32) -> String {
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Is `s` something the matcher accepts as a time zone: an offset or an IANA name?
pub fn valid_tz(s: &str) -> bool {
    let d = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
    TimeSpec::from_inputs(d, None, None, 0.0, None, Some(s), None).is_ok()
}

/// The region text the matcher gets: coordinates typed into the city field go
/// alone ("52.29,21.0,20"), else the place parts joined for one Nominatim query.
fn region_spec(q: &SearchQuery) -> Option<String> {
    region_text(&q.region)
}

/// The region text for a lookup (see [`region_spec`]).
pub fn region_text(r: &crate::query::RegionHint) -> Option<String> {
    let city = r.city.trim();
    if !city.is_empty() && city.split(',').all(|p| p.trim().parse::<f64>().is_ok()) {
        return Some(city.to_string());
    }
    let label = r.label();
    (!label.is_empty()).then_some(label)
}

/// The box of a region as the search would use it (south, west, north,
/// east), for showing it on the map before a search. Place names are looked
/// up once with OpenStreetMap Nominatim and cached, like the search does.
pub fn region_box(spec: &str) -> Result<(f64, f64, f64, f64)> {
    let r = region::resolve(spec, &cache::default_root(), REGION_MARGIN_KM * 1000.0)?;
    Ok((r.bbox.lat_min, r.bbox.lon_min, r.bbox.lat_max, r.bbox.lon_max))
}

/// The matcher's time constraints from the form.
pub fn time_spec(q: &SearchQuery, bbox: Option<&BBox>) -> Result<TimeSpec> {
    let day = to_naive(q.day)?;
    let day_end = q.day_end.map(to_naive).transpose()?;
    let (start, hours) = match q.time {
        TimeHint::Unknown => (None, None),
        TimeHint::ExactStart(s) => (Some(clock(s)), None),
        TimeHint::Window { from, to } => (None, Some(format!("{}-{}", &clock(from)[..5], &clock(to)[..5]))),
    };
    TimeSpec::from_inputs(day, day_end, start.as_deref(), 120.0, hours.as_deref(), q.tz.as_deref(), bbox)
}

impl LocationSource for AdsbSource {
    fn name(&self) -> String {
        "adsb.lol".into()
    }

    fn locate(&self, job: &LocateJob) -> Result<LocationResults> {
        let ctl = &job.ctl;
        let q = &job.query;
        let started = Instant::now();

        // 1. region
        let region = match region_spec(q) {
            Some(spec) => {
                ctl.progress(format!("Looking up “{spec}”"), None);
                let r = region::resolve(&spec, &self.cache_root, REGION_MARGIN_KM * 1000.0)
                    .with_context(|| format!("region “{spec}”"))?;
                Some(r)
            }
            None => None,
        };
        ctl.check()?;

        // 2. time constraints
        let time = time_spec(q, region.as_ref().map(|r| &r.bbox))?;
        let duration = q.duration_s.max(1.0);
        let days = time.utc_days(duration);

        // 3. day archives (download once, ≈3.8 GB read and ≈0.9 GB kept per day)
        for (k, d) in days.iter().enumerate() {
            self.ensure_day(*d, k, days.len(), ctl)?;
        }
        ctl.check()?;

        // 4. the matcher (detections are tracked in 64-bit masks)
        let (dets, index) = select(&job.detections, locate::MAX_DETECTIONS);
        let audio = job.audio.as_deref();
        ctl.progress(
            format!(
                "Matching {} fly-bys against ADS-B ({} archive{})",
                dets.len(),
                days.len(),
                if days.len() == 1 { "" } else { "s" }
            ),
            None,
        );
        let (out, session) = locate::locate_session(LocateInput {
            detections: dets,
            audio,
            duration,
            time,
            region: region.clone(),
            cfg: MatchConfig::default(),
            cache_root: self.cache_root.clone(),
            streams: self.streams,
            flat_ground: None,
            truth: None,
            cuts: job.cuts.iter().map(|&at| locate::Cut { at, start_unix: None, start_tol_s: None }).collect(),
            cut_gap_max_s: None,
        })?;
        ctl.check()?;

        // 5. flight tracks for the map
        ctl.progress("Loading matched tracks", None);
        let tracks = load_tracks(&self.cache_root, &out, duration).unwrap_or_else(|e| {
            log::warn!("tracks for the map: {e:#}");
            HashMap::new()
        });
        let mut res = convert(&out, &index, &tracks, duration);
        res.session = Some(std::sync::Arc::new(session));
        res.cuts = job.cuts.clone();
        res.notes.push(format!(
            "{} candidate flights, {} detections used, search {:.0} s (matcher {:.0} s).",
            out.candidate_flights,
            out.detections_used,
            started.elapsed().as_secs_f64(),
            out.stats.seconds
        ));
        Ok(res)
    }
}

impl AdsbSource {
    /// Make sure a UTC day is in the cache, downloading it with progress if not.
    fn ensure_day(&self, day: NaiveDate, k: usize, n: usize, ctl: &LocateControl) -> Result<()> {
        let of = if n > 1 { format!(" ({} of {n})", k + 1) } else { String::new() };
        loop {
            if archive::is_cached(&self.cache_root, day) {
                return Ok(());
            }
            let mine = fetching().lock().unwrap().insert(day);
            if mine {
                break;
            }
            // another search of this process is downloading it: wait
            ctl.progress(format!("Waiting for the {day} download{of}"), None);
            ctl.check()?;
            std::thread::sleep(Duration::from_millis(300));
        }
        let root = self.cache_root.clone();
        let streams = self.streams;
        let (tx, rx) = std::sync::mpsc::channel();
        // Detached: if the search is cancelled the download still completes
        // and fills the cache for the next search.
        std::thread::spawn(move || {
            // released even if the download panics, so later searches do not wait forever
            struct Release(NaiveDate);
            impl Drop for Release {
                fn drop(&mut self) {
                    if let Ok(mut s) = fetching().lock() {
                        s.remove(&self.0);
                    }
                }
            }
            let _release = Release(day);
            let r = archive::fetch_day(&root, day, streams).map(|_| ());
            let _ = tx.send(r);
        });
        let part = archive::day_dir(&self.cache_root, day).join("kept.bin.part");
        let t0 = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(r) => return r.with_context(|| format!("adsb.lol archive for {day}")),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("download of {day} stopped"),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            ctl.check()?;
            let got = std::fs::metadata(&part).map(|m| m.len() as f64).unwrap_or(0.0);
            let el = t0.elapsed().as_secs_f64();
            let msg = if got > 0.0 {
                format!(
                    "Downloading adsb.lol {day}{of}: {:.0} MB of ≈{:.0} MB kept · {:.0} s (first time only)",
                    got / 1e6,
                    KEPT_BYTES_ESTIMATE / 1e6,
                    el
                )
            } else {
                format!("Opening the adsb.lol archive for {day}{of} · {el:.0} s (first time only)")
            };
            ctl.progress(msg, Some((got / KEPT_BYTES_ESTIMATE).min(0.97) as f32));
        }
    }
}

/// Up to `n` detections, the same choice as `locate::select_detections`
/// (confident aircraft first, then by contrast), with their indices.
fn select(dets: &[Detection], n: usize) -> (Vec<Detection>, Vec<usize>) {
    let mut idx: Vec<usize> = (0..dets.len()).collect();
    if dets.len() > n {
        let key = |d: &Detection| (d.confident && !d.likely_ground_vehicle) as u8;
        idx.sort_by(|a, b| key(&dets[*b]).cmp(&key(&dets[*a])).then(dets[*b].contrast.total_cmp(&dets[*a].contrast)));
        idx.truncate(n);
        idx.sort_unstable();
    }
    (idx.iter().map(|&i| dets[i].clone()).collect(), idx)
}

/// Airborne positions (unix s, lat, lon, alt m) of every matched aircraft, from
/// the cached day archives, around the candidates and the recording.
fn load_tracks(root: &Path, out: &LocateOutput, duration: f64) -> Result<HashMap<String, Vec<(f64, f64, f64, f64)>>> {
    let want: HashSet<&str> = out.candidates.iter().flat_map(|c| c.flights.iter().map(|f| f.hex.as_str())).collect();
    if want.is_empty() {
        return Ok(HashMap::new());
    }
    let mut bbox = BBox::empty();
    let (mut t0, mut t1) = (f64::INFINITY, f64::NEG_INFINITY);
    for c in &out.candidates {
        bbox.extend(c.lat, c.lon);
        t0 = t0.min(c.start_unix);
        t1 = t1.max(c.start_unix + duration);
    }
    let bbox = bbox.expand(20_000.0);
    let (t0, t1) = (t0 - 600.0, t1 + 600.0);
    let mut tracks = Vec::new();
    for d in &out.days {
        let Ok(day) = d.parse::<NaiveDate>() else { continue };
        if let Ok(day) = Day::open(root, day) {
            tracks.extend(day.tracks(&bbox, t0, t1, 9000.0)?.into_iter().filter(|t| want.contains(t.hex.as_str())));
        }
    }
    let mut m = HashMap::new();
    for t in cache::merge_same_hex(tracks) {
        let pts: Vec<(f64, f64, f64, f64)> = t
            .points
            .iter()
            .filter(|p| !p.ground && p.t >= t0 && p.t <= t1)
            .map(|p| (p.t, p.lat, p.lon, p.alt_geom.or(p.alt_baro).unwrap_or(0.0)))
            .collect();
        m.insert(t.hex.clone(), pts);
    }
    Ok(m)
}

fn convert(
    out: &LocateOutput,
    index: &[usize],
    tracks: &HashMap<String, Vec<(f64, f64, f64, f64)>>,
    duration: f64,
) -> LocationResults {
    let best = out.candidates.iter().map(|c| c.score).fold(0.0f64, f64::max).max(1e-9);
    let candidates = out
        .candidates
        .iter()
        .map(|c| {
            let tr = c
                .flights
                .iter()
                .filter_map(|m| {
                    let detection = *index.get(m.detection)?;
                    let pts = tracks
                        .get(&m.hex)
                        .map(|p| {
                            p.iter()
                                .filter(|q| q.0 >= c.start_unix - 300.0 && q.0 <= c.start_unix + duration + 300.0)
                                .map(|q| (q.0 - c.start_unix, q.1, q.2, q.3))
                                .collect()
                        })
                        .unwrap_or_default();
                    let label = m
                        .callsign
                        .clone()
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| m.reg.clone())
                        .unwrap_or_else(|| m.hex.clone());
                    Some(MatchedTrack {
                        detection,
                        callsign: label.trim().to_string(),
                        icao: m.hex.clone(),
                        reg: m.reg.clone(),
                        type_code: m.type_code.clone(),
                        class: format!("{:?}", m.class),
                        dt_s: m.heard_rec - m.tc,
                        heard_rec: m.heard_rec,
                        heard_utc: m.heard_utc.clone(),
                        slant_m: m.slant_m,
                        speed_ms: m.speed_ms,
                        height_agl_m: m.height_agl_m,
                        curve_score: m.curve_score,
                        points: pts,
                    })
                })
                .collect();
            let u = &c.uncertainty;
            LocationCandidate {
                lat: c.lat,
                lon: c.lon,
                radius_m: u.radius_m,
                ellipse: Some((u.major_m, u.minor_m, u.azimuth_deg)),
                score: (c.score / best).clamp(0.0, 1.0),
                raw_score: c.score,
                margin: Some(c.margin),
                matched: c.matched,
                violations: c.violations.len(),
                // with cuts and no fly-by before the first one, the recording's own start is unknown
                start_utc: (c.start_part.unwrap_or(0) == 0).then(|| c.start_utc.clone()),
                c0: Some(c.c0),
                ground_m: Some(c.ground_m),
                label: format!("#{}", c.rank),
                tracks: tr,
                probability: c.probability,
                same_place_as: c.same_place_as,
            }
        })
        .collect();
    let b = out.search_box;
    let mut notes: Vec<String> = Vec::new();
    if out.candidates.is_empty() {
        notes.push("No location found: no combination of ADS-B flights explains the fly-bys. Check the day, time and region.".into());
    }
    notes.push(format!(
        "UTC offset {}: {}.",
        out.utc_offsets_h.iter().map(|h| format!("{h:+}")).collect::<Vec<_>>().join(", "),
        out.utc_offset_source
    ));
    notes.push(format!("Archives: {}.", out.days.join(", ")));
    notes.extend(out.warnings.iter().cloned());
    let world = b.lat_max - b.lat_min > 170.0;
    let ambiguity = out.ambiguity.as_ref().map(|a| {
        let mass = |k: usize| a.top_mass.iter().find(|m| m.0 == k).map_or(0.0, |m| m.1);
        Ambiguity {
            confident: a.confident,
            similar_places: a.similar_places,
            top1: mass(1),
            top10: mass(10),
            elsewhere: a.elsewhere,
            cells: a
                .area
                .iter()
                .map(|c| PlausibleCell { bounds: (c.lat_min, c.lon_min, c.lat_max, c.lon_max), probability: c.probability })
                .collect(),
            hints: a.hints.clone(),
        }
    });
    LocationResults {
        source: "adsb.lol".into(),
        is_sample: false,
        candidates,
        search_box: (!world).then_some((b.lat_min, b.lon_min, b.lat_max, b.lon_max)),
        notes,
        ambiguity,
        session: None,
        det_index: index.to_vec(),
        cuts: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::RegionHint;

    fn q(time: TimeHint, tz: Option<&str>, city: &str) -> SearchQuery {
        SearchQuery {
            day: Date { y: 2026, m: 8, d: 15 },
            day_end: None,
            time,
            utc_offset_min: 120,
            utc_offset_guessed: tz.is_none(),
            tz: tz.map(str::to_string),
            region: RegionHint { city: city.into(), ..Default::default() },
            duration_s: 2400.0,
        }
    }

    fn warsaw() -> BBox {
        BBox { lat_min: 52.05, lat_max: 52.41, lon_min: 20.78, lon_max: 21.34 }
    }

    fn unix(s: &str) -> f64 {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp() as f64
    }

    #[test]
    fn exact_start_in_the_regions_zone() {
        // 16:48 local in Warsaw in August = 14:48Z, ±120 s
        let ts = time_spec(&q(TimeHint::ExactStart(16 * 3600 + 48 * 60), None, "Warszawa"), Some(&warsaw())).unwrap();
        let w = ts.start_windows();
        let t = unix("2026-08-15T14:48:00Z");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!((w[0].0 - (t - 120.0)).abs() < 1.0 && (w[0].1 - (t + 120.0)).abs() < 1.0, "{w:?}");
        assert_eq!(ts.utc_days(2400.0), vec![NaiveDate::from_ymd_opt(2026, 8, 15).unwrap()]);
    }

    #[test]
    fn hours_and_typed_zone() {
        let h = TimeHint::Window { from: 13 * 3600, to: 17 * 3600 + 30 * 60 };
        let a = time_spec(&q(h, Some("+02:00"), ""), None).unwrap();
        let b = time_spec(&q(h, Some("Europe/Warsaw"), ""), None).unwrap();
        assert_eq!(a.start_windows(), b.start_windows());
        // 13:00 local = 11:00Z, minus the matcher's 30 min slack
        assert!((a.start_windows()[0].0 - (unix("2026-08-15T11:00:00Z") - 1800.0)).abs() < 1.0);
        assert!(valid_tz("Europe/Warsaw") && valid_tz("-05:00") && !valid_tz("Mars/Olympus"));
    }

    #[test]
    fn region_text() {
        assert_eq!(region_spec(&q(TimeHint::Unknown, None, "52.29, 21.0, 20")).as_deref(), Some("52.29, 21.0, 20"));
        let mut x = q(TimeHint::Unknown, None, "Boxtel");
        x.region.country = "Netherlands".into();
        assert_eq!(region_spec(&x).as_deref(), Some("Boxtel, Netherlands"));
        assert_eq!(region_spec(&q(TimeHint::Unknown, None, " ")), None);
    }

    #[test]
    fn map_projection_round_trip() {
        let (x, y) = crate::map::merc(52.2947, 21.0034);
        let (la, lo) = crate::map::unmerc(x, y);
        assert!((la - 52.2947).abs() < 1e-9 && (lo - 21.0034).abs() < 1e-9);
        // tile 10/571/336 contains Warsaw
        let v = crate::map::MapView { x, y, zoom: 10.0 };
        let (_, keys) = v.tiles(10.0, 10.0);
        assert_eq!(keys, vec![(10, 571, 336)]);
    }
}
