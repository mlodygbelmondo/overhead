//! Map layer: OpenStreetMap basemap (Web Mercator, see [`crate::tiles`]) or
//! a plain graticule without it, the search area, location candidates with
//! their uncertainty ellipses, the matched tracks and the aircraft positions
//! at the shared timeline cursor (Rerun air-traffic style).

use std::f64::consts::PI;
use std::sync::Arc;

use gpui::{App, Bounds, ContentMask, Corners, Hsla, PathBuilder, Pixels, Point, RenderImage, Window, point, px, size};

use crate::draw::{self, Align};
use crate::heat::Heat;
use crate::location::{Ambiguity, LocationResults};
use crate::theme::Theme;
use crate::tiles::{ATTRIBUTION, TileKey};

/// Earth circumference at the equator (m), Web Mercator.
const EARTH_M: f64 = 40_075_016.686;
const TILE_PX: f64 = 256.0;
pub const MAX_ZOOM: f64 = 18.0;

/// Web Mercator, world in 0..1 (x east, y south).
pub fn merc(lat: f64, lon: f64) -> (f64, f64) {
    let lat = lat.clamp(-85.05, 85.05).to_radians();
    ((lon + 180.0) / 360.0, (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / PI) / 2.0)
}

pub fn unmerc(x: f64, y: f64) -> (f64, f64) {
    let n = PI * (1.0 - 2.0 * y);
    (n.sinh().atan().to_degrees(), x * 360.0 - 180.0)
}

/// What the map shows: centre in world units and a continuous zoom level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapView {
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
}

impl MapView {
    /// The view that fits a lat/lon box into `w`×`h` px with a margin.
    pub fn fit(lat0: f64, lon0: f64, lat1: f64, lon1: f64, w: f64, h: f64) -> Self {
        let (x0, y1) = merc(lat0, lon0);
        let (x1, y0) = merc(lat1, lon1);
        let dx = (x1 - x0).abs().max(1e-9);
        let dy = (y1 - y0).abs().max(1e-9);
        let zx = ((w - 24.0).max(40.0) / (dx * TILE_PX)).log2();
        let zy = ((h - 24.0).max(40.0) / (dy * TILE_PX)).log2();
        MapView { x: (x0 + x1) / 2.0, y: (y0 + y1) / 2.0, zoom: zx.min(zy).clamp(1.0, MAX_ZOOM) }
    }

    pub fn world_px(&self) -> f64 {
        TILE_PX * self.zoom.exp2()
    }

    /// Screen position of a lat/lon inside `b`.
    pub fn xy(&self, b: &Bounds<Pixels>, lat: f64, lon: f64) -> Point<Pixels> {
        let (x, y) = merc(lat, lon);
        let s = self.world_px();
        let cx = f32::from(b.left() + b.size.width / 2.0) as f64;
        let cy = f32::from(b.top() + b.size.height / 2.0) as f64;
        point(px((cx + (x - self.x) * s) as f32), px((cy + (y - self.y) * s) as f32))
    }

    /// Lat/lon under a screen position.
    pub fn latlon(&self, b: &Bounds<Pixels>, p: Point<Pixels>) -> (f64, f64) {
        let s = self.world_px();
        let cx = f32::from(b.left() + b.size.width / 2.0) as f64;
        let cy = f32::from(b.top() + b.size.height / 2.0) as f64;
        unmerc(self.x + (f32::from(p.x) as f64 - cx) / s, self.y + (f32::from(p.y) as f64 - cy) / s)
    }

    /// Pixels per metre on the ground at this view's centre.
    pub fn px_per_m(&self) -> f64 {
        let (lat, _) = unmerc(self.x, self.y);
        self.world_px() / (EARTH_M * lat.to_radians().cos())
    }

