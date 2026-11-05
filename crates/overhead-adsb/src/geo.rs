//! WGS84 geodesy: geodetic <-> ECEF, local east/north/up frames, a global
//! ground grid and bounding boxes.

use serde::{Deserialize, Serialize};

pub type V3 = [f64; 3];

const A: f64 = 6_378_137.0;
const F: f64 = 1.0 / 298.257_223_563;
const E2: f64 = F * (2.0 - F);
/// Metres per degree of latitude (mean).
pub const M_PER_DEG_LAT: f64 = 111_132.0;

#[inline]
pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
#[inline]
pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
pub fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
#[inline]
pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
#[inline]
pub fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}

/// `x.floor() as i32`, bit for bit (NaN, saturation), without the libm call
/// that `floor` costs on baseline x86-64 (no SSE4.1).
#[inline]
pub fn floor_i32(x: f64) -> i32 {
    let t = x as i32;
    if (t as f64) > x { t.saturating_sub(1) } else { t }
}

/// Geodetic (degrees, metres above the ellipsoid) to ECEF (m).
pub fn ecef(lat: f64, lon: f64, h: f64) -> V3 {
    let (sl, cl) = lat.to_radians().sin_cos();
    let (so, co) = lon.to_radians().sin_cos();
    let n = A / (1.0 - E2 * sl * sl).sqrt();
    [(n + h) * cl * co, (n + h) * cl * so, (n * (1.0 - E2) + h) * sl]
}

/// ECEF to geodetic (degrees, m); Bowring's method, mm accuracy near the surface.
pub fn geodetic(p: V3) -> (f64, f64, f64) {
    let (x, y, z) = (p[0], p[1], p[2]);
    let lon = y.atan2(x);
    let r = (x * x + y * y).sqrt();
    let b = A * (1.0 - F);
    let ep2 = (A * A - b * b) / (b * b);
    let th = (z * A).atan2(r * b);
    let (st, ct) = th.sin_cos();
    let lat = (z + ep2 * b * st.powi(3)).atan2(r - E2 * A * ct.powi(3));
    let sl = lat.sin();
    let n = A / (1.0 - E2 * sl * sl).sqrt();
    let h = r / lat.cos() - n;
    (lat.to_degrees(), lon.to_degrees(), h)
}

/// Local east/north/up frame at a geodetic origin.
#[derive(Clone, Copy, Debug)]
pub struct Enu {
    pub origin: V3,
    e: V3,
    n: V3,
    u: V3,
}

impl Enu {
    pub fn new(lat: f64, lon: f64, h: f64) -> Self {
        let (sl, cl) = lat.to_radians().sin_cos();
        let (so, co) = lon.to_radians().sin_cos();
        Self {
            origin: ecef(lat, lon, h),
            e: [-so, co, 0.0],
            n: [-sl * co, -sl * so, cl],
            u: [cl * co, cl * so, sl],
        }
    }
    /// ECEF point to local coordinates.
    pub fn to_local(&self, p: V3) -> V3 {
        let d = sub(p, self.origin);
        [dot(d, self.e), dot(d, self.n), dot(d, self.u)]
    }
    /// ECEF vector (no translation) to local.
    pub fn vec_to_local(&self, v: V3) -> V3 {
        [dot(v, self.e), dot(v, self.n), dot(v, self.u)]
    }
    /// Local vector to ECEF (no translation).
    pub fn vec_to_ecef(&self, v: V3) -> V3 {
        add(add(scale(self.e, v[0]), scale(self.n, v[1])), scale(self.u, v[2]))
    }
    pub fn to_ecef(&self, l: V3) -> V3 {
        add(self.origin, self.vec_to_ecef(l))
    }
}

/// Unit vectors (east, north, up) at a geodetic position, as ECEF vectors.
pub fn enu_axes(lat: f64, lon: f64) -> (V3, V3, V3) {
    let (sl, cl) = lat.to_radians().sin_cos();
    let (so, co) = lon.to_radians().sin_cos();
    ([-so, co, 0.0], [-sl * co, -sl * so, cl], [cl * co, cl * so, sl])
}

