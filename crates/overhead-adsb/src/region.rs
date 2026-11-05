//! User inputs: the search region (place name, point + radius, or box) and
//! the time constraints (day or day range, optional start time or hours).

use std::path::Path;

use anyhow::{Context, Result, bail};
use std::sync::OnceLock;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::geo::BBox;
use crate::http;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Region {
    pub name: String,
    pub bbox: BBox,
}

/// Parse `lat,lon[,radius_km]`, `lat_min,lon_min,lat_max,lon_max`, or look a
/// place name up with OpenStreetMap Nominatim (cached).
pub fn resolve(spec: &str, cache_dir: &Path, margin_m: f64) -> Result<Region> {
    let nums: Vec<f64> = spec.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let all_numeric = spec.split(',').all(|s| s.trim().parse::<f64>().is_ok());
    if all_numeric {
        let bbox = match nums.as_slice() {
            [lat, lon] => BBox::around(*lat, *lon, 20_000.0),
            [lat, lon, r] => BBox::around(*lat, *lon, r * 1000.0),
            [a, b, c, d] => BBox { lat_min: a.min(*c), lat_max: a.max(*c), lon_min: b.min(*d), lon_max: b.max(*d) },
            _ => bail!("region: expected lat,lon[,radius_km] or lat_min,lon_min,lat_max,lon_max"),
        };
        return Ok(Region { name: spec.to_string(), bbox: bbox.expand(margin_m) });
    }
    let bbox = geocode(spec, cache_dir)?;
    Ok(Region { name: spec.to_string(), bbox: bbox.expand(margin_m) })
}

fn geocode(name: &str, cache_dir: &Path) -> Result<BBox> {
    // v2: the main part of the place only (v1 kept Nominatim's box, which for a
    // country includes its overseas territories)
    let path = cache_dir.join("geocode-v2.json");
    let mut cache: std::collections::BTreeMap<String, BBox> =
        std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let key = name.trim().to_lowercase();
    if let Some(b) = cache.get(&key) {
        return Ok(*b);
    }
    let q: String = name
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => (b as char).to_string(),
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    // the outline simplified to ~2 km: a few kB even for a country
    let url = format!("https://nominatim.openstreetmap.org/search?q={q}&format=json&limit=1&polygon_geojson=1&polygon_threshold=0.02");
    let body = http::get_bytes(&url)?.context("geocoding failed")?;
    let v: serde_json::Value = serde_json::from_slice(&body)?;
    let first = v.get(0).with_context(|| format!("place not found: {name}"))?;
    let bb = first.get("boundingbox").and_then(|b| b.as_array()).context("no bounding box")?;
    let f = |i: usize| -> Result<f64> { Ok(bb[i].as_str().context("bbox")?.parse()?) };
    let full = BBox { lat_min: f(0)?, lat_max: f(1)?, lon_min: f(2)?, lon_max: f(3)? };
    let point = (|| -> Option<(f64, f64)> {
        Some((first.get("lat")?.as_str()?.parse().ok()?, first.get("lon")?.as_str()?.parse().ok()?))
    })();
    let bbox = match (first.get("geojson"), point) {
        (Some(g), Some((lat, lon))) => main_part(g, lat, lon).unwrap_or(full),
        _ => full,
    };
    eprintln!(
        "region \"{name}\": {} ({:.3}..{:.3} N, {:.3}..{:.3} E)",
        first.get("display_name").and_then(|d| d.as_str()).unwrap_or("?"),
        bbox.lat_min,
        bbox.lat_max,
        bbox.lon_min,
        bbox.lon_max
    );
    cache.insert(key, bbox);
    std::fs::create_dir_all(cache_dir).ok();
    std::fs::write(&path, serde_json::to_vec_pretty(&cache)?).ok();
    Ok(bbox)
}

/// Parts of a place farther than this from the rest are not searched
/// (overseas territories); nearer ones are (coastal islands, Corsica, the
/// Balearics).
const NEAR_KM: f64 = 300.0;