    /// Integer tile zoom used for drawing, and the tiles covering `w`×`h` px.
    pub fn tiles(&self, w: f64, h: f64) -> (u8, Vec<TileKey>) {
        let z = (self.zoom + 0.25).floor().clamp(0.0, MAX_ZOOM) as u8;
        let n = 1u32 << z;
        let s = self.world_px();
        let (x0, x1) = (self.x - w / 2.0 / s, self.x + w / 2.0 / s);
        let (y0, y1) = (self.y - h / 2.0 / s, self.y + h / 2.0 / s);
        let to = |v: f64| (v * n as f64).floor() as i64;
        let mut out = Vec::new();
        for ty in to(y0).max(0)..=to(y1).min(n as i64 - 1) {
            for tx in to(x0).max(0)..=to(x1).min(n as i64 - 1) {
                out.push((z, tx as u32, ty as u32));
            }
        }
        // the centre first: it loads first
        let d = |&(_, tx, ty): &TileKey| {
            let (dx, dy) = ((tx as f64 + 0.5) / n as f64 - self.x, (ty as f64 + 0.5) / n as f64 - self.y);
            dx * dx + dy * dy
        };
        out.sort_by(|a, b| d(a).total_cmp(&d(b)));
        (z, out)
    }
}

pub struct MapScene {
    pub results: Option<Arc<LocationResults>>,
    pub selected_candidate: usize,
    pub selected_detection: Option<usize>,
    pub cursor_t: Option<f64>,
    pub theme: Theme,
    pub dark: bool,
    pub empty_hint: String,
    pub view: MapView,
    /// Basemap tiles that are ready, with their keys (None = no basemap).
    pub tiles: Option<Vec<(TileKey, Arc<RenderImage>)>>,
    pub tiles_loading: bool,
    /// Distinct places, best first (candidate indices): place `n + 1` is `places[n]`.
    pub places: Vec<usize>,
    /// Draw the other possible places, each labelled with its chance.
    pub show_others: bool,
    /// Expert view: the probability of each cell of the possible area.
    pub details: bool,
    /// A score map under everything else.
    pub heat: Option<Heat>,
    /// A second candidate compared with the selected one (its own pin).
    pub compare: Option<usize>,
    /// A place the user checks by hand.
    pub probe: Option<(f64, f64)>,
    /// The region typed in the form (south, west, north, east), before a search.
    pub region: Option<(f64, f64, f64, f64)>,
}

/// The number people see for a candidate: its place (1 = best), if it is one.
pub fn place_number(places: &[usize], cand: usize) -> Option<usize> {
    places.iter().position(|&c| c == cand).map(|n| n + 1)
}

/// What the pin of the selected candidate says.
pub fn pin_label(r: &LocationResults, places: &[usize], cand: usize) -> String {
    let Some(c) = r.candidates.get(cand) else { return String::new() };
    let n = place_number(places, cand);
    let chance = c.probability.map(|p| format!(" · {}", fmt_chance(p))).unwrap_or_default();
    match (r.ambiguous().is_some(), n) {
        (false, Some(1)) => "Recorded here".into(),
        (true, Some(1)) => format!("Best guess{chance}"),
        (_, Some(n)) => format!("Place {n}{chance}"),
        // a candidate within 2 km of a better one: the same place
        (_, None) => match c.same_place_as.and_then(|r1| place_number(places, r1 - 1)) {
            Some(n) => format!("Near place {n}"),
            None => "Selected".into(),
        },
    }
}

pub fn fmt_chance(p: f64) -> String {
    if p >= 0.995 {
        ">99 %".into()
    } else if p >= 0.0095 {
        format!("{:.0} %", 100.0 * p)
    } else if p >= 0.001 {
        format!("{:.1} %", 100.0 * p)
    } else {
        "<0.1 %".into()
    }
}

pub fn paint(bounds: Bounds<Pixels>, s: &MapScene, window: &mut Window, cx: &mut App) {
    let t = s.theme;
    draw::rect(window, bounds, t.map_bg);
    window.with_content_mask(Some(ContentMask { bounds }), |window| paint_inner(bounds, s, window, cx));
}

/// A map pin: a round head above its point, with a light ring for contrast.
pub fn pin(window: &mut Window, p: Point<Pixels>, color: Hsla, ring: Hsla, scale: f32) {
    let r = 7.0 * scale;
    let head = point(p.x, p.y - px(15.0 * scale));
    let mut pb = PathBuilder::fill();
    pb.move_to(p);
    pb.line_to(point(head.x - px(r * 0.82), head.y + px(r * 0.6)));
    pb.line_to(point(head.x + px(r * 0.82), head.y + px(r * 0.6)));
    pb.close();
    if let Ok(path) = pb.build() {
        window.paint_path(path, color);
    }
    draw::circle(window, head, r + 1.5, Some(ring), None);
    draw::circle(window, head, r, Some(color), None);
    draw::circle(window, head, r * 0.36, Some(ring), None);
    draw::circle(window, p, 2.0 * scale, Some(color), Some((1.0, ring)));
}

