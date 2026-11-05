//! readsb `trace_full_<hex>.json` files (the per-aircraft daily traces in the
//! adsb.lol `globe_history` archives).
//!
//! Each file is gzip-compressed JSON:
//! `{"icao", "r" (registration), "t" (type), "desc", "dbFlags", "timestamp", "trace": [...]}`
//! and each trace point is an array
//! `[dt, lat, lon, alt_baro_ft | "ground" | null, gs_kt, track_deg, flags,
//!   baro_rate_fpm, details | null, source, alt_geom_ft, geom_rate_fpm, ias, roll]`
//! with `dt` seconds after `timestamp`. Flags: 1 stale, 2 new leg,
//! 4 vertical rate is geometric, 8 altitude is geometric.

use std::io::Read;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::geo::BBox;
use crate::types::{self, AircraftClass};

const FT: f64 = 0.3048;
const KT: f64 = 0.514_444;
const FPM: f64 = 0.00508;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Point {
    /// Unix time (s).
    pub t: f64,
    pub lat: f64,
    pub lon: f64,
    /// Barometric (pressure) altitude (m); `None` on the ground or unknown.
    pub alt_baro: Option<f64>,
    /// Geometric (GNSS, WGS84 ellipsoid) altitude (m).
    pub alt_geom: Option<f64>,
    pub ground: bool,
    /// Ground speed (m/s) and track (deg true).
    pub gs: Option<f64>,
    pub track: Option<f64>,
    /// Vertical rate (m/s).
    pub vrate: Option<f64>,
    pub stale: bool,
    pub new_leg: bool,
    /// Position from multilateration (much less accurate than ADS-B's GNSS position).
    #[serde(default)]
    pub mlat: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub hex: String,
    pub reg: Option<String>,
    pub type_code: Option<String>,
    pub desc: Option<String>,
    pub class: AircraftClass,
    /// Callsigns with the time they were first seen.
    pub callsigns: Vec<(f64, String)>,
    pub points: Vec<Point>,
}

#[derive(Deserialize)]
struct Raw {
    icao: String,
    r: Option<String>,
    t: Option<String>,
    desc: Option<String>,
    timestamp: f64,
    trace: Vec<Vec<Value>>,
}

fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(|v| v.as_f64())
}

/// Header fields that appear before the trace array: enough to classify an
/// aircraft from the first few hundred decompressed bytes.
#[derive(Clone, Debug, Default)]
pub struct Header {
    pub hex: Option<String>,
    pub reg: Option<String>,
    pub type_code: Option<String>,
    pub desc: Option<String>,
}

fn field(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":");
    let i = s.find(&pat)? + pat.len();
    let rest = s[i..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let j = rest.find('"')?;
    Some(rest[..j].to_string())
}

