//! Score maps as images on the map (the reference video's heat maps): one
//! fly-by alone or all of them together, over the whole search (the
//! matcher's coarse cells) or around a place (the fine curve-score grid).

use std::sync::Arc;

use overhead_adsb::explain::{CoarseMap, FineMap};
use gpui::RenderImage;
use image::{Frame, ImageBuffer, Rgba};

/// Which score a heat layer shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layer {
    /// All matched fly-bys with one start (what the search ranks by).
    Combined,
    /// One fly-by alone: the session's detection index.
    Detection(usize),
}

/// Where a heat layer is computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The whole search area, on the matcher's coarse cells.
    Search,
    /// Around a candidate (0-based), on a 100 m grid, with its flights.
    Around(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HeatKey {
    pub scope: Scope,
    pub layer: Layer,
}

/// A rendered heat layer and the box it covers.
#[derive(Clone)]
pub struct Heat {
    pub key: HeatKey,
    pub image: Arc<RenderImage>,
    /// South, west, north, east edges (degrees).
    pub bounds: (f64, f64, f64, f64),
    /// Highest value (the colour scale runs from 0 to it).
    pub max: f64,
    /// Where the highest value is.
    pub best: (f64, f64),
}

/// Colour of a value in 0..1: transparent at the bottom, the inferno map above.
fn pixel(lut: &[[u8; 3]; 256], x: f32) -> Rgba<u8> {
    if !x.is_finite() || x <= 0.03 {
        return Rgba([0, 0, 0, 0]);
    }
    let x = x.clamp(0.0, 1.0);
    let [r, g, b] = lut[(x * 255.0) as usize];
    let a = (60.0 + 170.0 * x.powf(0.7)) as u8;
    // GPUI sprites are BGRA
    Rgba([b, g, r, a])
}

fn image(w: u32, h: u32, values: impl Fn(u32, u32) -> f32) -> Arc<RenderImage> {
    let lut = overhead_core::colormap::lut();
    let mut buf: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(w.max(1), h.max(1));
    for y in 0..h {
        for x in 0..w {
            buf.put_pixel(x, y, pixel(&lut, values(x, y)));
        }
    }
    Arc::new(RenderImage::new(vec![Frame::new(buf)]))
}

/// The fine map of a candidate: one layer (`Combined` or a detection).
pub fn fine(m: &FineMap, key: HeatKey) -> Option<Heat> {
    let layer = match key.layer {
        Layer::Combined => &m.combined,
        Layer::Detection(d) => m.per_detection.iter().find(|l| l.detection == Some(d))?,
    };
    let max = layer.max.max(1e-6);
    let (rows, cols) = (m.rows as u32, m.cols as u32);
    // rows run from the south: the image's top row is the northernmost
    let img = image(cols, rows, |x, y| {
        let r = (rows - 1 - y) as usize;
        let v = layer.values.get(r * m.cols + x as usize).copied().unwrap_or(f32::NAN);
        (v as f64 / max) as f32
    });
    let (s, w) = (m.lat0 - 0.5 * m.dlat, m.lon0 - 0.5 * m.dlon);
    let (n, e) = (s + m.rows as f64 * m.dlat, w + m.cols as f64 * m.dlon);
    Some(Heat { key, image: img, bounds: (s, w, n, e), max: layer.max, best: (layer.best_lat, layer.best_lon) })
}

/// The coarse map of the whole search: the combined score of each cell, or
/// one detection's best score there. Cells lie on a grid of `cell_m`.
pub fn coarse(m: &CoarseMap, key: HeatKey) -> Option<Heat> {
    let val = |c: &overhead_adsb::explain::CoarseMapCell| -> Option<f64> {
        match key.layer {
            Layer::Combined => Some(c.combined.max(0.0)),
            Layer::Detection(d) => c.per_detection.get(d).and_then(|x| x.as_ref()).map(|p| p.0),
        }
    };
    let cells: Vec<(f64, f64, f64)> = m.cells.iter().filter_map(|c| val(c).map(|v| (c.lat, c.lon, v))).collect();
    if cells.is_empty() {
        return None;
    }
    let (mut s, mut w, mut n, mut e) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for &(la, lo, _) in &cells {
        (s, w, n, e) = (s.min(la), w.min(lo), n.max(la), e.max(lo));
    }
    let lat_mid = 0.5 * (s + n);
    let dlat = m.cell_m / 111_195.0;
    let dlon = m.cell_m / (111_195.0 * lat_mid.to_radians().cos().max(0.05));
    let rows = (((n - s) / dlat).round() as i64 + 1).clamp(1, 2000) as u32;
    let cols = (((e - w) / dlon).round() as i64 + 1).clamp(1, 2000) as u32;
    let mut grid = vec![f32::NAN; (rows * cols) as usize];
    let (mut max, mut best) = (0.0f64, (s, w));
    for &(la, lo, v) in &cells {
        let r = (((la - s) / dlat).round() as i64).clamp(0, rows as i64 - 1) as u32;
        let c = (((lo - w) / dlon).round() as i64).clamp(0, cols as i64 - 1) as u32;
        let i = ((rows - 1 - r) * cols + c) as usize;
        if !(grid[i] >= v as f32) {
            grid[i] = v as f32;
        }
        if v > max {
            (max, best) = (v, (la, lo));
        }
    }
    let mx = max.max(1e-6);
    let img = image(cols, rows, |x, y| grid[(y * cols + x) as usize] / mx as f32);
    let bounds = (s - 0.5 * dlat, w - 0.5 * dlon, s + (rows as f64 - 0.5) * dlat, w + (cols as f64 - 0.5) * dlon);
    Some(Heat { key, image: img, bounds, max, best })
}

#[cfg(test)]
mod tests {
    use super::*;
    use overhead_adsb::explain::{CoarseMapCell, FineLayer};

    #[test]
    fn coarse_grid_covers_every_cell() {
        let cell = |lat: f64, lon: f64, v: f64| CoarseMapCell {
            lat,
            lon,
            combined: v,
            matched: 1,
            flights: vec![],
            per_detection: vec![Some((v / 2.0, "X".into())), None],
        };
        let m = CoarseMap { cell_m: 1000.0, cells: vec![cell(52.0, 13.0, 1.0), cell(52.018, 13.0, 3.0), cell(52.0, 13.03, 2.0)] };
        let h = coarse(&m, HeatKey { scope: Scope::Search, layer: Layer::Combined }).unwrap();
        assert_eq!(h.max, 3.0);
        assert_eq!(h.best, (52.018, 13.0));
        let (s, w, n, e) = h.bounds;
        assert!(s < 52.0 && n > 52.018 && w < 13.0 && e > 13.03, "{:?}", h.bounds);
        // a detection with no score anywhere has no layer
        assert!(coarse(&m, HeatKey { scope: Scope::Search, layer: Layer::Detection(1) }).is_none());
        assert_eq!(coarse(&m, HeatKey { scope: Scope::Search, layer: Layer::Detection(0) }).unwrap().max, 1.5);
    }

    #[test]
    fn fine_layers_by_detection() {
        let layer = |d: Option<usize>, v: f32| FineLayer { detection: d, flights: vec![], values: vec![v; 9], max: v as f64, best_lat: 52.0, best_lon: 13.0 };
        let m = FineMap {
            candidate: 1,
            lat0: 52.0,
            lon0: 13.0,
            dlat: 0.001,
            dlon: 0.0015,
            rows: 3,
            cols: 3,
            step_m: 100.0,
            per_detection: vec![layer(Some(2), 0.9)],
            combined: layer(None, 2.5),
            seconds: 0.0,
        };
        assert!((fine(&m, HeatKey { scope: Scope::Around(0), layer: Layer::Detection(2) }).unwrap().max - 0.9).abs() < 1e-6);
        assert!(fine(&m, HeatKey { scope: Scope::Around(0), layer: Layer::Detection(0) }).is_none());
        let h = fine(&m, HeatKey { scope: Scope::Around(0), layer: Layer::Combined }).unwrap();
        assert!((h.bounds.0 - 51.9995).abs() < 1e-9 && (h.bounds.2 - 52.0025).abs() < 1e-9);
    }
}