/// A small aircraft glyph (a diamond) at `p`.
pub fn plane(window: &mut Window, p: Point<Pixels>, r: f32, color: Hsla, ring: Hsla) {
    let pts = [point(p.x, p.y - px(r)), point(p.x + px(r), p.y), point(p.x, p.y + px(r)), point(p.x - px(r), p.y)];
    draw::polygon(window, &pts, Some(color), Some((1.2, ring)));
}

fn paint_inner(bounds: Bounds<Pixels>, s: &MapScene, window: &mut Window, cx: &mut App) {
    let t = s.theme;
    let v = s.view;
    let center = point(bounds.left() + bounds.size.width / 2.0, bounds.top() + bounds.size.height / 2.0);
    let xy = |lat: f64, lon: f64| v.xy(&bounds, lat, lon);
    let ring = if s.dark { gpui::white() } else { gpui::white() };
    let halo = t.map_bg.opacity(0.9);

    // basemap
    match &s.tiles {
        Some(tiles) => {
            let wp = v.world_px();
            // coarser stand-ins first, the sharp tiles over them
            let mut tiles: Vec<&(TileKey, Arc<RenderImage>)> = tiles.iter().collect();
            tiles.sort_by_key(|t| t.0.0);
            for ((z, x, y), img) in tiles {
                let n = (1u32 << z) as f64;
                let (wx0, wy0) = (*x as f64 / n, *y as f64 / n);
                let cxp = f32::from(center.x) as f64;
                let cyp = f32::from(center.y) as f64;
                let a = point(px((cxp + (wx0 - v.x) * wp) as f32), px((cyp + (wy0 - v.y) * wp) as f32));
                let side = px((wp / n) as f32 + 0.5);
                // grey in both themes: the tiles' own colours (red motorway
                // junction numbers, green parks) would compete with the marks
                let _ = window.paint_image(Bounds::new(a, size(side, side)), Corners::default(), img.clone(), 0, true);
            }
            // calm the basemap down so the overlays read
            draw::rect(window, bounds, if s.dark { t.map_bg.opacity(0.62) } else { gpui::white().opacity(0.35) });
        }
        None => paint_graticule(bounds, s, window, cx),
    }

    // a score map, under the marks
    if let Some(h) = &s.heat {
        let (so, we, no, ea) = h.bounds;
        let a = xy(no, we);
        let b = xy(so, ea);
        if b.x > a.x && b.y > a.y {
            let _ = window.paint_image(Bounds::from_corners(a, b), Corners::default(), h.image.clone(), 0, false);
        }
    }

    let Some(r) = &s.results else {
        if let Some((a0, o0, a1, o1)) = s.region {
            let b = Bounds::from_corners(xy(a1, o0), xy(a0, o1));
            draw::round_rect(window, b, 2.0, t.accent.opacity(0.06), Some(t.accent.opacity(0.6)));
            draw::text(window, cx, "your region", point(b.left() + px(4.), b.top() + px(3.)), 10.0, t.accent, false, Align::Left);
        }
        let msg = s.empty_hint.as_str();
        let tw = draw::text_width(window, msg, 11.5, false);
        draw::chip(window, cx, msg, point(center.x - tw / 2.0 - px(5.), center.y - px(10.)), 11.5, t.text_muted, t.panel_bg, false);
        paint_furniture(bounds, s, window, cx);
        return;
    };

    if let Some((a0, o0, a1, o1)) = r.search_box {
        let p0 = xy(a1, o0);
        let p1 = xy(a0, o1);
        draw::frame(window, Bounds::from_corners(p0, p1), t.text_muted.opacity(0.55));
        draw::text(window, cx, "search area", point(p0.x + px(4.), p0.y + px(3.)), 9.5, t.text_muted, false, Align::Left);
    }

    // possible area of an ambiguous answer: cells shaded by their probability
    if let Some(a) = r.ambiguous() {
        paint_plausible(window, cx, &bounds, s, a);
    }

    let cand = r.candidates.get(s.selected_candidate);
    // labels that would overlap one already drawn are left out (zoomed out they
    // pile up); the pins' labels (drawn last, on top) keep their place free
    let mut taken: Vec<Bounds<Pixels>> = [Some(s.selected_candidate), s.compare]
        .into_iter()
        .flatten()
        .filter_map(|k| r.candidates.get(k))
        .map(|c| {
            let p = xy(c.lat, c.lon);
            Bounds::new(point(p.x - px(70.), p.y - px(46.)), size(px(140.), px(50.)))
        })
        .collect();
    // the aircraft of the selected place: tracks, where each was when it was heard
    if let Some(c) = cand {
        for tr in &c.tracks {
            let sel = Some(tr.detection) == s.selected_detection;
            let pts: Vec<Point<Pixels>> = tr.points.iter().map(|&(_, la, lo, _)| xy(la, lo)).collect();
            let color = if sel { t.accent } else { t.text.opacity(0.6) };
            draw::polyline(window, &pts, if sel { 2.4 } else { 1.4 }, color);
        }
        // the selected fly-by's label goes first
        let mut order: Vec<&_> = c.tracks.iter().collect();
        order.sort_by_key(|tr| Some(tr.detection) != s.selected_detection);
        for tr in order {
            let sel = Some(tr.detection) == s.selected_detection;
            let color = if sel { t.accent } else { t.text.opacity(0.85) };
            // emitted at the closest approach, heard `slant / c` later
            if let Some((la, lo, _)) = tr.position_at(tr.heard_rec - tr.slant_m / 340.0) {
                let p = xy(la, lo);
                draw::circle(window, p, if sel { 4.5 } else { 3.5 }, Some(color), Some((1.5, halo)));
                if bounds.contains(&p) {
                    let label = format!("{} · heard {}", tr.callsign, draw::fmt_time(tr.heard_rec, 1.0));
                    let at = point(p.x + px(7.), p.y - px(9.));
                    let rect = Bounds::new(at, size(draw::text_width(window, &label, 9.5, false) + px(10.), px(17.)));
                    if sel || !taken.iter().any(|b| b.intersects(&rect)) {
                        taken.push(rect);
                        draw::chip(window, cx, &label, at, 9.5, if sel { t.accent } else { t.text }, halo.opacity(0.8), false);
                    }
                }
            }
        }
        for tr in &c.tracks {
            let sel = Some(tr.detection) == s.selected_detection;
            if let Some(ct) = s.cursor_t
                && let Some((la, lo, _alt)) = tr.position_at(ct)
            {
                let p = xy(la, lo);
                plane(window, p, if sel { 6.0 } else { 5.0 }, if sel { t.accent } else { t.text }, halo);
            }
        }
    }

    // the other possible places, each with its chance; a label goes to the
    // free side, never under the pins' labels or another place's
    if s.show_others {
        for (n, &k) in s.places.iter().enumerate() {
            if k == s.selected_candidate || Some(k) == s.compare {
                continue;
            }
            let Some(c) = r.candidates.get(k) else { continue };
            let p = xy(c.lat, c.lon);
            draw::circle(window, p, 6.0, Some(halo.opacity(0.7)), Some((1.5, t.text_muted)));
            draw::circle(window, p, 2.0, Some(t.text_muted), None);
            if bounds.contains(&p) {
                let label = match c.probability {
                    Some(pr) => format!("Place {} · {}", n + 1, fmt_chance(pr)),
                    None => format!("Place {}", n + 1),
                };
                let w = draw::text_width(window, &label, 9.5, false) + px(10.);
                let spots = [point(p.x + px(8.), p.y - px(8.)), point(p.x + px(8.), p.y + px(6.)), point(p.x - w - px(8.), p.y - px(8.)), point(p.x - w - px(8.), p.y + px(6.))];
                let rect = |a: Point<Pixels>| Bounds::new(a, size(w, px(17.)));
                // no free side (zoomed far out): the dot alone, the legend names it
                if let Some(at) = spots.into_iter().find(|&a| !taken.iter().any(|b| b.intersects(&rect(a)))) {
                    taken.push(rect(at));
                    draw::chip(window, cx, &label, at, 9.5, t.text_muted, halo.opacity(0.8), false);
                }
            }
        }
    }

    // a place checked by hand
    if let Some((la, lo)) = s.probe {
        let p = xy(la, lo);
        draw::circle(window, p, 7.0, None, Some((2.0, t.ok)));
        draw::vline(window, p.x, p.y - px(11.), p.y + px(11.), t.ok);
        draw::hline(window, p.x - px(11.), p.x + px(11.), p.y, t.ok);
        draw::chip(window, cx, "Your test place", point(p.x + px(10.), p.y + px(4.)), 9.5, t.ok, halo.opacity(0.85), false);
    }

    // uncertainty and pins: the compared place, then the selected one on top
    let ppm = v.px_per_m();
    let mut pins: Vec<(usize, Hsla)> = vec![];
    if let Some(k) = s.compare.filter(|&k| k != s.selected_candidate) {
        pins.push((k, t.alt));
    }
    pins.push((s.selected_candidate, t.accent));
    for &(k, col) in &pins {
        let Some(c) = r.candidates.get(k) else { continue };
        let p = xy(c.lat, c.lon);
        let fill = col.opacity(0.16);
        let stroke = (1.5, col.opacity(0.9));
        match c.ellipse {
            Some((a, b, az)) if (a * ppm) as f32 > 3.0 => ellipse(window, p, a * ppm, b * ppm, az, fill, stroke),
            _ => draw::circle(window, p, ((c.radius_m * ppm) as f32).max(3.0), Some(fill), Some(stroke)),
        }
    }
    for &(k, col) in &pins {
        let Some(c) = r.candidates.get(k) else { continue };
        let p = xy(c.lat, c.lon);
        pin(window, p, col, ring, 1.0);
        let label = if col == t.alt { format!("Compared: {}", pin_label(r, &s.places, k)) } else { pin_label(r, &s.places, k) };
        let w = draw::text_width(window, &label, 10.5, false) + px(10.);
        draw::chip(window, cx, &label, point(p.x - w / 2.0, p.y - px(42.)), 10.5, t.accent_text, col, false);
    }
    paint_furniture(bounds, s, window, cx);
}