/// Decompress just the beginning of a gzipped trace and read its header.
pub fn peek_header(gz: &[u8]) -> Header {
    let mut dec = flate2::read::GzDecoder::new(gz);
    let mut buf = vec![0u8; 768];
    let mut n = 0;
    while n < buf.len() {
        match dec.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    let s = String::from_utf8_lossy(&buf[..n]);
    let head = s.split("\"trace\"").next().unwrap_or("");
    Header { hex: field(head, "icao"), reg: field(head, "r"), type_code: field(head, "t"), desc: field(head, "desc") }
}

pub fn parse_gz(gz: &[u8]) -> Result<Track> {
    let mut s = Vec::with_capacity(gz.len() * 6);
    flate2::read::GzDecoder::new(gz).read_to_end(&mut s).context("gunzip trace")?;
    parse_json(&s)
}

pub fn parse_json(bytes: &[u8]) -> Result<Track> {
    let raw: Raw = serde_json::from_slice(bytes).context("trace json")?;
    let mut points = Vec::with_capacity(raw.trace.len());
    let mut callsigns: Vec<(f64, String)> = Vec::new();
    let mut category: Option<String> = None;
    for p in &raw.trace {
        let (Some(dt), Some(lat), Some(lon)) = (num(p.first()), num(p.get(1)), num(p.get(2))) else { continue };
        let t = raw.timestamp + dt;
        let flags = p.get(6).and_then(|v| v.as_i64()).unwrap_or(0);
        let ground = p.get(3).and_then(|v| v.as_str()) == Some("ground");
        let alt3 = num(p.get(3)).map(|a| a * FT);
        let (mut alt_baro, mut alt_geom) = (alt3, num(p.get(10)).map(|a| a * FT));
        if flags & 8 != 0 {
            alt_geom = alt_geom.or(alt3);
            alt_baro = None;
        }
        let vrate = num(p.get(7)).or_else(|| num(p.get(11))).map(|v| v * FPM);
        if let Some(Value::Object(d)) = p.get(8) {
            if let Some(cs) = d.get("flight").and_then(|v| v.as_str()) {
                let cs = cs.trim();
                if !cs.is_empty() && callsigns.last().is_none_or(|l| l.1 != cs) {
                    callsigns.push((t, cs.to_string()));
                }
            }
            if category.is_none() {
                category = d.get("category").and_then(|v| v.as_str()).map(str::to_string);
            }
        }
        points.push(Point {
            t,
            lat,
            lon,
            alt_baro,
            alt_geom,
            ground,
            gs: num(p.get(4)).map(|v| v * KT),
            track: num(p.get(5)),
            vrate,
            stale: flags & 1 != 0,
            new_leg: flags & 2 != 0,
            mlat: p.get(9).and_then(|v| v.as_str()) == Some("mlat"),
        });
    }
    let class = types::classify(raw.t.as_deref(), category.as_deref());
    Ok(Track {
        hex: raw.icao.to_ascii_lowercase(),
        reg: raw.r,
        type_code: raw.t,
        desc: raw.desc,
        class,
        callsigns,
        points,
    })
}

impl Track {
    /// Callsign in use at time `t` (last one seen before, else the first).
    pub fn callsign_at(&self, t: f64) -> Option<&str> {
        self.callsigns.iter().rev().find(|c| c.0 <= t).or(self.callsigns.first()).map(|c| c.1.as_str())
    }

    /// Bounding box and time span of the airborne points.
    pub fn airborne_extent(&self) -> Option<(BBox, f64, f64)> {
        let mut b = BBox::empty();
        let (mut t0, mut t1) = (f64::INFINITY, f64::NEG_INFINITY);
        for p in self.points.iter().filter(|p| !p.ground) {
            b.extend(p.lat, p.lon);
            t0 = t0.min(p.t);
            t1 = t1.max(p.t);
        }
        (!b.is_empty()).then_some((b, t0, t1))
    }

    pub fn max_gs(&self) -> f64 {
        self.points.iter().filter_map(|p| p.gs).fold(0.0, f64::max)
    }

    /// Merge another trace of the same aircraft (e.g. from a second archive pod).
    pub fn merge(&mut self, other: Track) {
        self.points.extend(other.points);
        self.points.sort_by(|a, b| a.t.total_cmp(&b.t));
        self.points.dedup_by(|a, b| (a.t - b.t).abs() < 0.05);
        self.callsigns.extend(other.callsigns);
        self.callsigns.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.reg = self.reg.take().or(other.reg);
        self.type_code = self.type_code.take().or(other.type_code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"icao":"48ae01",
"r":"SP-ABC",
"t":"C172",
"dbFlags":0,"desc":"CESSNA 172",
"timestamp": 1767052800.000,
"trace":[
[10.0,52.1,21.0,"ground",5.0,90.0,0,null,null,"adsb_icao",null,null,null,null],
[70.5,52.2,21.1,1500,100.0,90.0,0,512,{"type":"adsb_icao","flight":"SPABC   ","category":"A1"},"adsb_icao",1650,null,null,null],
[80.5,52.2,21.11,1525,100.0,90.0,8,null,null,"adsb_icao",null,null,null,null]
]}"#;

    #[test]
    fn parses_sample() {
        let t = parse_json(SAMPLE.as_bytes()).unwrap();
        assert_eq!(t.hex, "48ae01");
        assert_eq!(t.class, AircraftClass::Piston);
        assert_eq!(t.points.len(), 3);
        assert!(t.points[0].ground);
        let p = &t.points[1];
        assert!((p.t - 1767052870.5).abs() < 1e-6);
        assert!((p.alt_baro.unwrap() - 1500.0 * FT).abs() < 1e-9);
        assert!((p.alt_geom.unwrap() - 1650.0 * FT).abs() < 1e-9);
        assert!((p.gs.unwrap() - 100.0 * KT).abs() < 1e-9);
        assert!(t.points[2].alt_baro.is_none() && t.points[2].alt_geom.is_some());
        assert_eq!(t.callsign_at(1e10), Some("SPABC"));
    }

    #[test]
    fn header_peek() {
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(SAMPLE.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        let h = peek_header(&gz);
        assert_eq!(h.type_code.as_deref(), Some("C172"));
        assert_eq!(h.reg.as_deref(), Some("SP-ABC"));
        assert_eq!(h.hex.as_deref(), Some("48ae01"));
    }
}
