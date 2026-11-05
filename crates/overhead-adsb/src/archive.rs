//! Fetching one day of adsb.lol history into the local cache.
//!
//! adsb.lol publishes one GitHub release per day and readsb instance ("pod") in
//! `adsblol/globe_history_<year>`, tag `v<YYYY>.<MM>.<DD>-planes-readsb-prod-0`
//! (2023 also has `prod-1`). The asset is a plain tar, split into 2 GB pieces
//! (`.tar.aa`, `.tar.ab`, ...; about 3-4 GB per day in 2025-2026), containing
//! `heatmap/*.bin.ttf` (≈0.8 GB of 30-minute position snapshots), a few small
//! files, and `traces/<xx>/trace_full_<hex>.json`: one gzipped full-day trace
//! per aircraft, ordered by the last two hex digits, so not by place or time.
//!
//! There is no index and no per-region split, so a day has to be read once.
//! We skip the heatmaps with a handful of range requests, then stream the
//! trace section with several parallel range requests (each worker syncs to
//! the next tar header in its slice). Every trace's type code is read from its
//! first few hundred decompressed bytes; jets are only indexed, propeller and
//! rotor aircraft (and unknown types) are kept verbatim in `kept.bin` with an
//! index of their airborne bounding box and time span. Later queries for any
//! region of that day are then local and fast.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::geo::BBox;
use crate::http;
use crate::trace;
use crate::types::{self, AircraftClass};

pub const CACHE_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Part {
    pub url: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub parts: Vec<Part>,
}

impl Release {
    pub fn size(&self) -> u64 {
        self.parts.iter().map(|p| p.size).sum()
    }
}

/// Locate the archive(s) of `day`: every `prod-N` pod that exists.
pub fn resolve(day: NaiveDate) -> Result<Vec<Release>> {
    let (y, m, d) = (day.year(), day.month(), day.day());
    let mut out = Vec::new();
    for pod in 0..3 {
        let tag = format!("v{y}.{m:02}.{d:02}-planes-readsb-prod-{pod}");
        let base = format!("https://github.com/adsblol/globe_history_{y}/releases/download/{tag}/{tag}");
        let mut parts = Vec::new();
        if let Some(size) = http::probe_size(&format!("{base}.tar"))? {
            parts.push(Part { url: format!("{base}.tar"), size });
        } else {
            for suffix in ["aa", "ab", "ac", "ad", "ae", "af", "ag", "ah"] {
                let url = format!("{base}.tar.{suffix}");
                match http::probe_size(&url)? {
                    Some(size) => parts.push(Part { url, size }),
                    None => break,
                }
            }
        }
        if parts.is_empty() {
            if pod == 0 {
                continue;
            }
            break;
        }
        out.push(Release { tag, parts });
    }
    if out.is_empty() {
        bail!(
            "no adsb.lol archive found for {day} (globe_history_{y}, tag v{y}.{m:02}.{d:02}-planes-readsb-prod-0). \
             Archives appear a day or two after the day ends; coverage starts on 2023-03-10."
        );
    }
    Ok(out)
}

/// One entry of a day index (every aircraft in the archive).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexEntry {
    pub hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_code: Option<String>,
    pub class: AircraftClass,
    pub pod: u8,
    /// Byte range of the raw gzipped trace in `kept.bin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept: Option<(u64, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<BBox>,
    #[serde(default)]
    pub t0: f64,
    #[serde(default)]
    pub t1: f64,
    #[serde(default)]
    pub points: u32,
    /// Lowest airborne altitude (m, baro or geometric); NaN if unknown.
    #[serde(default = "nan", deserialize_with = "null_as_nan")]
    pub min_alt: f32,
}

fn nan() -> f32 {
    f32::NAN
}

fn null_as_nan<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    Ok(Option::<f32>::deserialize(d)?.unwrap_or(f32::NAN))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DayMeta {
    pub version: u32,
    pub day: String,
    pub releases: Vec<Release>,
    pub entries: usize,
    pub kept: usize,
    pub kept_bytes: u64,
    pub downloaded_bytes: u64,
    pub seconds: f64,
}

