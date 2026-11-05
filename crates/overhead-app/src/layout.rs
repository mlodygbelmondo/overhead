//! Panel sizes, collapsed panels and the window position, kept between runs
//! in `layout.json` in the settings folder (`%APPDATA%\Overhead` on Windows,
//! `~/.config/overhead` elsewhere).

use std::fs;
use std::path::PathBuf;

use gpui::{Bounds, Pixels, WindowBounds, point, px, size};
use serde::{Deserialize, Serialize};

use crate::theme;

/// The draggable dividers between panels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    /// Input form | centre (vertical line).
    Sidebar,
    /// Centre | right panel (vertical line).
    Inspector,
    /// Spectrogram (or map) above | fly-by table below (horizontal line).
    Table,
    /// Right panel: location and map above | results and fly-by below (horizontal line).
    Right,
    /// The middle: map or spectrogram above | the analysis drawer below (horizontal line).
    Drawer,
}

pub const SIDEBAR_W: f32 = 320.0;
pub const SIDEBAR_MIN: f32 = 260.0;
pub const SIDEBAR_MAX: f32 = 440.0;
pub const INSPECTOR_W: f32 = 380.0;
pub const INSPECTOR_MIN: f32 = 300.0;
pub const INSPECTOR_MAX: f32 = 760.0;
pub const TABLE_H: f32 = 236.0;
pub const TABLE_MIN: f32 = 104.0;
/// The analysis drawer (in place of the table, or under the map).
pub const ANALYSIS_H: f32 = 320.0;
pub const ANALYSIS_MIN: f32 = 140.0;
pub const RIGHT_TOP_H: f32 = 460.0;
pub const RIGHT_TOP_MIN: f32 = 220.0;
/// Width of the folded input form (a rail with two buttons).
pub const RAIL_W: f32 = 40.0;
/// Narrowest centre column (spectrogram) next to the side panels.
pub const CENTER_MIN: f32 = 360.0;
/// Below this width a widened form gives its extra width back before the
/// centre column gets narrower (the form is never pushed under its default).
pub const CENTER_PREF: f32 = 560.0;
/// Least height the spectrogram (or the map in its place) keeps above the table.
pub const CENTER_TOP_MIN: f32 = 180.0;
/// Least height of the results below the map in the right panel.
pub const RIGHT_BOTTOM_MIN: f32 = 120.0;

/// Window placement as GPUI reports it (logical pixels).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub maximized: bool,
}

impl WindowState {
    pub fn from_bounds(b: WindowBounds) -> Self {
        let (r, maximized) = match b {
            WindowBounds::Windowed(r) => (r, false),
            WindowBounds::Maximized(r) | WindowBounds::Fullscreen(r) => (r, true),
        };
        Self {
            x: f32::from(r.origin.x),
            y: f32::from(r.origin.y),
            w: f32::from(r.size.width),
            h: f32::from(r.size.height),
            maximized,
        }
    }

    pub fn bounds(&self) -> Bounds<Pixels> {
        Bounds::new(point(px(self.x), px(self.y)), size(px(self.w), px(self.h)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub sidebar_w: f32,
    pub inspector_w: f32,
    pub table_h: f32,
    /// `table_h` was set by dragging; otherwise the table is as tall as its rows need.
    pub table_user: bool,
    /// Height of the analysis drawer.
    pub analysis_h: f32,
    pub right_top_h: f32,
    /// The fly-by table in the right column (no spectrogram, simple view);
    /// `None`: as tall as its rows need.
    pub right_table_h: Option<f32>,
    /// The input form is shown (otherwise a narrow rail).
    pub sidebar_open: bool,
    /// The spectrogram is shown (otherwise the map takes its place).
    pub spectrogram: bool,
    pub window: Option<WindowState>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar_w: SIDEBAR_W,
            inspector_w: INSPECTOR_W,
            table_h: TABLE_H,
            table_user: false,
            analysis_h: ANALYSIS_H,
            right_top_h: RIGHT_TOP_H,
            right_table_h: None,
            sidebar_open: true,
            // simple by default: the map is the answer; the spectrogram is one click away
            spectrogram: false,
            window: None,
        }
    }
}

impl Layout {
    /// Back to the default size (`table_right`: the right column holds the fly-by table).
    pub fn reset(&mut self, s: Split, table_right: bool) {
        let d = Self::default();
        match s {
            Split::Sidebar => self.sidebar_w = d.sidebar_w,
            Split::Inspector => self.inspector_w = d.inspector_w,
            Split::Drawer => self.analysis_h = d.analysis_h,
            Split::Table => {
                self.table_h = d.table_h;
                self.table_user = false;
            }
            Split::Right if table_right => self.right_table_h = None,
            Split::Right => self.right_top_h = d.right_top_h,
        }
    }

