//! Spectrogram hero (iZotope RX style): rulers on the edges, fitted curves,
//! selection highlight, hover crosshair and a shared time cursor; plus the
//! whole-file overview strip.

use overhead_core::detect::{Detection, Origin};
use overhead_core::model::{DEFAULT_C, Flyby};
use gpui::{App, Bounds, ContentMask, Corners, Hsla, Pixels, Point, Window, point, px, size};

use crate::analysis::SpecImage;
use crate::draw::{self, Align};
use crate::theme::Theme;

pub const LEFT_RULER: f32 = 50.0;
pub const BOTTOM_RULER: f32 = 22.0;
pub const BOTTOM_RULER_CLOCK: f32 = 34.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub t0: f64,
    pub t1: f64,
    pub f0: f64,
    pub f1: f64,
}

impl View {
    pub fn x_of(&self, plot: &Bounds<Pixels>, t: f64) -> Pixels {
        plot.origin.x + plot.size.width * (((t - self.t0) / (self.t1 - self.t0)) as f32)
    }
    pub fn y_of(&self, plot: &Bounds<Pixels>, f: f64) -> Pixels {
        plot.origin.y + plot.size.height * (1.0 - ((f - self.f0) / (self.f1 - self.f0)) as f32)
    }
    pub fn t_at(&self, plot: &Bounds<Pixels>, x: Pixels) -> f64 {
        self.t0 + ((x - plot.origin.x) / plot.size.width) as f64 * (self.t1 - self.t0)
    }
    pub fn f_at(&self, plot: &Bounds<Pixels>, y: Pixels) -> f64 {
        self.f0 + (1.0 - ((y - plot.origin.y) / plot.size.height) as f64) * (self.f1 - self.f0)
    }
}

/// What a detection contributes to the canvas.
#[derive(Clone, Debug)]
pub struct Curve {
    pub flyby: Flyby,
    /// Multipliers of `flyby.f0` for every drawn line.
    pub ratios: Vec<f64>,
    pub t_span: (f64, f64),
    pub tc_range: (f64, f64),
    pub ground: bool,
    pub confident: bool,
    pub origin: Origin,
    /// Points traced by hand, drawn as dots.
    pub trace: Vec<[f64; 2]>,
}

impl Curve {
    pub fn from_detection(d: &Detection) -> Self {
        let mut ratios = d.harmonics.clone();
        for &f in &d.other_lines {
            let r = f / d.flyby.f0;
            if !ratios.iter().any(|&q| (q - r).abs() < 0.02 * r) {
                ratios.push(r);
            }
        }
        let half = (d.region.t_end - d.region.t_start).max(20.0) * 0.6;
        Curve {
            flyby: d.flyby,
            ratios,
            t_span: (d.flyby.tc - half, d.flyby.tc + half),
            tc_range: d.ranges.tc,
            ground: d.likely_ground_vehicle,
            confident: d.confident,
            origin: d.origin,
            trace: d.trace.clone().unwrap_or_default(),
        }
    }

    pub fn freq(&self, ratio: f64, t: f64) -> f64 {
        ratio * self.flyby.freq_at(t, DEFAULT_C)
    }
}

pub struct SpecScene {
    pub view: View,
    pub tile: Option<SpecImage>,
    pub fallback: Option<SpecImage>,
    pub curves: Vec<Curve>,
    pub selected: Option<usize>,
    pub hover: Option<(f64, f64)>,
    pub cursor_t: Option<f64>,
    pub show_curves: bool,
    /// Local clock (seconds of day) of the first sample, when known.
    pub clock_start: Option<f64>,
    pub theme: Theme,
    pub loading: bool,
    /// Edit mode: handles on the selected curve, the points being traced.
    pub edit: Option<EditOverlay>,
    /// Modelled curves of candidates (what their aircraft would sound like there).
    pub models: Vec<ModelOverlay>,
    /// Cuts in the recording, and the ones the audio suggests (s).
    pub cuts: Vec<f64>,
    pub suggested_cuts: Vec<f64>,
}