/// Great-circle-ish horizontal distance (m) for short ranges.
pub fn ground_distance(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let dy = (lat2 - lat1) * M_PER_DEG_LAT;
    let dx = (lon2 - lon1) * M_PER_DEG_LAT * (0.5 * (lat1 + lat2)).to_radians().cos();
    (dx * dx + dy * dy).sqrt()
}

/// Latitude/longitude box (degrees).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub lat_min: f64,
    pub lat_max: f64,
    pub lon_min: f64,
    pub lon_max: f64,
}

impl BBox {
    pub const WORLD: BBox = BBox { lat_min: -90.0, lat_max: 90.0, lon_min: -180.0, lon_max: 180.0 };

    pub fn around(lat: f64, lon: f64, radius_m: f64) -> Self {
        let dlat = radius_m / M_PER_DEG_LAT;
        let dlon = radius_m / (M_PER_DEG_LAT * lat.to_radians().cos().max(0.01));
        Self { lat_min: lat - dlat, lat_max: lat + dlat, lon_min: lon - dlon, lon_max: lon + dlon }
    }
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        lat >= self.lat_min && lat <= self.lat_max && lon >= self.lon_min && lon <= self.lon_max
    }
    /// Grow by `m` metres on every side.
    pub fn expand(&self, m: f64) -> Self {
        let dlat = m / M_PER_DEG_LAT;
        let lat_ref = self.lat_min.abs().max(self.lat_max.abs()).min(85.0);
        let dlon = m / (M_PER_DEG_LAT * lat_ref.to_radians().cos());
        Self {
            lat_min: (self.lat_min - dlat).max(-90.0),
            lat_max: (self.lat_max + dlat).min(90.0),
            lon_min: (self.lon_min - dlon).max(-180.0),
            lon_max: (self.lon_max + dlon).min(180.0),
        }
    }
    pub fn intersects(&self, o: &BBox) -> bool {
        self.lat_min <= o.lat_max && o.lat_min <= self.lat_max && self.lon_min <= o.lon_max && o.lon_min <= self.lon_max
    }
    pub fn intersection(&self, o: &BBox) -> Option<BBox> {
        let b = BBox {
            lat_min: self.lat_min.max(o.lat_min),
            lat_max: self.lat_max.min(o.lat_max),
            lon_min: self.lon_min.max(o.lon_min),
            lon_max: self.lon_max.min(o.lon_max),
        };
        (b.lat_min <= b.lat_max && b.lon_min <= b.lon_max).then_some(b)
    }
    pub fn centre(&self) -> (f64, f64) {
        (0.5 * (self.lat_min + self.lat_max), 0.5 * (self.lon_min + self.lon_max))
    }
    pub fn area_km2(&self) -> f64 {
        let (lat, _) = self.centre();
        (self.lat_max - self.lat_min) * 111.132 * (self.lon_max - self.lon_min) * 111.132 * lat.to_radians().cos()
    }
    pub fn extend(&mut self, lat: f64, lon: f64) {
        self.lat_min = self.lat_min.min(lat);
        self.lat_max = self.lat_max.max(lat);
        self.lon_min = self.lon_min.min(lon);
        self.lon_max = self.lon_max.max(lon);
    }
    pub fn empty() -> Self {
        Self { lat_min: f64::INFINITY, lat_max: f64::NEG_INFINITY, lon_min: f64::INFINITY, lon_max: f64::NEG_INFINITY }
    }
    pub fn is_empty(&self) -> bool {
        !(self.lat_min <= self.lat_max)
    }
}

/// Global ground grid with roughly square cells of `spacing` metres: rows of
/// constant latitude step, each row with its own longitude step.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    pub spacing: f64,
    pub dlat: f64,
}

/// Cell index (row, column).
pub type Cell = (i32, i32);