    /// Height of the fly-by table in the right column for `rows` fly-bys.
    pub fn right_table(&self, rows: usize, h: f32) -> f32 {
        // header, column titles and the rows (no empty block under one or two)
        let auto = table_auto(rows, 1, 10);
        self.right_table_h.unwrap_or(auto).clamp(TABLE_MIN, (h - RIGHT_BOTTOM_MIN).max(TABLE_MIN))
    }

    /// Sizes that fit a body of `w` x `h` logical pixels (the stored ones are
    /// kept as they are, so a window made smaller and larger again gets them back).
    /// `rows`: fly-bys in the middle table (its height follows them until dragged).
    pub fn fitted(&self, w: f32, h: f32, rows: usize) -> Fitted {
        let mut side = if self.sidebar_open { self.sidebar_w.clamp(SIDEBAR_MIN, SIDEBAR_MAX) } else { RAIL_W };
        let mut insp = self.inspector_w.clamp(INSPECTOR_MIN, INSPECTOR_MAX);
        // the centre keeps at least CENTER_MIN: the right panel gives first, then the form
        let spare = w - side - CENTER_MIN;
        if insp > spare {
            insp = spare.max(INSPECTOR_MIN);
        }
        // a form widened past its default gives that back before the centre
        // gets narrower than CENTER_PREF
        if self.sidebar_open && side > SIDEBAR_W && w - side - insp < CENTER_PREF {
            side = (w - insp - CENTER_PREF).clamp(SIDEBAR_W, side);
        }
        if self.sidebar_open && side + insp + CENTER_MIN > w {
            side = (w - insp - CENTER_MIN).clamp(SIDEBAR_MIN, side);
        }
        // as tall as its rows need, but the spectrogram (or map) above keeps most of the height
        let table_h = if self.table_user { self.table_h } else { table_auto(rows, 3, 8).min((h * 0.42).max(TABLE_MIN)) };
        let table = table_h.clamp(TABLE_MIN, (h - CENTER_TOP_MIN).max(TABLE_MIN));
        let right_top = self.right_top_h.clamp(RIGHT_TOP_MIN, (h - RIGHT_BOTTOM_MIN).max(RIGHT_TOP_MIN));
        let analysis = self.analysis_h.clamp(ANALYSIS_MIN, (h - CENTER_TOP_MIN).max(ANALYSIS_MIN));
        Fitted { sidebar_w: side, inspector_w: insp, table_h: table, right_top_h: right_top, analysis_h: analysis }
    }
}

/// Height of a fly-by table showing `rows` rows (at least `lo`, at most `hi`):
/// its header, the column titles, the rows and their dividers.
fn table_auto(rows: usize, lo: usize, hi: usize) -> f32 {
    // nothing found: room for the two-line message
    let n = if rows == 0 { 3 } else { rows.clamp(lo, hi) };
    theme::HEADER_H + theme::ROW_H * (n as f32 + 1.0) + theme::SP1
}

/// Panel sizes for the current window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fitted {
    pub sidebar_w: f32,
    pub inspector_w: f32,
    pub table_h: f32,
    pub right_top_h: f32,
    pub analysis_h: f32,
}