/// The box around the part of a place people mean: the polygon holding the
/// place's point (Nominatim's `lat`/`lon`) and every polygon within
/// `NEAR_KM` of it or of another one kept. `None` if the outline has no
/// polygons (a point or a line).
fn main_part(geojson: &serde_json::Value, lat: f64, lon: f64) -> Option<BBox> {
    let ring = |r: &serde_json::Value| -> Option<Vec<(f64, f64)>> {
        r.as_array()?.iter().map(|p| Some((p.get(1)?.as_f64()?, p.get(0)?.as_f64()?))).collect()
    };
    // outer rings, as (lat, lon)
    let coords = geojson.get("coordinates")?;
    let outers: Vec<Vec<(f64, f64)>> = match geojson.get("type")?.as_str()? {
        "Polygon" => vec![ring(coords.get(0)?)?],
        "MultiPolygon" => coords.as_array()?.iter().filter_map(|poly| ring(poly.get(0)?)).collect(),
        _ => return None,
    };
    let outers: Vec<Vec<(f64, f64)>> = outers.into_iter().filter(|r| r.len() >= 3).collect();
    if outers.is_empty() {
        return None;
    }
    let boxes: Vec<BBox> = outers
        .iter()
        .map(|r| {
            r.iter().fold(
                BBox { lat_min: f64::INFINITY, lat_max: f64::NEG_INFINITY, lon_min: f64::INFINITY, lon_max: f64::NEG_INFINITY },
                |b, &(la, lo)| BBox { lat_min: b.lat_min.min(la), lat_max: b.lat_max.max(la), lon_min: b.lon_min.min(lo), lon_max: b.lon_max.max(lo) },
            )
        })
        .collect();
    // the polygon with the point in it, else the nearest one
    let main = outers.iter().position(|r| inside(r, lat, lon)).unwrap_or_else(|| {
        let p = BBox { lat_min: lat, lat_max: lat, lon_min: lon, lon_max: lon };
        (0..boxes.len()).min_by(|&a, &b| gap_km(&boxes[a], &p).total_cmp(&gap_km(&boxes[b], &p))).unwrap_or(0)
    });
    let mut kept = vec![false; boxes.len()];
    kept[main] = true;
    let mut grew = true;
    while grew {
        grew = false;
        for i in 0..boxes.len() {
            if !kept[i] && (0..boxes.len()).any(|j| kept[j] && gap_km(&boxes[i], &boxes[j]) <= NEAR_KM) {
                kept[i] = true;
                grew = true;
            }
        }
    }
    boxes.iter().zip(&kept).filter(|(_, k)| **k).map(|(b, _)| *b).reduce(|a, b| BBox {
        lat_min: a.lat_min.min(b.lat_min),
        lat_max: a.lat_max.max(b.lat_max),
        lon_min: a.lon_min.min(b.lon_min),
        lon_max: a.lon_max.max(b.lon_max),
    })
}

/// Ray casting: `lat, lon` inside the ring.
fn inside(ring: &[(f64, f64)], lat: f64, lon: f64) -> bool {
    let mut odd = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let ((ya, xa), (yb, xb)) = (ring[i], ring[j]);
        if (ya > lat) != (yb > lat) && lon < xa + (lat - ya) / (yb - ya) * (xb - xa) {
            odd = !odd;
        }
        j = i;
    }
    odd
}

/// Distance between two boxes (0 if they touch or overlap), km.
fn gap_km(a: &BBox, b: &BBox) -> f64 {
    let dlat = (b.lat_min - a.lat_max).max(a.lat_min - b.lat_max).max(0.0);
    let dlon = (b.lon_min - a.lon_max).max(a.lon_min - b.lon_max).max(0.0);
    let mid = ((a.lat_min + a.lat_max + b.lat_min + b.lat_max) / 4.0).to_radians().cos();
    111.32 * (dlat * dlat + (dlon * mid) * (dlon * mid)).sqrt()
}

/// When the recording started, as the user gave it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StartTime {
    /// An absolute instant (RFC 3339 with an offset, or local time with `--tz`).
    Utc(DateTime<Utc>),
    /// A local date and time whose UTC offset is not known exactly.
    Local(NaiveDateTime),
    /// A local time of day on (each of) the recording day(s).
    TimeOfDay(NaiveTime),
}

