//! Small immediate-mode drawing helpers for canvases.

use gpui::{
    App, BorderStyle, Bounds, Hsla, PathBuilder, Pixels, Point, SharedString, TextRun, Window, fill, font, outline,
    point, px, size,
};

use crate::theme::{MONO, SANS};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Paint one line of text; `pos.y` is the top of the line box. Returns its width.
#[allow(clippy::too_many_arguments)]
pub fn text(
    window: &mut Window,
    cx: &mut App,
    s: impl Into<SharedString>,
    pos: Point<Pixels>,
    size_px: f32,
    color: Hsla,
    mono: bool,
    align: Align,
) -> Pixels {
    let s: SharedString = s.into();
    if s.is_empty() {
        return px(0.);
    }
    let run = TextRun {
        len: s.len(),
        font: font(if mono { MONO } else { SANS }),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(s, px(size_px), &[run], None);
    let w = line.width;
    let x = match align {
        Align::Left => pos.x,
        Align::Center => pos.x - w / 2.0,
        Align::Right => pos.x - w,
    };
    line.paint(point(x, pos.y), px(size_px * 1.3), window, cx).ok();
    w
}

pub fn text_width(window: &mut Window, s: &str, size_px: f32, mono: bool) -> Pixels {
    let s: SharedString = s.to_string().into();
    let run = TextRun {
        len: s.len(),
        font: font(if mono { MONO } else { SANS }),
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window.text_system().shape_line(s, px(size_px), &[run], None).width
}

pub fn rect(window: &mut Window, b: Bounds<Pixels>, color: Hsla) {
    window.paint_quad(fill(b, color));
}

pub fn rect_xywh(window: &mut Window, x: Pixels, y: Pixels, w: Pixels, h: Pixels, color: Hsla) {
    window.paint_quad(fill(Bounds::new(point(x, y), size(w, h)), color));
}

pub fn frame(window: &mut Window, b: Bounds<Pixels>, color: Hsla) {
    window.paint_quad(outline(b, color, BorderStyle::Solid));
}

pub fn round_rect(window: &mut Window, b: Bounds<Pixels>, r: f32, bg: Hsla, border: Option<Hsla>) {
    window.paint_quad(gpui::quad(
        b,
        px(r),
        bg,
        if border.is_some() { px(1.) } else { px(0.) },
        border.unwrap_or(gpui::transparent_black()),
        BorderStyle::Solid,
    ));
}

/// Crisp 1 px horizontal / vertical rules (quads, not paths).
pub fn hline(window: &mut Window, x0: Pixels, x1: Pixels, y: Pixels, color: Hsla) {
    rect_xywh(window, x0, y, x1 - x0, px(1.), color);
}

pub fn vline(window: &mut Window, x: Pixels, y0: Pixels, y1: Pixels, color: Hsla) {
    rect_xywh(window, x, y0, px(1.), y1 - y0, color);
}

pub fn polyline(window: &mut Window, pts: &[Point<Pixels>], width: f32, color: Hsla) {
    if pts.len() < 2 {
        return;
    }
    let mut pb = PathBuilder::stroke(px(width));
    pb.move_to(pts[0]);
    for &q in &pts[1..] {
        pb.line_to(q);
    }
    if let Ok(p) = pb.build() {
        window.paint_path(p, color);
    }
}

/// The same polyline shifted sideways by `offset` px along its normal, so a
/// pair of these (±offset) frames a feature without covering it.
pub fn offset_polyline(window: &mut Window, pts: &[Point<Pixels>], offset: f32, width: f32, color: Hsla) {
    if pts.len() < 2 {
        return;
    }
    let n = pts.len();
    let shifted: Vec<Point<Pixels>> = (0..n)
        .map(|i| {
            let a = pts[i.saturating_sub(1)];
            let b = pts[(i + 1).min(n - 1)];
            let (dx, dy) = (f32::from(b.x - a.x), f32::from(b.y - a.y));
            let len = (dx * dx + dy * dy).sqrt().max(1e-3);
            point(pts[i].x - px(dy / len * offset), pts[i].y + px(dx / len * offset))
        })
        .collect();
    polyline(window, &shifted, width, color);
}

pub fn circle(window: &mut Window, c: Point<Pixels>, r: f32, fill_color: Option<Hsla>, stroke: Option<(f32, Hsla)>) {
    let n = ((r * 0.9) as usize).clamp(16, 96);
    let pts: Vec<Point<Pixels>> = (0..n)
        .map(|i| {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            point(c.x + px(r * a.cos()), c.y + px(r * a.sin()))
        })
        .collect();
    if let Some(color) = fill_color {
        let mut pb = PathBuilder::fill();
        pb.move_to(pts[0]);
        for &q in &pts[1..] {
            pb.line_to(q);
        }
        pb.close();
        if let Ok(p) = pb.build() {
            window.paint_path(p, color);
        }
    }
    if let Some((w, color)) = stroke {
        let mut pb = PathBuilder::stroke(px(w));
        pb.move_to(pts[0]);
        for &q in &pts[1..] {
            pb.line_to(q);
        }
        pb.close();
        if let Ok(p) = pb.build() {
            window.paint_path(p, color);
        }
    }
}

/// Closed polygon, filled and/or outlined.
pub fn polygon(window: &mut Window, pts: &[Point<Pixels>], fill_color: Option<Hsla>, stroke: Option<(f32, Hsla)>) {
    if pts.len() < 3 {
        return;
    }
    if let Some(color) = fill_color {
        let mut pb = PathBuilder::fill();
        pb.move_to(pts[0]);
        for &q in &pts[1..] {
            pb.line_to(q);
        }
        pb.close();
        if let Ok(p) = pb.build() {
            window.paint_path(p, color);
        }
    }
    if let Some((w, color)) = stroke {
        let mut pb = PathBuilder::stroke(px(w));
        pb.move_to(pts[0]);
        for &q in &pts[1..] {
            pb.line_to(q);
        }
        pb.close();
        if let Ok(p) = pb.build() {
            window.paint_path(p, color);
        }
    }
}

pub fn triangle_down(window: &mut Window, tip: Point<Pixels>, w: f32, h: f32, color: Hsla) {
    let mut pb = PathBuilder::fill();
    pb.move_to(point(tip.x - px(w / 2.0), tip.y - px(h)));
    pb.line_to(point(tip.x + px(w / 2.0), tip.y - px(h)));
    pb.line_to(tip);
    pb.close();
    if let Ok(p) = pb.build() {
        window.paint_path(p, color);
    }
}

/// Text on a small rounded chip; `pos` is the chip's top-left. Returns the chip bounds.
#[allow(clippy::too_many_arguments)]
pub fn chip(
    window: &mut Window,
    cx: &mut App,
    s: &str,
    pos: Point<Pixels>,
    size_px: f32,
    fg: Hsla,
    bg: Hsla,
    mono: bool,
) -> Bounds<Pixels> {
    let w = text_width(window, s, size_px, mono);
    let h = px(size_px * 1.3 + 4.0);
    let b = Bounds::new(pos, size(w + px(10.), h));
    round_rect(window, b, 3.0, bg, None);
    text(window, cx, s.to_string(), point(pos.x + px(5.), pos.y + px(2.)), size_px, fg, mono, Align::Left);
    b
}

/// "Nice" tick step >= `min_step` from 1-2-5 style candidates.
pub fn nice_step(min_step: f64, candidates: &[f64]) -> f64 {
    candidates.iter().copied().find(|&c| c >= min_step).unwrap_or_else(|| {
        let p = 10f64.powf(min_step.log10().ceil());
        if p / 2.0 >= min_step { p / 2.0 } else { p }
    })
}

pub fn fmt_time(t: f64, step: f64) -> String {
    let neg = t < 0.0;
    let t = t.abs();
    let total = t.floor() as i64;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    let frac = t - total as f64;
    let mut out = if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") };
    if step < 1.0 {
        let digits = if step < 0.1 { 2 } else { 1 };
        let f = format!("{:.*}", digits, frac);
        out.push_str(&f[1..]);
    }
    if neg { format!("-{out}") } else { out }
}

pub fn fmt_clock(sec_of_day: f64) -> String {
    let s = sec_of_day.rem_euclid(86_400.0).floor() as i64;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}