pub fn day_dir(cache: &Path, day: NaiveDate) -> PathBuf {
    cache.join(day.format("%Y-%m-%d").to_string())
}

pub fn is_cached(cache: &Path, day: NaiveDate) -> bool {
    let dir = day_dir(cache, day);
    match fs::read(dir.join("meta.json")) {
        Ok(b) => serde_json::from_slice::<DayMeta>(&b).is_ok_and(|m| m.version == CACHE_VERSION),
        Err(_) => false,
    }
}

/// A tar split over several HTTP resources, addressed by one virtual offset.
struct Virtual {
    parts: Vec<Part>,
}

impl Virtual {
    fn len(&self) -> u64 {
        self.parts.iter().map(|p| p.size).sum()
    }
    fn locate(&self, off: u64) -> (usize, u64) {
        let mut o = off;
        for (i, p) in self.parts.iter().enumerate() {
            if o < p.size {
                return (i, o);
            }
            o -= p.size;
        }
        (self.parts.len(), 0)
    }
    fn get(&self, start: u64, end: u64) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity((end - start) as usize);
        let mut off = start;
        while off < end {
            let (i, local) = self.locate(off);
            if i >= self.parts.len() {
                break;
            }
            let take = (self.parts[i].size - local).min(end - off);
            out.extend(http::get_range(&self.parts[i].url, local, local + take)?);
            off += take;
        }
        Ok(out)
    }
    fn stream<'a>(&'a self, start: u64, counter: &'a AtomicU64) -> VirtualReader<'a> {
        VirtualReader { v: self, off: start, cur: None, counter }
    }
}

struct VirtualReader<'a> {
    v: &'a Virtual,
    off: u64,
    cur: Option<Box<dyn Read + Send + Sync>>,
    counter: &'a AtomicU64,
}

impl Read for VirtualReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.cur.is_none() {
                let (i, local) = self.v.locate(self.off);
                if i >= self.v.parts.len() {
                    return Ok(0);
                }
                let r = http::open_range(&self.v.parts[i].url, local, None).map_err(std::io::Error::other)?;
                self.cur = Some(r);
            }
            let n = match self.cur.as_mut().unwrap().read(buf) {
                Ok(n) => n,
                Err(e) => {
                    // reconnect at the current offset
                    eprintln!("  stream error at {}: {e}; reconnecting", self.off);
                    self.cur = None;
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    continue;
                }
            };
            if n == 0 {
                let (i, local) = self.v.locate(self.off);
                if i < self.v.parts.len() && local != 0 {
                    // connection ended early inside a part
                    self.cur = None;
                    continue;
                }
                self.cur = None;
                if i >= self.v.parts.len() {
                    return Ok(0);
                }
                continue;
            }
            self.off += n as u64;
            self.counter.fetch_add(n as u64, Ordering::Relaxed);
            return Ok(n);
        }
    }
}

struct TarHeader {
    name: String,
    size: u64,
}

fn parse_header(h: &[u8]) -> Option<TarHeader> {
    if h.len() < 512 || h[257..262] != *b"ustar" {
        return None;
    }
    let octal = |s: &[u8]| -> Option<u64> {
        let s = std::str::from_utf8(s).ok()?.trim_matches(|c: char| c == '\0' || c == ' ');
        if s.is_empty() { Some(0) } else { u64::from_str_radix(s, 8).ok() }
    };
    let stored = octal(&h[148..156])?;
    let sum: u64 = h.iter().enumerate().map(|(i, b)| if (148..156).contains(&i) { 32 } else { *b as u64 }).sum();
    if sum != stored {
        return None;
    }
    let name = String::from_utf8_lossy(&h[..100]).trim_end_matches('\0').to_string();
    let prefix = String::from_utf8_lossy(&h[345..500]).trim_end_matches('\0').to_string();
    let name = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
    Some(TarHeader { name, size: octal(&h[124..136])? })
}

fn padded(size: u64) -> u64 {
    size.div_ceil(512) * 512
}