/// The modelled Doppler curve of one aircraft at a candidate place, drawn
/// over the spectrogram to compare it with the line that was recorded.
#[derive(Clone, Debug)]
pub struct ModelOverlay {
    pub color: Hsla,
    /// Polylines `[t, f]` (one per harmonic and known part of the track).
    pub pieces: Vec<Vec<[f64; 2]>>,
    /// Label at a point of the curve: (t, f, text).
    pub label: Option<(f64, f64, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct EditOverlay {
    /// Draw the move/steepness handles on the selected curve.
    pub handles: bool,
    /// Points traced so far (not yet fitted).
    pub pending: Vec<[f64; 2]>,
    /// A refit of the selected curve is running.
    pub refitting: bool,
}

/// Edit handles of a curve on its lowest line: the centre (drag to move in
/// time and frequency), the steepness handle at the closest approach (drag
/// sideways: steeper = closer) and the spread handle on the approach side
/// (drag up/down: more spread = faster).
pub fn handles(v: &View, plot: &Bounds<Pixels>, c: &Curve) -> [Point<Pixels>; 3] {
    let r0 = c.ratios.iter().copied().fold(f64::INFINITY, f64::min);
    let r0 = if r0.is_finite() { r0 } else { 1.0 };
    let tc = c.flyby.tc;
    let at = |t: f64| point(v.x_of(plot, t), v.y_of(plot, c.freq(r0, t)));
    // the spread handle where the curve is near its approach asymptote, kept in view
    let lead = (2.5 * c.flyby.d / c.flyby.v.max(1.0)).clamp(6.0, 120.0);
    let span = v.t1 - v.t0;
    let lead = lead.min(0.4 * span).max(0.08 * span);
    let centre = at(tc);
    let steep = at(tc + 0.35 * lead);
    let spread = at(tc - lead);
    [centre, steep, spread]
}

pub fn plot_rect(bounds: Bounds<Pixels>, clock: bool) -> Bounds<Pixels> {
    let bottom = if clock { BOTTOM_RULER_CLOCK } else { BOTTOM_RULER };
    Bounds::new(
        point(bounds.origin.x + px(LEFT_RULER), bounds.origin.y),
        size(bounds.size.width - px(LEFT_RULER), bounds.size.height - px(bottom)),
    )
}

fn paint_spec_image(window: &mut Window, img: &SpecImage, v: &View, plot: &Bounds<Pixels>) {
    let a = point(v.x_of(plot, img.t0), v.y_of(plot, img.f1));
    let b = point(v.x_of(plot, img.t1), v.y_of(plot, img.f0));
    if b.x <= a.x || b.y <= a.y {
        return;
    }
    let _ = window.paint_image(Bounds::from_corners(a, b), Corners::default(), img.image.clone(), 0, false);
}

pub fn paint(bounds: Bounds<Pixels>, s: &SpecScene, window: &mut Window, cx: &mut App) -> Bounds<Pixels> {
    let t = s.theme;
    let plot = plot_rect(bounds, s.clock_start.is_some());
    let v = s.view;
    draw::rect(window, bounds, t.surface);
    draw::rect(window, plot, t.canvas_bg);

    window.with_content_mask(Some(ContentMask { bounds: plot }), |window| {
        if let Some(fb) = &s.fallback {
            paint_spec_image(window, fb, &v, &plot);
        }
        if let Some(tile) = &s.tile {
            paint_spec_image(window, tile, &v, &plot);
        }

        // frequency grid (faint) so the eye can read off harmonics
        let f_step = f_step(&v, plot.size.height);
        let mut f = (v.f0 / f_step).ceil() * f_step;
        while f <= v.f1 {
            draw::hline(window, plot.left(), plot.right(), v.y_of(&plot, f), t.canvas_grid);
            f += f_step;
        }

        // modelled curves under the recorded ones (whose chips stay readable), their labels on top
        paint_models(window, cx, s, &plot, false);
        if s.show_curves {
            paint_curves(window, cx, s, &plot);
        }
        paint_models(window, cx, s, &plot, true);
        paint_cuts(window, cx, s, &plot);
        if let Some(e) = &s.edit {
            paint_edit(window, cx, s, e, &plot);
        }

        if let Some(ct) = s.cursor_t {
            let x = v.x_of(&plot, ct);
            draw::vline(window, x, plot.top(), plot.bottom(), gpui::white().opacity(0.85));
            draw::triangle_down(window, point(x + px(0.5), plot.top() + px(7.)), 9.0, 7.0, t.canvas_accent());
        }

        if let Some((ht, hf)) = s.hover {
            let x = v.x_of(&plot, ht);
            let y = v.y_of(&plot, hf);
            let c = gpui::white().opacity(0.22);
            draw::vline(window, x, plot.top(), plot.bottom(), c);
            draw::hline(window, plot.left(), plot.right(), y, c);
            let label = format!("{}  ·  {:.1} Hz", draw::fmt_time(ht, 0.1), hf);
            // in the bottom-left corner, never over a curve's label next to the mouse
            let pos = point(plot.left() + px(6.), plot.bottom() - px(24.));
            draw::chip(window, cx, &label, pos, 11.0, gpui::white().opacity(0.92), gpui::black().opacity(0.72), true);
        }

        if s.loading {
            draw::chip(
                window,
                cx,
                "rendering…",
                point(plot.right() - px(86.), plot.top() + px(8.)),
                10.5,
                gpui::white().opacity(0.7),
                gpui::black().opacity(0.6),
                false,
            );
        }
    });

    paint_rulers(window, cx, s, bounds, &plot);
    plot
}

fn f_step(v: &View, height: Pixels) -> f64 {
    let px_per_hz = f32::from(height) as f64 / (v.f1 - v.f0);
    draw::nice_step(26.0 / px_per_hz, &[1., 2., 5., 10., 20., 25., 50., 100., 200., 250., 500., 1000.])
}

fn t_step(v: &View, width: Pixels) -> f64 {
    let px_per_s = f32::from(width) as f64 / (v.t1 - v.t0);
    draw::nice_step(
        78.0 / px_per_s,
        &[0.1, 0.2, 0.5, 1., 2., 5., 10., 15., 30., 60., 120., 300., 600., 900., 1800., 3600.],
    )
}

fn paint_rulers(window: &mut Window, cx: &mut App, s: &SpecScene, bounds: Bounds<Pixels>, plot: &Bounds<Pixels>) {
    let t = s.theme;
    let v = s.view;
    // dividers between rulers and plot
    draw::vline(window, plot.left() - px(1.), plot.top(), plot.bottom() + px(1.), t.border);
    draw::hline(window, plot.left() - px(1.), plot.right(), plot.bottom(), t.border);

    let f_step = f_step(&v, plot.size.height);
    let mut f = (v.f0 / f_step).ceil() * f_step;
    while f <= v.f1 {
        let y = v.y_of(plot, f);
        draw::hline(window, plot.left() - px(5.), plot.left() - px(1.), y, t.border_strong);
        if y > plot.top() + px(6.) && y < plot.bottom() - px(4.) {
            let label = if f_step < 1.0 { format!("{f:.1}") } else { format!("{f:.0}") };
            draw::text(window, cx, label, point(plot.left() - px(8.), y - px(7.)), 10.5, t.text_muted, true, Align::Right);
        }
        f += f_step;
    }
    draw::text(window, cx, "Hz", point(bounds.left() + px(6.), plot.bottom() + px(4.)), 10.0, t.text_faint, true, Align::Left);

    let ts = t_step(&v, plot.size.width);
    let mut tt = (v.t0 / ts).ceil() * ts;
    while tt <= v.t1 {
        let x = v.x_of(plot, tt);
        draw::vline(window, x, plot.bottom(), plot.bottom() + px(5.), t.border_strong);
        if x < plot.right() - px(20.) {
            draw::text(window, cx, draw::fmt_time(tt, ts), point(x + px(3.), plot.bottom() + px(4.)), 10.5, t.text_muted, true, Align::Left);
            if let Some(c0) = s.clock_start {
                draw::text(window, cx, draw::fmt_clock(c0 + tt), point(x + px(3.), plot.bottom() + px(17.)), 10.0, t.text_faint, true, Align::Left);
            }
        }
        tt += ts;
    }
    // minor ticks
    let minor = ts / 5.0;
    let mut tt = (v.t0 / minor).ceil() * minor;
    while tt <= v.t1 {
        let x = v.x_of(plot, tt);
        draw::vline(window, x, plot.bottom(), plot.bottom() + px(2.), t.border_strong);
        tt += minor;
    }
    if let Some(ct) = s.cursor_t {
        let x = v.x_of(plot, ct);
        if x >= plot.left() && x <= plot.right() {
            let label = draw::fmt_time(ct, 0.1);
            let w = draw::text_width(window, &label, 10.5, true) + px(10.);
            draw::chip(window, cx, &label, point(x - w / 2.0, plot.bottom() + px(2.)), 10.5, t.accent_text, t.accent, true);
        }
    }
}

fn paint_curves(window: &mut Window, cx: &mut App, s: &SpecScene, plot: &Bounds<Pixels>) {
    let v = s.view;
    let t = s.theme;
    let order: Vec<usize> = (0..s.curves.len()).filter(|&i| Some(i) != s.selected).chain(s.selected).collect();
    for i in order {
        let c = &s.curves[i];
        let sel = Some(i) == s.selected;
        let ta = c.t_span.0.max(v.t0);
        let tb = c.t_span.1.min(v.t1);
        if tb <= ta {
            continue;
        }
        if sel {
            // tc uncertainty band and heard-CPA line
            let (xa, xb) = (v.x_of(plot, c.tc_range.0), v.x_of(plot, c.tc_range.1));
            draw::rect_xywh(window, xa, plot.top(), (xb - xa).max(px(1.)), plot.size.height, t.canvas_accent().opacity(0.10));
            draw::vline(window, v.x_of(plot, c.flyby.tc), plot.top(), plot.bottom(), t.canvas_accent().opacity(0.45));
        }
        let xa = v.x_of(plot, ta);
        let xb = v.x_of(plot, tb);
        let n = ((f32::from(xb - xa) / 2.0) as usize).clamp(8, 1500);
        let color: Hsla = if sel {
            t.canvas_accent()
        } else if c.origin == Origin::Traced {
            t.ok
        } else if c.ground {
            t.warn.opacity(0.85)
        } else if !c.confident {
            gpui::white().opacity(0.45)
        } else {
            t.curve
        };
        for &r in &c.ratios {
            let pts: Vec<Point<Pixels>> = (0..=n)
                .map(|k| {
                    let tt = ta + (tb - ta) * k as f64 / n as f64;
                    point(v.x_of(plot, tt), v.y_of(plot, c.freq(r, tt)))
                })
                .collect();
            // Two thin rails either side of the fitted curve, never on it:
            // the spectral line the curve explains stays visible in the gap.
            let (gap, w, a) = if sel { (5.0, 1.5, 0.95) } else { (3.5, 1.0, 0.8) };
            for side in [-gap, gap] {
                if sel {
                    draw::offset_polyline(window, &pts, side, 3.0, gpui::black().opacity(0.35));
                }
                draw::offset_polyline(window, &pts, side, w, color.opacity(a));
            }
        }
        // the points it was traced from
        for p in &c.trace {
            let q = point(v.x_of(plot, p[0]), v.y_of(plot, p[1]));
            draw::circle(window, q, 3.0, Some(color.opacity(0.95)), Some((1.0, gpui::black().opacity(0.6))));
        }
        // label at the heard CPA of the lowest line
        let r0 = c.ratios.iter().copied().fold(f64::INFINITY, f64::min);
        let (lx, ly) = (v.x_of(plot, c.flyby.tc), v.y_of(plot, c.freq(r0, c.flyby.tc)));
        if lx > plot.left() && lx < plot.right() && ly > plot.top() && ly < plot.bottom() {
            let tag = match c.origin {
                Origin::Detected => "",
                Origin::Edited => " edited",
                Origin::Traced => " traced",
            };
            let label = if sel {
                format!("{}{tag}  {:.0} m/s  {:.1} km", i + 1, c.flyby.v, c.flyby.d / 1000.0)
            } else {
                format!("{}{tag}", i + 1)
            };
            let (fg, bg) = if sel { (t.accent_text, t.canvas_accent()) } else { (gpui::white().opacity(0.9), gpui::black().opacity(0.6)) };
            let w = draw::text_width(window, &label, 10.5, true) + px(10.);
            let x = (lx + px(6.)).min(plot.right() - w - px(4.));
            draw::chip(window, cx, &label, point(x, ly - px(26.)), 10.5, fg, bg, true);
        }
    }
}

/// Candidates' modelled curves: a solid line on a dark underlay (the
/// detected curves are drawn as two rails, so both stay readable).
/// The modelled curves (`labels` false) or their labels (`labels` true).
fn paint_models(window: &mut Window, cx: &mut App, s: &SpecScene, plot: &Bounds<Pixels>, labels: bool) {
    let v = s.view;
    // labels already drawn: the next one goes below them, not over them
    let mut placed: Vec<Bounds<Pixels>> = vec![];
    for (mi, m) in s.models.iter().enumerate() {
        for piece in m.pieces.iter().filter(|_| !labels) {
            let pts: Vec<Point<Pixels>> = piece
                .iter()
                .filter(|p| p[0] >= v.t0 - 5.0 && p[0] <= v.t1 + 5.0)
                .map(|p| point(v.x_of(plot, p[0]), v.y_of(plot, p[1])))
                .collect();
            // dashed: modelled, not heard
            for dash in dashes(&pts, 7.0, 4.0) {
                draw::polyline(window, &dash, 3.4, gpui::black().opacity(0.55));
                draw::polyline(window, &dash, 1.8, m.color);
            }
        }
        if let Some((tt, f, text)) = m.label.as_ref().filter(|_| labels) {
            let inside = |p: &Point<Pixels>| p.x > plot.left() && p.x < plot.right() && p.y > plot.top() && p.y < plot.bottom();
            // on the labelled line, away from the closest point where the
            // fly-by's own chip is: A on the approach, B on the way out
            let near = point(v.x_of(plot, *tt), v.y_of(plot, *f));
            let line = m
                .pieces
                .iter()
                .min_by(|a, b| {
                    let d = |pc: &Vec<[f64; 2]>| pc.iter().map(|p| (v.x_of(plot, p[0]) - near.x).abs() + (v.y_of(plot, p[1]) - near.y).abs()).fold(px(f32::MAX), |a, b| a.min(b));
                    f32::from(d(a)).total_cmp(&f32::from(d(b)))
                })
                .map(|pc| pc.iter().map(|p| point(v.x_of(plot, p[0]), v.y_of(plot, p[1]))).filter(inside).collect::<Vec<_>>())
                .unwrap_or_default();
            let at = if line.len() >= 2 {
                let frac = if mi % 2 == 0 { 0.2 } else { 0.8 };
                Some(line[((line.len() - 1) as f32 * frac) as usize])
            } else {
                inside(&near).then_some(near)
            };
            if let Some(Point { x, y }) = at {
                let w = draw::text_width(window, text, 10.5, false) + px(10.);
                let x = (x - w / 2.0).clamp(plot.left() + px(2.), plot.right() - w - px(2.));
                // under the line, or above it near the bottom of the plot
                let mut ly = if y + px(28.) > plot.bottom() { y - px(26.) } else { y + px(8.) };
                let step = if y + px(28.) > plot.bottom() { px(-20.) } else { px(20.) };
                let mut r = Bounds::new(point(x, ly), size(w, px(18.)));
                for _ in 0..4 {
                    if !placed.iter().any(|p| p.intersects(&r)) {
                        break;
                    }
                    ly += step;
                    r.origin.y = ly;
                }
                placed.push(r);
                draw::chip(window, cx, text, point(x, ly), 10.5, gpui::black(), m.color, false);
            }
        }
    }
}

/// A polyline cut into dashes `on` px long with `off` px gaps.
fn dashes(pts: &[Point<Pixels>], on: f32, off: f32) -> Vec<Vec<Point<Pixels>>> {
    let mut out = vec![];
    let mut cur: Vec<Point<Pixels>> = vec![];
    let (mut drawing, mut left) = (true, on);
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (dx, dy) = (f32::from(b.x - a.x), f32::from(b.y - a.y));
        let len = (dx * dx + dy * dy).sqrt();
        let mut done = 0.0;
        if drawing && cur.is_empty() {
            cur.push(a);
        }
        while len - done > left {
            done += left;
            let f = done / len;
            let p = point(a.x + px(dx * f), a.y + px(dy * f));
            if drawing {
                cur.push(p);
                out.push(std::mem::take(&mut cur));
            } else {
                cur.push(p);
            }
            drawing = !drawing;
            left = if drawing { on } else { off };
        }
        left -= len - done;
        if drawing {
            cur.push(b);
        } else {
            cur.clear();
        }
    }
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

/// Cuts (where material was removed): a red line with a label; the audio's
/// suggestions as dashed amber lines.
fn paint_cuts(window: &mut Window, cx: &mut App, s: &SpecScene, plot: &Bounds<Pixels>) {
    let v = s.view;
    let t = s.theme;
    for &c in &s.suggested_cuts {
        let x = v.x_of(plot, c);
        if x < plot.left() || x > plot.right() {
            continue;
        }
        let mut y = plot.top();
        while y < plot.bottom() {
            draw::vline(window, x, y, (y + px(6.)).min(plot.bottom()), t.warn.opacity(0.8));
            y += px(11.);
        }
    }
    for &c in &s.cuts {
        let x = v.x_of(plot, c);
        if x < plot.left() || x > plot.right() {
            continue;
        }
        draw::rect_xywh(window, x - px(1.), plot.top(), px(2.), plot.size.height, t.danger.opacity(0.9));
        draw::chip(window, cx, "cut", point(x + px(4.), plot.top() + px(4.)), 10.0, gpui::white(), t.danger, false);
    }
}

fn paint_edit(window: &mut Window, cx: &mut App, s: &SpecScene, e: &EditOverlay, plot: &Bounds<Pixels>) {
    let v = s.view;
    let t = s.theme;
    if e.handles
        && let Some(c) = s.selected.and_then(|i| s.curves.get(i))
    {
        let [centre, steep, spread] = handles(&v, plot, c);
        let a = t.canvas_accent();
        draw::circle(window, centre, 7.0, Some(a.opacity(0.35)), Some((2.0, a)));
        for (p, label) in [(steep, "steepness"), (spread, "speed")] {
            draw::round_rect(window, Bounds::new(point(p.x - px(5.), p.y - px(5.)), size(px(10.), px(10.))), 2.0, gpui::white(), Some(a));
            draw::chip(window, cx, label, point(p.x + px(9.), p.y - px(8.)), 10.0, gpui::white().opacity(0.9), gpui::black().opacity(0.6), false);
        }
        if e.refitting {
            draw::chip(window, cx, "snapping to the line…", point(centre.x + px(12.), centre.y + px(10.)), 10.5, t.accent_text, a, false);
        }
    }
    if !e.pending.is_empty() {
        let mut pts: Vec<[f64; 2]> = e.pending.clone();
        pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
        let px_pts: Vec<Point<Pixels>> = pts.iter().map(|p| point(v.x_of(plot, p[0]), v.y_of(plot, p[1]))).collect();
        draw::polyline(window, &px_pts, 1.0, t.ok.opacity(0.7));
        for q in &px_pts {
            draw::circle(window, *q, 4.0, Some(t.ok), Some((1.5, gpui::black().opacity(0.7))));
        }
    }
}

/// Index of the curve closest to a point (within `max_px`).
pub fn hit_curve(s_view: &View, plot: &Bounds<Pixels>, curves: &[Curve], pos: Point<Pixels>, max_px: f32) -> Option<usize> {
    let t = s_view.t_at(plot, pos.x);
    let mut best: Option<(usize, f32)> = None;
    for (i, c) in curves.iter().enumerate() {
        if t < c.t_span.0 || t > c.t_span.1 {
            continue;
        }
        for &r in &c.ratios {
            let y = s_view.y_of(plot, c.freq(r, t));
            let d = f32::from((y - pos.y).abs());
            if d <= max_px && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
    }
    best.map(|(i, _)| i)
}

pub struct OverviewScene {
    pub image: Option<SpecImage>,
    pub envelope: Vec<f32>,
    pub duration: f64,
    pub view: View,
    pub markers: Vec<(f64, bool)>,
    pub selected: Option<usize>,
    pub cursor_t: Option<f64>,
    pub theme: Theme,
    pub cuts: Vec<f64>,
}

pub const ENVELOPE_H: f32 = 16.0;

/// Overview: waveform envelope on top, whole-file spectrogram below, the
/// main view's window, fly-by markers and the cursor.
pub fn paint_overview(bounds: Bounds<Pixels>, s: &OverviewScene, window: &mut Window, _cx: &mut App) {
    let t = s.theme;
    draw::rect(window, bounds, t.surface);
    let inner = Bounds::new(
        point(bounds.left() + px(LEFT_RULER), bounds.top() + px(4.)),
        size(bounds.size.width - px(LEFT_RULER), bounds.size.height - px(8.)),
    );
    if s.duration <= 0.0 {
        return;
    }
    let x_of = |tt: f64| inner.left() + inner.size.width * ((tt / s.duration) as f32);
    let env_b = Bounds::new(inner.origin, size(inner.size.width, px(ENVELOPE_H)));
    let spec_b = Bounds::new(
        point(inner.left(), inner.top() + px(ENVELOPE_H + 2.)),
        size(inner.size.width, inner.size.height - px(ENVELOPE_H + 2.)),
    );
    // envelope as a mirrored bar strip
    if !s.envelope.is_empty() {
        let n = s.envelope.len();
        let w = f32::from(env_b.size.width);
        let cols = (w as usize).max(1);
        let mid = env_b.top() + env_b.size.height / 2.0;
        for c in 0..cols {
            let a = c * n / cols;
            let b = ((c + 1) * n / cols).max(a + 1).min(n);
            let v = s.envelope[a..b].iter().copied().fold(0.0f32, f32::max);
            let h = env_b.size.height * (0.08 + 0.92 * v) / 2.0;
            draw::rect_xywh(window, env_b.left() + px(c as f32), mid - h, px(1.), h * 2.0, t.text_muted.opacity(0.55));
        }
    }
    draw::rect(window, spec_b, t.canvas_bg);
    if let Some(img) = &s.image {
        let _ = window.paint_image(spec_b, Corners::default(), img.image.clone(), 0, false);
    }
    // window of the main view
    let (xa, xb) = (x_of(s.view.t0), x_of(s.view.t1));
    let dim = gpui::black().opacity(if t.mode == crate::theme::Mode::Dark { 0.55 } else { 0.40 });
    draw::rect_xywh(window, inner.left(), inner.top(), xa - inner.left(), inner.size.height, dim);
    draw::rect_xywh(window, xb, inner.top(), inner.right() - xb, inner.size.height, dim);
    let vb = Bounds::new(point(xa, inner.top()), size((xb - xa).max(px(2.)), inner.size.height));
    draw::round_rect(window, vb, 2.0, gpui::transparent_black(), Some(t.accent.opacity(0.9)));

    for (i, &(tc, ground)) in s.markers.iter().enumerate() {
        let x = x_of(tc);
        let sel = Some(i) == s.selected;
        let color = if sel { t.canvas_accent() } else if ground { t.warn } else { gpui::white().opacity(0.85) };
        draw::vline(window, x, spec_b.top(), spec_b.bottom(), color.opacity(if sel { 0.9 } else { 0.35 }));
        draw::triangle_down(window, point(x + px(0.5), spec_b.top() + px(6.)), if sel { 9.0 } else { 7.0 }, 6.0, color);
    }
    for &c in &s.cuts {
        draw::rect_xywh(window, x_of(c) - px(1.), inner.top(), px(2.), inner.size.height, t.danger.opacity(0.9));
    }
    if let Some(ct) = s.cursor_t {
        draw::vline(window, x_of(ct), inner.top(), inner.bottom(), gpui::white().opacity(0.9));
    }
}

#[cfg(test)]
mod dash_tests {
    use super::*;

    #[test]
    fn dashes_split_a_line() {
        let pts = [point(px(0.), px(0.)), point(px(22.), px(0.))];
        let d = dashes(&pts, 7.0, 4.0);
        // 0-7, 11-18, then 22-22 is too short to keep
        assert_eq!(d.len(), 2);
        assert_eq!(f32::from(d[0][1].x), 7.0);
        assert_eq!(f32::from(d[1][0].x), 11.0);
        assert_eq!(f32::from(d[1][1].x), 18.0);
    }
}
