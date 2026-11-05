//! The recording's timeline under the map or the spectrogram (Rerun
//! style): the whole recording as one bar, the fly-bys on it, the cuts and
//! the cuts the audio suggests, and the playing position.

use gpui::{App, Bounds, Pixels, Point, Window, point, px, size};

use crate::draw::{self, Align};
use crate::theme::Theme;

/// A fly-by on the timeline.
#[derive(Clone, Copy, Debug)]
pub struct Mark {
    pub t: f64,
    /// An aircraft of the selected place explains it.
    pub matched: bool,
    pub selected: bool,
}

pub struct TimelineScene {
    pub duration: f64,
    pub marks: Vec<Mark>,
    pub cuts: Vec<f64>,
    /// Shown only while the cuts are being edited.
    pub suggested: Vec<f64>,
    pub cursor: Option<f64>,
    pub hover: Option<f64>,
    /// The cut being dragged.
    pub dragging: Option<usize>,
    /// The time window the spectrogram shows, when it is shown.
    pub window: Option<(f64, f64)>,
    pub theme: Theme,
}

/// Height of the bar the timeline sits in.
pub const BAR_H: f32 = 40.0;
const PAD_X: f32 = 8.0;
const TRACK_Y: f32 = 13.0;
const TRACK_H: f32 = 6.0;

/// The part of the canvas that maps to the recording.
pub fn track(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(point(bounds.left() + px(PAD_X), bounds.top()), size(bounds.size.width - px(2.0 * PAD_X), bounds.size.height))
}

pub fn t_at(bounds: Bounds<Pixels>, duration: f64, x: Pixels) -> f64 {
    let tr = track(bounds);
    (((x - tr.left()) / tr.size.width) as f64 * duration).clamp(0.0, duration)
}

pub fn x_of(bounds: Bounds<Pixels>, duration: f64, t: f64) -> Pixels {
    let tr = track(bounds);
    tr.left() + tr.size.width * ((t / duration.max(1e-9)) as f32)
}

/// What is under the pointer: a cut (index), else a suggested cut (index).
pub enum Hit {
    Cut(usize),
    Suggested(usize),
    Track,
}

pub fn hit(bounds: Bounds<Pixels>, s: &TimelineScene, pos: Point<Pixels>) -> Hit {
    let near = |t: f64| (f32::from(x_of(bounds, s.duration, t) - pos.x)).abs() <= 5.0;
    if let Some(i) = s.cuts.iter().position(|&c| near(c)) {
        return Hit::Cut(i);
    }
    if let Some(i) = s.suggested.iter().position(|&c| near(c)) {
        return Hit::Suggested(i);
    }
    Hit::Track
}

pub fn paint(bounds: Bounds<Pixels>, s: &TimelineScene, window: &mut Window, cx: &mut App) {
    let t = s.theme;
    if s.duration <= 0.0 {
        return;
    }
    let tr = track(bounds);
    let x = |tt: f64| x_of(bounds, s.duration, tt);
    let y0 = bounds.top() + px(TRACK_Y);
    let bar = Bounds::new(point(tr.left(), y0), size(tr.size.width, px(TRACK_H)));
    draw::round_rect(window, bar, 3.0, t.border_strong.opacity(0.7), None);
    // the part the spectrogram shows
    if let Some((a, b)) = s.window
        && (b - a) < 0.98 * s.duration
    {
        let (xa, xb) = (x(a), x(b));
        draw::round_rect(window, Bounds::new(point(xa, y0 - px(3.)), size((xb - xa).max(px(2.)), px(TRACK_H + 6.))), 2.0, t.accent.opacity(0.12), Some(t.accent.opacity(0.5)));
    }
    // played so far
    if let Some(c) = s.cursor {
        draw::round_rect(window, Bounds::new(bar.origin, size((x(c) - tr.left()).max(px(0.)), px(TRACK_H))), 3.0, t.accent.opacity(0.45), None);
    }
    // time ticks with labels
    let px_per_s = f32::from(tr.size.width) as f64 / s.duration;
    let step = draw::nice_step(90.0 / px_per_s, &[1., 2., 5., 10., 15., 30., 60., 120., 300., 600., 900., 1800., 3600.]);
    let mut tt = 0.0;
    while tt <= s.duration + 1e-6 {
        let xx = x(tt);
        draw::vline(window, xx, y0 + px(TRACK_H + 2.), y0 + px(TRACK_H + 5.), t.text_faint);
        if xx < tr.right() - px(24.) {
            draw::text(window, cx, draw::fmt_time(tt, step), point(xx, y0 + px(TRACK_H + 6.)), 9.5, t.text_faint, true, Align::Center);
        }
        tt += step;
    }
    // fly-bys
    for m in &s.marks {
        let xx = x(m.t);
        let col = if m.selected {
            t.accent
        } else if m.matched {
            t.ok
        } else {
            t.text_muted
        };
        let h = if m.selected { 14.0 } else { 10.0 };
        draw::rect_xywh(window, xx - px(1.), y0 + px(TRACK_H / 2.0 - h / 2.0), px(2.), px(h), col);
    }
    // suggested cuts: dashed
    for &c in &s.suggested {
        let xx = x(c);
        let mut y = bounds.top() + px(2.);
        while y < bounds.bottom() - px(14.) {
            draw::vline(window, xx, y, y + px(3.), t.warn);
            y += px(5.);
        }
        draw::text(window, cx, "?", point(xx + px(3.), bounds.top()), 9.5, t.warn, false, Align::Left);
    }
    // cuts
    for (i, &c) in s.cuts.iter().enumerate() {
        let xx = x(c);
        let drag = s.dragging == Some(i);
        draw::rect_xywh(window, xx - px(1.), bounds.top() + px(2.), px(2.), px(TRACK_Y + TRACK_H + 4.), t.danger);
        draw::round_rect(window, Bounds::new(point(xx - px(4.), bounds.top() + px(1.)), size(px(8.), px(8.))), 2.0, if drag { t.text } else { t.danger }, None);
    }
    // the playing position / cursor
    if let Some(c) = s.cursor {
        let xx = x(c);
        draw::vline(window, xx, bounds.top() + px(4.), y0 + px(TRACK_H + 4.), t.accent);
        draw::circle(window, point(xx + px(0.5), y0 + px(TRACK_H / 2.0)), 5.0, Some(t.accent), Some((1.5, t.panel_bg)));
    }
    if let Some(h) = s.hover {
        let xx = x(h);
        let label = draw::fmt_time(h, 1.0);
        let w = draw::text_width(window, &label, 9.5, true) + px(10.);
        let lx = (xx - w / 2.0).clamp(bounds.left(), bounds.right() - w);
        draw::vline(window, xx, y0 - px(2.), y0 + px(TRACK_H + 2.), t.text_muted);
        draw::chip(window, cx, &label, point(lx, y0 + px(TRACK_H + 4.)), 9.5, t.text, t.surface, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_and_x_round_trip() {
        let b = Bounds::new(point(px(100.), px(0.)), size(px(616.), px(40.)));
        assert_eq!(f32::from(x_of(b, 600.0, 0.0)), 108.0);
        assert_eq!(f32::from(x_of(b, 600.0, 600.0)), 708.0);
        assert!((t_at(b, 600.0, px(408.)) - 300.0).abs() < 1e-6);
        assert_eq!(t_at(b, 600.0, px(0.)), 0.0);
    }
}
