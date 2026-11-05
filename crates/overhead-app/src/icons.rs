//! A handful of line icons painted as paths (no icon font needed).

use gpui::{BorderStyle, Bounds, Corners, Hsla, PathBuilder, Pixels, Point, Window, canvas, fill, outline, point, prelude::*, px, size};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Folder,
    Sun,
    Moon,
    Close,
    Minimize,
    Maximize,
    Restore,
    Check,
    Warn,
    Wave,
    Search,
    /// A window with its left column marked (show/hide the input form).
    PanelLeft,
    ChevronLeft,
    ChevronRight,
    Play,
    Pause,
    Copy,
    /// An arrow out of a box: opens in the web browser.
    External,
    /// Frame the result on the map.
    Target,
    Plus,
    Minus,
    /// A cut in the recording.
    Scissors,
    ChevronDown,
    Chart,
    /// The app's mark: a plane over the S-shaped Doppler curve (assets/icon.svg).
    Logo,
}

pub fn icon(kind: Icon, size: f32, color: Hsla) -> impl IntoElement {
    canvas(|_, _, _| {}, move |b, _, window, cx| {
        let bg = crate::theme::theme(cx).surface;
        paint_icon(window, kind, b, color, bg)
    })
        .w(px(size))
        .h(px(size))
        .flex_none()
}

/// A window caption glyph (minimize, maximize, restore, close) drawn like
/// the Windows 11 ones where Segoe Fluent Icons is not available: 10 x 10
/// px, 1 px lines on whole pixels.
pub fn caption_glyph(kind: Icon, color: Hsla) -> impl IntoElement {
    canvas(|_, _, _| {}, move |b, _, window, _| {
        let s = window.scale_factor();
        // whole device pixels, so the 1 px lines stay sharp
        let snap = |v: Pixels| px((f32::from(v) * s).round() / s);
        let o = point(snap(b.origin.x), snap(b.origin.y));
        let line = px(1.0);
        match kind {
            Icon::Minimize => window.paint_quad(fill(Bounds::new(point(o.x, o.y + px(5.)), size(px(10.), line)), color)),
            Icon::Maximize => {
                let r = Bounds::new(o, size(px(10.), px(10.)));
                window.paint_quad(outline(r, color, BorderStyle::Solid).corner_radii(Corners::all(px(1.5))));
            }
            Icon::Restore => {
                // the front window, and the corner of the one behind it
                let front = Bounds::new(point(o.x, o.y + px(2.)), size(px(8.), px(8.)));
                window.paint_quad(outline(front, color, BorderStyle::Solid).corner_radii(Corners::all(px(1.5))));
                let mut pb = PathBuilder::stroke(line);
                pb.move_to(point(o.x + px(2.), o.y + px(0.5)));
                pb.line_to(point(o.x + px(9.5), o.y + px(0.5)));
                pb.line_to(point(o.x + px(9.5), o.y + px(8.)));
                if let Ok(path) = pb.build() {
                    window.paint_path(path, color);
                }
            }
            _ => {
                for (a, c) in [((0.0, 0.0), (10.0, 10.0)), ((10.0, 0.0), (0.0, 10.0))] {
                    let mut pb = PathBuilder::stroke(line);
                    pb.move_to(point(o.x + px(a.0), o.y + px(a.1)));
                    pb.line_to(point(o.x + px(c.0), o.y + px(c.1)));
                    if let Ok(path) = pb.build() {
                        window.paint_path(path, color);
                    }
                }
            }
        }
    })
    .w(px(10.))
    .h(px(10.))
    .flex_none()
}

fn p(b: &Bounds<Pixels>, x: f32, y: f32) -> Point<Pixels> {
    // icon space is 16 x 16
    let s = b.size.width.min(b.size.height) / 16.0;
    point(b.origin.x + s * x, b.origin.y + s * y)
}

fn stroke(window: &mut Window, b: &Bounds<Pixels>, pts: &[(f32, f32)], close: bool, color: Hsla) {
    let w = (b.size.width / 16.0) * 1.35;
    let mut pb = PathBuilder::stroke(w);
    pb.move_to(p(b, pts[0].0, pts[0].1));
    for &(x, y) in &pts[1..] {
        pb.line_to(p(b, x, y));
    }
    if close {
        pb.close();
    }
    if let Ok(path) = pb.build() {
        window.paint_path(path, color);
    }
}

fn fill_poly(window: &mut Window, b: &Bounds<Pixels>, pts: &[(f32, f32)], color: Hsla) {
    let mut pb = PathBuilder::fill();
    pb.move_to(p(b, pts[0].0, pts[0].1));
    for &(x, y) in &pts[1..] {
        pb.line_to(p(b, x, y));
    }
    pb.close();
    if let Ok(path) = pb.build() {
        window.paint_path(path, color);
    }
}