fn path() -> Option<PathBuf> {
    crate::update::config_dir().map(|d| d.join("layout.json"))
}

pub fn load() -> Layout {
    path()
        .and_then(|p| fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Layout>(&b).ok())
        .map(sanitize)
        .unwrap_or_default()
}

/// Drop values a corrupt or hand-edited file could hold (NaN, negative sizes).
fn sanitize(mut l: Layout) -> Layout {
    let d = Layout::default();
    let ok = |v: f32| v.is_finite() && v > 0.0 && v < 100_000.0;
    if !ok(l.sidebar_w) {
        l.sidebar_w = d.sidebar_w;
    }
    if !ok(l.inspector_w) {
        l.inspector_w = d.inspector_w;
    }
    if !ok(l.table_h) {
        l.table_h = d.table_h;
    }
    if !ok(l.right_top_h) {
        l.right_top_h = d.right_top_h;
    }
    if !ok(l.analysis_h) {
        l.analysis_h = d.analysis_h;
    }
    if l.right_table_h.is_some_and(|v| !ok(v)) {
        l.right_table_h = None;
    }
    if let Some(w) = l.window
        && !(ok(w.w) && ok(w.h) && w.x.is_finite() && w.y.is_finite() && w.x.abs() < 100_000.0 && w.y.abs() < 100_000.0)
    {
        l.window = None;
    }
    l
}

pub fn save(l: &Layout) {
    let Some(p) = path() else { return };
    let Ok(json) = serde_json::to_vec_pretty(l) else { return };
    let res = (|| -> std::io::Result<()> {
        if let Some(dir) = p.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &p)
    })();
    match res {
        Ok(()) => log::debug!("layout saved to {}", p.display()),
        Err(e) => log::warn!("cannot save the layout to {}: {e}", p.display()),
    }
}