/// Time constraints on the recording start.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimeSpec {
    /// Local recording day(s), inclusive.
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
    /// Possible UTC offsets of the local clock (s east of UTC): one when the
    /// time zone is given, else every offset in force in the region on those
    /// days (time zone database), else every offset in use worldwide.
    pub offsets: Vec<i32>,
    /// Where `offsets` came from.
    pub offset_source: String,
    /// Start time and its tolerance (s).
    pub start: Option<(StartTime, f64)>,
    /// Approximate local hours of the start, e.g. 13:00-17:00 (on each day).
    pub hours: Option<(NaiveTime, NaiveTime)>,
    /// Slack added on both sides of `hours` (s).
    pub hours_slack_s: f64,
}

pub fn parse_hours(s: &str) -> Result<(NaiveTime, NaiveTime)> {
    let (a, b) = s.split_once('-').context("hours: expected HH[:MM]-HH[:MM]")?;
    let p = |x: &str| -> Result<NaiveTime> {
        let x = x.trim();
        if x == "24" || x == "24:00" {
            return Ok(NaiveTime::from_hms_opt(23, 59, 59).unwrap());
        }
        NaiveTime::parse_from_str(x, "%H:%M").or_else(|_| NaiveTime::parse_from_str(&format!("{x}:00"), "%H:%M")).context("hours")
    };
    Ok((p(a)?, p(b)?))
}

/// Parse a start time: RFC 3339 (absolute), local `YYYY-MM-DD HH:MM[:SS]`, or local `HH:MM[:SS]`.
pub fn parse_start(s: &str) -> Result<StartTime> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(StartTime::Utc(t.with_timezone(&Utc)));
    }
    for f in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
            return Ok(StartTime::Local(n));
        }
    }
    for f in ["%H:%M:%S", "%H:%M"] {
        if let Ok(t) = NaiveTime::parse_from_str(s, f) {
            return Ok(StartTime::TimeOfDay(t));
        }
    }
    bail!("cannot parse start time {s:?}")
}

/// A fixed UTC offset: "+02:00", "-5", "UTC+1", "Z".
pub fn parse_offset(s: &str) -> Result<i32> {
    let s = s.trim().trim_start_matches("UTC").trim_start_matches("utc");
    if s.is_empty() || s == "Z" {
        return Ok(0);
    }
    let sign = if s.starts_with('-') { -1 } else { 1 };
    let body = s.trim_start_matches(['+', '-']);
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<i32>()?, m.parse::<i32>()?),
        None => (body.parse::<i32>()?, 0),
    };
    Ok(sign * (h * 3600 + m * 60))
}

/// UTC offsets (s) of an IANA zone at noon UTC of each day.
fn zone_offsets(tz: Tz, days: &[NaiveDate]) -> Vec<i32> {
    days.iter()
        .map(|d| tz.offset_from_utc_datetime(&d.and_hms_opt(12, 0, 0).unwrap()).fix().local_minus_utc())
        .collect()
}

fn finder() -> &'static tzf_rs::DefaultFinder {
    static F: OnceLock<tzf_rs::DefaultFinder> = OnceLock::new();
    F.get_or_init(tzf_rs::DefaultFinder::new)
}

/// IANA zone at a point (time zone boundaries from OpenStreetMap via tzf).
pub fn zone_at(lat: f64, lon: f64) -> Option<Tz> {
    finder().get_tz_name(lon, lat).parse().ok()
}

/// Every UTC offset in use somewhere (whole and fractional hours).
pub fn all_offsets() -> Vec<i32> {
    let mut v: Vec<i32> = (-12..=14).map(|h| h * 3600).collect();
    v.extend([-9 * 60 - 30, -3 * 60 - 30, 3 * 60 + 30, 4 * 60 + 30, 5 * 60 + 30, 5 * 60 + 45, 6 * 60 + 30, 8 * 60 + 45, 9 * 60 + 30, 10 * 60 + 30, 12 * 60 + 45].map(|m| m * 60));
    v.sort_unstable();
    v
}

