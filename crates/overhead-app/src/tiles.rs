//! OpenStreetMap raster tiles for the map panel, cached on disk.
//!
//! Tiles come from `tile.openstreetmap.org` (with the app's User-Agent, at
//! most two connections, as the OSM tile usage policy asks) and are kept
//! under `$OVERHEAD_DATA/tiles/osm/{z}/{x}/{y}.png` (default
//! `~/overhead-data/tiles`). A cached tile is used as is for 30 days and after
//! that whenever the network fails, so the map works offline for any area
//! that was viewed once. Map data © OpenStreetMap contributors (ODbL); the
//! map panel shows the attribution.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use gpui::RenderImage;
use image::{Frame, ImageBuffer, Rgba};

pub type TileKey = (u8, u32, u32);

pub const ATTRIBUTION: &str = "© OpenStreetMap contributors";
const URL: &str = "https://tile.openstreetmap.org";
const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
const WORKERS: usize = 2;

pub fn cache_dir() -> PathBuf {
    overhead_adsb::cache::data_root().join("tiles").join("osm")
}

type Decoded = Option<(u32, u32, Vec<u8>)>;

/// What the loader threads share with the map.
#[derive(Default)]
struct Shared {
    /// Tiles the map wants now, most wanted first. Replaced on every frame,
    /// so tiles of a view already left (zoomed or panned past) are never loaded.
    queue: Vec<TileKey>,
    loading: HashSet<TileKey>,
    done: Vec<(TileKey, Decoded)>,
}

pub struct Tiles {
    /// Decoded tiles and the frame each was last drawn in.
    ready: HashMap<TileKey, (Arc<RenderImage>, u64)>,
    failed: HashSet<TileKey>,
    shared: Arc<(Mutex<Shared>, Condvar)>,
    frame: u64,
}

impl Tiles {
    pub fn new() -> Self {
        let shared: Arc<(Mutex<Shared>, Condvar)> = Arc::default();
        let dir = cache_dir();
        for _ in 0..WORKERS {
            let (shared, dir) = (shared.clone(), dir.clone());
            std::thread::spawn(move || {
                let (lock, cv) = &*shared;
                loop {
                    let k = {
                        let mut sh = lock.lock().unwrap();
                        while sh.queue.is_empty() {
                            sh = cv.wait(sh).unwrap();
                        }
                        let k = sh.queue.remove(0);
                        sh.loading.insert(k);
                        k
                    };
                    let r = load(&dir, k);
                    if r.is_none() {
                        log::warn!("map tile {}/{}/{} unavailable", k.0, k.1, k.2);
                    }
                    let mut sh = lock.lock().unwrap();
                    sh.loading.remove(&k);
                    sh.done.push((k, r));
                }
            });
        }
        Self { ready: HashMap::new(), failed: HashSet::new(), shared, frame: 0 }
    }

    /// The tiles the map needs now, most wanted first (the view's centre).
    /// Earlier wishes that are not loading yet are dropped.
    pub fn want(&mut self, keys: &[TileKey]) {
        self.frame += 1;
        let (lock, cv) = &*self.shared;
        let mut sh = lock.lock().unwrap();
        let done: HashSet<TileKey> = sh.done.iter().map(|d| d.0).collect();
        let queue: Vec<TileKey> = keys
            .iter()
            .copied()
            .filter(|k| !self.ready.contains_key(k) && !self.failed.contains(k) && !sh.loading.contains(k) && !done.contains(k))
            .collect();
        if queue != sh.queue {
            sh.queue = queue;
            cv.notify_all();
        }
    }

    /// Take finished tiles; true if anything new is ready.
    pub fn poll(&mut self) -> bool {
        let got: Vec<(TileKey, Decoded)> = std::mem::take(&mut self.shared.0.lock().unwrap().done);
        let changed = !got.is_empty();
        for (k, r) in got {
            match r {
                Some((w, h, bgra)) => {
                    if let Some(buf) = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(w, h, bgra) {
                        self.ready.insert(k, (Arc::new(RenderImage::new(vec![Frame::new(buf)])), self.frame));
                    }
                }
                None => {
                    self.failed.insert(k);
                }
            }
        }
        changed
    }