impl Grid {
    pub fn new(spacing: f64) -> Self {
        Self { spacing, dlat: spacing / M_PER_DEG_LAT }
    }
    pub fn dlon(&self, row: i32) -> f64 {
        let lat = (row as f64 + 0.5) * self.dlat;
        self.dlat / lat.to_radians().cos().max(0.02)
    }
    pub fn cell(&self, lat: f64, lon: f64) -> Cell {
        let row = floor_i32(lat / self.dlat);
        (row, floor_i32(lon / self.dlon(row)))
    }
    pub fn centre(&self, c: Cell) -> (f64, f64) {
        ((c.0 as f64 + 0.5) * self.dlat, (c.1 as f64 + 0.5) * self.dlon(c.0))
    }
}

/// [`Grid::cell`] and [`Grid::centre`] with each row's longitude step
/// computed once (the same values, without a cosine per call).
pub struct GridRows {
    grid: Grid,
    row0: i32,
    dlon: Vec<f64>,
}

impl GridRows {
    /// For rows from `lat_min` to `lat_max` (other rows are computed directly).
    pub fn new(grid: Grid, lat_min: f64, lat_max: f64) -> Self {
        let row0 = floor_i32(lat_min / grid.dlat) - 1;
        let n = (floor_i32(lat_max / grid.dlat) + 2 - row0).clamp(0, 1 << 20) as usize;
        Self { grid, row0, dlon: vec![f64::NAN; n] }
    }
    #[inline]
    pub fn dlon(&mut self, row: i32) -> f64 {
        let i = row.wrapping_sub(self.row0) as usize;
        match self.dlon.get_mut(i) {
            Some(d) => {
                if d.is_nan() {
                    *d = self.grid.dlon(row);
                }
                *d
            }
            None => self.grid.dlon(row),
        }
    }
    /// Row height (degrees of latitude).
    pub fn dlat(&self) -> f64 {
        self.grid.dlat
    }
    #[inline]
    pub fn cell(&mut self, lat: f64, lon: f64) -> Cell {
        let row = floor_i32(lat / self.grid.dlat);
        (row, floor_i32(lon / self.dlon(row)))
    }
    #[inline]
    pub fn centre(&mut self, c: Cell) -> (f64, f64) {
        ((c.0 as f64 + 0.5) * self.grid.dlat, (c.1 as f64 + 0.5) * self.dlon(c.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecef_roundtrip() {
        for (lat, lon, h) in [(52.2, 21.0, 120.0), (-33.9, 151.2, 5.0), (0.0, 0.0, 3000.0), (64.1, -21.9, 40.0)] {
            let (a, b, c) = geodetic(ecef(lat, lon, h));
            assert!((a - lat).abs() < 1e-8 && (b - lon).abs() < 1e-8 && (c - h).abs() < 1e-3, "{a} {b} {c}");
        }
    }

    #[test]
    fn enu_matches_distances() {
        let enu = Enu::new(52.0, 21.0, 100.0);
        let p = ecef(52.0 + 1000.0 / M_PER_DEG_LAT, 21.0, 1100.0);
        let l = enu.to_local(p);
        assert!(l[0].abs() < 5.0 && (l[1] - 1000.0).abs() < 10.0 && (l[2] - 1000.0).abs() < 1.0, "{l:?}");
    }

    #[test]
    fn floor_i32_matches_floor() {
        let xs = [
            0.0, -0.0, 0.5, -0.5, 1.0, -1.0, 2.999, -2.999, 1e-300, -1e-300, 3e9, -3e9, 2147483647.5, -2147483648.5,
            -2147483647.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 52.123 / 0.0018, -112.3 / 0.0029,
        ];
        for x in xs {
            assert_eq!(floor_i32(x), x.floor() as i32, "{x}");
        }
        let mut s = 0x9e3779b97f4a7c15u64;
        for _ in 0..100_000 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let x = (s as f64 / u64::MAX as f64 - 0.5) * 2e5;
            assert_eq!(floor_i32(x), x.floor() as i32, "{x}");
            assert_eq!(x.abs() as usize, x.abs().floor() as usize);
        }
    }

    #[test]
    fn grid_cell_contains_point() {
        let g = Grid::new(200.0);
        let c = g.cell(52.123, 21.456);
        let (lat, lon) = g.centre(c);
        assert!(ground_distance(lat, lon, 52.123, 21.456) < 200.0);
    }
}