/// The possible area: grid cells shaded by their share of the best cell's
/// probability (adjacent cells read as one area); their probabilities in
/// the expert view.
fn paint_plausible(window: &mut Window, cx: &mut App, bounds: &Bounds<Pixels>, s: &MapScene, a: &Ambiguity) {
    let t = s.theme;
    let v = s.view;
    let p_max = a.cells.iter().map(|c| c.probability).fold(0.0, f64::max).max(1e-12);
    // weakest first, so the likeliest cells are drawn on top
    let mut cells: Vec<_> = a.cells.iter().collect();
    cells.sort_by(|x, y| x.probability.total_cmp(&y.probability));
    let mut top: Option<Point<Pixels>> = None;
    for c in cells {
        let (a0, o0, a1, o1) = c.bounds;
        let p0 = v.xy(bounds, a1, o0);
        let p1 = v.xy(bounds, a0, o1);
        let rel = (c.probability / p_max).clamp(0.0, 1.0).sqrt() as f32;
        // at least 5 px so that a province-wide view still shows every cell
        let (w, h) = (p1.x - p0.x, p1.y - p0.y);
        let (mx, my) = (p0.x + w / 2.0, p0.y + h / 2.0);
        let (w, h) = (w.max(px(5.)), h.max(px(5.)));
        let b = Bounds::new(point(mx - w / 2.0, my - h / 2.0), size(w, h));
        draw::rect(window, b, t.warn.opacity(0.14 + 0.30 * rel));
        top = Some(match top {
            Some(q) if q.y <= b.top() => q,
            _ => point(b.left(), b.top()),
        });
        if s.details && w > px(30.) && h > px(14.) && c.probability >= 0.01 {
            let label = format!("{:.0}%", 100.0 * c.probability);
            draw::text(window, cx, label, point(mx, my - px(6.)), 9.5, t.text, true, Align::Center);
        }
    }
    if let Some(q) = top {
        draw::chip(window, cx, "possible area", point(q.x, q.y - px(18.)), 9.5, t.warn, t.map_bg.opacity(0.85), false);
    }
}