/// The plane of the app's mark (assets/icon.svg), in icon space.
const LOGO_PLANE: [(f32, f32); 26] = [
    (13.125, 4.750), (13.063, 4.939), (12.894, 5.062), (12.640, 5.130), (12.325, 5.150), (11.225, 5.150), (10.425, 7.250),
    (9.875, 7.250), (10.225, 5.150), (9.125, 5.150), (8.625, 5.900), (8.275, 5.900), (8.475, 4.850), (8.175, 4.750),
    (8.475, 4.650), (8.275, 3.600), (8.625, 3.600), (9.125, 4.350), (10.225, 4.350), (9.875, 2.250), (10.425, 2.250),
    (11.225, 4.350), (12.325, 4.350), (12.640, 4.370), (12.894, 4.438), (13.063, 4.561),
];

/// A stroke `w` icon units wide with round ends.
fn stroke_round(window: &mut Window, b: &Bounds<Pixels>, pts: &[(f32, f32)], w: f32, color: Hsla) {
    let mut pb = PathBuilder::stroke((b.size.width.min(b.size.height) / 16.0) * w);
    pb.move_to(p(b, pts[0].0, pts[0].1));
    for &(x, y) in &pts[1..] {
        pb.line_to(p(b, x, y));
    }
    if let Ok(path) = pb.build() {
        window.paint_path(path, color);
    }
    for &(x, y) in [pts[0], pts[pts.len() - 1]].iter() {
        fill_poly(window, b, &circle_pts(x, y, w / 2.0, 16), color);
    }
}