/// Offset of the first `traces/` entry, walking headers with small range requests.
fn traces_start(v: &Virtual) -> Result<u64> {
    let mut off = 0u64;
    for _ in 0..400 {
        let h = v.get(off, off + 512)?;
        if h.len() < 512 || h.iter().all(|b| *b == 0) {
            bail!("tar ended before traces/");
        }
        let hdr = parse_header(&h).with_context(|| format!("bad tar header at {off}"))?;
        if hdr.name.contains("traces/") {
            return Ok(off);
        }
        off += 512 + padded(hdr.size);
    }
    bail!("traces/ not found in the first 400 entries")
}

struct Found {
    entry: IndexEntry,
    blob: Option<Vec<u8>>,
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut n = 0;
    while n < buf.len() {
        let k = r.read(&mut buf[n..])?;
        if k == 0 {
            return Ok(false);
        }
        n += k;
    }
    Ok(true)
}

fn skip(r: &mut impl Read, mut n: u64) -> std::io::Result<bool> {
    let mut buf = [0u8; 65536];
    while n > 0 {
        let k = n.min(buf.len() as u64) as usize;
        if !read_full(r, &mut buf[..k])? {
            return Ok(false);
        }
        n -= k as u64;
    }
    Ok(true)
}

fn process_entry(name: &str, data: Vec<u8>, pod: u8) -> Option<Found> {
    let file = name.rsplit('/').next()?;
    let hex = file.strip_prefix("trace_full_")?.strip_suffix(".json")?.to_ascii_lowercase();
    let h = trace::peek_header(&data);
    let class = types::classify(h.type_code.as_deref(), None);
    let base = IndexEntry {
        hex,
        reg: h.reg,
        type_code: h.type_code,
        class,
        pod,
        kept: None,
        bbox: None,
        t0: 0.0,
        t1: 0.0,
        points: 0,
        min_alt: f32::NAN,
    };
    if !class.is_candidate() {
        return Some(Found { entry: base, blob: None });
    }
    // full parse for the airborne extent (unknown types may resolve via category)
    let Ok(track) = trace::parse_gz(&data) else { return Some(Found { entry: base, blob: None }) };
    let mut e = base;
    e.class = track.class;
    let Some((bbox, t0, t1)) = track.airborne_extent() else { return Some(Found { entry: e, blob: None }) };
    if !track.class.is_candidate() {
        return Some(Found { entry: e, blob: None });
    }
    e.bbox = Some(bbox);
    e.t0 = t0;
    e.t1 = t1;
    e.points = track.points.len() as u32;
    e.min_alt = track
        .points
        .iter()
        .filter(|p| !p.ground)
        .filter_map(|p| p.alt_geom.or(p.alt_baro))
        .fold(f64::INFINITY, f64::min) as f32;
    Some(Found { entry: e, blob: Some(data) })
}

/// Scan the trace section `[a, b)` (entries whose header starts there).
/// Returns whether the slice was read completely: up to `b`, or for the last
/// slice up to the tar's end-of-archive block. Running out of data earlier
/// means a truncated stream or a release whose parts are still uploading.
fn scan_slice(v: &Virtual, a: u64, b: u64, last: bool, pod: u8, counter: &AtomicU64, sink: &Mutex<Sink>) -> Result<bool> {
    let mut r = std::io::BufReader::with_capacity(1 << 20, v.stream(a, counter));
    let mut off = a;
    let mut block = vec![0u8; 512];
    // sync to the first header at or after `a`
    loop {
        if off >= b {
            return Ok(!last);
        }
        if !read_full(&mut r, &mut block)? {
            return Ok(false);
        }
        if parse_header(&block).is_some() {
            break;
        }
        off += 512;
    }
    let mut batch = Vec::new();
    let mut complete = false;
    loop {
        if block.iter().all(|x| *x == 0) {
            complete = true;
            break;
        }
        let Some(h) = parse_header(&block) else { bail!("lost tar sync at offset {off}") };
        let mut data = vec![0u8; h.size as usize];
        if !read_full(&mut r, &mut data)? {
            break;
        }
        if !skip(&mut r, padded(h.size) - h.size)? {
            break;
        }
        if h.name.contains("/trace_full_") {
            if let Some(f) = process_entry(&h.name, data, pod) {
                batch.push(f);
            }
        }
        if batch.len() >= 64 {
            sink.lock().unwrap().push(std::mem::take(&mut batch))?;
        }
        off += 512 + padded(h.size);
        if off >= b && !last {
            complete = true;
            break;
        }
        if !read_full(&mut r, &mut block)? {
            break;
        }
    }
    sink.lock().unwrap().push(batch)?;
    Ok(complete)
}