impl TimeSpec {
    /// Build the constraints from the user's inputs. `tz` is a fixed offset
    /// ("+02:00") or an IANA zone ("Europe/Warsaw"); without it the offsets in
    /// force inside `region` on those days are taken from the time zone
    /// database (several near a zone border), and without a region every
    /// offset in use is allowed.
    pub fn from_inputs(
        day: NaiveDate,
        day_end: Option<NaiveDate>,
        start: Option<&str>,
        start_tol: f64,
        hours: Option<&str>,
        tz: Option<&str>,
        region: Option<&BBox>,
    ) -> Result<Self> {
        let last = day_end.unwrap_or(day);
        if last < day || (last - day).num_days() > 2 {
            bail!("--day-end must be at most 2 days after --day (a 2-3 day range)");
        }
        let mut days = Vec::new();
        let mut d = day - Duration::days(1);
        while d <= last + Duration::days(1) {
            days.push(d);
            d += Duration::days(1);
        }
        let start = start.map(parse_start).transpose()?;
        let (mut offsets, source) = match tz {
            Some(tz) => match parse_offset(tz) {
                Ok(o) => (vec![o], format!("given ({tz})")),
                Err(_) => {
                    let z: Tz = tz.parse().map_err(|_| anyhow::anyhow!("--tz: not an offset or IANA zone: {tz}"))?;
                    (zone_offsets(z, &days), format!("given ({tz})"))
                }
            },
            None => match region {
                Some(b) if b.lat_max - b.lat_min < 60.0 && b.lon_max - b.lon_min < 120.0 => {
                    let mut zones: Vec<Tz> = Vec::new();
                    let n = 8;
                    for i in 0..=n {
                        for j in 0..=n {
                            let lat = b.lat_min + (b.lat_max - b.lat_min) * i as f64 / n as f64;
                            let lon = b.lon_min + (b.lon_max - b.lon_min) * j as f64 / n as f64;
                            if let Some(z) = zone_at(lat, lon)
                                && !zones.contains(&z)
                            {
                                zones.push(z);
                            }
                        }
                    }
                    if zones.is_empty() {
                        (all_offsets(), "unknown (no time zone found in the region)".to_string())
                    } else {
                        let names: Vec<String> = zones.iter().map(|z| z.name().to_string()).collect();
                        (zones.iter().flat_map(|z| zone_offsets(*z, &days)).collect(), format!("time zone database: {}", names.join(", ")))
                    }
                }
                _ => (all_offsets(), "unknown (no region or --tz): every offset in use".to_string()),
            },
        };
        offsets.sort_unstable();
        offsets.dedup();
        // an offset with an RFC 3339 start is irrelevant
        if let Some(StartTime::Utc(_)) = start {
            offsets.truncate(1);
        }
        let hours = hours.map(parse_hours).transpose()?;
        Ok(TimeSpec { first_day: day, last_day: last, offsets, offset_source: source, start: start.map(|s| (s, start_tol)), hours, hours_slack_s: 1800.0 })
    }

    fn local_to_unix(n: NaiveDateTime, offset: i32) -> f64 {
        (n.and_utc().timestamp() - offset as i64) as f64
    }

    fn days(&self) -> Vec<NaiveDate> {
        let mut v = Vec::new();
        let mut d = self.first_day;
        while d <= self.last_day {
            v.push(d);
            d += Duration::days(1);
        }
        v
    }

    /// Windows of possible recording start times (unix s), sorted and merged.
    pub fn start_windows(&self) -> Vec<(f64, f64)> {
        let mut w: Vec<(f64, f64)> = Vec::new();
        let midnight = NaiveTime::from_hms_opt(0, 0, 0).unwrap();
        for &off in &self.offsets {
            match &self.start {
                Some((StartTime::Utc(t), tol)) => {
                    let t = t.timestamp() as f64;
                    w.push((t - tol, t + tol));
                }
                Some((StartTime::Local(n), tol)) => {
                    let t = Self::local_to_unix(*n, off);
                    w.push((t - tol, t + tol));
                }
                Some((StartTime::TimeOfDay(tod), tol)) => {
                    for d in self.days() {
                        let t = Self::local_to_unix(d.and_time(*tod), off);
                        w.push((t - tol, t + tol));
                    }
                }
                None => match self.hours {
                    Some((a, b)) => {
                        for d in self.days() {
                            let lo = Self::local_to_unix(d.and_time(a), off) - self.hours_slack_s;
                            let mut hi = Self::local_to_unix(d.and_time(b), off) + self.hours_slack_s;
                            if b < a {
                                hi += 86400.0;
                            }
                            w.push((lo, hi));
                        }
                    }
                    None => {
                        let lo = Self::local_to_unix(self.first_day.and_time(midnight), off);
                        let hi = Self::local_to_unix((self.last_day + Duration::days(1)).and_time(midnight), off);
                        w.push((lo, hi));
                    }
                },
            }
        }
        w.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut out: Vec<(f64, f64)> = Vec::new();
        for x in w {
            match out.last_mut() {
                Some(l) if x.0 <= l.1 => l.1 = l.1.max(x.1),
                _ => out.push(x),
            }
        }
        out
    }

