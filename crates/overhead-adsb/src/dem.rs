//! Terrain height from SRTM (AWS Terrain Tiles, "skadi" 1° tiles, heights in
//! metres above the EGM96 geoid). Tiles are downloaded once, reduced from 1"
//! to 3" (≈90 m) and cached as raw little-endian i16. Missing tiles (sea) are
//! height 0 above the geoid.
//!
//! [`Dem::coarse`] is a low-resolution variant (Terrarium zoom 7) for the
//! coarse pass search over very large areas.
//!
//! [`Dem::height`] returns heights above the WGS84 ellipsoid (SRTM + EGM96
//! undulation), the frame of ADS-B geometric altitude and of [`crate::geo::ecef`].

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::Result;

use crate::http;

const N: usize = 1201;

pub struct Tile {
    /// Row 0 is the northern edge.
    h: Vec<i16>,
}

impl Tile {
    fn at(&self, r: usize, c: usize) -> f64 {
        let v = self.h[r * N + c];
        if v == i16::MIN { 0.0 } else { v as f64 }
    }
}

/// Coarse terrain for searches over very large areas: Terrarium tiles of the
/// same AWS bucket at zoom 7 (256 px over 2.8° of longitude, ≈1.2 km pixels at
/// the equator, ≈750 m at 52° N), so a continent costs a few hundred MB
/// instead of tens of GB of SRTM. Only the coarse pass search uses it; curve
/// scoring and refinement always use SRTM.
const TZ: u32 = 7;
const TN: usize = 256;

pub struct Dem {
    dir: Option<PathBuf>,
    /// Terrarium zoom-7 tiles instead of SRTM.
    coarse: bool,
    tiles: RwLock<HashMap<(i32, i32), Option<Arc<Tile>>>>,
    /// Fixed height for every point (testing / offline).
    flat: Option<f64>,
    failed: Mutex<Vec<String>>,
}

fn tile_name(lat: i32, lon: i32) -> String {
    format!(
        "{}{:02}{}{:03}",
        if lat >= 0 { 'N' } else { 'S' },
        lat.abs(),
        if lon >= 0 { 'E' } else { 'W' },
        lon.abs()
    )
}