    pub fn loading(&self) -> bool {
        let sh = self.shared.0.lock().unwrap();
        !sh.queue.is_empty() || !sh.loading.is_empty() || !sh.done.is_empty()
    }

    /// Once there are more than `max` decoded tiles, forget the ones not
    /// drawn for the longest time (they reload from disk); returns their
    /// images so the window can free them on the GPU.
    pub fn trim(&mut self, max: usize) -> Vec<Arc<RenderImage>> {
        if self.ready.len() <= max {
            return vec![];
        }
        let mut by_age: Vec<(u64, TileKey)> = self.ready.iter().map(|(k, (_, f))| (*f, *k)).collect();
        by_age.sort_unstable();
        let n = self.ready.len() - max * 3 / 4;
        self.failed.clear();
        by_age.into_iter().take(n).filter_map(|(_, k)| self.ready.remove(&k).map(|v| v.0)).collect()
    }

    /// A decoded tile; it counts as drawn in this frame.
    pub fn get(&mut self, k: &TileKey) -> Option<Arc<RenderImage>> {
        let frame = self.frame;
        self.ready.get_mut(k).map(|(img, f)| {
            *f = frame;
            img.clone()
        })
    }

    /// Ready tiles that cover `k` while it loads: its nearest loaded
    /// ancestor (up to 6 levels coarser), else its loaded children.
    pub fn stand_ins(&mut self, k: TileKey, out: &mut Vec<(TileKey, Arc<RenderImage>)>) {
        let (z, x, y) = k;
        for up in 1..=6u8.min(z) {
            let a = (z - up, x >> up, y >> up);
            if let Some(img) = self.get(&a) {
                if !out.iter().any(|o| o.0 == a) {
                    out.push((a, img));
                }
                return;
            }
        }
        for c in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let ch = (z + 1, 2 * x + c.0, 2 * y + c.1);
            if let Some(img) = self.get(&ch) {
                out.push((ch, img));
            }
        }
    }
}

/// Cached or downloaded tile, decoded to BGRA (GPUI sprites are BGRA).
fn load(dir: &std::path::Path, (z, x, y): TileKey) -> Decoded {
    let path = dir.join(z.to_string()).join(x.to_string()).join(format!("{y}.png"));
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < MAX_AGE);
    let bytes = if fresh {
        std::fs::read(&path).ok()
    } else {
        match overhead_adsb::http::get_bytes(&format!("{URL}/{z}/{x}/{y}.png")) {
            Ok(Some(b)) => {
                if let Some(p) = path.parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                let tmp = path.with_extension("png.part");
                if std::fs::write(&tmp, &b).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
                Some(b)
            }
            // offline or refused: an old copy is better than nothing
            _ => std::fs::read(&path).ok(),
        }
    }?;
    decode(&bytes)
}

fn decode(bytes: &[u8]) -> Decoded {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()?];
    let info = r.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    let px = &buf[..info.buffer_size()];
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    match info.color_type {
        png::ColorType::Rgba => px.chunks_exact(4).for_each(|p| out.extend_from_slice(&[p[2], p[1], p[0], p[3]])),
        png::ColorType::Rgb => px.chunks_exact(3).for_each(|p| out.extend_from_slice(&[p[2], p[1], p[0], 255])),
        png::ColorType::GrayscaleAlpha => px.chunks_exact(2).for_each(|p| out.extend_from_slice(&[p[0], p[0], p[0], p[1]])),
        png::ColorType::Grayscale => px.iter().for_each(|&g| out.extend_from_slice(&[g, g, g, 255])),
        png::ColorType::Indexed => return None,
    }
    Some((w, h, out))
}
