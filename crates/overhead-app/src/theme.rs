//! Colours and type. Zed-like chrome: flat panels, 1 px dividers, one accent.

use gpui::{App, Global, Hsla, Rgba, rgb, rgba};

pub const SANS: &str = "IBM Plex Sans";
pub const MONO: &str = "IBM Plex Mono";

/// Type scale (px). Every piece of text uses one of these; nothing is
/// smaller than `TEXT_XS`.
/// Body: values, inputs, buttons, table rows, the file name.
pub const TEXT: f32 = 14.0;
/// Labels, secondary text, panel headers, controls in bars.
pub const TEXT_SM: f32 = 13.0;
/// Captions: hints under a field, badges, units, axis labels.
pub const TEXT_XS: f32 = 12.0;
/// Card and empty-state titles, a big number.
pub const TEXT_LG: f32 = 16.0;
/// Line height, relative to the text size.
pub const LINE: f32 = 1.45;

/// Spacing scale (px): every padding, gap and margin of the shell is one of these.
pub const SP1: f32 = 4.0;
pub const SP2: f32 = 8.0;
pub const SP3: f32 = 12.0;
pub const SP4: f32 = 16.0;
pub const SP6: f32 = 24.0;

/// Inner horizontal padding of every panel (headers, rows, forms).
pub const PANEL_PX: f32 = SP3;
/// Title bar height (the Windows caption buttons fill it, 46 px wide each).
pub const TITLE_H: f32 = 40.0;
/// Width of a caption button (minimize, maximize, close), as Windows draws them.
pub const CAPTION_W: f32 = 46.0;
/// Height of every panel header (Recording, Spectrogram, Fly-bys, Location, Fly-by …).
pub const HEADER_H: f32 = 36.0;
/// Status bar height.
pub const STATUS_H: f32 = 26.0;
/// Table and list rows.
pub const ROW_H: f32 = 32.0;
/// Key/value rows in details.
pub const KV_H: f32 = 24.0;
/// Buttons, toggles and segmented controls inside headers and bars.
pub const CONTROL_H: f32 = 28.0;
/// Text fields and the primary button.
pub const FIELD_H: f32 = 32.0;
pub const RADIUS: f32 = 6.0;
/// Small controls inside a control (segments, badges, check boxes).
pub const RADIUS_SM: f32 = 4.0;
/// Icons next to text, and on their own in a button.
pub const ICON: f32 = 14.0;
pub const ICON_SM: f32 = 12.0;
/// Width of the invisible grab area of a splitter (the visible line is 1 px).
pub const SPLIT_GRAB: f32 = 7.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub mode: Mode,
    /// Window background behind panels (also the spectrogram surround).
    pub app_bg: Hsla,
    pub panel_bg: Hsla,
    /// Title bar, panel headers, table header.
    pub surface: Hsla,
    /// Inputs and wells.
    pub field_bg: Hsla,
    pub hover: Hsla,
    pub selected: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_faint: Hsla,
    pub accent: Hsla,
    pub accent_text: Hsla,
    pub accent_soft: Hsla,
    pub warn: Hsla,
    pub danger: Hsla,
    pub ok: Hsla,
    /// The second of two things compared (a candidate next to the selected one).
    pub alt: Hsla,
    /// Spectrogram canvas (always dark: the data is the hero).
    pub canvas_bg: Hsla,
    pub canvas_grid: Hsla,
    /// Unselected fitted curves.
    pub curve: Hsla,
    pub map_bg: Hsla,
    pub map_grid: Hsla,
    pub shadow: Hsla,
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn ca(hex: u32) -> Hsla {
    let v: Rgba = rgba(hex);
    v.into()
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            mode: Mode::Dark,
            app_bg: c(0x121418),
            panel_bg: c(0x17191e),
            surface: c(0x1c1f25),
            field_bg: c(0x121418),
            hover: c(0x23262d),
            selected: c(0x252c38),
            border: c(0x272a31),
            border_strong: c(0x353a44),
            text: c(0xd7dbe2),
            text_muted: c(0xa0a6b0),
            text_faint: c(0x737a85),
            accent: c(0x58b4ff),
            accent_text: c(0x0b1622),
            accent_soft: ca(0x58b4ff26),
            warn: c(0xe2b866),
            danger: c(0xe5737d),
            ok: c(0x8cc47a),
            alt: c(0xf07ab8),
            canvas_bg: c(0x07070b),
            canvas_grid: ca(0xffffff14),
            curve: ca(0xffffffa0),
            map_bg: c(0x101216),
            map_grid: ca(0xffffff12),
            shadow: ca(0x00000080),
        }
    }

    pub fn light() -> Self {
        Self {
            mode: Mode::Light,
            app_bg: c(0xeceef1),
            panel_bg: c(0xf8f9fa),
            surface: c(0xf1f2f4),
            field_bg: c(0xffffff),
            hover: c(0xe7e9ed),
            selected: c(0xdde8f7),
            border: c(0xdfe2e7),
            border_strong: c(0xc9ced6),
            text: c(0x1d2127),
            text_muted: c(0x535a66),
            text_faint: c(0x7d848f),
            accent: c(0x1f6fd6),
            accent_text: c(0xffffff),
            accent_soft: ca(0x1f6fd61f),
            warn: c(0xa8700f),
            danger: c(0xc23b46),
            ok: c(0x3b8a36),
            alt: c(0xb4337a),
            canvas_bg: c(0x07070b),
            canvas_grid: ca(0xffffff14),
            curve: ca(0xffffffa0),
            map_bg: c(0xf3f4f1),
            map_grid: ca(0x1d21271a),
            shadow: ca(0x0000002e),
        }
    }

    pub fn toggled(&self) -> Self {
        match self.mode {
            Mode::Dark => Self::light(),
            Mode::Light => Self::dark(),
        }
    }

    /// Accent used on top of the (always dark) spectrogram canvas.
    pub fn canvas_accent(&self) -> Hsla {
        c(0x58b4ff)
    }
}

impl Global for Theme {}

pub fn theme(cx: &App) -> Theme {
    *cx.global::<Theme>()
}

/// Background of the app's mark: the blue gradient of assets/icon.svg, the
/// same in both themes.
pub fn logo_background() -> gpui::Background {
    gpui::linear_gradient(135.0, gpui::linear_color_stop(c(0x5cb8ff), 0.0), gpui::linear_color_stop(c(0x1c5cc0), 1.0))
}
