//! EGM96 geoid undulation `N` (geoid height above the WGS84 ellipsoid).
//!
//! ADS-B geometric altitude is a GNSS height above the ellipsoid, while SRTM
//! terrain heights are above the EGM96 geoid; the two differ by `N`, which is
//! +30 m over Poland, +43 m over the Netherlands and −30 m over Arizona. The
//! table is EGM96 sampled on a 1° grid (generated with the `egm96` crate's
//! spherical-harmonic model, stored in decimetres); bilinear interpolation
//! of it is within 0.5 m RMS (6 m worst case) of the full model.

static GRID: &[u8] = include_bytes!("../data/egm96_1deg.i16le");
const COLS: usize = 361;

fn at(r: usize, c: usize) -> f64 {
    let i = 2 * (r * COLS + c);
    i16::from_le_bytes([GRID[i], GRID[i + 1]]) as f64 / 10.0
}

/// Geoid height above the ellipsoid (m) at a point.
pub fn undulation(lat: f64, lon: f64) -> f64 {
    let lat = lat.clamp(-90.0, 90.0);
    let lon = (lon + 180.0).rem_euclid(360.0) - 180.0;
    let y = 90.0 - lat;
    let x = lon + 180.0;
    let (r, c) = ((y as usize).min(179), (x as usize).min(359));
    let (fy, fx) = (y - r as f64, x - c as f64);
    (at(r, c) * (1.0 - fx) + at(r, c + 1) * fx) * (1.0 - fy) + (at(r + 1, c) * (1.0 - fx) + at(r + 1, c + 1) * fx) * fy
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_values() {
        // EGM96 (full model): Warsaw 31.3, Netherlands 43.3, Phoenix -29.8, Frankfurt 48.1
        for (lat, lon, n) in [(52.23, 21.01, 31.3), (52.1, 5.2, 43.3), (33.7, -111.9, -29.8), (50.0, 8.7, 48.1)] {
            assert!((super::undulation(lat, lon) - n).abs() < 1.0, "{lat},{lon}");
        }
        assert_eq!(super::GRID.len(), 181 * 361 * 2);
    }
}