    /// Earliest and latest possible start (unix s).
    pub fn start_hull(&self) -> (f64, f64) {
        let w = self.start_windows();
        (w.first().map_or(0.0, |x| x.0), w.last().map_or(0.0, |x| x.1))
    }

    /// UTC days whose archives are needed for the start windows and recording length.
    pub fn utc_days(&self, duration: f64) -> Vec<NaiveDate> {
        let mut out: Vec<NaiveDate> = Vec::new();
        for (lo, hi) in self.start_windows() {
            let mut d = DateTime::from_timestamp((lo - 900.0) as i64, 0).unwrap().date_naive();
            let b = DateTime::from_timestamp((hi + duration + 900.0) as i64, 0).unwrap().date_naive();
            while d <= b {
                if !out.contains(&d) {
                    out.push(d);
                }
                d += Duration::days(1);
            }
        }
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overseas_parts_are_left_out() {
        // a country in Europe with islands 20 km off its coast and a territory
        // in the Caribbean (as the Netherlands is in OpenStreetMap)
        let sq = |la0: f64, lo0: f64, la1: f64, lo1: f64| serde_json::json!([[[lo0, la0], [lo1, la0], [lo1, la1], [lo0, la1], [lo0, la0]]]);
        let g = serde_json::json!({
            "type": "MultiPolygon",
            "coordinates": [sq(12.0, -68.4, 12.4, -68.1), sq(50.8, 3.4, 53.2, 7.2), sq(53.4, 4.7, 53.6, 6.0)],
        });
        let b = main_part(&g, 52.24, 5.63).unwrap();
        assert_eq!((b.lat_min, b.lat_max, b.lon_min, b.lon_max), (50.8, 53.6, 3.4, 7.2));
        // the point in the territory: that part is the place
        let b = main_part(&g, 12.2, -68.2).unwrap();
        assert_eq!((b.lat_min, b.lon_max), (12.0, -68.1));
        // a single polygon, and an outline without polygons
        let one = serde_json::json!({"type": "Polygon", "coordinates": sq(50.0, 19.8, 50.1, 20.0)});
        assert_eq!(main_part(&one, 50.05, 19.9).unwrap().lat_max, 50.1);
        assert!(main_part(&serde_json::json!({"type": "Point", "coordinates": [19.9, 50.05]}), 50.05, 19.9).is_none());
    }

    /// Live Nominatim lookups (network): `cargo test -p overhead-adsb live_countries -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_countries() {
        let dir = std::env::temp_dir().join(format!("overhead-geocode-{}", std::process::id()));
        // (place, a point that must be inside, a point that must not)
        let cases: &[(&str, (f64, f64), Option<(f64, f64)>)] = &[
            ("Netherlands", (52.37, 4.90), Some((12.15, -68.27))),
            ("France", (43.30, 5.37), Some((4.93, -52.33))),
            ("Spain", (39.57, 2.65), Some((28.29, -16.63))),
            ("Portugal", (38.72, -9.14), Some((37.74, -25.67))),
            ("Norway", (69.65, 18.96), Some((78.22, 15.65))),
            ("Denmark", (55.10, 14.70), Some((64.18, -51.72))),
            ("Italy", (39.22, 9.12), None),
            ("Greece", (35.34, 25.13), None),
            ("United Kingdom", (60.15, -1.15), Some((-51.70, -57.85))),
            ("United States", (40.71, -74.01), Some((21.31, -157.86))),
            ("Poland", (54.35, 18.65), None),
            ("Germany", (54.18, 7.89), None),
            ("Kraków", (50.06, 19.94), None),
            ("małopolskie", (49.30, 19.95), None),
            ("Zeeland, Netherlands", (51.50, 3.61), None),
        ];
        for (name, yes, no) in cases {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let b = geocode(name, &dir).unwrap();
            println!("{name:24} {:7.2}..{:7.2} N {:8.2}..{:7.2} E  ({:.1} x {:.1} deg)", b.lat_min, b.lat_max, b.lon_min, b.lon_max, b.lat_max - b.lat_min, b.lon_max - b.lon_min);
            assert!(b.contains(yes.0, yes.1), "{name} must hold {yes:?}");
            if let Some(n) = no {
                assert!(!b.contains(n.0, n.1), "{name} must not hold {n:?}");
            }
        }
    }

