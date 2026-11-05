//! Links to look at a place in a web map: Google Maps, Google Street View
//! and OpenStreetMap (the reference video ends by checking the found place
//! in Street View). Plain URL builders, no network.

use serde::{Deserialize, Serialize};

/// Links for one place.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MapLinks {
    pub google_maps: String,
    pub street_view: String,
    pub openstreetmap: String,
}

fn ll(lat: f64, lon: f64) -> String {
    format!("{lat:.6},{lon:.6}")
}

/// Google Maps with a pin at the place (Maps URLs API: `search`).
pub fn google_maps(lat: f64, lon: f64) -> String {
    format!("https://www.google.com/maps/search/?api=1&query={}", ll(lat, lon))
}

/// Google Street View at the place (Maps URLs API: `map_action=pano`);
/// Google opens the nearest panorama, if there is one within its search
/// radius. `heading` (degrees from north) points the view.
pub fn street_view(lat: f64, lon: f64, heading: Option<f64>) -> String {
    let mut u = format!("https://www.google.com/maps/@?api=1&map_action=pano&viewpoint={}", ll(lat, lon));
    if let Some(h) = heading {
        u.push_str(&format!("&heading={:.0}", h.rem_euclid(360.0)));
    }
    u
}

/// OpenStreetMap with a marker at the place, at `zoom` (1–19).
pub fn openstreetmap(lat: f64, lon: f64, zoom: u8) -> String {
    let z = zoom.clamp(1, 19);
    format!("https://www.openstreetmap.org/?mlat={lat:.6}&mlon={lon:.6}#map={z}/{lat:.6}/{lon:.6}")
}

/// All three for a place (OpenStreetMap at street level, zoom 16).
pub fn for_place(lat: f64, lon: f64) -> MapLinks {
    MapLinks { google_maps: google_maps(lat, lon), street_view: street_view(lat, lon, None), openstreetmap: openstreetmap(lat, lon, 16) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let l = for_place(51.43263, 4.15276);
        assert_eq!(l.google_maps, "https://www.google.com/maps/search/?api=1&query=51.432630,4.152760");
        assert_eq!(l.street_view, "https://www.google.com/maps/@?api=1&map_action=pano&viewpoint=51.432630,4.152760");
        assert_eq!(l.openstreetmap, "https://www.openstreetmap.org/?mlat=51.432630&mlon=4.152760#map=16/51.432630/4.152760");
        // southern and western hemispheres keep their signs; headings wrap
        assert_eq!(street_view(-33.5, -70.25, Some(-90.0)), "https://www.google.com/maps/@?api=1&map_action=pano&viewpoint=-33.500000,-70.250000&heading=270");
        assert!(openstreetmap(1.0, 2.0, 30).contains("#map=19/"));
    }
}