struct Sink {
    kept: BufWriter<File>,
    kept_off: u64,
    index: Vec<IndexEntry>,
}

impl Sink {
    fn push(&mut self, batch: Vec<Found>) -> Result<()> {
        for f in batch {
            let mut e = f.entry;
            if let Some(blob) = f.blob {
                self.kept.write_all(&blob)?;
                e.kept = Some((self.kept_off, blob.len() as u32));
                self.kept_off += blob.len() as u64;
            }
            self.index.push(e);
        }
        Ok(())
    }
}

/// Download and index one day into `<cache>/<YYYY-MM-DD>/` unless already there.
pub fn fetch_day(cache: &Path, day: NaiveDate, workers: usize) -> Result<DayMeta> {
    let dir = day_dir(cache, day);
    if is_cached(cache, day) {
        return Ok(serde_json::from_slice(&fs::read(dir.join("meta.json"))?)?);
    }
    fs::create_dir_all(&dir)?;
    let started = Instant::now();
    let releases = resolve(day)?;
    let tmp = dir.join("kept.bin.part");
    let sink = Mutex::new(Sink { kept: BufWriter::new(File::create(&tmp)?), kept_off: 0, index: Vec::new() });
    let counter = AtomicU64::new(0);
    for (pod, rel) in releases.iter().enumerate() {
        let v = Virtual { parts: rel.parts.clone() };
        let total = v.len();
        let start = traces_start(&v)?;
        eprintln!(
            "{day}: {} ({:.2} GB, traces from {:.2} GB), {workers} parallel streams",
            rel.tag,
            total as f64 / 1e9,
            start as f64 / 1e9
        );
        let n = workers.max(1) as u64;
        let span = (total - start).div_ceil(n);
        let base = counter.load(Ordering::Relaxed);
        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|s| -> Result<()> {
            let progress = s.spawn(|| {
                let t = Instant::now();
                let mut ticks = 0u32;
                while !done.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    ticks += 1;
                    let got = counter.load(Ordering::Relaxed) - base;
                    if ticks % 20 == 0 {
                        eprintln!(
                            "  {:.2}/{:.2} GB  {:.0} MB/s",
                            got as f64 / 1e9,
                            (total - start) as f64 / 1e9,
                            got as f64 / 1e6 / t.elapsed().as_secs_f64()
                        );
                    }
                }
            });
            let handles: Vec<_> = (0..n)
                .map(|w| {
                    let edge = |k: u64| if k >= n { total } else { start + (k * span) / 512 * 512 };
                    let (a, b) = (edge(w), edge(w + 1));
                    let (v, counter, sink) = (&v, &counter, &sink);
                    s.spawn(move || scan_slice(v, a, b, w + 1 == n, pod as u8, counter, sink))
                })
                .collect();
            let mut res = Ok(());
            for (w, h) in handles.into_iter().enumerate() {
                match h.join().expect("scan worker panicked") {
                    Err(e) => res = Err(e),
                    Ok(false) => {
                        res = Err(anyhow::anyhow!(
                            "{}: slice {w} of the archive ended early (truncated download, or the release is still being uploaded); \
                             not caching this day, try again later",
                            rel.tag
                        ))
                    }
                    Ok(true) => {}
                }
            }
            done.store(true, Ordering::Relaxed);
            progress.join().ok();
            res
        })?;
    }
    let mut sink = sink.into_inner().unwrap();
    sink.kept.flush()?;
    drop(sink.kept);
    // deduplicate entries a worker may have seen twice (none expected)
    sink.index.sort_by(|a, b| (a.pod, &a.hex).cmp(&(b.pod, &b.hex)));
    let mut idx = BufWriter::new(File::create(dir.join("index.jsonl"))?);
    for e in &sink.index {
        serde_json::to_writer(&mut idx, e)?;
        idx.write_all(b"\n")?;
    }
    idx.flush()?;
    fs::rename(&tmp, dir.join("kept.bin"))?;
    let kept = sink.index.iter().filter(|e| e.kept.is_some()).count();
    let meta = DayMeta {
        version: CACHE_VERSION,
        day: day.format("%Y-%m-%d").to_string(),
        releases,
        entries: sink.index.len(),
        kept,
        kept_bytes: sink.kept_off,
        downloaded_bytes: counter.load(Ordering::Relaxed),
        seconds: started.elapsed().as_secs_f64(),
    };
    fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(&meta)?)?;
    eprintln!(
        "{day}: {} aircraft, {} kept ({:.0} MB), {:.2} GB read in {:.0} s",
        meta.entries,
        meta.kept,
        meta.kept_bytes as f64 / 1e6,
        meta.downloaded_bytes as f64 / 1e9,
        meta.seconds
    );
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, size: u64) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[156] = b'0';
        h[257..265].copy_from_slice(b"ustar  \0");
        let sum: u64 = h.iter().enumerate().map(|(i, b)| if (148..156).contains(&i) { 32 } else { *b as u64 }).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        h
    }

    /// A complete tar is accepted; one cut short (a part still uploading, or a
    /// truncated stream) must not be.
    #[test]
    fn truncated_archive_is_detected() {
        let mut tar = Vec::new();
        for i in 0..40 {
            let data = vec![b'x'; 3000 + i * 17];
            tar.extend(header(&format!("./traces/{i:02x}/trace_full_aa00{i:02x}.json"), data.len() as u64));
            tar.extend(&data);
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        }
        tar.extend(vec![0u8; 1024]);
        let dir = std::env::temp_dir().join(format!("overhead-adsb-tar-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let scan = |bytes: &[u8], workers: u64| -> Vec<bool> {
            let path = dir.join("t.tar");
            fs::write(&path, bytes).unwrap();
            let v = Virtual { parts: vec![Part { url: format!("file://{}", path.display()), size: bytes.len() as u64 }] };
            let total = v.len();
            let sink = Mutex::new(Sink { kept: BufWriter::new(File::create(dir.join("k")).unwrap()), kept_off: 0, index: Vec::new() });
            let counter = AtomicU64::new(0);
            let span = total.div_ceil(workers);
            (0..workers)
                .map(|w| {
                    let edge = |k: u64| if k >= workers { total } else { (k * span) / 512 * 512 };
                    scan_slice(&v, edge(w), edge(w + 1), w + 1 == workers, 0, &counter, &sink).unwrap()
                })
                .collect()
        };
        assert!(scan(&tar, 1).iter().all(|c| *c));
        assert!(scan(&tar, 3).iter().all(|c| *c));
        let cut = &tar[..tar.len() / 2 / 512 * 512 + 100];
        assert!(!scan(cut, 1).iter().all(|c| *c));
        assert!(!scan(cut, 3).iter().all(|c| *c));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tar_header_roundtrip() {
        let mut h = vec![0u8; 512];
        let name = b"./traces/1c/trace_full_89631c.json";
        h[..name.len()].copy_from_slice(name);
        h[100..108].copy_from_slice(b"0000644\0");
        h[124..136].copy_from_slice(b"00000227405\0");
        h[156] = b'0';
        h[257..265].copy_from_slice(b"ustar  \0");
        let sum: u64 = h.iter().enumerate().map(|(i, b)| if (148..156).contains(&i) { 32 } else { *b as u64 }).sum();
        let s = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(s.as_bytes());
        let p = parse_header(&h).unwrap();
        assert_eq!(p.size, 0o227405);
        assert!(p.name.ends_with("trace_full_89631c.json"));
        h[0] ^= 1;
        assert!(parse_header(&h).is_none());
    }
}