    #[test]
    fn numeric_regions() {
        let r = resolve("52.2,21.0,10", Path::new("/nonexistent"), 0.0).unwrap();
        assert!((r.bbox.lat_max - r.bbox.lat_min - 0.18).abs() < 0.01);
        let r = resolve("49,14,55,24.2", Path::new("/nonexistent"), 0.0).unwrap();
        assert_eq!(r.bbox.lat_min, 49.0);
    }

    fn utc(t: f64) -> String {
        DateTime::from_timestamp(t as i64, 0).unwrap().to_rfc3339()
    }

    #[test]
    fn time_window() {
        let day = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        let ts = TimeSpec::from_inputs(day, None, None, 120.0, Some("13-17"), Some("+02:00"), None).unwrap();
        let w = ts.start_windows();
        // 13:00+02 = 11:00Z minus 30 min slack
        assert_eq!(w.len(), 1);
        assert_eq!(utc(w[0].0), "2026-08-15T10:30:00+00:00");
        assert_eq!(utc(w[0].1), "2026-08-15T15:30:00+00:00");
        assert_eq!(ts.utc_days(3600.0), vec![day]);
        assert_eq!(parse_offset("+02:00").unwrap(), 7200);
        assert_eq!(parse_offset("-5").unwrap(), -18000);
        let ts = TimeSpec::from_inputs(day, None, Some("14:30"), 120.0, None, Some("+02:00"), None).unwrap();
        assert_eq!(utc(ts.start_windows()[0].0 + 120.0), "2026-08-15T12:30:00+00:00");
    }

    #[test]
    fn time_zone_from_region() {
        let day = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        // Warsaw in summer is UTC+2 (the longitude alone would say +1)
        let waw = BBox::around(52.23, 21.01, 20_000.0);
        let ts = TimeSpec::from_inputs(day, None, Some("16:48"), 120.0, None, None, Some(&waw)).unwrap();
        assert_eq!(ts.offsets, vec![7200]);
        assert_eq!(utc(ts.start_windows()[0].0 + 120.0), "2026-08-15T14:48:00+00:00");
        // Madrid (longitude -3.7, summer UTC+2)
        let mad = BBox::around(40.42, -3.70, 20_000.0);
        let ts = TimeSpec::from_inputs(day, None, None, 120.0, Some("10-12"), None, Some(&mad)).unwrap();
        assert_eq!(ts.offsets, vec![7200]);
        // winter
        let jan = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        assert_eq!(TimeSpec::from_inputs(jan, None, None, 120.0, None, None, Some(&waw)).unwrap().offsets, vec![3600]);
        // Phoenix: no DST
        let phx = BBox::around(33.45, -112.07, 20_000.0);
        assert_eq!(TimeSpec::from_inputs(day, None, None, 120.0, None, None, Some(&phx)).unwrap().offsets, vec![-7 * 3600]);
        // IANA name
        let ts = TimeSpec::from_inputs(day, None, Some("16:48"), 120.0, None, Some("Europe/Warsaw"), None).unwrap();
        assert_eq!(ts.offsets, vec![7200]);
        // no region and no tz: one start window per offset in use
        let ts = TimeSpec::from_inputs(day, None, Some("16:48"), 120.0, None, None, None).unwrap();
        assert_eq!(ts.start_windows().len(), all_offsets().len());
    }

    #[test]
    fn day_ranges() {
        let day = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        assert!(TimeSpec::from_inputs(day, Some(day + Duration::days(3)), None, 120.0, None, Some("Z"), None).is_err());
        // hours apply on every day of a range
        let ts = TimeSpec::from_inputs(day, Some(day + Duration::days(2)), None, 120.0, Some("13-14"), Some("Z"), None).unwrap();
        let w = ts.start_windows();
        assert_eq!(w.len(), 3);
        assert_eq!(utc(w[2].0), "2026-08-17T12:30:00+00:00");
        assert_eq!(utc(w[2].1), "2026-08-17T14:30:00+00:00");
    }
}