/// Uncertainty ellipse: semi-axes `a` (along `az_deg` from north) and `b`, in px.
fn ellipse(window: &mut Window, c: Point<Pixels>, a: f64, b: f64, az_deg: f64, fill: gpui::Hsla, stroke: (f32, gpui::Hsla)) {
    let az = az_deg.to_radians();
    let (ux, uy) = (az.sin(), -az.cos()); // major axis direction on screen (y down)
    let (vx, vy) = (az.cos(), az.sin());
    let pts: Vec<Point<Pixels>> = (0..72)
        .map(|k| {
            let th = k as f64 / 72.0 * std::f64::consts::TAU;
            let (p, q) = (a * th.cos(), b * th.sin());
            point(c.x + px((p * ux + q * vx) as f32), c.y + px((p * uy + q * vy) as f32))
        })
        .collect();
    draw::polygon(window, &pts, Some(fill), Some(stroke));
}

fn paint_graticule(bounds: Bounds<Pixels>, s: &MapScene, window: &mut Window, cx: &mut App) {
    let t = s.theme;
    let v = s.view;
    let w = f32::from(bounds.size.width) as f64;
    let h = f32::from(bounds.size.height) as f64;
    let (la_top, lo_left) = v.latlon(&bounds, bounds.origin);
    let (la_bot, lo_right) = v.latlon(&bounds, point(bounds.right(), bounds.bottom()));
    let deg_per_px = (lo_right - lo_left) / w.max(1.0);
    let step = draw::nice_step(deg_per_px * 70.0, &[0.01, 0.02, 0.05, 0.1, 0.2, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 45.0]);
    let _ = h;
    let mut la = (la_bot / step).floor() * step;
    while la <= la_top {
        let y = v.xy(&bounds, la, lo_left).y;
        draw::hline(window, bounds.left(), bounds.right(), y, t.map_grid);
        if y > bounds.top() + px(12.) && y < bounds.bottom() - px(28.) {
            draw::text(window, cx, format!("{la:.2}°"), point(bounds.left() + px(4.), y + px(2.)), 9.5, t.text_faint, true, Align::Left);
        }
        la += step;
    }
    let mut lo = (lo_left / step).floor() * step;
    while lo <= lo_right {
        let x = v.xy(&bounds, la_top, lo).x;
        draw::vline(window, x, bounds.top(), bounds.bottom(), t.map_grid);
        if x > bounds.left() + px(44.) && x < bounds.right() - px(44.) {
            draw::text(window, cx, format!("{lo:.2}°"), point(x + px(3.), bounds.bottom() - px(14.)), 9.5, t.text_faint, true, Align::Left);
        }
        lo += step;
    }
}