impl Dem {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir: Some(dir), coarse: false, tiles: RwLock::new(HashMap::new()), flat: None, failed: Mutex::new(Vec::new()) }
    }

    pub fn flat(h: f64) -> Self {
        Self { dir: None, coarse: false, tiles: RwLock::new(HashMap::new()), flat: Some(h), failed: Mutex::new(Vec::new()) }
    }

    /// Coarse DEM (Terrarium zoom 7) sharing the cache directory.
    pub fn coarse(dir: PathBuf) -> Self {
        Self { dir: Some(dir), coarse: true, tiles: RwLock::new(HashMap::new()), flat: None, failed: Mutex::new(Vec::new()) }
    }

    /// A coarse DEM over the same cache (`None` for a flat or already coarse DEM).
    pub fn coarse_twin(&self) -> Option<Dem> {
        if self.flat.is_some() || self.coarse {
            return None;
        }
        self.dir.clone().map(Dem::coarse)
    }

    pub fn is_flat(&self) -> bool {
        self.flat.is_some()
    }

    fn load_terrarium(&self, x: i32, y: i32) -> Result<Option<Arc<Tile>>> {
        let Some(dir) = &self.dir else { return Ok(None) };
        let dir = dir.join(format!("terrarium{TZ}"));
        let path = dir.join(format!("{x}_{y}.i16"));
        if let Ok(b) = std::fs::read(&path)
            && b.len() == TN * TN * 2
        {
            let h = b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            return Ok(Some(Arc::new(Tile { h })));
        }
        std::fs::create_dir_all(&dir)?;
        let url = format!("https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{TZ}/{x}/{y}.png");
        let Some(bytes) = http::get_bytes(&url)? else { return Ok(None) };
        let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = dec.read_info()?;
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(TN * TN * 4)];
        let info = reader.next_frame(&mut buf)?;
        let ch = info.color_type.samples();
        if info.width as usize != TN || info.height as usize != TN || ch < 3 {
            anyhow::bail!("unexpected terrarium tile {}x{} with {ch} channels", info.width, info.height);
        }
        let h: Vec<i16> = (0..TN * TN)
            .map(|i| {
                let p = &buf[i * ch..i * ch + 3];
                let v = p[0] as f64 * 256.0 + p[1] as f64 + p[2] as f64 / 256.0 - 32768.0;
                v.max(-500.0).round() as i16
            })
            .collect();
        let bytes: Vec<u8> = h.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(&path, bytes)?;
        Ok(Some(Arc::new(Tile { h })))
    }

    /// Web-mercator tile and pixel position of a point at zoom `TZ`.
    fn mercator(lat: f64, lon: f64) -> (i32, i32, f64, f64) {
        let n = (1u32 << TZ) as f64;
        let lat = lat.clamp(-85.0, 85.0).to_radians();
        let x = (lon + 180.0) / 360.0 * n;
        let y = (1.0 - lat.tan().asinh() / std::f64::consts::PI) / 2.0 * n;
        let (tx, ty) = (x.floor(), y.floor());
        (tx as i32, ty as i32, (x - tx) * TN as f64, (y - ty) * TN as f64)
    }

    fn load(&self, la: i32, lo: i32) -> Result<Option<Arc<Tile>>> {
        if self.coarse {
            return self.load_terrarium(la, lo);
        }
        let Some(dir) = &self.dir else { return Ok(None) };
        let name = tile_name(la, lo);
        let path = dir.join(format!("{name}.dem3"));
        let none = dir.join(format!("{name}.none"));
        if none.exists() {
            return Ok(None);
        }
        if let Ok(b) = std::fs::read(&path)
            && b.len() == N * N * 2
        {
            let h = b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            return Ok(Some(Arc::new(Tile { h })));
        }
        std::fs::create_dir_all(dir)?;
        let url = format!("https://s3.amazonaws.com/elevation-tiles-prod/skadi/{}/{name}.hgt.gz", &name[..3]);
        let Some(gz) = http::get_bytes(&url)? else {
            std::fs::write(&none, b"")?;
            return Ok(None);
        };
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut raw)?;
        let side = ((raw.len() / 2) as f64).sqrt().round() as usize;
        let step = (side - 1) / (N - 1);
        let mut h = vec![0i16; N * N];
        for r in 0..N {
            for c in 0..N {
                let i = (r * step * side + c * step) * 2;
                h[r * N + c] = i16::from_be_bytes([raw[i], raw[i + 1]]);
            }
        }
        let bytes: Vec<u8> = h.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(&path, bytes)?;
        Ok(Some(Arc::new(Tile { h })))
    }

    fn tile(&self, la: i32, lo: i32) -> Option<Arc<Tile>> {
        if let Some(t) = self.tiles.read().unwrap().get(&(la, lo)) {
            return t.clone();
        }
        // `load` already retries the download; a tile that still fails is
        // treated as 0 m for this session but recorded, so the result can say
        // that its heights above ground are unreliable there
        let t = self.load(la, lo).unwrap_or_else(|e| {
            let name = if self.coarse { format!("terrarium {TZ}/{la}/{lo}") } else { tile_name(la, lo) };
            eprintln!("WARNING: DEM tile {name} failed: {e:#}; using 0 m (heights above ground there are wrong)");
            self.failed.lock().unwrap().push(name);
            None
        });
        self.tiles.write().unwrap().insert((la, lo), t.clone());
        t
    }

    /// Tiles that could not be loaded (treated as 0 m).
    pub fn failed_tiles(&self) -> Vec<String> {
        self.failed.lock().unwrap().clone()
    }

    /// Terrain height above the WGS84 ellipsoid (m) at a point: SRTM (bilinear) plus the geoid undulation.
    /// A flat DEM returns its fixed height unchanged.
    pub fn height(&self, lat: f64, lon: f64) -> f64 {
        if let Some(h) = self.flat {
            return h;
        }
        self.height_msl(lat, lon) + crate::geoid::undulation(lat, lon)
    }

    /// Terrain height above the geoid (≈ mean sea level), as SRTM gives it.
    pub fn height_msl(&self, lat: f64, lon: f64) -> f64 {
        if let Some(h) = self.flat {
            return h;
        }
        if self.coarse {
            let (tx, ty, px, py) = Self::mercator(lat, lon);
            let Some(t) = self.tile(tx, ty) else { return 0.0 };
            let at = |r: usize, c: usize| t.h[r.min(TN - 1) * TN + c.min(TN - 1)] as f64;
            let (x, y) = ((px - 0.5).max(0.0), (py - 0.5).max(0.0));
            let (c, r) = (x as usize, y as usize);
            let (fx, fy) = (x - c as f64, y - r as f64);
            let top = at(r, c) * (1.0 - fx) + at(r, c + 1) * fx;
            let bot = at(r + 1, c) * (1.0 - fx) + at(r + 1, c + 1) * fx;
            return (top * (1.0 - fy) + bot * fy).max(0.0);
        }
        let (la, lo) = (crate::geo::floor_i32(lat), crate::geo::floor_i32(lon));
        let Some(t) = self.tile(la, lo) else { return 0.0 };
        let y = (la as f64 + 1.0 - lat) * (N - 1) as f64;
        let x = (lon - lo as f64) * (N - 1) as f64;
        let (r, c) = ((y as usize).min(N - 2), (x as usize).min(N - 2));
        let (fy, fx) = (y - r as f64, x - c as f64);
        let top = t.at(r, c) * (1.0 - fx) + t.at(r, c + 1) * fx;
        let bot = t.at(r + 1, c) * (1.0 - fx) + t.at(r + 1, c + 1) * fx;
        top * (1.0 - fy) + bot * fy
    }

    /// Download every tile touching a box ahead of a parallel search.
    pub fn prefetch(&self, bbox: &crate::geo::BBox) {
        if self.flat.is_some() {
            return;
        }
        if self.coarse {
            let (x0, y0, _, _) = Self::mercator(bbox.lat_max, bbox.lon_min);
            let (x1, y1, _, _) = Self::mercator(bbox.lat_min, bbox.lon_max);
            for x in x0..=x1 {
                for y in y0..=y1 {
                    self.tile(x, y);
                }
            }
            return;
        }
        for la in bbox.lat_min.floor() as i32..=bbox.lat_max.floor() as i32 {
            for lo in bbox.lon_min.floor() as i32..=bbox.lon_max.floor() as i32 {
                self.tile(la, lo);
            }
        }
    }
}