fn circle_pts(cx: f32, cy: f32, r: f32, n: usize) -> Vec<(f32, f32)> {
    (0..n)
        .map(|i| {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

fn paint_icon(window: &mut Window, kind: Icon, b: Bounds<Pixels>, color: Hsla, bg: Hsla) {
    let b = &b;
    match kind {
        Icon::Folder => stroke(
            window,
            b,
            &[(1.5, 4.0), (6.0, 4.0), (7.5, 5.5), (14.5, 5.5), (14.5, 13.0), (1.5, 13.0)],
            true,
            color,
        ),
        Icon::Sun => {
            stroke(window, b, &circle_pts(8.0, 8.0, 3.0, 20), true, color);
            for i in 0..8 {
                let a = i as f32 / 8.0 * std::f32::consts::TAU;
                let (c, s) = (a.cos(), a.sin());
                stroke(window, b, &[(8.0 + 5.0 * c, 8.0 + 5.0 * s), (8.0 + 6.6 * c, 8.0 + 6.6 * s)], false, color);
            }
        }
        Icon::Moon => {
            // crescent: a disc with a background-coloured disc bitten out
            fill_poly(window, b, &circle_pts(7.6, 8.4, 5.6, 32), color);
            fill_poly(window, b, &circle_pts(10.4, 5.6, 4.6, 32), bg);
        }
        Icon::Close => {
            stroke(window, b, &[(4.0, 4.0), (12.0, 12.0)], false, color);
            stroke(window, b, &[(12.0, 4.0), (4.0, 12.0)], false, color);
        }
        Icon::Minimize => stroke(window, b, &[(4.0, 8.5), (12.0, 8.5)], false, color),
        Icon::Maximize => stroke(window, b, &[(4.0, 4.0), (12.0, 4.0), (12.0, 12.0), (4.0, 12.0)], true, color),
        Icon::Restore => {
            stroke(window, b, &[(4.0, 6.0), (10.0, 6.0), (10.0, 12.0), (4.0, 12.0)], true, color);
            stroke(window, b, &[(6.0, 6.0), (6.0, 4.0), (12.0, 4.0), (12.0, 10.0), (10.0, 10.0)], false, color);
        }
        Icon::Check => stroke(window, b, &[(3.5, 8.5), (6.5, 11.5), (12.5, 4.5)], false, color),
        Icon::Warn => {
            stroke(window, b, &[(8.0, 2.5), (14.5, 13.5), (1.5, 13.5)], true, color);
            stroke(window, b, &[(8.0, 6.5), (8.0, 9.5)], false, color);
            stroke(window, b, &[(8.0, 11.3), (8.0, 11.9)], false, color);
        }
        Icon::Wave => {
            let pts: Vec<(f32, f32)> =
                (0..=28).map(|i| { let x = 1.5 + i as f32 * 13.0 / 28.0; (x, 8.0 - 4.5 * ((x - 1.5) * 0.9).sin() * (1.0 - (x - 8.0).abs() / 8.0)) }).collect();
            stroke(window, b, &pts, false, color);
        }
        Icon::Logo => {
            // the S of a fly-by's pitch, the same cubic as assets/icon.svg
            let (p0, p1, p2, p3) = ((2.25, 7.0), (6.875, 7.0), (8.25, 11.875), (13.75, 11.875));
            let curve: Vec<(f32, f32)> = (0..=32)
                .map(|i| {
                    let t = i as f32 / 32.0;
                    let (a, b2, c, d) = ((1.0 - t).powi(3), 3.0 * (1.0 - t).powi(2) * t, 3.0 * (1.0 - t) * t * t, t.powi(3));
                    (a * p0.0 + b2 * p1.0 + c * p2.0 + d * p3.0, a * p0.1 + b2 * p1.1 + c * p2.1 + d * p3.1)
                })
                .collect();
            stroke_round(window, b, &curve, 1.5, color);
            fill_poly(window, b, &LOGO_PLANE, color);
        }
        Icon::Search => {
            stroke(window, b, &circle_pts(7.0, 7.0, 4.2, 20), true, color);
            stroke(window, b, &[(10.2, 10.2), (14.0, 14.0)], false, color);
        }
        Icon::PanelLeft => {
            stroke(window, b, &[(2.0, 3.0), (14.0, 3.0), (14.0, 13.0), (2.0, 13.0)], true, color);
            stroke(window, b, &[(6.0, 3.0), (6.0, 13.0)], false, color);
        }
        Icon::ChevronLeft => stroke(window, b, &[(10.0, 3.5), (5.5, 8.0), (10.0, 12.5)], false, color),
        Icon::ChevronRight => stroke(window, b, &[(6.0, 3.5), (10.5, 8.0), (6.0, 12.5)], false, color),
        Icon::ChevronDown => stroke(window, b, &[(3.5, 6.0), (8.0, 10.5), (12.5, 6.0)], false, color),
        Icon::Play => fill_poly(window, b, &[(4.5, 2.5), (13.5, 8.0), (4.5, 13.5)], color),
        Icon::Pause => {
            fill_poly(window, b, &[(4.0, 3.0), (7.0, 3.0), (7.0, 13.0), (4.0, 13.0)], color);
            fill_poly(window, b, &[(9.0, 3.0), (12.0, 3.0), (12.0, 13.0), (9.0, 13.0)], color);
        }
        Icon::Copy => {
            stroke(window, b, &[(5.5, 5.5), (13.5, 5.5), (13.5, 14.0), (5.5, 14.0)], true, color);
            stroke(window, b, &[(3.0, 11.0), (2.5, 11.0), (2.5, 2.5), (10.5, 2.5), (10.5, 3.0)], false, color);
        }
        Icon::External => {
            stroke(window, b, &[(7.0, 3.0), (3.0, 3.0), (3.0, 13.0), (13.0, 13.0), (13.0, 9.0)], false, color);
            stroke(window, b, &[(9.0, 2.5), (13.5, 2.5), (13.5, 7.0)], false, color);
            stroke(window, b, &[(13.2, 2.8), (7.5, 8.5)], false, color);
        }
        Icon::Target => {
            stroke(window, b, &circle_pts(8.0, 8.0, 4.5, 24), true, color);
            fill_poly(window, b, &circle_pts(8.0, 8.0, 1.6, 12), color);
            for (a, c) in [((8.0, 0.8), (8.0, 3.0)), ((8.0, 13.0), (8.0, 15.2)), ((0.8, 8.0), (3.0, 8.0)), ((13.0, 8.0), (15.2, 8.0))] {
                stroke(window, b, &[a, c], false, color);
            }
        }
        Icon::Plus => {
            stroke(window, b, &[(3.0, 8.0), (13.0, 8.0)], false, color);
            stroke(window, b, &[(8.0, 3.0), (8.0, 13.0)], false, color);
        }
        Icon::Minus => stroke(window, b, &[(3.0, 8.0), (13.0, 8.0)], false, color),
        Icon::Scissors => {
            stroke(window, b, &circle_pts(4.5, 12.0, 2.2, 14), true, color);
            stroke(window, b, &circle_pts(11.5, 12.0, 2.2, 14), true, color);
            stroke(window, b, &[(5.8, 10.3), (11.5, 2.0)], false, color);
            stroke(window, b, &[(10.2, 10.3), (4.5, 2.0)], false, color);
        }
        Icon::Chart => {
            stroke(window, b, &[(2.0, 2.0), (2.0, 14.0), (14.0, 14.0)], false, color);
            fill_poly(window, b, &circle_pts(5.5, 10.0, 1.3, 10), color);
            fill_poly(window, b, &circle_pts(8.5, 6.5, 1.3, 10), color);
            fill_poly(window, b, &circle_pts(12.0, 4.5, 1.3, 10), color);
            fill_poly(window, b, &circle_pts(10.5, 9.5, 1.3, 10), color);
        }
    }
}