/// Scale bar, attribution, loading note.
fn paint_furniture(bounds: Bounds<Pixels>, s: &MapScene, window: &mut Window, cx: &mut App) {
    let t = s.theme;
    let scale = s.view.px_per_m();
    let m = draw::nice_step(
        80.0 / scale,
        &[10., 20., 50., 100., 200., 500., 1000., 2000., 5000., 10_000., 20_000., 50_000., 100_000., 200_000., 500_000., 1e6, 2e6],
    );
    let bar = (m * scale) as f32;
    let y = bounds.bottom() - px(24.);
    let x0 = bounds.right() - px(bar + 12.);
    draw::round_rect(window, Bounds::new(point(x0 - px(6.), y - px(17.)), size(px(bar + 12.), px(24.))), 3.0, t.panel_bg.opacity(0.75), None);
    draw::rect_xywh(window, x0, y, px(bar), px(2.), t.text);
    draw::rect_xywh(window, x0, y - px(3.), px(1.), px(5.), t.text);
    draw::rect_xywh(window, x0 + px(bar - 1.), y - px(3.), px(1.), px(5.), t.text);
    let label = if m >= 1000.0 { format!("{:.0} km", m / 1000.0) } else { format!("{m:.0} m") };
    draw::text(window, cx, label, point(x0 + px(bar / 2.0), y - px(15.)), 10.0, t.text, true, Align::Center);
    if s.tiles.is_some() {
        let tw = draw::text_width(window, ATTRIBUTION, 9.0, false);
        let pos = point(bounds.left() + px(2.), bounds.bottom() - px(15.));
        draw::round_rect(window, Bounds::new(pos, size(tw + px(8.), px(14.))), 2.0, t.panel_bg.opacity(0.8), None);
        draw::text(window, cx, ATTRIBUTION, point(pos.x + px(4.), pos.y + px(1.)), 9.0, t.text_muted, false, Align::Left);
        if s.tiles_loading {
            draw::text(window, cx, "loading map…", point(bounds.right() - px(6.), bounds.top() + px(4.)), 9.5, t.text_faint, false, Align::Right);
        }
    }
}