/// Where to open the window: the saved place if it is still on a screen,
/// else `fallback`. `screens` are the displays' bounds.
pub fn restore_bounds(saved: Option<WindowState>, screens: &[Bounds<Pixels>], fallback: Bounds<Pixels>, min: (f32, f32)) -> WindowBounds {
    let Some(s) = saved else { return WindowBounds::Windowed(fallback) };
    let mut b = s.bounds();
    b.size.width = b.size.width.max(px(min.0));
    b.size.height = b.size.height.max(px(min.1));
    // a saved place must still be (mostly) on one of the screens: a monitor
    // may have been unplugged since. On Wayland every origin is 0,0 and the
    // compositor places the window, so only the size matters there.
    let visible = screens.is_empty()
        || screens.iter().any(|sc| {
            let ix = (b.right().min(sc.right()) - b.left().max(sc.left())).max(px(0.));
            let iy = (b.bottom().min(sc.bottom()) - b.top().max(sc.top())).max(px(0.));
            ix >= px(120.) && iy >= px(60.) && b.top() >= sc.top() - px(8.)
        });
    if !visible {
        b = Bounds::new(fallback.origin, b.size);
        if let Some(sc) = screens.first() {
            let w = b.size.width.min(sc.size.width);
            let h = b.size.height.min(sc.size.height);
            b = Bounds::new(point(sc.left() + (sc.size.width - w) / 2.0, sc.top() + (sc.size.height - h) / 2.0), size(w, h));
        }
    }
    if s.maximized { WindowBounds::Maximized(b) } else { WindowBounds::Windowed(b) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitted_keeps_the_centre() {
        let l = Layout { sidebar_w: 400.0, inspector_w: 700.0, ..Default::default() };
        let f = l.fitted(1084.0, 600.0, 7);
        assert!(f.sidebar_w + f.inspector_w + CENTER_MIN <= 1084.0 + 0.5, "{f:?}");
        assert!(f.sidebar_w >= SIDEBAR_MIN && f.inspector_w >= INSPECTOR_MIN);
        // a big window gives the stored sizes back
        let f = l.fitted(2400.0, 1300.0, 7);
        assert_eq!((f.sidebar_w, f.inspector_w), (400.0, 700.0));
        // a smaller window: the widened form goes back towards its default
        // before the centre gets narrower than CENTER_PREF
        let l = Layout { sidebar_w: 412.0, ..Default::default() };
        let f = l.fitted(1072.0, 700.0, 7);
        assert_eq!(f.inspector_w, INSPECTOR_MIN);
        assert_eq!(f.sidebar_w, SIDEBAR_W);
        let f = l.fitted(1300.0, 700.0, 7);
        assert_eq!((f.sidebar_w, f.inspector_w), (1300.0 - INSPECTOR_W - CENTER_PREF, INSPECTOR_W));
    }

    #[test]
    fn table_follows_its_rows_until_dragged() {
        let l = Layout::default();
        let seven = l.fitted(1600.0, 1000.0, 7).table_h;
        assert!(seven >= theme::HEADER_H + 8.0 * theme::ROW_H, "{seven}");
        assert!(l.fitted(1600.0, 1000.0, 1).table_h < seven);
        let l = Layout { table_user: true, table_h: 300.0, ..Default::default() };
        assert_eq!(l.fitted(1600.0, 1000.0, 1).table_h, 300.0);
        // one fly-by in the right column: no tall empty block under it
        assert!(Layout::default().right_table(1, 900.0) <= theme::HEADER_H + 2.0 * theme::ROW_H + theme::SP2);
    }

    #[test]
    fn fitted_clamps_heights() {
        let l = Layout { table_h: 2000.0, table_user: true, right_top_h: 10.0, ..Default::default() };
        let f = l.fitted(1600.0, 800.0, 7);
        assert_eq!(f.table_h, 800.0 - CENTER_TOP_MIN);
        assert_eq!(f.right_top_h, RIGHT_TOP_MIN);
    }

    #[test]
    fn json_roundtrip_and_old_files() {
        let l = Layout {
            sidebar_open: false,
            window: Some(WindowState { x: 10.0, y: 20.0, w: 1300.0, h: 800.0, maximized: true }),
            ..Default::default()
        };
        let s = serde_json::to_string(&l).unwrap();
        assert_eq!(serde_json::from_str::<Layout>(&s).unwrap(), l);
        // missing fields take their defaults
        let p: Layout = serde_json::from_str(r#"{"sidebar_w": 300}"#).unwrap();
        assert_eq!(p.sidebar_w, 300.0);
        assert!(!p.spectrogram && p.sidebar_open && p.window.is_none());
        assert_eq!(sanitize(Layout { table_h: f32::NAN, ..Default::default() }).table_h, TABLE_H);
    }

    #[test]
    fn restore_moves_lost_windows_back() {
        let screen = Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1080.)));
        let fb = Bounds::new(point(px(100.), px(100.)), size(px(1200.), px(800.)));
        let on = WindowState { x: 300.0, y: 200.0, w: 1300.0, h: 820.0, maximized: false };
        assert_eq!(restore_bounds(Some(on), &[screen], fb, (1100.0, 680.0)), WindowBounds::Windowed(on.bounds()));
        // on a monitor that is gone: centred on the first screen, size kept
        let off = WindowState { x: 3000.0, y: 200.0, w: 1300.0, h: 820.0, maximized: true };
        match restore_bounds(Some(off), &[screen], fb, (1100.0, 680.0)) {
            WindowBounds::Maximized(b) => {
                assert_eq!(b.size, size(px(1300.), px(820.)));
                assert_eq!(b.origin, point(px(310.), px(130.)));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(restore_bounds(None, &[screen], fb, (1100.0, 680.0)), WindowBounds::Windowed(fb));
    }
}
