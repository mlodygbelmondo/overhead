//! The main window: title bar, input sidebar, spectrogram + fly-by table,
//! inspector (fly-by detail and location results), status bar.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use overhead_core::audio::Audio;
use overhead_core::background::{self, Background};
use overhead_core::detect::{DetectStats, Detection, Origin};
use overhead_core::edit as core_edit;
use overhead_core::model::{DEFAULT_C, Flyby};
use overhead_core::spectrogram::SpecRequest;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, CursorStyle, Decorations, DragMoveEvent, Entity, ExternalPaths,
    FocusHandle, Focusable, FontWeight, Hsla, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathPromptOptions, Pixels, Point, RenderImage, ResizeEdge, ScrollDelta, ScrollHandle, ScrollWheelEvent, SharedString, Size,
    Subscription, Task, Window, WindowControlArea, actions, canvas, deferred, div, point, prelude::*, px, linear_color_stop, linear_gradient, relative,
};

use crate::adsb::{self, AdsbSource};
use crate::explain_data;
use crate::heat::{Heat, HeatKey, Layer, Scope};
use crate::history;
use crate::analysis::{self, SpecImage};
use crate::icons::{Icon, icon};
use crate::layout::{self, Layout, Split};
use crate::location::{LocateControl, LocateJob, LocationResults, LocationSource, SampleSource};
use crate::map::{self, MapScene, MapView};
use crate::tiles::Tiles;
use crate::query::{self, RegionHint, SearchQuery, TimeHint, TimeMode};
use crate::spectro::{self, Curve, EditOverlay, OverviewScene, SpecScene, View};
use crate::text_input::{InputEvent, TextInput};
use crate::theme::{self, MONO, Theme};

mod explain_view;
mod listen;
mod result;

actions!(
    overhead,
    [
        Open,
        ToggleTheme,
        Quit,
        SelectNext,
        SelectPrev,
        ClearSelection,
        ZoomAll,
        ZoomSelection,
        ToggleWhiten,
        ToggleCurves,
        FocusNext,
        FocusPrev,
        FindLocation,
        ToggleDetails,
        ToggleEdit,
        Undo,
        DeleteFlyby,
        RemoveTracePoint,
        FitTrace,
        ToggleSpectrogram,
        ToggleSidebar,
        TogglePlay,
        ToggleAnalysis
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-o", Open, None),
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-shift-l", ToggleTheme, None),
        KeyBinding::new("ctrl-enter", FindLocation, None),
        KeyBinding::new("tab", FocusNext, Some("TextInput")),
        KeyBinding::new("shift-tab", FocusPrev, Some("TextInput")),
        KeyBinding::new("down", SelectNext, Some("Spectro")),
        KeyBinding::new("j", SelectNext, Some("Spectro")),
        KeyBinding::new("up", SelectPrev, Some("Spectro")),
        KeyBinding::new("k", SelectPrev, Some("Spectro")),
        KeyBinding::new("escape", ClearSelection, Some("Spectro")),
        KeyBinding::new("0", ZoomAll, Some("Spectro")),
        KeyBinding::new("f", ZoomSelection, Some("Spectro")),
        KeyBinding::new("w", ToggleWhiten, Some("Spectro")),
        KeyBinding::new("c", ToggleCurves, Some("Spectro")),
        KeyBinding::new("ctrl-z", Undo, Some("Spectro")),
        KeyBinding::new("delete", DeleteFlyby, Some("Spectro")),
        KeyBinding::new("backspace", RemoveTracePoint, Some("Spectro")),
        KeyBinding::new("e", ToggleEdit, Some("Spectro")),
        KeyBinding::new("enter", FitTrace, Some("Spectro")),
        KeyBinding::new("ctrl-shift-d", ToggleDetails, None),
        KeyBinding::new("ctrl-shift-s", ToggleSpectrogram, None),
        KeyBinding::new("ctrl-b", ToggleSidebar, None),
        KeyBinding::new("space", TogglePlay, Some("Spectro")),
        KeyBinding::new("ctrl-shift-a", ToggleAnalysis, None),
        // a text field inside the spectrogram's column (the boost band) types these keys
        KeyBinding::new("0", gpui::NoAction, Some("TextInput")),
        KeyBinding::new("f", gpui::NoAction, Some("TextInput")),
        KeyBinding::new("w", gpui::NoAction, Some("TextInput")),
        KeyBinding::new("c", gpui::NoAction, Some("TextInput")),
        KeyBinding::new("e", gpui::NoAction, Some("TextInput")),
        KeyBinding::new("j", gpui::NoAction, Some("TextInput")),
        KeyBinding::new("k", gpui::NoAction, Some("TextInput")),
        KeyBinding::new("space", gpui::NoAction, Some("TextInput")),
        KeyBinding::new("ctrl-z", gpui::NoAction, Some("TextInput")),
        KeyBinding::new("up", gpui::NoAction, Some("TextInput")),
        KeyBinding::new("down", gpui::NoAction, Some("TextInput")),
    ]);
}

/// Start-up options (command line).
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub file: Option<PathBuf>,
    pub light: bool,
    pub day: Option<String>,
    pub day_end: Option<String>,
    pub start: Option<String>,
    pub hours: Option<(String, String)>,
    pub utc: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
    pub select: Option<usize>,
    pub locate: bool,
    pub detections_json: Option<PathBuf>,
    pub zoom: Option<(f64, f64)>,
    /// Initial frequency window (Hz).
    pub fzoom: Option<(f64, f64)>,
    pub cursor: Option<f64>,
    /// Debug: invented location results instead of the ADS-B search.
    pub sample_data: bool,
    /// No OpenStreetMap basemap (plain graticule, no network for the map).
    pub no_basemap: bool,
    /// Select this location candidate (1-based) when the search ends.
    pub candidate: Option<usize>,
    /// Initial map framing: the whole search area instead of the candidate.
    pub map_area: bool,
    /// Start with the expert details shown.
    pub details: bool,
    /// Start in "Edit fly-bys" mode (when detection ends).
    pub edit: bool,
    /// Show (or hide) the spectrogram, whatever the saved layout says.
    pub spectrogram: Option<bool>,
    /// Show the spectrogram in this run without changing the saved choice
    /// (--edit, --zoom and --fzoom need it).
    pub spectrogram_once: bool,
    /// Ignore the saved layout and window place.
    pub default_layout: bool,
    /// Panel sizes and the window place to start with.
    pub layout: Layout,
    /// Cuts in the recording (s).
    pub cuts: Vec<f64>,
    /// Open this analysis tab when the search ends.
    pub analysis: Option<String>,
    /// Compare this candidate (1-based) with the selected one.
    pub compare: Option<usize>,
    /// Show the other possible places on the map.
    pub others: bool,
}

/// How the map frames the results until the user pans or zooms it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MapFit {
    /// The selected candidate and its surroundings.
    Candidate,
    /// The whole search area with every candidate.
    Area,
    /// An ambiguous answer: the plausible area (the cells holding most of the probability).
    Plausible,
}

/// Map pan in progress.
struct MapDrag {
    start: Point<Pixels>,
    view: MapView,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Decoding,
    Overview,
    Detecting,
    Done,
    Failed,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Stage::Idle => "Idle",
            Stage::Decoding => "Decoding audio",
            Stage::Overview => "Overview spectrogram",
            Stage::Detecting => "Detecting fly-bys",
            Stage::Done => "Done",
            Stage::Failed => "Failed",
        }
    }
}

struct FileState {
    path: PathBuf,
    name: String,
    size_bytes: u64,
    audio: Option<Arc<Audio>>,
    overview: Option<SpecImage>,
    envelope: Vec<f32>,
}

enum LocState {
    Idle,
    Running(Instant),
    /// Shared, not copied: the map takes it on every frame.
    Done(Arc<LocationResults>),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Tc,
    F0,
    Speed,
    Dist,
    Contrast,
}

struct Drag {
    start: Point<Pixels>,
    view: View,
    moved: bool,
}

/// "Edit fly-bys" mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    /// Select, move, reshape and delete curves.
    Move,
    /// Click points along a faint line.
    Trace,
}

struct EditState {
    tool: Tool,
    /// Points traced so far `[t, f]`.
    pending: Vec<[f64; 2]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handle {
    /// Move in time and frequency.
    Body,
    /// Closest distance: steeper or gentler.
    Steep,
    /// Speed: wider or narrower frequency swing.
    Spread,
}

struct CurveDrag {
    idx: usize,
    handle: Handle,
    start: Point<Pixels>,
    orig: Flyby,
    /// Frequency of the spread handle when the drag started.
    spread_f: f64,
    moved: bool,
}

/// Detections, the deleted ones and the cuts, for undo.
type Snapshot = (Vec<Detection>, Vec<Detection>, Vec<f64>);
const UNDO_STEPS: usize = 30;
/// Most a curve body drag changes the rest frequency per pixel (fraction).
const MAX_REL_PER_PX: f64 = 0.003;

struct Form {
    day: Entity<TextInput>,
    day_end: Entity<TextInput>,
    start: Entity<TextInput>,
    hour_from: Entity<TextInput>,
    hour_to: Entity<TextInput>,
    utc: Entity<TextInput>,
    city: Entity<TextInput>,
    state: Entity<TextInput>,
    country: Entity<TextInput>,
    time_mode: TimeMode,
    show_range: bool,
}

impl Form {
    fn order(&self) -> Vec<Entity<TextInput>> {
        let mut v = vec![self.day.clone()];
        if self.show_range {
            v.push(self.day_end.clone());
        }
        match self.time_mode {
            TimeMode::Exact => v.push(self.start.clone()),
            TimeMode::Approx => {
                v.push(self.hour_from.clone());
                v.push(self.hour_to.clone());
            }
            TimeMode::Unknown => {}
        }
        if self.time_mode != TimeMode::Unknown {
            v.push(self.utc.clone());
        }
        v.extend([self.city.clone(), self.state.clone(), self.country.clone()]);
        v
    }
}

pub struct Workspace {
    focus: FocusHandle,
    nav_focus: FocusHandle,
    file: Option<FileState>,
    stage: Stage,
    started: Option<Instant>,
    stage_started: Option<Instant>,
    stage_times: Vec<(Stage, f64)>,
    /// The stage that was running when the analysis failed.
    failed_stage: Option<Stage>,
    error: Option<String>,
    detections: Vec<Detection>,
    curves: Vec<Curve>,
    stats: Option<DetectStats>,
    /// Measured when nothing was found, to say why (talking, music).
    background: Option<Background>,
    detections_from_file: bool,
    selected: Option<usize>,
    sort: (SortKey, bool),
    view: View,
    view_touched: bool,
    whiten: bool,
    n_fft: usize,
    show_curves: bool,
    tile: Option<SpecImage>,
    tile_task: Option<Task<()>>,
    tile_wanted: Option<SpecRequest>,
    plot: Rc<Cell<Bounds<Pixels>>>,
    overview_rect: Rc<Cell<Bounds<Pixels>>>,
    hover: Option<(f64, f64)>,
    cursor_t: Option<f64>,
    drag: Option<Drag>,
    overview_drag: bool,
    form: Form,
    location: LocState,
    loc_selected: usize,
    locate_queued: bool,
    source: Arc<dyn LocationSource>,
    drop_images: Vec<Arc<RenderImage>>,
    generation: u64,
    job_task: Option<Task<()>>,
    loc_task: Option<Task<()>>,
    loc_ctl: Option<Arc<LocateControl>>,
    map_tiles: Option<Tiles>,
    /// Set once the user pans or zooms the map; `None` = framed by `map_fit`.
    map_view: Option<MapView>,
    map_fit: MapFit,
    map_rect: Rc<Cell<Bounds<Pixels>>>,
    map_drag: Option<MapDrag>,
    ticker: Option<Task<()>>,
    opts: Options,
    last_title: String,
    updater: Entity<crate::update::UpdateUi>,
    /// Expert information shown (rest frequency, flags, FFT controls, numbers);
    /// kept for the whole session.
    details: bool,
    edit: Option<EditState>,
    curve_drag: Option<CurveDrag>,
    deleted: Vec<Detection>,
    undo: Vec<Snapshot>,
    refit_seq: u64,
    refit_task: Option<Task<()>>,
    refitting: Option<usize>,
    /// Last message of the edit mode (text, is an error).
    edit_note: Option<(String, bool)>,
    /// The fly-bys differ from the ones the last search used.
    dets_changed: bool,
    /// The fly-bys (JSON) the last search started with.
    searched_dets: Option<String>,
    /// The no-fly-by card is folded to one line.
    noflyby_folded: bool,
    /// The user unfolded the no-fly-by card (it starts folded on a short spectrogram).
    noflyby_unfolded: bool,
    /// Last spectrogram view written to the log (for scripted UI tests).
    logged_view: Rc<Cell<Option<View>>>,
    /// Panel sizes, folded panels and the window place (kept between runs).
    layout: Layout,
    /// A pending write of `layout` (a drag writes once it settles).
    layout_save: Option<Task<()>>,
    /// The panels row between the title bar and the status bar.
    body_rect: Rc<Cell<Bounds<Pixels>>>,
    /// How much the window is larger than the panels row (title bar, status
    /// bar, frame), measured with `body_rect`, so a resized window sizes its
    /// panels in the same frame.
    body_margin: Rc<Cell<Option<(f32, f32)>>>,
    /// The centre column (spectrogram or map, and the table).
    center_rect: Rc<Cell<Bounds<Pixels>>>,
    /// The right column (location, results, fly-by).
    right_rect: Rc<Cell<Bounds<Pixels>>>,
    /// The spectrogram was opened by the app (no fly-by found: tracing needs it), not by the user.
    spectro_auto: bool,
    /// The spectrogram is shown only for the analysis (the compared curves);
    /// closing the analysis hides it again.
    spectro_for_analysis: bool,
    /// Shown for this run by the command line (not saved).
    spectro_once: bool,
    /// The splitter being dragged.
    split_dragging: Option<Split>,
    /// Width of the middle column in this frame (toolbars and the table fit their content to it).
    center_w: f32,
    /// The input form's scroll position (a fade at its bottom shows there is more).
    sidebar_scroll: ScrollHandle,
    /// Other possible places on the map: `None` = automatic (shown when the
    /// answer is uncertain and there are few of them).
    show_others: Option<bool>,
    /// None: open when the map is big enough.
    legend_open: Option<bool>,
    /// When the coordinates were copied (the button says so for a moment).
    copied: Option<Instant>,
    /// The analysis drawer and its tab.
    analysis: Option<Tab>,
    explain: Option<explain_data::Explain>,
    /// The score map on the map, and the rendered one.
    heat_key: Option<HeatKey>,
    heat: Option<Heat>,
    /// Candidate compared with the selected one, and whether the modelled
    /// curves are drawn on the spectrogram.
    compare: Option<usize>,
    show_models: bool,
    /// Scatter axes (session detection indices).
    score_axes: Option<(usize, usize)>,
    /// Listening.
    player: Option<crate::audio::Player>,
    player_state: PlayerState,
    play_task: Option<Task<()>>,
    boost: bool,
    boost_band: (f64, f64),
    boost_db: f64,
    volume_db: f64,
    boost_lo: Entity<TextInput>,
    boost_hi: Entity<TextInput>,
    /// The expanded row of the timeline bar.
    bar_panel: Option<BarPanel>,
    /// Cuts in the recording (s, sorted), the audio's suggestions, and the
    /// cuts the last search used.
    cuts: Vec<f64>,
    cut_suggestions: Vec<overhead_core::cuts::CutSuggestion>,
    cuts_task: Option<Task<()>>,
    searched_cuts: Option<Vec<f64>>,
    timeline_rect: Rc<Cell<Bounds<Pixels>>>,
    tl_hover: Option<f64>,
    tl_drag: Option<TlDrag>,
    /// The cuts before a cut drag began (for its undo step).
    cut_drag_from: Option<Vec<f64>>,
    /// The region typed in the form, looked up for the map before a search.
    region_preview: Option<(String, (f64, f64, f64, f64))>,
    region_task: Option<Task<()>>,
    region_asked: Option<String>,
    /// Where the map pointer went down (a click without a drag selects a place).
    map_press: Option<Point<Pixels>>,
    /// Recent recordings (start screen, Open menu) and a pending write.
    history: history::History,
    history_save: Option<Task<()>>,
    /// The title bar's Open menu (open file, recent recordings) is shown.
    open_menu: bool,
    /// Windows' own caption glyph font, if this system has it.
    caption_font: Option<&'static str>,
    /// The caption button under the pointer (its glyph turns white over the red close).
    caption_hover: Option<WindowControlArea>,
    _subs: Vec<Subscription>,
}

/// Tabs of the analysis drawer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Places,
    Heat,
    Scores,
    Silence,
    Timing,
}

impl Tab {
    fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "places" | "compare" | "candidates" => Tab::Places,
            "heat" | "heatmap" | "maps" => Tab::Heat,
            "scores" | "scatter" => Tab::Scores,
            "silence" => Tab::Silence,
            "timing" | "offsets" => Tab::Timing,
            _ => return None,
        })
    }
}

/// The player: not opened yet, opening, ready, or no sound device.
#[derive(Clone, Debug, PartialEq)]
enum PlayerState {
    None,
    Opening,
    Ready,
    Failed(String),
}

/// The row that opens under the timeline bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarPanel {
    Boost,
    Cuts,
}

/// A drag on the timeline: moving the playing position, or a cut.
#[derive(Clone, Copy, Debug)]
enum TlDrag {
    Seek,
    Cut(usize),
}

/// What a splitter drag carries.
#[derive(Clone, Copy)]
struct DragSplit(Split);

/// The (invisible) view under the pointer while a splitter is dragged.
struct SplitGhost;

impl Render for SplitGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

const OVERVIEW_H: f32 = 56.0;
const ADSB_COL_W: f32 = 132.0;
/// The aircraft column of the simple fly-by table grows up to this width.
const AIRCRAFT_MAX_W: f32 = 300.0;
/// Least height of the map (it is the answer).
const MAP_MIN_H: f32 = 140.0;

impl Workspace {
    pub fn new(opts: Options, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mk = |ph: &str, mono: bool, cx: &mut Context<Self>| {
            let e = cx.new(|cx| TextInput::new(ph, mono, cx));
            cx.subscribe(&e, |this: &mut Self, _, ev: &InputEvent, cx| match ev {
                InputEvent::Changed => {
                    this.refresh_auto_offset(cx);
                    this.schedule_history_save(cx);
                    cx.notify()
                }
                InputEvent::Submit => this.find_location(cx),
            })
            .detach();
            e
        };
        // the boost band (Hz): applied as typed
        let mk_band = |ph: &str, cx: &mut Context<Self>| {
            let e = cx.new(|cx| TextInput::new(ph, true, cx));
            cx.subscribe(&e, |this: &mut Self, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Changed | InputEvent::Submit) {
                    this.band_typed(cx);
                }
            })
            .detach();
            e
        };
        let boost_lo = mk_band("80", cx);
        let boost_hi = mk_band("500", cx);
        boost_lo.update(cx, |i, cx| i.set_text("80", cx));
        boost_hi.update(cx, |i, cx| i.set_text("500", cx));
        let form = Form {
            day: mk("YYYY-MM-DD", true, cx),
            day_end: mk("YYYY-MM-DD (optional)", true, cx),
            start: mk("HH:MM:SS", true, cx),
            hour_from: mk("from 14", true, cx),
            hour_to: mk("to 17", true, cx),
            utc: mk("auto", true, cx),
            city: mk("e.g. Kraków", false, cx),
            state: mk("e.g. małopolskie", false, cx),
            country: mk("e.g. Poland", false, cx),
            time_mode: TimeMode::Unknown,
            show_range: false,
        };

        let mut this = Self {
            focus: cx.focus_handle(),
            nav_focus: cx.focus_handle(),
            file: None,
            stage: Stage::Idle,
            started: None,
            stage_started: None,
            stage_times: vec![],
            failed_stage: None,
            error: None,
            detections: vec![],
            curves: vec![],
            stats: None,
            background: None,
            detections_from_file: false,
            selected: None,
            sort: (SortKey::Tc, true),
            view: View { t0: 0.0, t1: 60.0, f0: 30.0, f1: 1000.0 },
            view_touched: false,
            whiten: true,
            n_fft: 8192,
            show_curves: true,
            tile: None,
            tile_task: None,
            tile_wanted: None,
            plot: Rc::new(Cell::new(Bounds::default())),
            overview_rect: Rc::new(Cell::new(Bounds::default())),
            hover: None,
            cursor_t: None,
            drag: None,
            overview_drag: false,
            form,
            location: LocState::Idle,
            loc_selected: 0,
            locate_queued: false,
            source: if opts.sample_data { Arc::new(SampleSource) } else { Arc::new(AdsbSource::new()) },
            drop_images: vec![],
            generation: 0,
            job_task: None,
            loc_task: None,
            loc_ctl: None,
            map_tiles: (!opts.no_basemap).then(Tiles::new),
            map_view: None,
            map_fit: if opts.map_area { MapFit::Area } else { MapFit::Candidate },
            map_rect: Rc::new(Cell::new(Bounds::default())),
            map_drag: None,
            ticker: None,
            opts: opts.clone(),
            last_title: String::new(),
            updater: cx.new(crate::update::UpdateUi::new),
            details: opts.details,
            edit: None,
            curve_drag: None,
            deleted: vec![],
            undo: vec![],
            refit_seq: 0,
            refit_task: None,
            refitting: None,
            edit_note: None,
            dets_changed: false,
            searched_dets: None,
            noflyby_folded: false,
            noflyby_unfolded: false,
            logged_view: Rc::new(Cell::new(None)),
            layout: opts.layout,
            layout_save: None,
            body_rect: Rc::new(Cell::new(Bounds::default())),
            body_margin: Rc::new(Cell::new(None)),
            center_rect: Rc::new(Cell::new(Bounds::default())),
            right_rect: Rc::new(Cell::new(Bounds::default())),
            spectro_auto: false,
            spectro_for_analysis: false,
            spectro_once: opts.spectrogram_once,
            _subs: vec![],
            split_dragging: None,
            center_w: 800.0,
            sidebar_scroll: ScrollHandle::new(),
            show_others: opts.others.then_some(true),
            legend_open: None,
            copied: None,
            analysis: None,
            explain: None,
            heat_key: None,
            heat: None,
            compare: None,
            show_models: true,
            score_axes: None,
            player: None,
            player_state: PlayerState::None,
            play_task: None,
            boost: false,
            boost_band: (80.0, 500.0),
            boost_db: 18.0,
            volume_db: 0.0,
            boost_lo,
            boost_hi,
            bar_panel: None,
            cuts: {
                let mut c = opts.cuts.clone();
                c.sort_by(f64::total_cmp);
                c
            },
            cut_suggestions: vec![],
            cuts_task: None,
            searched_cuts: None,
            timeline_rect: Rc::new(Cell::new(Bounds::default())),
            tl_hover: None,
            tl_drag: None,
            cut_drag_from: None,
            region_preview: None,
            region_task: None,
            region_asked: None,
            map_press: None,
            history: history::load(),
            history_save: None,
            open_menu: false,
            caption_font: {
                // Windows 11 has Segoe Fluent Icons, Windows 10 Segoe MDL2 Assets
                let names = cx.text_system().all_font_names();
                ["Segoe Fluent Icons", "Segoe MDL2 Assets"].into_iter().find(|f| names.iter().any(|n| n == f))
            },
            caption_hover: None,
        };
        // keep the window place: written when it settles, and at close / quit
        this._subs.push(cx.observe_window_bounds(window, |this, window, cx| {
            this.layout.window = Some(layout::WindowState::from_bounds(window.window_bounds()));
            this.schedule_layout_save(cx);
        }));
        this._subs.push(cx.on_app_quit(|this, cx| {
            layout::save(&this.layout);
            this.flush_history(cx);
            async {}
        }));
        let ws = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let bounds = window.window_bounds();
            ws.update(cx, |this, cx| {
                this.layout.window = Some(layout::WindowState::from_bounds(bounds));
                layout::save(&this.layout);
                this.flush_history(cx);
            })
            .ok();
            true
        });
        this.refresh_missing(cx);
        this.apply_form_options(&opts, cx);
        this.refresh_auto_offset(cx);
        if let Some(path) = opts.file.clone() {
            this.open_path(path, cx);
        }
        if opts.locate {
            this.find_location(cx);
        }
        window.focus(&this.nav_focus);
        this
    }

    fn apply_form_options(&mut self, o: &Options, cx: &mut Context<Self>) {
        let set = |e: &Entity<TextInput>, v: &Option<String>, cx: &mut Context<Self>| {
            if let Some(v) = v {
                e.update(cx, |i, cx| i.set_text(v, cx));
            }
        };
        set(&self.form.day, &o.day, cx);
        if o.day_end.is_some() {
            self.form.show_range = true;
            set(&self.form.day_end, &o.day_end, cx);
        }
        if o.start.is_some() {
            self.form.time_mode = TimeMode::Exact;
            set(&self.form.start, &o.start, cx);
        } else if let Some((a, b)) = &o.hours {
            self.form.time_mode = TimeMode::Approx;
            set(&self.form.hour_from, &Some(a.clone()), cx);
            set(&self.form.hour_to, &Some(b.clone()), cx);
        }
        set(&self.form.utc, &o.utc, cx);
        set(&self.form.city, &o.city, cx);
        set(&self.form.state, &o.state, cx);
        set(&self.form.country, &o.country, cx);
    }

    // ---------------------------------------------------------------- jobs

    fn duration(&self) -> f64 {
        self.file.as_ref().and_then(|f| f.audio.as_ref()).map_or(0.0, |a| a.duration())
    }

    fn set_stage(&mut self, s: Stage) {
        if let Some(t0) = self.stage_started {
            let dt = t0.elapsed().as_secs_f64();
            if s == Stage::Failed {
                log::warn!("{} failed after {:.1} s", self.stage.label(), dt);
                self.failed_stage = Some(self.stage);
            } else {
                log::info!("{} took {:.1} s", self.stage.label(), dt);
                self.stage_times.push((self.stage, dt));
            }
        }
        self.stage = s;
        self.stage_started = Some(Instant::now());
    }

    fn busy(&self) -> bool {
        matches!(self.stage, Stage::Decoding | Stage::Overview | Stage::Detecting)
            || matches!(self.location, LocState::Running(_))
            || self.map_tiles.as_ref().is_some_and(|t| t.loading())
    }

    fn start_ticker(&mut self, cx: &mut Context<Self>) {
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(120)).await;
                let keep = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        let busy = this.busy();
                        if !busy {
                            this.ticker = None;
                        }
                        busy
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    fn open_dialog(&mut self, cx: &mut Context<Self>) {
        self.open_menu = false;
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open recording".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| this.open_path(p, cx)).ok();
            }
        })
        .detach();
    }

    pub fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // what was typed for the recording open until now
        if self.history_save.is_some() {
            self.remember_form(cx);
        }
        self.generation += 1;
        let generation = self.generation;
        let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if let Some(f) = self.file.take() {
            if let Some(ov) = f.overview {
                self.drop_images.push(ov.image);
            }
        }
        if let Some(t) = self.tile.take() {
            self.drop_images.push(t.image);
        }
        // a recording opened before: the day, time and region typed for it
        // (the command line's own values win for the file it gives)
        let key = history::key(&path);
        let from_cli = self.generation == 1 && self.opts.file.is_some();
        let saved = self.history.get(&key).map(|e| e.form.clone()).filter(|f| *f != history::FormValues::default() && !from_cli);
        self.history.opened(&key, &name, now_unix());
        history::save(&self.history);
        self.open_menu = false;
        self.file = Some(FileState { path: path.clone(), name, size_bytes, audio: None, overview: None, envelope: vec![] });
        if let Some(form) = saved {
            self.set_form_values(&form, cx);
        }
        self.detections.clear();
        self.curves.clear();
        self.stats = None;
        self.background = None;
        self.selected = None;
        self.edit = None;
        self.spectro_auto = false;
        self.curve_drag = None;
        self.deleted.clear();
        self.undo.clear();
        self.refit_task = None;
        self.refitting = None;
        self.edit_note = None;
        self.dets_changed = false;
        self.searched_dets = None;
        self.error = None;
        self.cursor_t = None;
        self.hover = None;
        self.view_touched = false;
        self.location = LocState::Idle;
        self.locate_queued = false;
        self.loc_task = None;
        self.loc_selected = 0;
        self.map_view = None;
        self.reset_analysis();
        // a new recording: its own cuts (the command line's apply to the first one only)
        if self.generation > 1 {
            self.cuts.clear();
        }
        self.cut_suggestions.clear();
        self.cuts_task = None;
        self.searched_cuts = None;
        self.tl_drag = None;
        self.player = None;
        self.player_state = PlayerState::None;
        self.play_task = None;
        if let Some(c) = self.loc_ctl.take() {
            c.cancel();
        }
        // A tile still rendering belongs to the old file; its result is dropped
        // by the generation check in ensure_tile.
        self.tile_task = None;
        self.tile_wanted = None;
        self.failed_stage = None;
        self.stage_times.clear();
        self.stage_started = None;
        self.started = Some(Instant::now());
        self.set_stage(Stage::Decoding);
        self.stage_times.clear();
        // Start-up options apply to the file given on the command line only.
        let json = self.opts.detections_json.take();
        let zoom = self.opts.zoom.take();
        let fzoom = self.opts.fzoom.take();
        self.detections_from_file = json.is_some();

        self.job_task = Some(cx.spawn(async move |this, cx| {
            let p = path.clone();
            let loaded = cx.background_executor().spawn(async move { analysis::load(&p) }).await;
            let audio = match loaded {
                Ok(a) => Arc::new(a),
                Err(e) => {
                    this.update(cx, |this, cx| {
                        if this.generation == generation {
                            this.history.remove(&history::key(&path));
                            history::save(&this.history);
                            this.error = Some(format!("{e:#}"));
                            this.set_stage(Stage::Failed);
                            this.locate_queued = false;
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };
            let go = this
                .update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    this.set_stage(Stage::Overview);
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !go {
                return;
            }
            let a2 = audio.clone();
            let (overview, envelope) = cx
                .background_executor()
                .spawn(async move {
                    let dur = a2.duration();
                    let req = SpecRequest {
                        t_start: 0.0,
                        t_end: dur,
                        f_min: 0.0,
                        f_max: 1000.0,
                        columns: ((dur / 0.25) as usize).clamp(256, 4096),
                        n_fft: 4096,
                        whiten: true,
                    };
                    (analysis::render_spec(&a2, req), analysis::envelope(&a2, 4096))
                })
                .await;
            let go = this
                .update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    let dur = audio.duration();
                    if let Some(f) = this.file.as_mut() {
                        f.audio = Some(audio.clone());
                        f.overview = Some(overview);
                        f.envelope = envelope;
                    }
                    this.view = View { t0: 0.0, t1: dur, f0: 30.0, f1: 1000.0 };
                    if let Some((a, b)) = zoom
                        && b.min(dur) > a.max(0.0)
                    {
                        this.view.t0 = a.max(0.0);
                        this.view.t1 = b.min(dur);
                        this.view_touched = true;
                    }
                    if let Some((a, b)) = fzoom
                        && b > a
                    {
                        this.view.f0 = a.max(0.0);
                        this.view.f1 = b.min(4000.0);
                        this.view_touched = true;
                    }
                    this.set_stage(Stage::Detecting);
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !go {
                return;
            }
            let a3 = audio.clone();
            let result: anyhow::Result<(Vec<Detection>, Option<DetectStats>, Option<Background>)> = cx
                .background_executor()
                .spawn(async move {
                    if let Some(j) = json {
                        let text = std::fs::read_to_string(&j)?;
                        let (all, _cuts) = parse_detections(&text)?;
                        let n = all.len();
                        let dur = a3.duration();
                        let kept: Vec<Detection> =
                            all.into_iter().filter(|d| d.flyby.tc >= 0.0 && d.flyby.tc <= dur + (0.25 * dur).min(30.0)).collect();
                        if kept.len() < n {
                            log::warn!("{} of {n} detections in {} lie outside this recording", n - kept.len(), j.display());
                            if kept.is_empty() {
                                anyhow::bail!(
                                    "None of the {n} detections in {} fall inside this {} recording",
                                    j.display(),
                                    fmt_dur(dur)
                                );
                            }
                        }
                        Ok((kept, None, None))
                    } else {
                        let (d, s) = analysis::run_detection(&a3);
                        let b = d.is_empty().then(|| background::measure(&a3));
                        Ok((d, Some(s), b))
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let ok = result.is_ok();
                match result {
                    Ok((dets, stats, bg)) => {
                        this.set_detections(dets, stats);
                        this.background = bg;
                        this.start_cut_suggestions(cx);
                        let (n, dur) = (this.detections.len(), this.duration());
                        if let Some(e) = this.history_entry() {
                            e.flybys = Some(n);
                            e.duration_s = Some(dur);
                            history::save(&this.history);
                        }
                    }
                    Err(e) => {
                        this.error = Some(format!("{e:#}"));
                        this.set_stage(Stage::Failed);
                    }
                }
                cx.notify();
                let queued = std::mem::take(&mut this.locate_queued);
                if queued && ok {
                    this.find_location(cx);
                }
            })
            .ok();
        }));
        self.start_ticker(cx);
        cx.notify();
    }

    fn set_detections(&mut self, mut dets: Vec<Detection>, stats: Option<DetectStats>) {
        dets.sort_by(|a, b| a.flyby.tc.total_cmp(&b.flyby.tc));
        self.curves = dets.iter().map(Curve::from_detection).collect();
        self.detections = dets;
        self.stats = stats;
        self.set_stage(Stage::Done);
        if !self.view_touched {
            let top = self
                .curves
                .iter()
                .map(|c| c.flyby.f0 * c.ratios.iter().copied().fold(1.0, f64::max) * c.flyby.f_approach(DEFAULT_C) / c.flyby.f0)
                .fold(0.0, f64::max);
            self.view.f1 = (top * 1.25).clamp(300.0, 1000.0);
        }
        if let Some(n) = self.opts.select.take()
            && n >= 1
            && n <= self.detections.len()
        {
            self.select(Some(n - 1), true);
        }
        if let Some(t) = self.opts.cursor.take() {
            self.cursor_t = Some(t);
        }
        if std::mem::take(&mut self.opts.edit) {
            self.edit = Some(EditState { tool: if self.detections.is_empty() { Tool::Trace } else { Tool::Move }, pending: vec![] });
        }
        // nothing found: what to do (trace by hand) is on the spectrogram
        if self.detections.is_empty() && !self.layout.spectrogram {
            self.spectro_auto = true;
        }
    }

    // ---------------------------------------------------------------- editing

    fn push_undo(&mut self) {
        self.undo.push((self.detections.clone(), self.deleted.clone(), self.cuts.clone()));
        if self.undo.len() > UNDO_STEPS {
            self.undo.remove(0);
        }
    }

    /// Replace the fly-bys (sorted by time); select the one nearest `select_tc`.
    fn apply_dets(&mut self, mut dets: Vec<Detection>, select_tc: Option<f64>) {
        dets.sort_by(|a, b| a.flyby.tc.total_cmp(&b.flyby.tc));
        self.curves = dets.iter().map(Curve::from_detection).collect();
        self.detections = dets;
        self.selected = select_tc.and_then(|tc| {
            (0..self.detections.len()).min_by(|&a, &b| {
                (self.detections[a].flyby.tc - tc).abs().total_cmp(&(self.detections[b].flyby.tc - tc).abs())
            })
        });
        self.refresh_changed();
        // a refit in flight refers to the old indices
        self.refit_seq += 1;
        self.refit_task = None;
        self.refitting = None;
    }

    /// Compare the fly-bys with the ones the last search used (an undo back
    /// to them is not a change; a refit landing after Search again is).
    fn refresh_changed(&mut self) {
        self.dets_changed = match &self.searched_dets {
            Some(s) => serde_json::to_string(&self.detections).map_or(true, |now| &now != s),
            None => true,
        };
    }

    /// The cuts differ from the ones the last search used.
    fn cuts_changed(&self) -> bool {
        self.searched_cuts.as_ref().is_some_and(|c| *c != self.cuts)
    }

    /// The fly-bys or the cuts differ from the last search's.
    fn inputs_changed(&self) -> bool {
        self.dets_changed || self.cuts_changed()
    }

    fn do_undo(&mut self) {
        let Some((dets, deleted, cuts)) = self.undo.pop() else {
            self.edit_note = Some(("Nothing to undo.".into(), false));
            return;
        };
        let tc = self.selected.map(|i| self.detections[i].flyby.tc);
        self.deleted = deleted;
        self.cuts = cuts;
        self.apply_dets(dets, tc);
        self.edit_note = Some(("Undone.".into(), false));
    }

    fn delete_selected(&mut self) {
        let Some(i) = self.selected else {
            self.edit_note = Some(("Select a curve first (click it).".into(), false));
            return;
        };
        self.push_undo();
        let mut dets = self.detections.clone();
        let d = dets.remove(i);
        self.edit_note = Some((format!("Deleted the fly-by at {}. Undo or Restore brings it back.", crate::draw::fmt_time(d.flyby.tc, 1.0)), false));
        self.deleted.push(d);
        self.apply_dets(dets, None);
    }

    fn restore_deleted(&mut self, k: usize) {
        if k >= self.deleted.len() {
            return;
        }
        self.push_undo();
        let d = self.deleted.remove(k);
        let tc = d.flyby.tc;
        let mut dets = self.detections.clone();
        dets.push(d);
        self.apply_dets(dets, Some(tc));
    }

    fn fit_pending(&mut self) {
        let Some(e) = self.edit.as_mut() else { return };
        match core_edit::trace_detection(&e.pending, DEFAULT_C) {
            Ok(d) => {
                e.pending.clear();
                let tc = d.flyby.tc;
                let note = if d.confident {
                    format!("Traced curve added: {:.0} m/s, closest {}.", d.flyby.v, fmt_dist(d.flyby.d))
                } else {
                    format!(
                        "Traced curve added ({:.0} m/s, closest {}), but loosely measured: add points on both sides of the steep part.",
                        d.flyby.v,
                        fmt_dist(d.flyby.d)
                    )
                };
                self.push_undo();
                let mut dets = self.detections.clone();
                dets.push(d);
                self.apply_dets(dets, Some(tc));
                self.edit_note = Some((note, false));
            }
            Err(msg) => self.edit_note = Some((msg, true)),
        }
    }

    fn enter_edit(&mut self, tool: Tool) {
        match self.edit.as_mut() {
            Some(e) => e.tool = tool,
            None => self.edit = Some(EditState { tool, pending: vec![] }),
        }
        self.edit_note = None;
        self.show_curves = true;
        // curves are edited on the spectrogram
        if !self.layout.spectrogram {
            self.spectro_auto = true;
        }
    }

    /// Snap a curve the user placed to the line near it, in the background.
    fn start_refit(&mut self, idx: usize, guess: Flyby, cx: &mut Context<Self>) {
        let Some(audio) = self.file.as_ref().and_then(|f| f.audio.clone()) else { return };
        let harmonics = self.detections[idx].harmonics.clone();
        self.refit_seq += 1;
        let seq = self.refit_seq;
        self.refitting = Some(idx);
        self.refit_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { core_edit::refit_near(&audio, guess, &harmonics, DEFAULT_C) })
                .await;
            this.update(cx, |this, cx| {
                if this.refit_seq != seq {
                    return;
                }
                this.refitting = None;
                this.refit_task = None;
                match res {
                    Some(d) if idx < this.detections.len() => {
                        let moved = (d.flyby.tc - guess.tc).abs() > 0.05 || (d.flyby.f0 / guess.f0 - 1.0).abs() > 1e-4;
                        this.edit_note = Some((
                            if moved {
                                format!("Snapped to the line: {:.0} m/s, closest {}.", d.flyby.v, fmt_dist(d.flyby.d))
                            } else {
                                "No brighter line nearby: kept where you put it.".to_string()
                            },
                            false,
                        ));
                        // drawn over the same time span as before the refit, so
                        // a steepness edit does not look like a different shape
                        let half = this.curves.get(idx).map(|c| 0.5 * (c.t_span.1 - c.t_span.0));
                        let mut curve = Curve::from_detection(&d);
                        if let Some(h) = half {
                            curve.t_span = (d.flyby.tc - h, d.flyby.tc + h);
                        }
                        this.curves[idx] = curve;
                        this.detections[idx] = d;
                        this.refresh_changed();
                    }
                    _ => this.edit_note = Some(("Could not refit here: kept where you put it.".into(), true)),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn save_dets(&mut self, cx: &mut Context<Self>) {
        let dir = self.file.as_ref().and_then(|f| f.path.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| PathBuf::from("."));
        let rx = cx.prompt_for_new_path(&dir, Some("flybys.json"));
        // with cuts: {"detections": [...], "cuts": [...]} (overhead-adsb locate reads both forms)
        let json = if self.cuts.is_empty() {
            serde_json::to_string_pretty(&self.detections).unwrap_or_default()
        } else {
            serde_json::to_string_pretty(&serde_json::json!({ "detections": self.detections, "cuts": self.cuts })).unwrap_or_default()
        };
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                let r = std::fs::write(&path, json);
                this.update(cx, |this, cx| {
                    this.edit_note = Some(match r {
                        Ok(()) => (format!("Saved to {} (overhead-adsb locate --detections takes it).", path.display()), false),
                        Err(e) => (format!("Could not save: {e}"), true),
                    });
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn load_dets(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Load fly-bys (JSON)".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.into_iter().next()
            {
                let r: anyhow::Result<(Vec<Detection>, Option<Vec<f64>>)> =
                    std::fs::read_to_string(&p).map_err(anyhow::Error::from).and_then(|b| parse_detections(&b));
                this.update(cx, |this, cx| {
                    match r {
                        Ok((dets, cuts)) => {
                            this.push_undo();
                            let n = dets.len();
                            this.apply_dets(dets, None);
                            let mut note = format!("Loaded {n} fly-bys from {}.", p.display());
                            if let Some(mut c) = cuts {
                                c.sort_by(f64::total_cmp);
                                note = format!("Loaded {n} fly-bys and {} cuts from {}.", c.len(), p.display());
                                this.cuts = c;
                            }
                            this.edit_note = Some((note, false));
                        }
                        Err(e) => this.edit_note = Some((format!("Could not load {}: {e:#}", p.display()), true)),
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn query(&self, cx: &App) -> Result<SearchQuery, String> {
        let f = &self.form;
        let (day, day_end) = query::validate_days(
            f.day.read(cx).text(),
            if f.show_range { f.day_end.read(cx).text() } else { "" },
        )?;
        let (time, utc, guessed) = self.time_hint(cx)?;
        let typed = f.utc.read(cx).text().trim().to_string();
        Ok(SearchQuery {
            day,
            day_end,
            time,
            utc_offset_min: utc,
            utc_offset_guessed: guessed,
            tz: (!guessed && !typed.is_empty()).then_some(typed),
            region: self.region(cx),
            duration_s: self.duration(),
        })
    }

    /// Time of day and UTC offset, validated on their own (the day may still be
    /// missing). The bool is true when the offset is the automatic guess.
    fn time_hint(&self, cx: &App) -> Result<(TimeHint, i32, bool), String> {
        let f = &self.form;
        let time = self.time_only(cx)?;
        let typed = f.utc.read(cx).text().trim();
        if typed.is_empty() {
            return Ok((time, self.auto_offset(time, cx), true));
        }
        if let Some(utc) = query::parse_offset(typed) {
            return Ok((time, utc, false));
        }
        // an IANA zone ("Europe/Warsaw"): the matcher applies it with DST
        if typed.contains('/') && adsb::valid_tz(typed) {
            return Ok((time, self.auto_offset(time, cx), false));
        }
        Err("UTC offset or zone: e.g. +02:00 or Europe/Warsaw, or leave empty for auto".into())
    }

    /// This computer's UTC offset on the recording day at the given time (or
    /// noon), e.g. +02:00 for a June day in Poland even when it is winter now.
    fn auto_offset(&self, time: TimeHint, cx: &App) -> i32 {
        let f = &self.form;
        let clock = match time {
            TimeHint::ExactStart(s) => s,
            TimeHint::Window { from, .. } => from,
            TimeHint::Unknown => 12 * 3600,
        };
        match query::validate_days(f.day.read(cx).text(), "") {
            Ok((d, _)) => query::local_offset_on(d, clock),
            Err(_) => query::local_offset_now(),
        }
    }

    /// Show the automatic offset as the UTC field's placeholder.
    fn refresh_auto_offset(&mut self, cx: &mut Context<Self>) {
        let ph = if self.opts.sample_data {
            let time = self.time_only(cx).unwrap_or(TimeHint::Unknown);
            format!("auto {}", query::format_offset(self.auto_offset(time, cx)).trim_start_matches("UTC"))
        } else {
            // the ADS-B search takes the zone of the region on that day
            "UTC auto".to_string()
        };
        self.form.utc.update(cx, |i, cx| i.set_placeholder(&ph, cx));
        self.refresh_region_preview(cx);
    }

    /// Look the form's region up (in the background, once it stops
    /// changing) so that the map shows it before a search.
    fn refresh_region_preview(&mut self, cx: &mut Context<Self>) {
        if self.opts.sample_data {
            return;
        }
        let spec = adsb::region_text(&self.region(cx));
        if spec == self.region_asked {
            return;
        }
        self.region_asked = spec.clone();
        let Some(spec) = spec else {
            self.region_preview = None;
            self.region_task = None;
            return;
        };
        self.region_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(900)).await;
            let still = this.update(cx, |this, _| this.region_asked.as_deref() == Some(spec.as_str())).unwrap_or(false);
            if !still {
                return;
            }
            let s2 = spec.clone();
            let r = cx.background_executor().spawn(async move { adsb::region_box(&s2) }).await;
            this.update(cx, |this, cx| {
                if this.region_asked.as_deref() != Some(spec.as_str()) {
                    return;
                }
                match r {
                    Ok(b) => {
                        log::info!("region {spec}: {:.3},{:.3} .. {:.3},{:.3}", b.0, b.1, b.2, b.3);
                        this.region_preview = Some((spec, b));
                    }
                    Err(e) => {
                        log::info!("region {spec} not found for the map: {e:#}");
                        this.region_preview = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn time_only(&self, cx: &App) -> Result<TimeHint, String> {
        let f = &self.form;
        let time = match f.time_mode {
            TimeMode::Unknown => TimeHint::Unknown,
            TimeMode::Exact => match f.start.read(cx).text().trim() {
                "" => TimeHint::Unknown,
                s => TimeHint::ExactStart(query::parse_clock(s).ok_or("Start time: use HH:MM or HH:MM:SS")?),
            },
            TimeMode::Approx => {
                let a = f.hour_from.read(cx).text().trim().to_string();
                let b = f.hour_to.read(cx).text().trim().to_string();
                if a.is_empty() && b.is_empty() {
                    TimeHint::Unknown
                } else {
                    let from = query::parse_clock(&a).ok_or("Hours: use e.g. 14 and 17")?;
                    let to = query::parse_clock(&b).ok_or("Hours: use e.g. 14 and 17")?;
                    TimeHint::Window { from, to }
                }
            }
        };
        Ok(time)
    }

    fn region(&self, cx: &App) -> RegionHint {
        RegionHint {
            city: self.form.city.read(cx).text().to_string(),
            state: self.form.state.read(cx).text().to_string(),
            country: self.form.country.read(cx).text().to_string(),
        }
    }

    fn find_location(&mut self, cx: &mut Context<Self>) {
        let Ok(q) = self.query(cx) else {
            cx.notify();
            return;
        };
        if self.file.is_none() || self.stage == Stage::Failed {
            return;
        }
        if self.stage == Stage::Done && self.detections.is_empty() {
            return;
        }
        if self.stage != Stage::Done {
            log::info!("Location search queued until detection ends");
            self.locate_queued = true;
            cx.notify();
            return;
        }
        let source = self.source.clone();
        if let Some(old) = self.loc_ctl.take() {
            old.cancel();
        }
        let ctl = Arc::new(LocateControl::default());
        self.loc_ctl = Some(ctl.clone());
        let job = LocateJob {
            query: q,
            detections: self.detections.clone(),
            audio: self.file.as_ref().and_then(|f| f.audio.clone()),
            cuts: self.cuts.clone(),
            ctl,
        };
        self.location = LocState::Running(Instant::now());
        self.remember_form(cx);
        let n = self.detections.len();
        if let Some(e) = self.history_entry() {
            e.flybys = Some(n);
        }
        self.dets_changed = false;
        self.searched_dets = serde_json::to_string(&self.detections).ok();
        self.searched_cuts = Some(self.cuts.clone());
        self.reset_analysis();
        self.loc_selected = 0;
        self.map_view = None;
        let generation = self.generation;
        let started = Instant::now();
        let my_ctl = job.ctl.clone();
        // A plain thread, not the GPUI executor: a search can take minutes
        // (archive download, matching) and must not hold an executor thread.
        let slot: Arc<std::sync::Mutex<Option<anyhow::Result<LocationResults>>>> = Arc::default();
        let slot2 = slot.clone();
        std::thread::Builder::new()
            .name("locate".into())
            .spawn(move || {
                let r = source.locate(&job);
                *slot2.lock().unwrap() = Some(r);
            })
            .expect("spawn locate thread");
        log::info!("Location search started ({})", self.source.name());
        self.loc_task = Some(cx.spawn(async move |this, cx| {
            let r = loop {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                if let Some(r) = slot.lock().unwrap().take() {
                    break r;
                }
            };
            this.update(cx, |this, cx| {
                // superseded (new file, new search or cancelled): drop the result
                if this.generation != generation || !this.loc_ctl.as_ref().is_some_and(|c| Arc::ptr_eq(c, &my_ctl)) {
                    return;
                }
                log::info!("Location search ({}) took {:.1} s", this.source.name(), started.elapsed().as_secs_f64());
                this.loc_ctl = None;
                this.location = match r {
                    Ok(r) => {
                        if let Some(c) = r.candidates.first() {
                            log::info!(
                                "Best candidate {:.5}, {:.5} ±{:.0} m, {} matched, {} candidates",
                                c.lat,
                                c.lon,
                                c.radius_m,
                                c.matched,
                                r.candidates.len()
                            );
                        } else {
                            log::info!("No location candidates");
                        }
                        if let Some(k) = this.opts.candidate.take()
                            && k >= 1
                            && k <= r.candidates.len()
                        {
                            this.loc_selected = k - 1;
                        }
                        // an ambiguous answer is framed by its plausible area, not by #1
                        match (r.ambiguous().is_some(), this.map_fit) {
                            (true, MapFit::Candidate) => this.map_fit = MapFit::Plausible,
                            (false, MapFit::Plausible) => this.map_fit = MapFit::Candidate,
                            _ => {}
                        }
                        this.explain = r.session.clone().map(|s| explain_data::Explain::new(s, r.det_index.clone()));
                        // --compare N is "Place N" as the analysis lists them
                        if let Some(k) = this.opts.compare.take()
                            && k >= 1
                            && let Some(&c) = r.places().get(k - 1)
                        {
                            this.compare = Some(c);
                        }
                        if let Some(tab) = this.opts.analysis.take().as_deref().and_then(Tab::parse) {
                            this.analysis = Some(tab);
                            if tab == Tab::Heat {
                                this.heat_key = Some(HeatKey { scope: Scope::Search, layer: Layer::Combined });
                                this.map_fit = MapFit::Area;
                                this.map_view = None;
                            }
                        }
                        if !r.is_sample
                            && let Some(e) = this.history_entry()
                        {
                            e.searched = true;
                            e.result = r.places().first().map(|&k| {
                                let c = &r.candidates[k];
                                history::Found {
                                    lat: c.lat,
                                    lon: c.lon,
                                    radius_m: c.radius_m,
                                    confident: r.ambiguous().is_none(),
                                    places: r.ambiguous().map_or(1, |a| a.similar_places.max(2)),
                                }
                            });
                            history::save(&this.history);
                        }
                        LocState::Done(Arc::new(r))
                    }
                    Err(e) => {
                        log::warn!("Location search failed: {e:#}");
                        LocState::Failed(format!("{e:#}"))
                    }
                };
                this.request_analysis(cx);
                cx.notify();
            })
            .ok();
        }));
        self.start_ticker(cx);
        cx.notify();
    }

    /// Stop the running search. Stages before the matcher stop at once; a
    /// search already matching finishes in the background and is dropped.
    fn cancel_location(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.loc_ctl.take() {
            c.cancel();
            log::info!("Location search cancelled");
        }
        self.loc_task = None;
        self.location = LocState::Failed("Cancelled.".into());
        cx.notify();
    }

    // ---------------------------------------------------------------- view

    fn select(&mut self, i: Option<usize>, zoom: bool) {
        self.selected = i;
        if let Some(i) = i {
            self.cursor_t = Some(self.detections[i].flyby.tc);
            if zoom {
                self.zoom_to(i);
            }
        }
    }

    fn zoom_to(&mut self, i: usize) {
        let d = &self.detections[i];
        let c = &self.curves[i];
        let dur = self.duration().max(1.0);
        let half = ((d.region.t_end - d.region.t_start) * 0.75).max(25.0);
        let tc = d.flyby.tc;
        // the lines heard (`f0` itself may not be one of them)
        let rmin = c.ratios.iter().copied().fold(f64::INFINITY, f64::min);
        let rmin = if rmin.is_finite() { rmin } else { 1.0 };
        let rmax = c.ratios.iter().copied().fold(rmin, f64::max);
        let f_hi = c.flyby.f_approach(DEFAULT_C) * rmax;
        let f_lo = c.flyby.f_recede(DEFAULT_C) * rmin;
        let pad = ((f_hi - f_lo) * 0.6).max(25.0);
        self.view = View {
            t0: (tc - half).max(0.0),
            t1: (tc + half).min(dur),
            f0: (f_lo - pad).max(0.0),
            f1: (f_hi + pad).min(4000.0),
        };
        self.view_touched = true;
        self.hover = None;
    }

    fn zoom_all(&mut self) {
        self.hover = None;
        let dur = self.duration();
        if dur > 0.0 {
            self.view.t0 = 0.0;
            self.view.t1 = dur;
            self.view.f0 = 30.0;
            self.view.f1 = if self.curves.is_empty() { 1000.0 } else { self.view.f1.max(300.0) };
            let top = self
                .curves
                .iter()
                .map(|c| c.flyby.f_approach(DEFAULT_C) * c.ratios.iter().copied().fold(1.0, f64::max))
                .fold(0.0, f64::max);
            if top > 0.0 {
                self.view.f1 = (top * 1.25).clamp(300.0, 1000.0);
            }
        }
    }

    fn clamp_view(&mut self) {
        let dur = self.duration().max(0.5);
        let v = &mut self.view;
        let span = (v.t1 - v.t0).clamp(dur.min(4.0), dur);
        if v.t0 < 0.0 {
            v.t0 = 0.0;
        }
        v.t1 = v.t0 + span;
        if v.t1 > dur {
            v.t1 = dur;
            v.t0 = (dur - span).max(0.0);
        }
        let fspan = (v.f1 - v.f0).clamp(15.0, 3990.0);
        if v.f0 < 0.0 {
            v.f0 = 0.0;
        }
        v.f1 = v.f0 + fspan;
        if v.f1 > 4000.0 {
            v.f1 = 4000.0;
            v.f0 = 4000.0 - fspan;
        }
    }

    fn ensure_tile(&mut self, scale: f32, cx: &mut Context<Self>) {
        let Some(audio) = self.file.as_ref().and_then(|f| f.audio.clone()) else { return };
        let plot = self.plot.get();
        if plot.size.width < px(16.) || plot.size.height < px(16.) {
            return;
        }
        let req = SpecRequest {
            t_start: self.view.t0,
            t_end: self.view.t1,
            f_min: self.view.f0.max(0.0),
            f_max: self.view.f1,
            columns: ((f32::from(plot.size.width) * scale).round() as usize).clamp(64, 4096),
            n_fft: self.n_fft,
            whiten: self.whiten,
        };
        if self.tile.as_ref().map(|t| t.req) == Some(req) {
            self.tile_wanted = None;
            return;
        }
        self.tile_wanted = Some(req);
        if self.tile_task.is_some() {
            return;
        }
        let generation = self.generation;
        self.tile_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(30)).await;
            let Ok(Some(req)) = this.update(cx, |this, _| this.tile_wanted) else {
                this.update(cx, |this, _| this.tile_task = None).ok();
                return;
            };
            let img = cx.background_executor().spawn(async move { analysis::render_spec(&audio, req) }).await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    this.drop_images.push(img.image);
                    return;
                }
                if let Some(old) = this.tile.replace(img) {
                    this.drop_images.push(old.image);
                }
                this.tile_task = None;
                cx.notify();
            })
            .ok();
        }));
    }

    // ---------------------------------------------------------------- map

    /// The map view: the user's pan/zoom, else framed by `map_fit`.
    fn map_view_for(&self, w: f64, h: f64) -> MapView {
        if let Some(v) = self.map_view {
            return v;
        }
        let results = match &self.location {
            LocState::Done(r) => Some(r),
            _ => None,
        };
        let Some(r) = results else {
            // before a search: the region typed in the form, else the whole world
            return match &self.region_preview {
                Some((_, (a0, o0, a1, o1))) => {
                    let (pa, po) = ((a1 - a0) * 0.1, (o1 - o0) * 0.1);
                    MapView::fit(a0 - pa, o0 - po, a1 + pa, o1 + po, w, h)
                }
                None => MapView::fit(-55.0, -170.0, 72.0, 170.0, w, h),
            };
        };
        let cand = r.candidates.get(self.loc_selected);
        if self.map_fit == MapFit::Plausible
            && let Some((mut a0, mut o0, mut a1, mut o1)) = r.ambiguous().and_then(|a| a.bounds())
        {
            if let Some(c) = cand {
                (a0, o0, a1, o1) = (a0.min(c.lat), o0.min(c.lon), a1.max(c.lat), o1.max(c.lon));
            }
            let (pa, po) = (((a1 - a0) * 0.08).max(0.01), ((o1 - o0) * 0.08).max(0.015));
            return MapView::fit(a0 - pa, o0 - po, a1 + pa, o1 + po, w, h);
        }
        match (self.map_fit, cand) {
            (MapFit::Candidate, Some(c)) => {
                // ≈4 km around the point, more for a large uncertainty
                let half = (c.radius_m * 4.0).clamp(1800.0, 40_000.0);
                let dlat = half / 111_320.0;
                let dlon = half / (111_320.0 * c.lat.to_radians().cos().max(0.05));
                let (mut a0, mut o0, mut a1, mut o1) = (c.lat - dlat, c.lon - dlon, c.lat + dlat, c.lon + dlon);
                // the other places shown and the compared one stay in view
                let others = if self.others_shown(r) { r.places() } else { vec![] };
                for k in others.into_iter().chain(self.compare) {
                    // (far ones would shrink the result to a dot: those need Search area)
                    if let Some(o) = r.candidates.get(k).filter(|o| (o.lat - c.lat).abs() < 0.3 && ((o.lon - c.lon) * c.lat.to_radians().cos()).abs() < 0.3) {
                        // more room above: the pin and its label stand over the point
                        let (pa, po) = (dlat * 0.25, dlon * 0.25);
                        (a0, o0, a1, o1) = (a0.min(o.lat - pa), o0.min(o.lon - po), a1.max(o.lat + 2.5 * pa), o1.max(o.lon + po));
                    }
                }
                MapView::fit(a0, o0, a1, o1, w, h)
            }
            _ => {
                let mut b = r.search_box;
                for c in &r.candidates {
                    let (a0, o0, a1, o1) = b.unwrap_or((c.lat, c.lon, c.lat, c.lon));
                    b = Some((a0.min(c.lat), o0.min(c.lon), a1.max(c.lat), o1.max(c.lon)));
                }
                match b {
                    Some((a0, o0, a1, o1)) => MapView::fit(a0 - 0.02, o0 - 0.03, a1 + 0.02, o1 + 0.03, w, h),
                    None => MapView::fit(47.0, 9.0, 56.0, 25.0, w, h),
                }
            }
        }
    }

    fn on_map_scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let b = self.map_rect.get();
        let dy = match ev.delta {
            ScrollDelta::Lines(p) => p.y as f64,
            ScrollDelta::Pixels(p) => f32::from(p.y) as f64 / 40.0,
        };
        let v = self.map_view_for(f32::from(b.size.width) as f64, f32::from(b.size.height) as f64);
        // zoom about the mouse position
        let (lat, lon) = v.latlon(&b, ev.position);
        let zoom = (v.zoom + dy * 0.35).clamp(1.0, map::MAX_ZOOM);
        let mut nv = MapView { zoom, ..v };
        let p = nv.xy(&b, lat, lon);
        let s = nv.world_px();
        nv.x += f32::from(p.x - ev.position.x) as f64 / s;
        nv.y += f32::from(p.y - ev.position.y) as f64 / s;
        self.map_view = Some(nv);
        cx.stop_propagation();
        cx.notify();
    }

    fn on_map_down(&mut self, ev: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let b = self.map_rect.get();
        let v = self.map_view_for(f32::from(b.size.width) as f64, f32::from(b.size.height) as f64);
        self.map_drag = Some(MapDrag { start: ev.position, view: v });
        self.map_press = Some(ev.position);
        cx.notify();
    }

    fn on_map_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = &self.map_drag else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.map_drag = None;
            return;
        }
        let delta = ev.position - d.start;
        if f32::from(delta.x).abs() + f32::from(delta.y).abs() < 2.0 {
            return;
        }
        let s = d.view.world_px();
        self.map_view = Some(MapView {
            x: d.view.x - f32::from(delta.x) as f64 / s,
            y: d.view.y - f32::from(delta.y) as f64 / s,
            zoom: d.view.zoom,
        });
        cx.notify();
    }

    fn on_map_up(&mut self, ev: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.map_drag = None;
        // a click, not a drag: select the place under the pointer
        if let Some(p) = self.map_press.take() {
            let d = ev.position - p;
            if f32::from(d.x).abs() + f32::from(d.y).abs() < 3.0 {
                self.map_click(ev.position);
            }
        }
        cx.notify();
    }

    fn on_spec_scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let plot = self.plot.get();
        let (dx, dy) = match ev.delta {
            ScrollDelta::Lines(p) => (p.x as f64, p.y as f64),
            ScrollDelta::Pixels(p) => (f32::from(p.x) as f64 / 40.0, f32::from(p.y) as f64 / 40.0),
        };
        if self.duration() <= 0.0 {
            return;
        }
        let v = self.view;
        if ev.modifiers.shift || dx.abs() > dy.abs() {
            let d = if dx.abs() > dy.abs() { dx } else { dy };
            let shift = -d * 0.08 * (v.t1 - v.t0);
            self.view.t0 += shift;
            self.view.t1 += shift;
        } else if ev.modifiers.control || ev.modifiers.alt {
            let f = v.f_at(&plot, ev.position.y).clamp(v.f0, v.f1);
            let k = (-dy * 0.12).exp();
            self.view.f0 = f - (f - v.f0) * k;
            self.view.f1 = f + (v.f1 - f) * k;
        } else {
            let t = v.t_at(&plot, ev.position.x).clamp(v.t0, v.t1);
            let k = (-dy * 0.15).exp();
            self.view.t0 = t - (t - v.t0) * k;
            self.view.t1 = t + (v.t1 - t) * k;
        }
        self.view_touched = true;
        self.clamp_view();
        cx.notify();
    }

    fn on_spec_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.nav_focus);
        if self.edit.as_ref().is_some_and(|e| e.tool == Tool::Move) && self.duration() > 0.0 {
            let plot = self.plot.get();
            // a handle of the selected curve, else any curve (selects it), else pan
            let mut grab: Option<(usize, Handle)> = None;
            if let Some(i) = self.selected {
                let [centre, steep, spread] = spectro::handles(&self.view, &plot, &self.curves[i]);
                let near = |p: Point<Pixels>| f32::from((p.x - ev.position.x).abs()) + f32::from((p.y - ev.position.y).abs()) < 14.0;
                grab = if near(steep) {
                    Some((i, Handle::Steep))
                } else if near(spread) {
                    Some((i, Handle::Spread))
                } else if near(centre) {
                    Some((i, Handle::Body))
                } else {
                    None
                };
            }
            if grab.is_none()
                && let Some(i) = spectro::hit_curve(&self.view, &plot, &self.curves, ev.position, 9.0)
            {
                self.select(Some(i), false);
                grab = Some((i, Handle::Body));
            }
            if let Some((idx, handle)) = grab {
                let c = &self.curves[idx];
                let spread_f = self.view.f_at(&plot, spectro::handles(&self.view, &plot, c)[2].y);
                log::info!("edit: grabbed {handle:?} of fly-by {}", idx + 1);
                self.curve_drag = Some(CurveDrag { idx, handle, start: ev.position, orig: c.flyby, spread_f, moved: false });
                cx.notify();
                return;
            }
        }
        self.drag = Some(Drag { start: ev.position, view: self.view, moved: false });
        cx.notify();
    }

    /// The fly-by while a curve is being dragged.
    fn dragged_flyby(&self, d: &CurveDrag, pos: Point<Pixels>) -> Flyby {
        let plot = self.plot.get();
        let v = &self.view;
        let o = d.orig;
        match d.handle {
            Handle::Body => {
                let dt = v.t_at(&plot, pos.x) - v.t_at(&plot, d.start.x);
                let f_from = v.f_at(&plot, d.start.y).max(1.0);
                let ratio = (v.f_at(&plot, pos.y).max(1.0) / f_from).clamp(0.5, 2.0);
                // at a low tone in a wide view one pixel is several percent of
                // the frequency: move at most 0.3 % per pixel so the curve can
                // be placed finely (it then lags the mouse; zoom in to follow it)
                let px_rel = ((v.f_at(&plot, d.start.y - px(1.)) - f_from) / f_from).abs().max(1e-6);
                let ratio = ratio.powf((MAX_REL_PER_PX / px_rel).min(1.0));
                Flyby { tc: o.tc + dt, f0: o.f0 * ratio, ..o }
            }
            Handle::Steep => {
                // right = gentler (farther), left = steeper (closer)
                let dx = f32::from(pos.x - d.start.x) as f64;
                Flyby { d: (o.d * 2f64.powf(dx / 70.0)).clamp(40.0, 25_000.0), ..o }
            }
            Handle::Spread => {
                // the swing above the rest frequency scales with the speed
                let r0 = self.curves.get(d.idx).map_or(1.0, |c| c.ratios.iter().copied().fold(f64::INFINITY, f64::min));
                let rest = o.f0 * if r0.is_finite() { r0 } else { 1.0 };
                let f = v.f_at(&plot, pos.y);
                let k = ((f - rest) / (d.spread_f - rest).max(0.05)).clamp(0.1, 4.0);
                Flyby { v: (o.v * k).clamp(8.0, 250.0), ..o }
            }
        }
    }

    fn finish_curve_drag(&mut self, d: CurveDrag, pos: Point<Pixels>, cx: &mut Context<Self>) {
        if !d.moved || d.idx >= self.detections.len() {
            log::info!("edit: released without a move");
            return;
        }
        let fb = self.dragged_flyby(&d, pos);
        log::info!("edit: fly-by {} placed at tc {:.1} f0 {:.1} v {:.0} d {:.0}", d.idx + 1, fb.tc, fb.f0, fb.v, fb.d);
        self.push_undo();
        let o = d.orig;
        // a traced curve keeps its points: each one moves with the curve (its
        // offset from the curve kept) and the curve is fitted to them again;
        // no snapping to a brighter line, the line was too faint for that
        if let Some(trace) = self.detections[d.idx].trace.clone().filter(|p| p.len() >= core_edit::MIN_TRACE_POINTS) {
            let dt = fb.tc - o.tc;
            let moved: Vec<[f64; 2]> = trace
                .iter()
                .map(|p| {
                    let t = p[0] + dt;
                    [t, fb.freq_at(t, DEFAULT_C) * p[1] / o.freq_at(p[0], DEFAULT_C).max(1e-6)]
                })
                .collect();
            match core_edit::trace_detection(&moved, DEFAULT_C) {
                Ok(det) => {
                    self.edit_note = Some((
                        format!("Traced curve moved with its points: {:.0} m/s, closest {}.", det.flyby.v, fmt_dist(det.flyby.d)),
                        false,
                    ));
                    self.detections[d.idx] = det;
                }
                Err(msg) => {
                    self.undo.pop();
                    self.edit_note = Some((format!("Cannot move the traced curve there: {msg}"), true));
                }
            }
            self.curves[d.idx] = Curve::from_detection(&self.detections[d.idx]);
            self.refresh_changed();
            return;
        }
        // the placement itself (its ranges moved along) until the refit answers
        let mut det = self.detections[d.idx].clone();
        let r = &mut det.ranges;
        r.tc = (r.tc.0 + fb.tc - o.tc, r.tc.1 + fb.tc - o.tc);
        r.v = (r.v.0 * fb.v / o.v, r.v.1 * fb.v / o.v);
        r.d = (r.d.0 * fb.d / o.d, r.d.1 * fb.d / o.d);
        det.flyby = fb;
        det.origin = Origin::Edited;
        det.trace = None;
        self.detections[d.idx] = det;
        self.curves[d.idx] = Curve::from_detection(&self.detections[d.idx]);
        self.refresh_changed();
        self.start_refit(d.idx, fb, cx);
    }

    fn on_spec_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let plot = self.plot.get();
        if let Some(mut d) = self.curve_drag.take() {
            if ev.pressed_button == Some(MouseButton::Left) {
                let delta = ev.position - d.start;
                if f32::from(delta.x).abs() + f32::from(delta.y).abs() > 3.0 {
                    d.moved = true;
                }
                if d.moved {
                    // live: the curve follows the mouse; it snaps on release
                    let fb = self.dragged_flyby(&d, ev.position);
                    if let Some(c) = self.curves.get_mut(d.idx) {
                        c.flyby = fb;
                        let half = (c.t_span.1 - c.t_span.0) / 2.0;
                        c.t_span = (fb.tc - half, fb.tc + half);
                    }
                }
                self.curve_drag = Some(d);
            } else {
                // released outside the plot
                self.finish_curve_drag(d, ev.position, cx);
            }
            cx.notify();
            return;
        }
        if let Some(drag) = self.drag.as_mut() {
            if ev.pressed_button == Some(MouseButton::Left) {
                let d = ev.position - drag.start;
                if f32::from(d.x).abs() + f32::from(d.y).abs() > 3.0 {
                    drag.moved = true;
                }
                if drag.moved {
                    let v0 = drag.view;
                    let dt = -(f32::from(d.x) / f32::from(plot.size.width)) as f64 * (v0.t1 - v0.t0);
                    let df = (f32::from(d.y) / f32::from(plot.size.height)) as f64 * (v0.f1 - v0.f0);
                    self.view = View { t0: v0.t0 + dt, t1: v0.t1 + dt, f0: v0.f0 + df, f1: v0.f1 + df };
                    self.view_touched = true;
                    self.clamp_view();
                }
            } else {
                self.drag = None;
            }
        }
        self.hover = if plot.contains(&ev.position) && self.duration() > 0.0 {
            Some((self.view.t_at(&plot, ev.position.x), self.view.f_at(&plot, ev.position.y)))
        } else {
            None
        };
        cx.notify();
    }

    fn on_spec_up(&mut self, ev: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = self.curve_drag.take() {
            self.finish_curve_drag(d, ev.position, cx);
            cx.notify();
            return;
        }
        let Some(drag) = self.drag.take() else { return };
        if drag.moved || self.duration() <= 0.0 {
            return;
        }
        let plot = self.plot.get();
        if !plot.contains(&ev.position) {
            return;
        }
        if let Some(e) = self.edit.as_mut()
            && e.tool == Tool::Trace
        {
            let (t, f) = (self.view.t_at(&plot, ev.position.x), self.view.f_at(&plot, ev.position.y));
            e.pending.push([t, f]);
            let n = e.pending.len();
            self.edit_note = Some((
                if n < core_edit::MIN_TRACE_POINTS {
                    format!("{n} point{} · at least {} needed", if n == 1 { "" } else { "s" }, core_edit::MIN_TRACE_POINTS)
                } else {
                    format!("{n} points · Fit when both sides of the steep part are covered")
                },
                false,
            ));
            cx.notify();
            return;
        }
        if self.show_curves
            && let Some(i) = spectro::hit_curve(&self.view, &plot, &self.curves, ev.position, 9.0)
        {
            self.select(Some(i), false);
        } else {
            self.cursor_t = Some(self.view.t_at(&plot, ev.position.x));
        }
        cx.notify();
    }

    fn overview_seek(&mut self, pos: Point<Pixels>) {
        let r = self.overview_rect.get();
        let dur = self.duration();
        let inner_l = r.left() + px(spectro::LEFT_RULER);
        let w = r.size.width - px(spectro::LEFT_RULER);
        if dur <= 0.0 || w <= px(1.) {
            return;
        }
        let t = (((pos.x - inner_l) / w) as f64 * dur).clamp(0.0, dur);
        let span = self.view.t1 - self.view.t0;
        self.view.t0 = t - span / 2.0;
        self.view.t1 = t + span / 2.0;
        self.view_touched = true;
        self.clamp_view();
    }

    // ---------------------------------------------------------------- actions

    fn act_open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(cx);
    }
    fn act_theme(&mut self, _: &ToggleTheme, window: &mut Window, cx: &mut Context<Self>) {
        let t = theme::theme(cx).toggled();
        cx.set_global(t);
        window.refresh();
    }
    fn act_quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        self.layout.window = Some(layout::WindowState::from_bounds(window.window_bounds()));
        layout::save(&self.layout);
        self.flush_history(cx);
        cx.quit();
    }
    fn sorted_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.detections.len()).collect();
        let (key, asc) = self.sort;
        let val = |i: usize| -> f64 {
            let d = &self.detections[i];
            match key {
                SortKey::Tc => d.flyby.tc,
                SortKey::F0 => d.flyby.f0,
                SortKey::Speed => d.flyby.v,
                SortKey::Dist => d.flyby.d,
                SortKey::Contrast => d.contrast,
            }
        };
        order.sort_by(|&a, &b| if asc { val(a).total_cmp(&val(b)) } else { val(b).total_cmp(&val(a)) });
        order
    }
    fn act_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(1);
        cx.notify();
    }
    fn act_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(-1);
        cx.notify();
    }
    fn step_selection(&mut self, d: i64) {
        let order = self.sorted_order();
        if order.is_empty() {
            return;
        }
        let pos = self.selected.and_then(|s| order.iter().position(|&o| o == s));
        let next = match pos {
            None => if d > 0 { 0 } else { order.len() - 1 },
            Some(p) => (p as i64 + d).clamp(0, order.len() as i64 - 1) as usize,
        };
        self.select(Some(order[next]), true);
    }
    fn act_clear(&mut self, _: &ClearSelection, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = None;
        cx.notify();
    }
    fn act_zoom_all(&mut self, _: &ZoomAll, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_all();
        cx.notify();
    }
    fn act_zoom_sel(&mut self, _: &ZoomSelection, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.selected {
            self.zoom_to(i);
        }
        cx.notify();
    }
    fn act_whiten(&mut self, _: &ToggleWhiten, _: &mut Window, cx: &mut Context<Self>) {
        self.whiten = !self.whiten;
        cx.notify();
    }
    fn act_curves(&mut self, _: &ToggleCurves, _: &mut Window, cx: &mut Context<Self>) {
        self.show_curves = !self.show_curves;
        cx.notify();
    }
    fn act_find(&mut self, _: &FindLocation, _: &mut Window, cx: &mut Context<Self>) {
        self.find_location(cx);
    }
    fn act_details(&mut self, _: &ToggleDetails, _: &mut Window, cx: &mut Context<Self>) {
        self.details = !self.details;
        cx.notify();
    }
    fn act_edit(&mut self, _: &ToggleEdit, _: &mut Window, cx: &mut Context<Self>) {
        if self.edit.is_some() {
            self.edit = None;
        } else if self.stage == Stage::Done {
            self.enter_edit(if self.detections.is_empty() { Tool::Trace } else { Tool::Move });
        }
        cx.notify();
    }
    fn act_undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        // cuts are also changed outside the edit mode (the timeline): their undo works there too
        if self.edit.is_some() || !self.undo.is_empty() {
            self.do_undo();
            cx.notify();
        }
    }
    fn act_delete(&mut self, _: &DeleteFlyby, _: &mut Window, cx: &mut Context<Self>) {
        if self.edit.is_some() {
            self.delete_selected();
            cx.notify();
        }
    }
    fn act_fit_trace(&mut self, _: &FitTrace, _: &mut Window, cx: &mut Context<Self>) {
        if self.edit.as_ref().is_some_and(|e| e.tool == Tool::Trace) {
            self.fit_pending();
            cx.notify();
        }
    }
    fn act_remove_point(&mut self, _: &RemoveTracePoint, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(e) = self.edit.as_mut() {
            e.pending.pop();
            cx.notify();
        }
    }
    fn focus_step(&mut self, d: i64, window: &mut Window, cx: &mut Context<Self>) {
        let order = self.form.order();
        let cur = order.iter().position(|e| e.read(cx).focus_handle(cx).is_focused(window));
        let n = order.len() as i64;
        let next = match cur {
            None => 0,
            Some(c) => ((c as i64 + d).rem_euclid(n)) as usize,
        };
        let fh = order[next].read(cx).focus_handle(cx);
        window.focus(&fh);
        cx.notify();
    }
    fn act_focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_step(1, window, cx);
    }
    fn act_focus_prev(&mut self, _: &FocusPrev, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_step(-1, window, cx);
    }
    fn act_spectrogram(&mut self, _: &ToggleSpectrogram, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_spectrogram(cx);
    }
    fn act_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.layout.sidebar_open = !self.layout.sidebar_open;
        self.schedule_layout_save(cx);
        cx.notify();
    }

    // ---------------------------------------------------------------- layout

    /// The spectrogram is on screen (the user's choice, or opened for editing).
    fn spectro_shown(&self) -> bool {
        self.layout.spectrogram || self.spectro_auto || self.spectro_for_analysis || self.spectro_once || self.edit.is_some()
    }

    /// No spectrogram: the map is in the middle.
    fn map_center(&self) -> bool {
        !self.spectro_shown() && self.file.as_ref().is_some_and(|f| f.overview.is_some())
    }

    /// The fly-by table is in the right column (map in the middle, simple view).
    fn table_right(&self) -> bool {
        self.map_center() && !self.details
    }

    fn toggle_spectrogram(&mut self, cx: &mut Context<Self>) {
        let shown = self.spectro_shown();
        if shown && self.edit.is_some() {
            // the curves are edited on the spectrogram: leave the edit mode with it
            self.edit = None;
        }
        self.layout.spectrogram = !shown;
        self.spectro_auto = false;
        self.spectro_for_analysis = false;
        self.spectro_once = false;
        log::info!("spectrogram shown: {}", !shown);
        self.schedule_layout_save(cx);
        cx.notify();
    }

    /// The open recording's history entry.
    fn history_entry(&mut self) -> Option<&mut history::Entry> {
        let path = history::key(&self.file.as_ref()?.path);
        self.history.get_mut(&path)
    }

    fn form_values(&self, cx: &App) -> history::FormValues {
        let f = &self.form;
        let text = |e: &Entity<TextInput>| e.read(cx).text().trim().to_string();
        history::FormValues {
            day: text(&f.day),
            day_end: if f.show_range { text(&f.day_end) } else { String::new() },
            time_mode: match f.time_mode {
                TimeMode::Unknown => "unknown",
                TimeMode::Approx => "approx",
                TimeMode::Exact => "exact",
            }
            .into(),
            start: text(&f.start),
            hour_from: text(&f.hour_from),
            hour_to: text(&f.hour_to),
            utc: text(&f.utc),
            city: text(&f.city),
            state: text(&f.state),
            country: text(&f.country),
        }
    }

    /// Fill the form with what was typed for a recording before.
    fn set_form_values(&mut self, v: &history::FormValues, cx: &mut Context<Self>) {
        let f = &self.form;
        for (e, s) in [
            (&f.day, &v.day),
            (&f.day_end, &v.day_end),
            (&f.start, &v.start),
            (&f.hour_from, &v.hour_from),
            (&f.hour_to, &v.hour_to),
            (&f.utc, &v.utc),
            (&f.city, &v.city),
            (&f.state, &v.state),
            (&f.country, &v.country),
        ] {
            e.update(cx, |i, cx| i.set_text(s, cx));
        }
        self.form.show_range = !v.day_end.is_empty();
        self.form.time_mode = match v.time_mode.as_str() {
            "approx" => TimeMode::Approx,
            "exact" => TimeMode::Exact,
            _ => TimeMode::Unknown,
        };
        self.refresh_auto_offset(cx);
        self.refresh_region_preview(cx);
    }

    /// Keep the form as typed for the open recording. A changed form makes
    /// the stored answer stale (it belonged to the old values).
    fn remember_form(&mut self, cx: &mut Context<Self>) {
        self.history_save = None;
        let v = self.form_values(cx);
        if let Some(e) = self.history_entry()
            && e.form != v
        {
            e.form = v;
            e.searched = false;
            e.result = None;
            history::save(&self.history);
        }
    }

    /// Typing not yet written: write it now (closing, quitting).
    fn flush_history(&mut self, cx: &mut Context<Self>) {
        if self.history_save.is_some() {
            self.remember_form(cx);
        }
    }

    /// Look up again which recent files still exist, off the UI thread (a
    /// network drive can take seconds to answer).
    fn refresh_missing(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self.history.entries.iter().map(|e| e.path.clone()).collect();
        cx.spawn(async move |this, cx| {
            let missing: Vec<(PathBuf, bool)> =
                cx.background_executor().spawn(async move { paths.into_iter().map(|p| { let m = !p.is_file(); (p, m) }).collect() }).await;
            this.update(cx, |this, cx| {
                for (p, m) in missing {
                    if let Some(e) = this.history.get_mut(&p) {
                        e.missing = m;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `remember_form` once typing settles.
    fn schedule_history_save(&mut self, cx: &mut Context<Self>) {
        if self.file.is_none() {
            return;
        }
        self.history_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(800)).await;
            this.update(cx, |this, cx| this.remember_form(cx)).ok();
        }));
    }

    /// Open a recording from the history (`open_path` brings back the day,
    /// time and region typed for it). The one already open stays as it is.
    fn open_recent(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.file.as_ref().is_some_and(|f| history::key(&f.path) == path) {
            return;
        }
        if !path.is_file() {
            if let Some(e) = self.history.get_mut(&path) {
                e.missing = true;
            }
            cx.notify();
            return;
        }
        self.open_path(path, cx);
    }

    fn forget_recent(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.history.remove(path);
        history::save(&self.history);
        cx.notify();
    }

    /// Write the layout once it has not changed for a moment (drags change it on every move).
    fn schedule_layout_save(&mut self, cx: &mut Context<Self>) {
        self.layout_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(600)).await;
            this.update(cx, |this, _| {
                layout::save(&this.layout);
                this.layout_save = None;
            })
            .ok();
        }));
    }

    fn drag_split(&mut self, split: Split, pos: Point<Pixels>) {
        let body = self.body_rect.get();
        let table_right = self.table_right();
        let l = &mut self.layout;
        match split {
            Split::Sidebar => {
                let w = f32::from(pos.x - body.left());
                // dragged far to the left: fold the form
                l.sidebar_open = w >= layout::SIDEBAR_MIN * 0.6;
                if l.sidebar_open {
                    // no wider than the window lets it be shown (the line stays under the pointer)
                    let bw = f32::from(body.size.width);
                    let insp = l.fitted(bw, f32::from(body.size.height), 0).inspector_w;
                    let room = (bw - insp - layout::CENTER_PREF).max(layout::SIDEBAR_W);
                    l.sidebar_w = w.clamp(layout::SIDEBAR_MIN, layout::SIDEBAR_MAX.min(room));
                }
            }
            Split::Inspector => {
                let side = l.fitted(f32::from(body.size.width), f32::from(body.size.height), 0).sidebar_w;
                let max = (f32::from(body.size.width) - side - layout::CENTER_MIN).min(layout::INSPECTOR_MAX);
                l.inspector_w = f32::from(body.right() - pos.x).clamp(layout::INSPECTOR_MIN, max.max(layout::INSPECTOR_MIN));
            }
            Split::Table => {
                let c = self.center_rect.get();
                let max = (f32::from(c.size.height) - layout::CENTER_TOP_MIN).max(layout::TABLE_MIN);
                l.table_h = f32::from(c.bottom() - pos.y).clamp(layout::TABLE_MIN, max);
                l.table_user = true;
            }
            Split::Drawer => {
                let c = self.center_rect.get();
                let max = (f32::from(c.size.height) - layout::CENTER_TOP_MIN).max(layout::ANALYSIS_MIN);
                l.analysis_h = f32::from(c.bottom() - pos.y).clamp(layout::ANALYSIS_MIN, max);
            }
            Split::Right => {
                let r = self.right_rect.get();
                let y = f32::from(pos.y - r.top());
                let room = f32::from(r.size.height) - layout::RIGHT_BOTTOM_MIN;
                if table_right {
                    l.right_table_h = Some(y.clamp(layout::TABLE_MIN, room.max(layout::TABLE_MIN)));
                } else {
                    l.right_top_h = y.clamp(layout::RIGHT_TOP_MIN, room.max(layout::RIGHT_TOP_MIN));
                }
            }
        }
    }

    /// A 1 px divider that can be dragged (double click: default size).
    fn splitter(&self, t: &Theme, split: Split, cx: &mut Context<Self>) -> AnyElement {
        let vertical = matches!(split, Split::Sidebar | Split::Inspector);
        let (id, group): (&'static str, &'static str) = match split {
            Split::Sidebar => ("split-sidebar", "g-split-sidebar"),
            Split::Inspector => ("split-inspector", "g-split-inspector"),
            Split::Table => ("split-table", "g-split-table"),
            Split::Right => ("split-right", "g-split-right"),
            Split::Drawer => ("split-drawer", "g-split-drawer"),
        };
        let grab = theme::SPLIT_GRAB;
        let off = px(-(grab - 1.0) / 2.0);
        let accent = t.accent;
        let cursor = if vertical { CursorStyle::ResizeLeftRight } else { CursorStyle::ResizeUpDown };
        let dragging = cx.has_active_drag() && self.split_dragging == Some(split);
        // the grab area is drawn after every panel (deferred), so the panel on
        // either side cannot cover it, and it blocks the pointer for them
        let handle = div()
            .id(id)
            .group(group)
            .absolute()
            .occlude()
            .cursor(cursor)
            .map(|d| if vertical { d.top_0().bottom_0().left(off).w(px(grab)) } else { d.left_0().right_0().top(off).h(px(grab)) })
            .flex()
            .justify_center()
            .items_center()
            .child(
                div()
                    .map(|d| if vertical { d.w(px(2.)).h_full() } else { d.h(px(2.)).w_full() })
                    .when(dragging, |d| d.bg(accent))
                    .group_hover(group, move |s| s.bg(accent.opacity(0.7))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if ev.click_count >= 2 {
                        let table_right = this.table_right();
                        this.layout.reset(split, table_right);
                        if split == Split::Sidebar {
                            this.layout.sidebar_open = true;
                        }
                        this.schedule_layout_save(cx);
                        cx.notify();
                    }
                }),
            )
            .on_drag(DragSplit(split), move |_, _, _, cx| cx.new(|_| SplitGhost));
        div()
            .flex_none()
            .relative()
            .bg(t.border)
            .map(|d| if vertical { d.w(px(1.)).h_full() } else { d.h(px(1.)).w_full() })
            .child(deferred(handle).with_priority(1))
            .into_any_element()
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

// -------------------------------------------------------------------- widgets

fn label_xs(t: &Theme, s: impl Into<SharedString>) -> gpui::Div {
    div().text_size(px(theme::TEXT_XS)).text_color(t.text_faint).child(s.into())
}

fn section_header(t: &Theme, title: &str, right: Option<AnyElement>) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap(px(theme::SP2))
        .h(px(theme::HEADER_H))
        .px(px(theme::PANEL_PX))
        .overflow_hidden()
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_size(px(theme::TEXT_SM))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.text_muted)
                .child(title.to_string()),
        )
        // squeezed, the right part is cut at its end with an ellipsis (not at its start)
        .children(right.map(|r| div().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(r)))
}

fn pill(t: &Theme, s: &str, fg: Hsla) -> gpui::Div {
    div()
        .px(px(6.))
        .h(px(20.))
        .flex()
        .flex_none()
        .items_center()
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(fg.opacity(0.45))
        .text_size(px(theme::TEXT_XS))
        .text_color(fg)
        .whitespace_nowrap()
        .child(s.to_string())
        .bg(t.panel_bg.opacity(0.0))
}

fn mono(s: impl Into<SharedString>) -> gpui::Div {
    div().font_family(MONO).child(s.into())
}

fn button(t: &Theme, id: &'static str, label: &str, ic: Option<Icon>, primary: bool, enabled: bool) -> gpui::Stateful<gpui::Div> {
    let (bg, fg, border) = if primary && enabled {
        (t.accent, t.accent_text, t.accent)
    } else if primary {
        (t.hover, t.text_faint, t.border_strong)
    } else {
        (t.surface, if enabled { t.text } else { t.text_faint }, t.border_strong)
    };
    let hover = if primary { t.accent.opacity(0.88) } else { t.hover };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(theme::SP2))
        .h(px(theme::CONTROL_H))
        .px(px(theme::SP3))
        .rounded(px(theme::RADIUS))
        .bg(bg)
        .border_1()
        .border_color(border)
        .whitespace_nowrap()
        .text_size(px(theme::TEXT_SM))
        .text_color(fg)
        .font_weight(if primary { FontWeight::MEDIUM } else { FontWeight::NORMAL })
        .when(enabled, |d| d.cursor_pointer().hover(move |s| s.bg(hover)))
        .children(ic.map(|i| icon(i, 13.0, fg)))
        .child(label.to_string())
}

fn toggle(t: &Theme, id: impl Into<gpui::ElementId>, label: &str, on: bool) -> gpui::Stateful<gpui::Div> {
    let hover = t.hover;
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .h(px(theme::CONTROL_H))
        .px(px(theme::SP2))
        .rounded(px(theme::RADIUS))
        .whitespace_nowrap()
        .text_size(px(theme::TEXT_SM))
        .cursor_pointer()
        .text_color(if on { t.text } else { t.text_muted })
        .when(on, |d| d.bg(t.selected))
        .hover(move |s| s.bg(hover))
        .child(label.to_string())
}

/// A title-bar switch: a small box (ticked when on) and a label.
fn switch(t: &Theme, id: &'static str, label: &str, on: bool) -> gpui::Stateful<gpui::Div> {
    let hover = t.hover;
    div()
        .id(id)
        .occlude()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(theme::SP2))
        .h(px(theme::CONTROL_H))
        .px(px(theme::SP2))
        .rounded(px(theme::RADIUS))
        .whitespace_nowrap()
        .text_size(px(theme::TEXT_SM))
        .text_color(if on { t.text } else { t.text_muted })
        .when(on, |d| d.bg(t.selected))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(
            div()
                .size(px(12.))
                .flex_none()
                .rounded(px(theme::RADIUS_SM))
                .border_1()
                .border_color(if on { t.accent } else { t.text_muted })
                .when(on, |d| d.bg(t.accent))
                .flex()
                .items_center()
                .justify_center()
                .when(on, |d| d.child(icon(Icon::Check, 10.0, t.accent_text))),
        )
        .child(label.to_string())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn fmt_dist(m: f64) -> String {
    if m >= 1000.0 { format!("{:.2} km", m / 1000.0) } else { format!("{m:.0} m") }
}

fn fmt_dur(s: f64) -> String {
    if s < 60.0 {
        format!("{s:.1} s")
    } else {
        let s = s.round() as i64;
        format!("{}:{:02}", s / 60, s % 60)
    }
}

// -------------------------------------------------------------------- render

impl Workspace {
    fn render_titlebar(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Linux (Wayland/X11) with client-side decorations: the app moves and
        // resizes itself. Windows: GPUI hides the system title bar but leaves
        // the frame to the OS; the title bar and the caption buttons are told
        // apart by hit testing (WindowControlArea), so dragging, Aero snap,
        // double click, Snap Layouts and the buttons are all native there.
        let client = matches!(window.window_decorations(), Decorations::Client { .. });
        let on_windows = cfg!(target_os = "windows");
        let controls = client || on_windows;
        let file_name = self.file.as_ref().map(|f| f.name.clone());
        let theme_icon = if t.mode == theme::Mode::Dark { Icon::Moon } else { Icon::Sun };
        let maximized = window.is_maximized();
        let hover = t.hover;
        // Windows 11 caption buttons: 46 px wide, the title bar's height, a
        // faint hover, red under the close glyph (which turns white). On
        // Windows the glyphs are Windows' own icon font.
        let caption_hover_bg: Hsla = if t.mode == theme::Mode::Dark { gpui::rgba(0xffffff14).into() } else { gpui::rgba(0x0000000f).into() };
        let close_red: Hsla = gpui::rgb(0xc42b1c).into();
        let white: Hsla = gpui::rgb(0xffffff).into();
        let caption_font = self.caption_font;
        let caption_on = self.caption_hover;
        let ctl = |id: &'static str, ic: Icon, area: WindowControlArea, cx: &mut Context<Self>| {
            let close = area == WindowControlArea::Close;
            let lit = close && caption_on == Some(area);
            let color = if lit { white } else { t.text };
            let glyph: AnyElement = match caption_font {
                Some(font) => div()
                    .font_family(font)
                    .text_size(px(10.))
                    .text_color(color)
                    .child(match ic {
                        Icon::Minimize => "\u{e921}",
                        Icon::Maximize => "\u{e922}",
                        Icon::Restore => "\u{e923}",
                        _ => "\u{e8bb}",
                    })
                    .into_any_element(),
                None => crate::icons::caption_glyph(ic, color).into_any_element(),
            };
            div()
                .id(id)
                .occlude()
                .w(px(theme::CAPTION_W))
                .h_full()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .when(close, |d| d.hover(move |s| s.bg(close_red)).active(move |s| s.bg(close_red.opacity(0.85))))
                .when(!close, |d| d.hover(move |s| s.bg(caption_hover_bg)).active(move |s| s.bg(caption_hover_bg.opacity(0.6))))
                .on_hover(cx.listener(move |this, h: &bool, _, cx| {
                    let now = if *h { Some(area) } else if this.caption_hover == Some(area) { None } else { this.caption_hover };
                    if now != this.caption_hover {
                        this.caption_hover = now;
                        cx.notify();
                    }
                }))
                .when(on_windows, |d| d.window_control_area(area))
                .when(!on_windows, |d| d.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()))
                .child(glyph)
        };
        let side_open = self.layout.sidebar_open;
        let spectro = self.spectro_shown();
        // the drag area lies under everything; the buttons block it (occlude).
        // On Windows a few pixels at the top are left to the OS for resizing.
        let drag = div()
            .id("title-drag")
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .top(px(if on_windows && !maximized { 4. } else { 0. }))
            .window_control_area(WindowControlArea::Drag)
            .when(client, |d| {
                d.on_mouse_down(MouseButton::Left, |ev, window, _| {
                    if ev.click_count == 2 {
                        window.zoom_window();
                    } else {
                        window.start_window_move();
                    }
                })
                .on_mouse_down(MouseButton::Right, |ev, window, _| window.show_window_menu(ev.position))
            });
        let title_btn = |id: &'static str, on: bool| {
            div()
                .id(id)
                .occlude()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(theme::SP2))
                .h(px(theme::CONTROL_H))
                .px(px(theme::SP2))
                .rounded(px(theme::RADIUS))
                .whitespace_nowrap()
                .text_size(px(theme::TEXT_SM))
                .text_color(if on { t.text } else { t.text_muted })
                .when(on, |d| d.bg(t.selected))
                .cursor_pointer()
                .hover(move |s| s.bg(hover).text_color(t.text))
        };
        let has_recent = !self.history.entries.is_empty();
        let menu_open = self.open_menu && has_recent;
        div()
            .id("titlebar")
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(theme::SP1))
            .h(px(theme::TITLE_H))
            .bg(t.surface)
            .border_b_1()
            .border_color(t.border)
            .pl(px(theme::SP2))
            .when(!controls, |d| d.pr(px(theme::SP2)))
            .child(drag)
            .child(
                title_btn("side-toggle", false)
                    .px(px(6.))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.layout.sidebar_open = !this.layout.sidebar_open;
                        log::info!("form shown: {}", this.layout.sidebar_open);
                        this.schedule_layout_save(cx);
                        cx.notify();
                    }))
                    .child(icon(Icon::PanelLeft, 16.0, if side_open { t.text } else { t.text_muted })),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w(px(0.))
                    .items_center()
                    .gap(px(theme::SP2))
                    .pl(px(theme::SP1))
                    .overflow_hidden()
                    .child(
                        div()
                            .size(px(20.))
                            .flex_none()
                            .rounded(px(5.))
                            .bg(theme::logo_background())
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(Icon::Logo, 20.0, gpui::white())),
                    )
                    .child(div().flex_none().text_size(px(theme::TEXT)).font_weight(FontWeight::SEMIBOLD).text_color(t.text).child("Overhead"))
                    .when_some(file_name, |d, name| {
                        d.child(div().flex_none().text_color(t.text_faint).text_size(px(theme::TEXT)).child("/")).child(
                            div()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(px(theme::TEXT))
                                .text_color(t.text_muted)
                                .child(name),
                        )
                    }),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(
                        title_btn("open-btn", menu_open)
                            // on press, with the state it had when drawn: a press
                            // on it while the menu is open also counts as one
                            // outside the menu, which has already closed it
                            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                if has_recent {
                                    this.open_menu = !menu_open;
                                    if this.open_menu {
                                        this.refresh_missing(cx);
                                    }
                                    cx.notify();
                                }
                            }))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if !has_recent {
                                    this.open_dialog(cx);
                                }
                            }))
                            .child(icon(Icon::Folder, theme::ICON, t.text_muted))
                            .child("Open")
                            .when(has_recent, |d| d.child(icon(Icon::ChevronDown, theme::ICON_SM, t.text_muted))),
                    )
                    .when(menu_open, |d| d.child(self.render_open_menu(t, cx))),
            )
            .child(div().w(px(1.)).h(px(18.)).flex_none().bg(t.border).mx(px(theme::SP1)))
            .child(title_btn("spectro-btn", spectro).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_spectrogram(cx))).child("Spectrogram"))
            .child(title_btn("details-btn", self.details).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.details = !this.details;
                cx.notify();
            })).child("Details"))
            .child(
                title_btn("theme-btn", false)
                    .w(px(theme::CONTROL_H))
                    .px(px(0.))
                    .justify_center()
                    .on_click(cx.listener(|_, _: &ClickEvent, window, cx| {
                        let nt = theme::theme(cx).toggled();
                        cx.set_global(nt);
                        window.refresh();
                    }))
                    .child(icon(theme_icon, theme::ICON, t.text_muted)),
            )
            .when(controls, |d| {
                d.child(div().w(px(theme::SP2)).flex_none())
                    .child(
                        ctl("min", Icon::Minimize, WindowControlArea::Min, cx)
                            .when(!on_windows, |d| d.on_click(|_, window, _| window.minimize_window())),
                    )
                    .child(
                        ctl("max", if maximized { Icon::Restore } else { Icon::Maximize }, WindowControlArea::Max, cx)
                            .when(!on_windows, |d| d.on_click(|_, window, _| window.zoom_window())),
                    )
                    .child(
                        ctl("close", Icon::Close, WindowControlArea::Close, cx).when(!on_windows, |d| {
                            d.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.layout.window = Some(layout::WindowState::from_bounds(window.window_bounds()));
                                layout::save(&this.layout);
                                this.flush_history(cx);
                                window.remove_window();
                            }))
                        }),
                    )
            })
    }

    /// The Open menu: a file, or one of the recent recordings.
    fn render_open_menu(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let hover = t.hover;
        let item = |id: gpui::ElementId| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(theme::SP2))
                .h(px(theme::ROW_H))
                .px(px(theme::SP2))
                .rounded(px(theme::RADIUS_SM))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
        };
        let now = now_unix();
        let rows: Vec<AnyElement> = self
            .history
            .entries
            .iter()
            .take(8)
            .enumerate()
            .map(|(i, e)| {
                let exists = !e.missing;
                let is_open = self.file.as_ref().is_some_and(|f| history::key(&f.path) == e.path);
                let path = e.path.clone();
                item(("menu-recent", i).into())
                    .when(!exists, |d| d.cursor_default())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.open_menu = false;
                        this.open_recent(path.clone(), cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(if exists { t.text } else { t.text_faint })
                            .child(e.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(theme::TEXT_XS))
                            .text_color(t.text_faint)
                            .child(if is_open {
                                "open".into()
                            } else if exists {
                                history::ago(e.opened, now)
                            } else {
                                "not found".into()
                            }),
                    )
                    .into_any_element()
            })
            .collect();
        // its top right corner under the button's right edge
        div().absolute().right_0().top(px(theme::CONTROL_H + 6.)).child(deferred(
            gpui::anchored().anchor(gpui::Corner::TopRight).snap_to_window_with_margin(px(theme::SP2)).child(
                div()
                    .id("open-menu")
                    .occlude()
                    .w(px(340.))
                    .p(px(theme::SP1))
                    .flex()
                    .flex_col()
                    .rounded(px(theme::RADIUS + 2.))
                    .bg(t.panel_bg)
                    .border_1()
                    .border_color(t.border_strong)
                    .shadow(vec![gpui::BoxShadow { color: t.shadow, blur_radius: px(16.), spread_radius: px(0.), offset: point(px(0.), px(4.)) }])
                    .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.open_menu = false;
                        cx.notify();
                    }))
                    .child(
                        item("menu-open-file".into())
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.open_menu = false;
                                this.open_dialog(cx);
                            }))
                            .child(icon(Icon::Folder, theme::ICON, t.text_muted))
                            .child(div().flex_1().text_size(px(theme::TEXT_SM)).child("Open file…"))
                            .child(mono("Ctrl O").text_size(px(theme::TEXT_XS)).text_color(t.text_faint)),
                    )
                    .child(div().h(px(1.)).mx(px(-theme::SP1)).my(px(theme::SP1)).bg(t.border))
                    .child(div().px(px(theme::SP2)).h(px(24.)).flex().items_center().text_size(px(theme::TEXT_XS)).text_color(t.text_faint).child("Recent"))
                    .children(rows),
            ),
        )
        .with_priority(2))
        .into_any_element()
    }

    fn render_sidebar(&mut self, t: &Theme, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let details = self.details;
        let f = &self.form;
        let day_text = f.day.read(cx).text().to_string();
        let day_end_text = if f.show_range { f.day_end.read(cx).text().to_string() } else { String::new() };
        let days = query::validate_days(&day_text, &day_end_text);
        let q = self.query(cx);
        let region = self.region(cx);
        let time_res = self.time_hint(cx);
        let time_hint = time_res.as_ref().map(|t| t.0).unwrap_or(TimeHint::Unknown);
        let ndays = days.as_ref().map(|(a, b)| b.map_or(1, |b| b.days() - a.days() + 1)).unwrap_or(1);
        let space = query::search_space(ndays, time_hint, &region, self.duration().max(600.0));

        let field = |label: &str, badge: Option<gpui::Div>, input: &Entity<TextInput>| {
            div()
                .flex()
                .flex_col()
                .gap(px(theme::SP1))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(theme::SP2))
                        .child(div().text_size(px(theme::TEXT_SM)).text_color(t.text).child(label.to_string()))
                        .children(badge),
                )
                .child(input.clone())
        };

        // ---- recording
        let rec: Option<AnyElement> = match &self.file {
            None => None,
            Some(file) => Some({
                let a = file.audio.as_ref();
                let kv = |k: &str, v: String| {
                    div()
                        .flex()
                        .justify_between()
                        .h(px(theme::KV_H))
                        .items_center()
                        .child(div().text_color(t.text_muted).child(k.to_string()))
                        .child(mono(v).text_color(t.text))
                };
                div()
                    .px(px(theme::PANEL_PX))
                    .pb(px(theme::SP3))
                    .flex()
                    .flex_col()
                    .text_size(px(theme::TEXT_SM))
                    .child(
                        div()
                            .text_size(px(theme::TEXT))
                            .text_color(t.text)
                            .mb(px(theme::SP1))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(file.name.clone()),
                    )
                    .when(details, |d| {
                        d.child(
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(t.text_faint)
                                .mb(px(theme::SP2))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(file.path.parent().map(|p| p.display().to_string()).unwrap_or_default()),
                        )
                    })
                    .child(kv("Duration", a.map_or("…".into(), |a| draw_time_long(a.duration()))))
                    .when(details, |d| {
                        d.child(kv(
                            "Sample rate",
                            a.map_or("…".into(), |a| format!("{} → {:.0} Hz", a.source_rate, a.sample_rate)),
                        ))
                        .child(kv("Decoder", a.map_or("…".into(), |a| a.decoder.to_string())))
                        .child(kv("Size", format!("{:.1} MB", file.size_bytes as f64 / 1e6)))
                    })
                    .when(a.is_some_and(|a| a.decode_errors > 0 || a.gap_fill_s > 0.0), |d| {
                        let a = a.unwrap();
                        d.child(
                            div()
                                .mt(px(theme::SP1))
                                .text_color(t.warn)
                                .child(format!("{} decode errors, {:.1} s gaps filled", a.decode_errors, a.gap_fill_s)),
                        )
                    })
                    .into_any_element()
            }),
        };

        // ---- day
        let day_err = match (&days, day_text.trim().is_empty()) {
            (Err(e), false) => Some(e.clone()),
            _ => None,
        };
        let day_ok = days.is_ok();
        let day_help = match &days {
            Ok((d0, Some(d1))) => Some(format!("{} days, {} to {}", d1.days() - d0.days() + 1, d0.weekday(), d1.weekday())),
            Ok((d0, None)) => Some(d0.weekday_long().to_string()),
            Err(_) => None,
        };
        let show_range = f.show_range;
        let mode = f.time_mode;
        let seg = |id: &'static str, label: &str, m: TimeMode, cx: &mut Context<Self>| {
            let on = mode == m;
            let hover = t.hover;
            div()
                .id(id)
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .h(px(theme::CONTROL_H))
                .text_size(px(theme::TEXT_SM))
                .cursor_pointer()
                .rounded(px(theme::RADIUS_SM))
                .text_color(if on { t.text } else { t.text_muted })
                .when(on, |d| d.bg(t.surface).border_1().border_color(t.border_strong))
                .when(!on, |d| d.hover(move |s| s.bg(hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.form.time_mode = m;
                    this.schedule_history_save(cx);
                    this.refresh_auto_offset(cx);
                    cx.notify();
                }))
                .child(label.to_string())
        };

        let time_fields: AnyElement = match mode {
            TimeMode::Unknown => div()
                .text_size(px(theme::TEXT_XS))
                .text_color(t.text_muted)
                .child("Whole day searched. Rough hours make it ~6× faster.")
                .into_any_element(),
            TimeMode::Exact => div()
                .flex()
                .gap(px(theme::SP2))
                .child(div().flex_1().child(f.start.clone()))
                .when(details, |d| d.child(div().w(px(104.)).child(f.utc.clone())))
                .into_any_element(),
            TimeMode::Approx => div()
                .flex()
                .gap(px(theme::SP2))
                .items_center()
                .child(div().w(px(62.)).child(f.hour_from.clone()))
                .child(div().text_color(t.text_faint).child("–"))
                .child(div().w(px(62.)).child(f.hour_to.clone()))
                .child(div().flex_1())
                .when(details, |d| d.child(div().w(px(104.)).child(f.utc.clone())))
                .into_any_element(),
        };
        let time_err = match (&time_res, mode) {
            (Err(e), TimeMode::Exact | TimeMode::Approx) => Some(e.clone()),
            _ => None,
        };
        let offset_note = match (&time_res, mode) {
            _ if !details => None,
            (Ok((_, off, true)), TimeMode::Exact | TimeMode::Approx) if self.opts.sample_data => Some(format!(
                "{} is a guess: this computer's time zone on {}. Type the offset if the recorder's clock used another zone.",
                query::format_offset(*off),
                days.as_ref().map_or("that day".to_string(), |(d, _)| d.iso()),
            )),
            (Ok((_, _, true)), TimeMode::Exact | TimeMode::Approx) => Some(if region.finest().is_some() {
                "Local time in the region's time zone on that day (DST included). Type +02:00 or Europe/Warsaw if the clock used another zone.".to_string()
            } else {
                "No region: every UTC offset in use is tried. A region or an offset (+02:00, Europe/Warsaw) narrows it.".to_string()
            }),
            _ => None,
        };

        // ---- search space meter
        let cells = 14usize;
        let filled = (space.precision * cells as f64).round() as usize;
        let meter = div().flex().gap(px(2.)).children((0..cells).map(|i| {
            div().flex_1().h(px(5.)).rounded(px(1.)).bg(if i < filled { t.accent } else { t.border_strong })
        }));
        let region_hint = match region.finest() {
            None => Some("A country makes it ~30× faster, a city ~5 000×."),
            Some(("country", _)) => Some("A state or city makes it 10–150× faster."),
            Some(("state", _)) => Some("The nearest city makes it ~15× faster."),
            _ => None,
        };

        let no_flybys = self.stage == Stage::Done && self.detections.is_empty();
        let can_find = q.is_ok() && self.file.is_some() && self.stage != Stage::Failed && !no_flybys;
        let find_label = if self.locate_queued {
            "Queued · runs after detection"
        } else if matches!(self.location, LocState::Running(_)) {
            "Searching…"
        } else if self.inputs_changed() && matches!(self.location, LocState::Done(_)) {
            "Search again"
        } else {
            "Find location"
        };
        let find_reason = if self.file.is_none() {
            Some("Open a recording first.".to_string())
        } else if self.stage == Stage::Failed {
            Some("Analysis failed; open another recording.".to_string())
        } else if no_flybys {
            Some(if self.deleted.is_empty() {
                "Needs at least one fly-by. None was found: trace one by hand in Edit fly-bys if you can see it.".to_string()
            } else {
                "Needs at least one fly-by: restore a deleted one or trace one by hand in Edit fly-bys.".to_string()
            })
        } else if let Err(e) = &q {
            Some(e.clone())
        } else if self.stage != Stage::Done {
            Some("Detection is still running; the search starts when it ends.".to_string())
        } else {
            None
        };

        let max = f32::from(self.sidebar_scroll.max_offset().height);
        let more_below = max > 1.0 && f32::from(-self.sidebar_scroll.offset().y) < max - 1.0;
        let hover = t.hover;
        let sub = |s: &str| div().text_size(px(theme::TEXT_SM)).text_color(t.text).child(s.to_string());
        let scroll = div()
            .id("sidebar-scroll")
            .track_scroll(&self.sidebar_scroll)
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll();
        let fold = div()
            .id("side-fold")
            .size(px(theme::CONTROL_H))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(theme::RADIUS))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.layout.sidebar_open = false;
                this.schedule_layout_save(cx);
                cx.notify();
            }))
            .child(icon(Icon::ChevronLeft, theme::ICON, t.text_muted))
            .into_any_element();
        let has_file = rec.is_some();
        let mut fold = Some(fold);
        let scroll = scroll
            .when_some(rec, |d, rec| {
                d.child(section_header(t, "Recording", fold.take())).child(rec).child(div().h(px(1.)).flex_none().bg(t.border))
            })
            .child(section_header(t, "When and where", if has_file { None } else { fold.take() }))
            .child(
                div()
                    .px(px(theme::PANEL_PX))
                    .pb(px(theme::SP4))
                    .flex()
                    .flex_col()
                    .gap(px(theme::SP4))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(theme::SP1))
                            .child(field(
                                "Recording day",
                                Some(if day_ok { pill(t, "required", t.ok) } else { pill(t, "required", t.accent) }),
                                &f.day,
                            ))
                            .when(show_range, |d| d.child(f.day_end.clone()))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(theme::SP2))
                                    .text_size(px(theme::TEXT_XS))
                                    .child(
                                        div()
                                            .min_w(px(0.))
                                            .text_color(if day_err.is_some() { t.danger } else { t.text_muted })
                                            .children(day_err.clone().or(day_help)),
                                    )
                                    .child(
                                        div()
                                            .id("range-toggle")
                                            .flex_none()
                                            .text_color(t.accent)
                                            .cursor_pointer()
                                            .hover(|s| s.underline())
                                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                                this.form.show_range = !this.form.show_range;
                                                this.schedule_history_save(cx);
                                                cx.notify();
                                            }))
                                            .child(if show_range { "One day" } else { "Not sure? 2–3 days" }),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(theme::SP2))
                            .child(sub("Time of day"))
                            .child(
                                div()
                                    .flex()
                                    .p(px(2.))
                                    .gap(px(2.))
                                    .rounded(px(theme::RADIUS))
                                    .bg(t.field_bg)
                                    .border_1()
                                    .border_color(t.border)
                                    // a narrow form gets short labels, so they keep some air
                                    .child(seg("tm-unknown", "Unknown", TimeMode::Unknown, cx))
                                    .child(seg("tm-approx", if width >= 300.0 { "Approx. hours" } else { "Approx." }, TimeMode::Approx, cx))
                                    .child(seg("tm-exact", if width >= 300.0 { "Exact start" } else { "Exact" }, TimeMode::Exact, cx)),
                            )
                            .child(time_fields)
                            .children(time_err.map(|e| div().text_size(px(theme::TEXT_XS)).text_color(t.danger).child(e)))
                            .children(offset_note.map(|n| div().text_size(px(theme::TEXT_XS)).text_color(t.text_faint).child(n))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(theme::SP2))
                            .child(sub("Approximate region"))
                            .child(field_compact(t, "City", &f.city))
                            .child(field_compact(t, "Voivodeship / state", &f.state))
                            .child(field_compact(t, "Country", &f.country))
                            .children(region_hint.map(|h| div().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child(h))),
                    ),
            )
            .when(details, |d| d.child(div().h(px(1.)).flex_none().bg(t.border)))
            .when(details, |d| {
                d.child(section_header(
                    t,
                    "Search space",
                    Some(mono(format!("×{} narrower", fmt_speedup(space.speedup))).text_size(px(theme::TEXT_XS)).text_color(t.text_muted).into_any_element()),
                ))
            })
            .when(details, |d| {
                d.child(
                    div()
                        .px(px(theme::PANEL_PX))
                        .pb(px(theme::SP4))
                        .flex()
                        .flex_col()
                        .gap(px(theme::SP2))
                        .child(meter)
                        .child(space_row(t, "Days", format!("{ndays}")))
                        .child(space_row(t, "Hours", space.hours_label.clone()))
                        .child(space_row(t, "Area", space.area_label.clone())),
                )
            });
        // the search button stays in view however short the window is
        let footer = div()
            .flex_none()
            .px(px(theme::PANEL_PX))
            .py(px(theme::SP3))
            .border_t_1()
            .border_color(t.border)
            .flex()
            .flex_col()
            .gap(px(theme::SP2))
            .child(
                button(t, "find", find_label, Some(Icon::Search), true, can_find)
                    .w_full()
                    .h(px(theme::HEADER_H))
                    .text_size(px(theme::TEXT))
                    .when(can_find, |b| b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.find_location(cx)))),
            )
            .children(find_reason.map(|r| div().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child(r)));
        div()
            .id("sidebar")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width))
            .h_full()
            .bg(t.panel_bg)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .child(scroll)
                    // more of the form below the fold: it fades out above the footer
                    .when(more_below, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(px(theme::SP6))
                                .bg(linear_gradient(180., linear_color_stop(t.panel_bg.opacity(0.0), 0.0), linear_color_stop(t.panel_bg, 1.0))),
                        )
                    }),
            )
            .child(footer)
    }

    /// The folded input form: unfold it, or search.
    fn render_rail(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = t.hover;
        let can_find = self.query(cx).is_ok() && self.file.is_some() && self.stage != Stage::Failed && !(self.stage == Stage::Done && self.detections.is_empty());
        let btn = |id: &'static str| {
            div()
                .id(id)
                .size(px(28.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(theme::RADIUS))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
        };
        div()
            .id("rail")
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .gap(px(theme::SP2))
            .py(px(theme::SP1))
            .w(px(layout::RAIL_W))
            .h_full()
            .bg(t.panel_bg)
            .child(
                btn("rail-open")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.layout.sidebar_open = true;
                        this.schedule_layout_save(cx);
                        cx.notify();
                    }))
                    .child(icon(Icon::ChevronRight, 14.0, t.text_muted)),
            )
            .child(div().flex_1())
            .child(
                btn("rail-find")
                    .mb(px(theme::SP2))
                    .when(can_find, |b| b.bg(t.accent).hover(|s| s.opacity(0.88)))
                    .when(can_find, |b| b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.find_location(cx))))
                    .child(icon(Icon::Search, 14.0, if can_find { t.accent_text } else { t.text_faint })),
            )
    }

    fn render_toolbar(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let v = self.view;
        let has = self.file.as_ref().is_some_and(|f| f.audio.is_some());
        let details = self.details;
        let editing = self.edit.is_some();
        let can_edit = has && self.stage == Stage::Done;
        // what fits the middle column: the view range text goes first, then the
        // long labels get short ones; the edit button is never cut (the view
        // controls before it shrink and clip instead)
        let w = self.center_w;
        let show_range = w >= 820.0;
        let short = w < 700.0;
        let view_controls = div()
            .flex()
            .items_center()
            .gap(px(theme::SP1))
            .min_w(px(0.))
            .overflow_hidden()
            .when(has && details, |d| {
                d.child(
                    toggle(t, "tg-whiten", "Whiten", self.whiten).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.whiten = !this.whiten;
                        cx.notify();
                    })),
                )
                .child(toggle(t, "tg-curves", "Curves", self.show_curves).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.show_curves = !this.show_curves;
                    cx.notify();
                })))
                .child(
                    toggle(t, "tg-fft", &if short { format!("{}", self.n_fft) } else { format!("FFT {}", self.n_fft) }, false).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.n_fft = match this.n_fft {
                                2048 => 4096,
                                4096 => 8192,
                                8192 => 16384,
                                _ => 2048,
                            };
                            cx.notify();
                        }),
                    ),
                )
                .child(div().w(px(1.)).h(px(16.)).flex_none().bg(t.border).mx(px(theme::SP1)))
            })
            .when(has, |d| {
                d.child(toggle(t, "tg-fit", "All", false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.zoom_all();
                    cx.notify();
                })))
                .child(toggle(t, "tg-zoomsel", if short { "Zoom" } else { "Zoom to fly-by" }, false).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        if let Some(i) = this.selected {
                            this.zoom_to(i);
                        }
                        cx.notify();
                    },
                )))
            });
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(theme::SP1))
            .h(px(theme::HEADER_H))
            .px(px(theme::PANEL_PX))
            .overflow_hidden()
            .bg(t.panel_bg)
            .border_b_1()
            .border_color(t.border)
            .child(div().flex_none().text_size(px(theme::TEXT_SM)).font_weight(FontWeight::MEDIUM).text_color(t.text_muted).mr(px(theme::SP2)).child("Spectrogram"))
            .when(has && details && show_range, |d| {
                d.child(
                    mono(format!(
                        "{} – {}   {:.0}–{:.0} Hz",
                        crate::draw::fmt_time(v.t0, 1.0),
                        crate::draw::fmt_time(v.t1, 1.0),
                        v.f0,
                        v.f1
                    ))
                    .flex_none()
                    .whitespace_nowrap()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(t.text_faint),
                )
            })
            .child(div().flex_1())
            .child(view_controls)
            .when(can_edit, |d| {
                d.child(div().w(px(1.)).h(px(16.)).flex_none().bg(t.border).mx(px(theme::SP1))).child(
                    button(t, "edit-btn", if editing { "Done editing" } else { "Edit fly-bys" }, None, editing, true)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            if this.edit.is_some() {
                                this.edit = None;
                            } else {
                                this.enter_edit(if this.detections.is_empty() { Tool::Trace } else { Tool::Move });
                            }
                            cx.notify();
                        })),
                )
            })
    }

    /// Tools of the "Edit fly-bys" mode, below the toolbar.
    fn render_edit_bar(&mut self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let e = self.edit.as_ref()?;
        let tool = e.tool;
        let n_pending = e.pending.len();
        let seg = |id: &'static str, label: &str, tl: Tool, cx: &mut Context<Self>| {
            let on = tool == tl;
            let hover = t.hover;
            div()
                .id(id)
                .flex()
                .items_center()
                .h(px(26.))
                .px(px(theme::SP3))
                .text_size(px(theme::TEXT_SM))
                .cursor_pointer()
                .rounded(px(theme::RADIUS_SM))
                .text_color(if on { t.text } else { t.text_muted })
                .when(on, |d| d.bg(t.surface).border_1().border_color(t.border_strong))
                .when(!on, |d| d.hover(move |s| s.bg(hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.enter_edit(tl);
                    cx.notify();
                }))
                .child(label.to_string())
        };
        let hint = match tool {
            Tool::Move => "Click a curve to select it · drag it to move · drag “steepness” sideways, “speed” up or down · on release it snaps to the line nearby",
            Tool::Trace => "Click along the faint line: 3–4 points before and 3–4 after its steep part · Backspace removes the last point",
        };
        let sel = self.selected.is_some();
        let busy = matches!(self.location, LocState::Running(_));
        let can_search = !self.detections.is_empty()
            && self.query(cx).is_ok()
            && !busy
            && (self.inputs_changed() || !matches!(self.location, LocState::Done(_)));
        // one row when the middle is wide enough, else two: the mode and
        // "Search again" first, the curve tools below (they wrap if needed)
        let w = self.center_w;
        let one_row = w >= if tool == Tool::Trace { 1060.0 } else { 800.0 };
        let short = w < 480.0;
        let title = div()
            .flex_none()
            .whitespace_nowrap()
            .text_size(px(theme::TEXT_SM))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.text)
            .mr(px(theme::SP1))
            .child("Edit fly-bys")
            .into_any_element();
        let modes = div()
            .flex()
            .flex_none()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(4.))
            .bg(t.field_bg)
            .border_1()
            .border_color(t.border)
            .child(seg("tool-move", if short { "Move" } else { "Move curves" }, Tool::Move, cx))
            .child(seg("tool-trace", if short { "Trace" } else { "Trace a curve" }, Tool::Trace, cx))
            .into_any_element();
        let mut tools: Vec<AnyElement> = vec![];
        if tool == Tool::Trace {
            let ok = n_pending >= core_edit::MIN_TRACE_POINTS;
            tools.push(
                button(t, "trace-fit", &format!("Fit traced curve ({n_pending})"), None, ok, ok)
                    .when(ok, |b| {
                        b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.fit_pending();
                            cx.notify();
                        }))
                    })
                    .into_any_element(),
            );
            tools.push(
                button(t, "trace-clear", "Clear points", None, false, n_pending > 0)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(e) = this.edit.as_mut() {
                            e.pending.clear();
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        tools.push(
            button(t, "edit-delete", "Delete curve", None, false, sel)
                .when(sel, |b| {
                    b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.delete_selected();
                        cx.notify();
                    }))
                })
                .into_any_element(),
        );
        tools.push(
            button(t, "edit-undo", "Undo", None, false, !self.undo.is_empty())
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.do_undo();
                    cx.notify();
                }))
                .into_any_element(),
        );
        let files: Vec<AnyElement> = vec![
            toggle(t, "edit-load", "Load…", false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.load_dets(cx))).into_any_element(),
            toggle(t, "edit-save", "Save…", false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save_dets(cx))).into_any_element(),
        ];
        let search = button(t, "edit-search", "Search again", Some(Icon::Search), true, can_search)
            .when(can_search, |b| b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.find_location(cx))))
            .into_any_element();
        let row = || div().flex().items_center().gap(px(theme::SP2)).px(px(theme::PANEL_PX));
        let rows: Vec<AnyElement> = if one_row {
            vec![row()
                .h(px(theme::HEADER_H + theme::SP1))
                .child(title)
                .child(modes)
                .children(tools)
                .child(div().flex_1())
                .children(files)
                .child(search)
                .into_any_element()]
        } else {
            // fixed rows, no wrapping: a wrapping row is measured as if every
            // button had a line of its own, which left a tall empty bar over a
            // squeezed spectrogram in a narrow middle
            let mut v = vec![row().h(px(theme::HEADER_H + theme::SP1)).child(title).child(modes).child(div().flex_1()).child(search).into_any_element()];
            if w >= 600.0 {
                v.push(row().pb(px(theme::SP2)).overflow_hidden().children(tools).child(div().flex_1()).children(files).into_any_element());
            } else {
                v.push(row().pb(px(theme::SP2)).overflow_hidden().children(tools).into_any_element());
                v.push(row().pb(px(theme::SP2)).children(files).into_any_element());
            }
            v
        };
        Some(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .bg(t.accent_soft)
                .border_b_1()
                .border_color(t.border)
                .children(rows)
                .child(
                    div()
                        .px(px(theme::PANEL_PX))
                        .pb(px(theme::SP2))
                        .text_size(px(theme::TEXT_XS))
                        .text_color(t.text_muted)
                        .child(hint),
                )
                .into_any_element(),
        )
    }

    fn render_spectrogram(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(file) = self.file.as_ref() else {
            return self.render_empty(t, cx).into_any_element();
        };
        if file.overview.is_none() {
            return self.render_progress_card(t, cx).into_any_element();
        }
        let clock_start = match self.time_hint(cx).map(|q| q.0) {
            Ok(TimeHint::ExactStart(s)) => Some(s as f64),
            _ => None,
        };
        let scene = SpecScene {
            view: self.view,
            tile: self.tile.clone(),
            fallback: file.overview.clone(),
            curves: self.curves.clone(),
            selected: self.selected,
            hover: self.hover,
            cursor_t: self.cursor_t,
            show_curves: self.show_curves,
            clock_start,
            theme: *t,
            loading: self.tile_task.is_some(),
            edit: self.edit.as_ref().map(|e| EditOverlay {
                handles: e.tool == Tool::Move,
                pending: e.pending.clone(),
                refitting: self.refitting.is_some() && self.refitting == self.selected,
            }),
            models: self.model_overlays(t),
            cuts: self.cuts.clone(),
            suggested_cuts: vec![],
        };
        let ov = OverviewScene {
            image: file.overview.clone(),
            envelope: file.envelope.clone(),
            duration: self.duration(),
            view: self.view,
            markers: self.detections.iter().map(|d| (d.flyby.tc, d.likely_ground_vehicle)).collect(),

            selected: self.selected,
            cursor_t: self.cursor_t,
            theme: *t,
            cuts: self.cuts.clone(),
        };
        let plot_cell = self.plot.clone();
        let ov_cell = self.overview_rect.clone();
        let logged = self.logged_view.clone();
        let _ = window;
        let empty = (self.stage == Stage::Done && self.detections.is_empty() && self.edit.is_none()).then(|| self.render_no_flybys(t, cx));
        let cursor = match &self.edit {
            Some(e) if e.tool == Tool::Trace => CursorStyle::Crosshair,
            Some(_) if self.curve_drag.is_some() => CursorStyle::ClosedHand,
            _ if self.drag.as_ref().is_some_and(|d| d.moved) => CursorStyle::ClosedHand,
            _ => CursorStyle::Crosshair,
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(200.))
            .child(
                div()
                    .id("overview")
                    .h(px(OVERVIEW_H))
                    .flex_none()
                    .border_b_1()
                    .border_color(t.border)
                    .cursor(CursorStyle::PointingHand)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                            this.overview_drag = true;
                            this.overview_seek(ev.position);
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                        if this.overview_drag && ev.pressed_button == Some(MouseButton::Left) {
                            this.overview_seek(ev.position);
                            cx.notify();
                        } else {
                            this.overview_drag = false;
                        }
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, _| this.overview_drag = false))
                    .child(
                        canvas(
                            move |b, _, _| {
                                ov_cell.set(b);
                            },
                            move |b, _, window, cx| spectro::paint_overview(b, &ov, window, cx),
                        )
                        .size_full(),
                    ),
            )
            .child(
                div()
                    .id("spec")
                    .flex_1()
                    .relative()
                    .cursor(cursor)
                    .on_scroll_wheel(cx.listener(Self::on_spec_scroll))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_spec_down))
                    .on_mouse_move(cx.listener(Self::on_spec_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_spec_up))
                    .on_hover(cx.listener(|this, h: &bool, _, cx| {
                        if !*h {
                            this.hover = None;
                            cx.notify();
                        }
                    }))
                    .child(
                        canvas(
                            move |_, _, _| {},
                            move |b, _, window, cx| {
                                let plot = spectro::paint(b, &scene, window, cx);
                                if plot_cell.get() != plot || logged.get() != Some(scene.view) {
                                    logged.set(Some(scene.view));
                                    log::info!(
                                        "spectrogram plot at {:.0},{:.0} size {:.0}x{:.0} view {:.3} {:.3} {:.3} {:.3}",
                                        f32::from(plot.origin.x),
                                        f32::from(plot.origin.y),
                                        f32::from(plot.size.width),
                                        f32::from(plot.size.height),
                                        scene.view.t0,
                                        scene.view.t1,
                                        scene.view.f0,
                                        scene.view.f1
                                    );
                                }
                                if plot_cell.get() != plot {
                                    plot_cell.set(plot);
                                    window.refresh();
                                }
                            },
                        )
                        .size_full(),
                    )
                    .children(empty),
            )
            .into_any_element()
    }

    /// Over the spectrogram when the detector found nothing: why, and what to do.
    fn render_no_flybys(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let item = |s: &str| {
            div().flex().gap(px(theme::SP2)).child(div().flex_none().text_color(t.text_faint).child("·")).child(div().flex_1().min_w(px(0.)).child(s.to_string()))
        };
        // in a corner, not over the middle where a faint line would be
        let busy = self.background.filter(Background::mostly_busy).map(|b| b.busy_fraction * 100.0);
        let card = div()
            .absolute()
            .top(px(8.))
            .right(px(8.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .rounded(px(theme::RADIUS + 2.0))
            .border_1()
            .border_color(t.border_strong)
            .bg(t.panel_bg.opacity(0.94));
        // a short or narrow spectrogram: the card starts folded (one row with the
        // trace button), and unfolded it scrolls instead of being cut
        let pb = self.plot.get();
        let (pw, ph) = (f32::from(pb.size.width), f32::from(pb.size.height));
        let small = ph > 0.0 && (ph < 420.0 || pw < 520.0);
        if self.noflyby_folded || (small && !self.noflyby_unfolded) {
            return card
                .px(px(theme::SP3))
                .py(px(theme::SP2))
                .flex()
                .items_center()
                .gap(px(theme::SP2))
                .text_size(px(theme::TEXT_SM))
                .child(div().font_weight(FontWeight::MEDIUM).text_color(t.text).child("No aircraft fly-bys found"))
                .child(toggle(t, "empty-unfold", "Why?", false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.noflyby_folded = false;
                    this.noflyby_unfolded = true;
                    cx.notify();
                })))
                .child(button(t, "empty-trace", "Trace by hand", None, true, true).h(px(theme::CONTROL_H)).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.enter_edit(Tool::Trace);
                    cx.notify();
                })))
                .into_any_element();
        }
        card.id("noflyby-card")
            .w(px(380.))
            .when(ph > 0.0, |d| d.max_h(px((ph - 16.0).max(120.0))))
            .overflow_y_scroll()
            .p(px(theme::SP4))
            .flex()
            .flex_col()
            .gap(px(theme::SP2))
            .text_size(px(theme::TEXT_SM))
            .text_color(t.text_muted)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(theme::TEXT_LG)).font_weight(FontWeight::MEDIUM).text_color(t.text).child("No aircraft fly-bys found"))
                    .child(toggle(t, "empty-fold", "Hide", false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.noflyby_folded = true;
                        this.noflyby_unfolded = false;
                        cx.notify();
                    }))),
            )
            .when_some(busy, |d, pct| {
                d.child(
                    div()
                        .p(px(theme::SP2))
                        .rounded(px(theme::RADIUS))
                        .bg(t.surface)
                        .border_l_2()
                        .border_color(t.warn)
                        .text_color(t.text)
                        .child(format!(
                            "Talking or music fills {pct:.0} % of this recording. It covers faint aircraft tones: only a loud, close fly-by would show."
                        )),
                )
            })
            .child(div().child("The location search needs at least one propeller aircraft or helicopter passing by. This can happen when:"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(item("no propeller aircraft or helicopter passed close (jets are not used: they have no clear tone line),"))
                    .child(item("talking, music, traffic, wind or machinery covers the tone,"))
                    .child(item("an aircraft turned overhead.")),
            )
            .child(div().text_color(t.text).child("What you can do"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(item("Look for a faint line that falls from a higher to a lower tone, and trace it by hand."))
                    .child(item("Record longer (20 min or more) away from roads, talking and music, outdoors or at an open window.")),
            )
            .child(
                div().flex().gap(px(theme::SP2)).mt(px(theme::SP1)).child(
                    button(t, "empty-trace", "Trace a curve by hand", None, true, true).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.enter_edit(Tool::Trace);
                        cx.notify();
                    })),
                ),
            )
            .into_any_element()
    }

    fn render_empty(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = t.hover;
        let now = now_unix();
        let recent: Vec<AnyElement> = self
            .history
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let exists = !e.missing;
                let (open_path, forget_path) = (e.path.clone(), e.path.clone());
                // what is known about it: day, region, length, fly-bys
                let mut facts: Vec<String> = vec![];
                if !e.form.day.is_empty() {
                    facts.push(if e.form.day_end.is_empty() { e.form.day.clone() } else { format!("{} – {}", e.form.day, e.form.day_end) });
                }
                facts.extend(e.form.region());
                facts.extend(e.duration_s.map(draw_time_long));
                facts.extend(e.flybys.map(|n| if n == 1 { "1 fly-by".to_string() } else { format!("{n} fly-bys") }));
                let (answer, answer_color) = match &e.result {
                    _ if !exists => ("File not found".to_string(), t.warn),
                    Some(f) if f.confident => (format!("Found {:.4}, {:.4}", f.lat, f.lon), t.ok),
                    Some(f) => (format!("{} possible places", f.places), t.warn),
                    None if e.searched => ("No place found".to_string(), t.text_muted),
                    None => (String::new(), t.text_muted),
                };
                let group: SharedString = format!("recent-{i}").into();
                div()
                    .id(("recent", i))
                    .group(group.clone())
                    .flex()
                    .items_center()
                    .gap(px(theme::SP3))
                    .px(px(theme::SP3))
                    .py(px(theme::SP2))
                    .rounded(px(theme::RADIUS))
                    .when(exists, |d| d.cursor_pointer().hover(move |s| s.bg(hover)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_recent(open_path.clone(), cx)))
                    .child(
                        div()
                            .size(px(32.))
                            .flex_none()
                            .rounded(px(theme::RADIUS))
                            .bg(t.surface)
                            .border_1()
                            .border_color(t.border)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(Icon::Wave, 16.0, if exists { t.text_muted } else { t.text_faint })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(theme::TEXT))
                                    .text_color(if exists { t.text } else { t.text_muted })
                                    .child(e.name.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(theme::SP2))
                                    .text_size(px(theme::TEXT_SM))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .when(!answer.is_empty(), |d| {
                                        d.child(div().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(answer_color).child(answer))
                                    })
                                    .when(!facts.is_empty(), |d| {
                                        d.child(div().flex_1().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(t.text_muted).child(facts.join(" · ")))
                                    }),
                            ),
                    )
                    .child(div().flex_none().text_size(px(theme::TEXT_SM)).text_color(t.text_faint).child(history::ago(e.opened, now)))
                    // forget it: shown on hover
                    .child(
                        div()
                            .id(("recent-forget", i))
                            .size(px(theme::CONTROL_H))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(theme::RADIUS_SM))
                            .invisible()
                            .group_hover(group, |s| s.visible())
                            .hover(|s| s.bg(t.border_strong))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.forget_recent(&forget_path, cx);
                            }))
                            .child(icon(Icon::Close, theme::ICON_SM, t.text_muted)),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .id("empty")
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .items_center()
            .overflow_y_scroll()
            .bg(t.app_bg)
            .p(px(theme::SP6))
            // centred while it fits, scrolled when the window is short
            .child(div().flex_1())
            .child(
                div()
                    .w_full()
                    .max_w(px(560.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(theme::SP6))
                    .child(
                        div()
                            .id("dropzone")
                            .h(px(168.))
                            .rounded(px(10.))
                            .border_1()
                            .border_dashed()
                            .border_color(t.border_strong)
                            .bg(t.panel_bg)
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_dialog(cx)))
                            .child(
                                div()
                                    .size(px(44.))
                                    .mb(px(theme::SP3))
                                    .rounded(px(11.))
                                    .bg(theme::logo_background())
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(icon(Icon::Logo, 44.0, gpui::white())),
                            )
                            .child(div().text_size(px(theme::TEXT_LG)).font_weight(FontWeight::MEDIUM).text_color(t.text).child("Drop a recording here"))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(theme::SP2))
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(t.text_muted)
                                    .child("or click to open")
                                    .child(mono("Ctrl O").text_size(px(theme::TEXT_XS)).text_color(t.text_faint)),
                            ),
                    )
                    .when(!recent.is_empty(), |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(theme::SP1))
                                .child(
                                    div()
                                        .px(px(theme::SP3))
                                        .pb(px(theme::SP1))
                                        .text_size(px(theme::TEXT_SM))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(t.text_muted)
                                        .child("Recent"),
                                )
                                .children(recent),
                        )
                    }),
            )
            .child(div().flex_1())
    }

    fn render_progress_card(&self, t: &Theme, cx: &App) -> impl IntoElement {
        let rows = self.stage_rows(t);
        let (loc_note, loc_color) = if self.stage == Stage::Failed {
            ("not run", t.text_faint)
        } else if self.locate_queued {
            ("queued", t.accent)
        } else if self.query(cx).is_ok() {
            ("ready", t.text_muted)
        } else {
            ("needs the day", t.text_faint)
        };
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .p(px(theme::SP6))
            .bg(t.app_bg)
            .child(
                div()
                    .w_full()
                    .max_w(px(440.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.panel_bg)
                    .p(px(theme::SP4))
                    .flex()
                    .flex_col()
                    .gap(px(theme::SP3))
                    .child(div().text_size(px(theme::TEXT)).text_color(t.text).child("Analysing recording"))
                    .children(rows)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(theme::SP3))
                            .child(div().w(px(14.)).flex().justify_center().child(
                                div().w(px(8.)).h(px(8.)).rounded(px(4.)).border_1().border_color(t.border_strong),
                            ))
                            .child(div().flex_1().text_size(px(theme::TEXT_SM)).text_color(t.text_faint).child("Match ADS-B → location"))
                            .child(mono(loc_note).text_size(px(theme::TEXT_XS)).text_color(loc_color)),
                    )
                    .child(
                        div()
                            .pt(px(theme::SP1))
                            .text_size(px(theme::TEXT_XS))
                            .text_color(t.text_faint)
                            .child("Fill in the day (and, if you can, the hours and region) while this runs."),
                    )
                    .children(self.error.clone().map(|e| div().text_size(px(theme::TEXT_SM)).text_color(t.danger).child(e))),
            )
    }

    /// Expected duration of a stage (s), when we can guess it.
    fn stage_estimate(&self, s: Stage) -> Option<f64> {
        match s {
            Stage::Decoding => self.file.as_ref().map(|f| analysis::decode_estimate_s(&f.path, f.size_bytes)),
            Stage::Detecting if !self.detections_from_file && self.duration() > 0.0 => {
                Some(self.duration() * analysis::DETECT_S_PER_AUDIO_S)
            }
            _ => None,
        }
    }

    fn stage_rows(&self, t: &Theme) -> Vec<AnyElement> {
        let stages = [Stage::Decoding, Stage::Overview, Stage::Detecting];
        stages
            .iter()
            .map(|&s| {
                let done = self.stage_times.iter().find(|(st, _)| *st == s).map(|x| x.1);
                let running = self.stage == s;
                let failed_here = self.stage == Stage::Failed && self.failed_stage == Some(s);
                let skipped = self.stage == Stage::Failed && done.is_none() && !failed_here;
                let (glyph, color): (AnyElement, Hsla) = if done.is_some() {
                    (icon(Icon::Check, 13.0, t.ok).into_any_element(), t.text)
                } else if running {
                    (div().w(px(8.)).h(px(8.)).rounded(px(4.)).bg(t.accent).into_any_element(), t.text)
                } else if failed_here {
                    (icon(Icon::Close, 13.0, t.danger).into_any_element(), t.danger)
                } else {
                    (div().w(px(8.)).h(px(8.)).rounded(px(4.)).border_1().border_color(t.border_strong).into_any_element(), t.text_faint)
                };
                let est = self.stage_estimate(s);
                let el = self.stage_started.map_or(0.0, |s| s.elapsed().as_secs_f64());
                let right = if let Some(d) = done {
                    fmt_dur(d)
                } else if failed_here {
                    "failed".to_string()
                } else if skipped {
                    "skipped".to_string()
                } else if running {
                    match est {
                        Some(e) => format!("{} / est. {}", fmt_dur(el), fmt_dur(e)),
                        None => fmt_dur(el),
                    }
                } else {
                    String::new()
                };
                let bar = running.then_some(est).flatten().map(|e| progress_bar(t, (el / e.max(0.5)).min(0.97) as f32));
                div()
                    .flex()
                    .flex_col()
                    .gap(px(theme::SP2))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(theme::SP3))
                            .child(div().w(px(14.)).flex().justify_center().child(glyph))
                            .child(div().flex_1().text_size(px(theme::TEXT_SM)).text_color(color).child(s.label()))
                            .child(mono(right).text_size(px(theme::TEXT_XS)).text_color(t.text_muted)),
                    )
                    .children(bar.map(|b| div().pl(px(theme::SP6)).child(b)))
                    .into_any_element()
            })
            .collect()
    }

    /// Matched aircraft per fly-by at the selected location (None before a
    /// search, or when the fly-bys changed since).
    fn matches(&self) -> Option<Vec<Option<String>>> {
        match &self.location {
            LocState::Done(_) if self.dets_changed => None,
            LocState::Done(r) => r.candidates.get(self.loc_selected).map(|c| {
                // not sure where: these are only the aircraft of the selected place
                let maybe = if r.ambiguous().is_some() { "maybe " } else { "" };
                (0..self.detections.len())
                    .map(|i| {
                        c.tracks.iter().find(|tr| tr.detection == i).map(|tr| match &tr.type_code {
                            Some(ty) => format!("{maybe}{} {ty}", tr.callsign),
                            None => format!("{maybe}{}", tr.callsign),
                        })
                    })
                    .collect()
            }),
            _ => None,
        }
    }

    fn empty_table_msg(&self) -> String {
        match self.stage {
            Stage::Idle => "Open a recording to find fly-bys.".to_string(),
            Stage::Done if self.edit.is_some() => "No fly-bys yet: click points along a faint line, then Fit traced curve.".to_string(),
            Stage::Done if !self.deleted.is_empty() => "All fly-bys are deleted: Restore one in Edit fly-bys, or trace one by hand.".to_string(),
            Stage::Done => "No aircraft fly-bys found in this recording. Trace one by hand (Edit fly-bys) or record longer in a quieter spot.".to_string(),
            Stage::Failed => self.error.clone().unwrap_or_default(),
            _ => {
                let el = self.stage_started.map_or(0.0, |s| s.elapsed().as_secs_f64());
                if self.detections_from_file {
                    "Loading detections…".to_string()
                } else if self.stage != Stage::Detecting {
                    format!("{}… {}", self.stage.label(), fmt_dur(el))
                } else {
                    format!("Detecting fly-bys… {} of est. {}", fmt_dur(el), fmt_dur(self.duration() * analysis::DETECT_S_PER_AUDIO_S))
                }
            }
        }
    }

    /// The simple table: time, aircraft, a confidence dot.
    fn render_table_simple(&mut self, t: &Theme, height: f32, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let order = self.sorted_order();
        let sample = matches!(&self.location, LocState::Done(r) if r.is_sample);
        // not sure where: the aircraft are only those of the selected place
        let unsure = matches!(&self.location, LocState::Done(r) if r.ambiguous().is_some());
        let matches = self.matches();
        let aircraft_head = match (&matches, unsure) {
            (None, _) => "Aircraft".to_string(),
            (Some(_), false) => "Aircraft (ADS-B)".to_string(),
            (Some(_), true) => format!("Aircraft if place {} is right", self.loc_selected + 1),
        };
        let heard_w = if compact { 76.0 } else { 100.0 };
        let head = |s: &str| div().px(px(theme::SP1)).whitespace_nowrap().overflow_hidden().text_color(t.text_faint).child(s.to_string());
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(theme::ROW_H))
            .px(px(theme::SP2))
            .border_b_1()
            .border_color(t.border)
            .text_size(px(theme::TEXT_XS))
            .child(div().w(px(22.)).flex_none())
            .child(head("#").w(px(30.)).flex_none())
            .child(head("Heard at").w(px(heard_w)).flex_none())
            .child(head(&aircraft_head).flex_1().min_w(px(0.)).max_w(px(AIRCRAFT_MAX_W)))
            .when(!compact, |d| d.child(head("").w(px(140.)).flex_none()));
        let body: AnyElement = if self.detections.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px(px(theme::SP6))
                .text_size(px(theme::TEXT_SM))
                .text_color(t.text_faint)
                // a block as wide as the table, so the message wraps instead of being cut
                .child(div().w_full().text_center().child(self.empty_table_msg()))
                .into_any_element()
        } else {
            let rows: Vec<AnyElement> = order
                .iter()
                .map(|&i| {
                    let d = &self.detections[i];
                    let sel = self.selected == Some(i);
                    let hover = t.hover;
                    let (dot, what) = flyby_quality(t, d);
                    let aircraft = match matches.as_ref().map(|m| m[i].clone()) {
                        None => "—".to_string(),
                        Some(None) => "no match".to_string(),
                        Some(Some(c)) if sample => format!("{c}?"),
                        Some(Some(c)) => c,
                    };
                    let matched = matches.as_ref().is_some_and(|m| m[i].is_some());
                    div()
                        .id(("srow", i))
                        .flex()
                        .items_center()
                        .h(px(theme::ROW_H))
                        .px(px(theme::SP2))
                        .relative()
                        .text_size(px(theme::TEXT_SM))
                        .border_b_1()
                        .border_color(t.border.opacity(0.6))
                        .cursor_pointer()
                        .when(sel, |r| r.bg(t.selected))
                        .when(!sel, |r| r.hover(move |s| s.bg(hover)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            window.focus(&this.nav_focus);
                            this.select(Some(i), true);
                            cx.notify();
                        }))
                        .when(sel, |r| r.child(div().absolute().left(px(0.)).top(px(0.)).bottom(px(0.)).w(px(2.)).bg(t.accent)))
                        .child(div().w(px(22.)).flex_none().flex().justify_center().child(div().size(px(8.)).rounded(px(4.)).bg(dot)))
                        .child(div().w(px(30.)).flex_none().px(px(theme::SP1)).font_family(MONO).text_color(t.text_faint).child(format!("{}", i + 1)))
                        .child(div().w(px(heard_w)).flex_none().px(px(theme::SP1)).font_family(MONO).whitespace_nowrap().child(crate::draw::fmt_time(d.flyby.tc, 1.0)))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                // a wide table keeps "what" next to the aircraft, not at the far edge
                                .max_w(px(AIRCRAFT_MAX_W))
                                .px(px(theme::SP1))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_color(if matched { t.text } else { t.text_faint })
                                .child(aircraft),
                        )
                        .when(!compact, |r| r.child(
                            div()
                                .w(px(140.))
                                .flex_none()
                                .px(px(theme::SP1))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(t.text_muted)
                                .child(what),
                        ))
                        .into_any_element()
                })
                .collect();
            div().id("table-body").flex_1().overflow_y_scroll().children(rows).into_any_element()
        };
        let n = self.detections.len();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .h(px(height))
            .bg(t.panel_bg)
            .child(section_header(
                t,
                "Fly-bys",
                Some(
                    div()
                        .flex()
                        .gap(px(theme::SP3))
                        .text_size(px(theme::TEXT_XS))
                        .text_color(t.text_faint)
                        .items_center()
                        .when(n > 0, |d| d.child(mono(format!("{n} found"))))
                        .when(self.dets_changed && matches!(self.location, LocState::Done(_)), |d| {
                            d.child(div().text_color(t.warn).child("changed since the search"))
                        })
                        .children(self.edit_entry(t, cx))
                        .into_any_element(),
                ),
            ))
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn render_table(&mut self, t: &Theme, height: f32, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        if !self.details {
            return self.render_table_simple(t, height, compact, cx);
        }
        let order = self.sorted_order();
        let sample = matches!(&self.location, LocState::Done(r) if r.is_sample);
        let matches: Option<Vec<Option<String>>> = match &self.location {
            LocState::Done(_) if self.dets_changed => None,
            LocState::Done(r) => r.candidates.get(self.loc_selected).map(|c| {
                (0..self.detections.len())
                    .map(|i| {
                        c.tracks.iter().find(|tr| tr.detection == i).map(|tr| match &tr.type_code {
                            Some(ty) => format!("{} {ty}", tr.callsign),
                            None => tr.callsign.clone(),
                        })
                    })
                    .collect()
            }),
            _ => None,
        };
        let adsb_head = if sample {
            "ADS-B · sample".to_string()
        } else if matches.is_some() {
            format!("ADS-B · location {}", self.loc_selected + 1)
        } else {
            "ADS-B".to_string()
        };
        // columns that fit the middle: the spans (lines, km/h) go first, then
        // the contrast, then frequency and distance; the aircraft always stays
        let w = self.center_w;
        let wide = w >= 820.0;
        // (each step keeps the flags column at least ~90 px wide)
        let show_contrast = w >= 740.0;
        let show_fd = w >= 600.0;
        let show_flags = w >= 440.0;
        let adsb_w = if show_contrast { ADSB_COL_W } else { 120.0 };
        let (sort_key, asc) = self.sort;
        let header_cell =|id: &'static str, label: &str, w: Option<f32>, key: Option<SortKey>, right: bool, cx: &mut Context<Self>| {
            let active = key == Some(sort_key);
            let arrow = if active { if asc { " ↑" } else { " ↓" } } else { "" };
            div()
                .id(id)
                .flex()
                .items_center()
                .when(right, |d| d.justify_end())
                .map(|d| match w {
                    Some(w) => d.w(px(w)).flex_none(),
                    None => d.flex_1(),
                })
                .px(px(theme::SP1))
                .whitespace_nowrap()
                .overflow_hidden()
                .text_color(if active { t.text } else { t.text_faint })
                .when_some(key, |d, k| {
                    d.cursor_pointer().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if this.sort.0 == k {
                            this.sort.1 = !this.sort.1;
                        } else {
                            this.sort = (k, true);
                        }
                        cx.notify();
                    }))
                })
                .child(format!("{label}{arrow}"))
        };
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(theme::ROW_H))
            .px(px(theme::SP2))
            .border_b_1()
            .border_color(t.border)
            .text_size(px(theme::TEXT_XS))
            .child(div().w(px(30.)).flex_none())
            .child(header_cell("h-n", "#", Some(30.), None, false, cx))
            .child(header_cell("h-tc", "Heard CPA", Some(92.), Some(SortKey::Tc), true, cx))
            .when(show_fd, |d| d.child(header_cell("h-f0", "Rest freq.", Some(84.), Some(SortKey::F0), true, cx)))
            .when(wide, |d| d.child(header_cell("h-lines", "Lines", Some(70.), None, true, cx)))
            .child(header_cell("h-v", "Speed", Some(62.), Some(SortKey::Speed), true, cx))
            .when(wide, |d| d.child(div().w(px(48.)).flex_none()))
            .when(show_fd, |d| d.child(header_cell("h-d", "Distance", Some(84.), Some(SortKey::Dist), true, cx)))
            .when(show_contrast, |d| d.child(header_cell("h-c", "Contrast", Some(72.), Some(SortKey::Contrast), true, cx)))
            .map(|d| if show_flags { d.child(header_cell("h-flags", "Flags", None, None, false, cx).min_w(px(0.))) } else { d.child(div().flex_1()) })
            .child(header_cell("h-adsb", &adsb_head, Some(adsb_w), None, false, cx));

        let body: AnyElement = if self.detections.is_empty() {
            let msg = self.empty_table_msg();
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px(px(theme::SP6))
                .text_size(px(theme::TEXT_SM))
                .text_color(t.text_faint)
                .child(div().w_full().text_center().child(msg))
                .into_any_element()
        } else {
            let rows: Vec<AnyElement> = order
                .iter()
                .map(|&i| {
                    let d = &self.detections[i];
                    let sel = self.selected == Some(i);
                    let hover = t.hover;
                    // The column fits about two multiples; longer lists show their span
                    // (the detail panel lists every line).
                    let lines = if d.harmonics.len() > 2 {
                        let lo = d.harmonics.iter().cloned().fold(f64::INFINITY, f64::min);
                        let hi = d.harmonics.iter().cloned().fold(0.0, f64::max);
                        format!("×{lo:.0}…×{hi:.0}")
                    } else if d.harmonics.len() > 1 {
                        d.harmonics.iter().map(|h| format!("×{h:.0}")).collect::<Vec<_>>().join(" ")
                    } else if !d.other_lines.is_empty() {
                        format!("{}", 1 + d.other_lines.len())
                    } else {
                        "1".into()
                    };
                    let adsb = matches.as_ref().map(|m| m[i].clone());
                    let mut flags: Vec<AnyElement> = vec![];
                    if d.likely_ground_vehicle {
                        flags.push(pill(t, "ground?", t.warn).into_any_element());
                    }
                    if !d.confident {
                        let which: Vec<&str> = ["v", "d", "tc"]
                            .iter()
                            .zip(d.ranges.clipped)
                            .filter(|(_, c)| *c)
                            .map(|(n, _)| *n)
                            .collect();
                        let label = if wide { format!("open range {}", which.join(" ")) } else { "open range".to_string() };
                        flags.push(pill(t, &label, t.text_muted).into_any_element());
                    }
                    if d.harmonics.len() > 1 {
                        flags.push(pill(t, "harmonics", t.text_muted).into_any_element());
                    }
                    match d.origin {
                        Origin::Traced => flags.insert(0, pill(t, "traced", t.ok).into_any_element()),
                        Origin::Edited => flags.insert(0, pill(t, "edited", t.accent).into_any_element()),
                        Origin::Detected => {}
                    }
                    // a narrow table keeps the first flag and counts the others
                    // (the fly-by's details list them all), so no pill is cut
                    if !wide && flags.len() > 1 {
                        let more = flags.len() - 1;
                        flags.truncate(1);
                        flags.push(div().flex_none().text_size(px(theme::TEXT_XS)).text_color(t.text_faint).child(format!("+{more}")).into_any_element());
                    }
                    let num = |s: String, w: f32| {
                        div()
                            .w(px(w))
                            .flex_none()
                            .px(px(theme::SP1))
                            .flex()
                            .justify_end()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .font_family(MONO)
                            .child(s)
                    };
                    div()
                        .id(("row", i))
                        .flex()
                        .items_center()
                        .h(px(theme::ROW_H))
                        .px(px(theme::SP2))
                        .relative()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(if d.likely_ground_vehicle || !d.confident { t.text_muted } else { t.text })
                        .border_b_1()
                        .border_color(t.border.opacity(0.6))
                        .cursor_pointer()
                        .when(sel, |r| r.bg(t.selected))
                        .when(!sel, |r| r.hover(move |s| s.bg(hover)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            window.focus(&this.nav_focus);
                            this.select(Some(i), true);
                            cx.notify();
                        }))
                        .when(sel, |r| {
                            r.child(div().absolute().left(px(0.)).top(px(0.)).bottom(px(0.)).w(px(2.)).bg(t.accent))
                        })
                        .child(div().w(px(30.)).flex_none().pl(px(theme::SP1)).child(confidence_glyph(t, d)))
                        .child(div().w(px(30.)).flex_none().px(px(theme::SP1)).font_family(MONO).text_color(t.text_faint).child(format!("{}", i + 1)))
                        .child(num(crate::draw::fmt_time(d.flyby.tc, 0.1), 92.))
                        .when(show_fd, |r| r.child(num(format!("{:.1} Hz", d.flyby.f0), 84.)))
                        .when(wide, |r| r.child(num(lines, 70.).text_color(t.text_muted)))
                        .child(num(format!("{:.0} m/s", d.flyby.v), 62.))
                        .when(wide, |r| r.child(
                            div()
                                .w(px(48.))
                                .flex_none()
                                .pr(px(theme::SP2))
                                .flex()
                                .justify_end()
                                .font_family(MONO)
                                .text_size(px(theme::TEXT_XS))
                                .text_color(t.text_faint)
                                .child(format!("{:.0}k/h", d.flyby.v * 3.6)),
                        ))
                        .when(show_fd, |r| r.child(num(fmt_dist(d.flyby.d), 84.)))
                        .when(show_contrast, |r| r.child(num(format!("{:.1}σ", d.contrast), 72.).text_color(t.text_muted)))
                        .child(div().flex_1().min_w(px(0.)).flex().items_center().gap(px(theme::SP1)).px(px(theme::SP1)).overflow_hidden().when(show_flags, |d| d.children(flags)))
                        .child(
                            div()
                                .w(px(adsb_w))
                                .flex_none()
                                .px(px(theme::SP1))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .font_family(MONO)
                                .text_color(match &adsb {
                                    Some(Some(_)) if sample => t.warn.opacity(0.8),
                                    Some(Some(_)) => t.text,
                                    _ => t.text_faint,
                                })
                                .child(match adsb {
                                    None => "—".to_string(),
                                    Some(None) => "no match".to_string(),
                                    // Invented by the sample source: never let it pass for a real match.
                                    Some(Some(c)) if sample => format!("{c}?"),
                                    Some(Some(c)) => c,
                                }),
                        )
                        .into_any_element()
                })
                .collect();
            div().id("table-body").flex_1().overflow_y_scroll().children(rows).into_any_element()
        };

        let n = self.detections.len();
        let flagged = self.detections.iter().filter(|d| d.likely_ground_vehicle).count();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .h(px(height))
            .bg(t.panel_bg)
            .child(section_header(
                t,
                "Fly-bys",
                Some(
                    div()
                        .flex()
                        .gap(px(theme::SP3))
                        .text_size(px(theme::TEXT_XS))
                        .text_color(t.text_faint)
                        .when(n > 0, |d| {
                            d.child(mono(format!("{n} found")))
                                .when(flagged > 0, |d| d.child(mono(format!("{flagged} ground?")).text_color(t.warn)))
                                .when(self.detections_from_file, |d| d.child("from JSON"))
                        })
                        .when(w >= 700.0, |d| d.child("↑↓ select · F zoom · 0 all"))
                        .items_center()
                        .children(self.edit_entry(t, cx))
                        .into_any_element(),
                ),
            ))
            .child(header)
            .child(body)
            .into_any_element()
    }

    /// "Edit fly-bys" in the fly-by table's header while the spectrogram (and
    /// its toolbar with the same button) is hidden: fixing, cutting and tracing
    /// curves stays one click away in the map-first view.
    fn edit_entry(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let has = self.file.as_ref().is_some_and(|f| f.audio.is_some());
        if self.spectro_shown() || !has || self.stage != Stage::Done {
            return None;
        }
        Some(
            toggle(t, "table-edit", "Edit fly-bys", false)
                .text_color(t.accent)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.enter_edit(if this.detections.is_empty() { Tool::Trace } else { Tool::Move });
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// The selected fly-by in a few plain words (no expert numbers).
    fn render_detail_simple(&mut self, t: &Theme) -> AnyElement {
        let Some(i) = self.selected else {
            return div()
                .flex_none()
                .h(px(96.))
                .flex()
                .items_center()
                .justify_center()
                .px(px(theme::SP6))
                .border_t_1()
                .border_color(t.border)
                .text_size(px(theme::TEXT_SM))
                .text_color(t.text_faint)
                .child(if self.detections.is_empty() {
                    "Fly-by details appear here."
                } else if self.spectro_shown() {
                    "Select a fly-by in the table or click its curve."
                } else {
                    "Select a fly-by in the table to see it here."
                })
                .into_any_element();
        };
        let d = self.detections[i].clone();
        let f = d.flyby;
        let (dot, what) = flyby_quality(t, &d);
        let aircraft = self.matches().and_then(|m| m.get(i).cloned().flatten());
        let kv = |k: &str, v: String| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .h(px(theme::KV_H))
                .text_size(px(theme::TEXT_SM))
                .child(div().text_color(t.text_muted).child(k.to_string()))
                .child(div().text_color(t.text).whitespace_nowrap().child(v))
        };
        div()
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(t.border)
            .pb(px(theme::SP3))
            .child(section_header(t, &format!("Fly-by {}", i + 1), Some(div().flex().items_center().gap(px(theme::SP2)).text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child(div().size(px(8.)).rounded(px(4.)).bg(dot)).child(what).into_any_element())))
            .child(
                div()
                    .px(px(theme::PANEL_PX))
                    .flex()
                    .flex_col()
                    .child(kv("Heard closest at", crate::draw::fmt_time(f.tc, 1.0)))
                    .child(kv("Aircraft", aircraft.unwrap_or_else(|| "—".into())))
                    .child(kv("Speed", format!("{:.0} km/h", f.v * 3.6)))
                    .child(kv("Closest distance", fmt_dist(f.d))),
            )
            .into_any_element()
    }

    /// Edit mode, right column: what happened, the selected curve, deleted curves.
    fn render_edit_panel(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let note = self.edit_note.clone();
        let sel = self.selected.map(|i| (i, self.detections[i].clone()));
        let deleted: Vec<(usize, Detection)> = self.deleted.iter().cloned().enumerate().collect();
        div()
            .flex_none()
            .flex()
            .flex_col()
            .child(section_header(t, "Editing fly-bys", Some(label_xs(t, format!("{} curves · {} undo steps", self.detections.len(), self.undo.len())).into_any_element())))
            .child(
                div()
                    .px(px(theme::PANEL_PX))
                    .pb(px(theme::SP3))
                    .flex()
                    .flex_col()
                    .gap(px(theme::SP2))
                    .text_size(px(theme::TEXT_SM))
                    .children(note.map(|(n, err)| {
                        div()
                            .px(px(theme::SP3))
                            .py(px(theme::SP2))
                            .rounded(px(theme::RADIUS))
                            .border_1()
                            .border_color(if err { t.danger.opacity(0.5) } else { t.border_strong })
                            .text_color(if err { t.danger } else { t.text })
                            .child(n)
                    }))
                    .child(match sel {
                        Some((i, d)) => {
                            let (dot, what) = flyby_quality(t, &d);
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(div().flex().items_center().gap(px(theme::SP2)).child(div().size(px(8.)).rounded(px(4.)).bg(dot)).child(div().text_color(t.text).child(format!("Fly-by {} · {what}", i + 1))))
                                .child(div().text_color(t.text_muted).child(format!(
                                    "heard {} · {:.0} km/h · closest {}",
                                    crate::draw::fmt_time(d.flyby.tc, 1.0),
                                    d.flyby.v * 3.6,
                                    fmt_dist(d.flyby.d)
                                )))
                                .when(d.trace.is_some(), |el| {
                                    el.child(div().text_size(px(theme::TEXT_XS)).text_color(t.text_faint).child(format!(
                                        "{} traced points; the search scores aircraft by how close their curve passes to them",
                                        d.trace.as_ref().map_or(0, |p| p.len())
                                    )))
                                })
                                .into_any_element()
                        }
                        None => div().text_color(t.text_faint).child("No curve selected.").into_any_element(),
                    })
                    .child(div().h(px(1.)).bg(t.border))
                    .child(div().text_color(t.text_muted).child(if deleted.is_empty() { "Deleted curves: none".to_string() } else { format!("Deleted curves ({})", deleted.len()) }))
                    .children(deleted.into_iter().map(|(k, d)| {
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().font_family(MONO).text_color(t.text_muted).child(format!("{} · {:.0} km/h", crate::draw::fmt_time(d.flyby.tc, 1.0), d.flyby.v * 3.6)))
                            .child(toggle(t, "restore", "Restore", false).id(("restore", k)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.restore_deleted(k);
                                cx.notify();
                            })))
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(theme::PANEL_PX))
                    .py(px(theme::SP2))
                    .border_t_1()
                    .border_color(t.border)
                    .text_size(px(theme::TEXT_XS))
                    .text_color(t.text_faint)
                    .child("Search again uses the curves as they are now. Save… writes them as JSON for overhead-adsb locate --detections."),
            )
            .into_any_element()
    }

    fn render_detail(&mut self, t: &Theme, _cx: &mut Context<Self>) -> AnyElement {
        let Some(i) = self.selected else {
            return div()
                .flex_none()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(t.border)
                .child(section_header(t, "Fly-by", None))
                .child(
                    div()
                        .h(px(72.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .px(px(theme::SP6))
                        .text_size(px(theme::TEXT_SM))
                        .text_color(t.text_faint)
                        .child(if self.detections.is_empty() {
                            "Details of a fly-by appear here."
                        } else if self.spectro_shown() {
                            "Select a fly-by in the table or click its curve."
                        } else {
                            "Select a fly-by in the table to see it here."
                        }),
                )
                .into_any_element();
        };
        let d = self.detections[i].clone();
        let f = d.flyby;
        let r = d.ranges;
        let track = match &self.location {
            LocState::Done(res) => res
                .candidates
                .get(self.loc_selected)
                .and_then(|c| c.tracks.iter().find(|tr| tr.detection == i).cloned()),
            _ => None,
        };
        let group = |title: &str| {
            div()
                .mt(px(theme::SP3))
                .mb(px(theme::SP1))
                .text_size(px(theme::TEXT_XS))
                .text_color(t.text_faint)
                .child(title.to_string())
        };
        let kv = |k: &str, v: String| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .h(px(theme::KV_H))
                .text_size(px(theme::TEXT_SM))
                .child(div().text_color(t.text_muted).child(k.to_string()))
                .child(mono(v).text_color(t.text))
        };
        let lines: Vec<String> = {
            let mut v: Vec<String> = d.harmonics.iter().map(|h| format!("{:.1} Hz ×{}", f.f0 * h, fmt_ratio(*h))).collect();
            for x in &d.other_lines {
                if !d.harmonics.iter().any(|h| (f.f0 * h - x).abs() < 0.02 * x) {
                    v.push(format!("{x:.1} Hz"));
                }
            }
            v
        };
        let clipped_note = {
            let which: Vec<&str> = ["speed", "distance", "CPA time"].iter().zip(r.clipped).filter(|(_, c)| *c).map(|(n, _)| *n).collect();
            if which.is_empty() { None } else { Some(format!("Open range: {} (ran to the edge of the scan)", which.join(", "))) }
        };
        let tc_lo = r.tc.0 - f.tc;
        let tc_hi = r.tc.1 - f.tc;
        div()
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(t.border)
            .child(section_header(
                t,
                &format!("Fly-by {}", i + 1),
                Some(div().flex().gap(px(theme::SP1)).child(confidence_glyph(t, &d)).into_any_element()),
            ))
            .child(
                div()
                    .px(px(theme::PANEL_PX))
                    .pb(px(theme::SP3))
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .gap(px(theme::SP2))
                            .child(mono(crate::draw::fmt_time(f.tc, 0.1)).text_size(px(18.)).text_color(t.text))
                            .child(
                                div()
                                    .pb(px(theme::SP1))
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(t.text_muted)
                                    .child(format!("heard closest approach  {:+.1} / {:+.1} s", tc_lo, tc_hi)),
                            ),
                    )
                    .child(range_bar(t, f.tc, r.tc, (f.tc - 30.0, f.tc + 30.0), false, r.clipped[2]))
                    .child(group(if matches!(&self.location, LocState::Done(r) if r.is_sample) {
                        "ADS-B MATCH · SAMPLE DATA, NOT REAL"
                    } else {
                        "ADS-B MATCH"
                    }))
                    .child(match track {
                        Some(tr) => div()
                            .flex()
                            .flex_col()
                            .child(kv("Aircraft", tr.callsign.clone()))
                            .child(kv(
                                "Reg · type · class",
                                format!(
                                    "{} · {} · {}",
                                    tr.reg.clone().unwrap_or_else(|| "–".into()),
                                    tr.type_code.clone().unwrap_or_else(|| "–".into()),
                                    tr.class
                                ),
                            ))
                            .child(kv("ICAO hex", tr.icao.clone()))
                            .child(kv("Heard CPA (model)", format!("{} · {:+.1} s", crate::draw::fmt_time(tr.heard_rec, 0.1), tr.dt_s)))
                            .when(!tr.heard_utc.is_empty(), |d| d.child(kv("  UTC", tr.heard_utc.replace('T', " "))))
                            .child(kv("Slant range at CPA", format!("{} (fit {})", fmt_dist(tr.slant_m), fmt_dist(f.d))))
                            .child(kv("Ground speed", format!("{:.0} m/s (fit {:.0})", tr.speed_ms, f.v)))
                            .child(kv("Height above ground", format!("{:.0} m", tr.height_agl_m)))
                            .when(tr.curve_score > 0.0, |d| d.child(kv("Curve score", format!("{:.2} × stage-1 fit", tr.curve_score))))
                            .into_any_element(),
                        None => div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(t.text_faint)
                            .child(match &self.location {
                                LocState::Done(_) => "No ADS-B flight explains this fly-by at the selected location.",
                                _ => "Run “Find location” to match aircraft.",
                            })
                            .into_any_element(),
                    })
                    .child(group("FLAGS"))
                    .child(flag_row(t, d.confident, "Confident", "All ranges closed", clipped_note.as_deref()))
                    .child(flag_row(
                        t,
                        !d.likely_ground_vehicle,
                        "Aircraft-like",
                        "Fast or far enough for an aircraft",
                        d.likely_ground_vehicle.then_some("Slow and close: could be a car, machine or bird (kept for ADS-B to decide)"),
                    ))
                    .child(group("MOTION"))
                    .child(kv("Speed", format!("{:.0} m/s · {:.0} km/h · {:.0} kn", f.v, f.v * 3.6, f.v * 1.943_84)))
                    .child(kv("  range", format!("{:.0} – {:.0} m/s", r.v.0, r.v.1)))
                    .child(range_bar(t, f.v, r.v, (5.0, 300.0), false, r.clipped[0]))
                    .child(group("GEOMETRY"))
                    .child(kv("Slant distance at CPA", fmt_dist(f.d)))
                    .child(kv("  range", format!("{} – {}", fmt_dist(r.d.0), fmt_dist(r.d.1))))
                    .child(range_bar(t, f.d, r.d, (30.0, 30_000.0), true, r.clipped[1]))
                    .child(kv("Sound delay at CPA", format!("{:.1} s", f.d / DEFAULT_C)))
                    .child(kv("Emitted at", crate::draw::fmt_time(f.t_emit_cpa(DEFAULT_C), 0.1)))
                    .child(group("SIGNAL"))
                    .child(kv("Rest frequency", format!("{:.2} Hz", f.f0)))
                    .child(kv("Approach / recede", format!("{:.1} / {:.1} Hz", f.f_approach(DEFAULT_C), f.f_recede(DEFAULT_C))))
                    .children(lines.iter().enumerate().map(|(k, l)| kv(if k == 0 { "Lines" } else { "" }, l.clone())))
                    .child(kv("Contrast", format!("{:.1} σ", d.contrast)))
                    .child(kv("Score (mean z)", format!("{:.2}", d.score)))
                    .child(kv(
                        "Steep-part coverage",
                        format!("{:.0} % · {:.2} bins rms", d.quality.coverage * 100.0, d.quality.rms_bins),
                    ))
            )
            .into_any_element()
    }

    fn render_location(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> (AnyElement, AnyElement) {
        let results = match &self.location {
            LocState::Done(r) => Some(r.clone()),
            _ => None,
        };
        // map view and the basemap tiles it needs
        let mb = self.map_rect.get();
        let (mw, mh) = if mb.size.width > px(10.) {
            (f32::from(mb.size.width) as f64, f32::from(mb.size.height) as f64)
        } else {
            (self.layout.inspector_w as f64 - 2.0 * theme::PANEL_PX as f64, 300.0)
        };
        let view = self.map_view_for(mw, mh);
        let _ = window;
        let drop = &mut self.drop_images;
        let tiles = self.map_tiles.as_mut().map(|tc| {
            tc.poll();
            let (_, keys) = view.tiles(mw, mh);
            tc.want(&keys);
            let mut out = Vec::new();
            let mut stand_ins = Vec::new();
            for k in keys {
                match tc.get(&k) {
                    Some(img) => out.push((k, img)),
                    // not loaded yet: a coarser (or finer) tile already loaded covers it
                    None => tc.stand_ins(k, &mut stand_ins),
                }
            }
            let stand_ins: Vec<_> = stand_ins.into_iter().filter(|s| !out.iter().any(|o| o.0 == s.0)).collect();
            out.extend(stand_ins);
            drop.extend(tc.trim(400));
            out
        });
        let tiles_loading = self.map_tiles.as_ref().is_some_and(|t| t.loading());
        if tiles_loading {
            self.start_ticker(cx);
        }
        let running = matches!(self.location, LocState::Running(_));
        let progress = if running { self.loc_ctl.as_ref().map(|c| c.snapshot()) } else { None };
        let places_list = results.as_ref().map(|r| r.places()).unwrap_or_default();
        let scene = MapScene {
            places: places_list,
            show_others: results.as_ref().is_some_and(|r| self.others_shown(r)),
            details: self.details,
            heat: self.heat.clone(),
            compare: self.compare,
            probe: self.explain.as_ref().and_then(|e| e.probe_at.map(|p| (p.0, p.1))),
            region: if results.is_none() { self.region_preview.as_ref().map(|r| r.1) } else { None },
            results: results.clone(),
            selected_candidate: self.loc_selected,
            selected_detection: self.selected,
            cursor_t: self.cursor_t,
            theme: *t,
            dark: t.mode == theme::Mode::Dark,
            view,
            tiles,
            tiles_loading,
            empty_hint: if self.file.is_none() {
                "Location candidates appear here".into()
            } else if running {
                "Searching ADS-B…".into()
            } else if self.stage == Stage::Failed {
                "Analysis failed: no fly-bys to match".into()
            } else if self.query(cx).is_err() {
                "Enter the day, then Find location".into()
            } else if self.locate_queued {
                "Queued: the search starts when detection ends".into()
            } else if let LocState::Failed(_) = self.location {
                "Search failed or cancelled: see the message above".into()
            } else if self.stage == Stage::Done && self.detections.is_empty() {
                "No fly-bys to match yet: trace one by hand in Edit fly-bys".into()
            } else if self.stage != Stage::Done {
                "Finding fly-bys; then press Find location (Ctrl+Enter)".into()
            } else {
                "Press Find location (Ctrl+Enter)".into()
            },
        };
        let badge: Option<AnyElement> = match &self.location {
            LocState::Done(r) if r.is_sample => Some(pill(t, "SAMPLE DATA", t.warn).into_any_element()),
            LocState::Done(r) => Some(label_xs(t, format!("{} · {} found", r.source, r.candidates.len())).into_any_element()),
            LocState::Running(s) => Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(theme::SP2))
                    .child(mono(format!("searching {}", fmt_dur(s.elapsed().as_secs_f64()))).text_size(px(theme::TEXT_XS)).text_color(t.text_muted))
                    .child(
                        toggle(t, "loc-cancel", "Cancel", false)
                            .text_color(t.danger)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_location(cx))),
                    )
                    .into_any_element(),
            ),
            LocState::Failed(e) if e == "Cancelled." => Some(pill(t, "cancelled", t.text_muted).into_any_element()),
            LocState::Failed(_) => Some(pill(t, "failed", t.danger).into_any_element()),
            LocState::Idle => None,
        };

        // progress of a running search
        let progress_el: Option<AnyElement> = progress.map(|(msg, frac)| {
            let msg = if msg.is_empty() { "Starting…".to_string() } else { msg };
            let el = match self.location {
                LocState::Running(s) => s.elapsed().as_secs_f64(),
                _ => 0.0,
            };
            div()
                .px(px(theme::PANEL_PX))
                .pb(px(theme::SP2))
                .flex()
                .flex_col()
                .gap(px(theme::SP1))
                .child(div().text_size(px(theme::TEXT_SM)).text_color(t.text).child(msg))
                .child(match frac {
                    Some(f) => progress_bar(t, f).into_any_element(),
                    None => indeterminate_bar(t, el).into_any_element(),
                })
                .into_any_element()
        });

        let ambiguous = results.as_ref().and_then(|r| r.ambiguous().cloned());
        let n_det = self.detections.len().max(1);
        let list: Vec<AnyElement> = results
            .as_ref()
            .map(|r| {
                r.candidates
                    .iter()
                    .enumerate()
                    .map(|(k, c)| {
                        let sel = k == self.loc_selected;
                        let hover = t.hover;
                        div()
                            .id(("cand", k))
                            .flex()
                            .items_center()
                            .gap(px(theme::SP2))
                            .h(px(theme::ROW_H))
                            .px(px(theme::PANEL_PX))
                            .text_size(px(theme::TEXT_SM))
                            .cursor_pointer()
                            .when(sel, |d| d.bg(t.selected))
                            .when(!sel, |d| d.hover(move |s| s.bg(hover)))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.loc_selected = k;
                                if this.map_fit == MapFit::Candidate {
                                    this.map_view = None;
                                }
                                cx.notify();
                            }))
                            .child(mono(format!("{}", k + 1)).w(px(14.)).text_color(if sel { t.accent } else { t.text_faint }))
                            .child(mono(format!("{:.5}, {:.5}", c.lat, c.lon)).flex_1().text_color(t.text))
                            .child(mono(format!("±{}", fmt_dist_short(c.radius_m))).text_color(t.text_muted).text_size(px(theme::TEXT_XS)))
                            .child(
                                // the place's probability (a candidate within 2 km of a better one shares its place)
                                mono(match (c.probability, c.same_place_as) {
                                    (_, Some(r)) => format!("≈{r}"),
                                    (Some(p), None) => fmt_prob(p),
                                    (None, None) => String::new(),
                                })
                                .w(px(38.))
                                .flex()
                                .justify_end()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(if c.same_place_as.is_some() {
                                    t.text_faint
                                } else if sel {
                                    t.text
                                } else {
                                    t.text_muted
                                }),
                            )
                            .child(
                                div().w(px(36.)).h(px(4.)).rounded(px(2.)).bg(t.border_strong).child(
                                    div()
                                        .h_full()
                                        .rounded(px(2.))
                                        .w(relative(c.probability.unwrap_or(if c.same_place_as.is_some() { 0.0 } else { c.score }) as f32))
                                        .bg(if sel { t.accent } else { t.text_muted }),
                                ),
                            )
                            .child(mono(format!("{}/{}", c.tracks.len(), n_det)).w(px(30.)).flex().justify_end().text_size(px(theme::TEXT_XS)).text_color(t.text_faint))
                            .into_any_element()
                    })
                    .collect()
            })
            .unwrap_or_default();

        // the selected candidate in numbers
        let summary: Option<AnyElement> = results.as_ref().and_then(|r| r.candidates.get(self.loc_selected)).filter(|_| !results.as_ref().unwrap().is_sample).map(|c| {
            let kv = |k: &str, v: String, color: Hsla| {
                div()
                    .flex()
                    .justify_between()
                    .h(px(theme::KV_H))
                    .items_center()
                    .text_size(px(theme::TEXT_XS))
                    .child(div().text_color(t.text_muted).child(k.to_string()))
                    .child(mono(v).text_color(color))
            };
            let (conf, conf_c) = match c.margin {
                Some(m) if m >= 1.0 => (format!("{m:.2} · clear"), t.ok),
                Some(m) => (format!("{m:.2} · ambiguous"), t.warn),
                None => ("–".into(), t.text_muted),
            };
            let prob = match (c.probability, c.same_place_as) {
                (_, Some(r)) => Some((format!("same place as {r}"), t.text_muted)),
                (Some(p), None) => Some((fmt_prob(p), if ambiguous.is_some() { t.warn } else { t.text })),
                (None, None) => None,
            };
            div()
                .mx(px(theme::PANEL_PX))
                .pb(px(theme::SP2))
                .border_b_1()
                .border_color(t.border)
                .flex()
                .flex_col()
                .children(prob.map(|(v, col)| kv("Probability of this place (±2 km)", v, col)))
                .child(kv("Margin to next place > 2 km", conf, conf_c))
                .children(c.ellipse.map(|(a, b, az)| {
                    kv("Uncertainty (1σ)", format!("±{} · {}×{} @ {:.0}°", fmt_dist_short(c.radius_m), fmt_dist_short(a), fmt_dist_short(b), az), t.text)
                }))
                .children(c.start_utc.as_ref().map(|s| kv("Recording start (UTC)", s.replace('T', " ").trim_end_matches('Z').to_string(), t.text)))
                .child(kv(
                    "Fly-bys explained · score",
                    format!("{} of {} · {:.2}", c.matched, self.detections.len(), c.raw_score),
                    t.text,
                ))
                .children(c.c0.map(|c0| kv("Speed of sound (fit)", format!("{c0:.1} m/s"), t.text)))
                .children(c.ground_m.map(|g| kv("Terrain height", format!("{g:.0} m"), t.text)))
                .when(c.violations > 0, |d| {
                    d.child(kv("Silent aircraft nearby", format!("{}", c.violations), t.warn))
                })
                .into_any_element()
        });

        let details = self.details;
        let notes: Vec<String> = match &self.location {
            LocState::Done(r) if details => r.notes.clone(),
            LocState::Failed(e) if details => vec![e.clone()],
            _ => vec![],
        };
        // a failed search, or one that found nothing: say what happened and what to do
        let problem: Option<(String, String)> = match &self.location {
            LocState::Failed(e) => Some(friendly_error(e)),
            LocState::Done(r) if r.candidates.is_empty() && !r.is_sample => Some((
                "No aircraft fits these fly-bys".to_string(),
                "No flight in the ADS-B history passed the way these fly-bys need on that day and in that region. Check the day (local date) and the region, widen the time, or correct the curves in Edit fly-bys.".to_string(),
            )),
            _ => None,
        };
        let problem_el: Option<AnyElement> = problem.map(|(title, body)| {
            let col = if title == "Cancelled" { t.text_muted } else { t.danger };
            div()
                .mx(px(theme::PANEL_PX))
                .mb(px(theme::SP2))
                .px(px(theme::SP3))
                .py(px(theme::SP2))
                .rounded(px(theme::RADIUS))
                .border_1()
                .border_color(col.opacity(0.45))
                .bg(col.opacity(0.08))
                .flex()
                .flex_col()
                .gap(px(theme::SP1))
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(FontWeight::MEDIUM).text_color(t.text).child(title))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child(body))
                .into_any_element()
        });
        let what = match (self.dets_changed, self.cuts_changed()) {
            (true, true) => "The fly-bys and the cuts changed",
            (false, true) => "The cuts changed",
            _ => "The fly-bys changed",
        };
        let stale: Option<AnyElement> = (self.inputs_changed() && matches!(self.location, LocState::Done(_))).then(|| {
            div()
                .mx(px(theme::PANEL_PX))
                .mb(px(theme::SP2))
                .text_size(px(theme::TEXT_XS))
                .text_color(t.warn)
                .child(format!("{what} since this search: press Search again."))
                .into_any_element()
        });
        // the answer in one sentence, with the coordinates and links
        let card: Option<AnyElement> = match results.as_ref() {
            Some(r) => self.render_result_card(t, r, cx),
            None => None,
        };
        // ambiguous, simple view: a few possible places with their chances
        let places: Option<AnyElement> = ambiguous.as_ref().filter(|_| !details).and_then(|a| {
            let r = results.as_ref()?;
            let distinct: Vec<(usize, &crate::location::LocationCandidate)> =
                r.candidates.iter().enumerate().filter(|(_, c)| c.same_place_as.is_none() && c.probability.is_some()).take(4).collect();
            let first = distinct.first().map(|(_, c)| (c.lat, c.lon));
            let rows: Vec<AnyElement> = distinct
                .into_iter()
                .enumerate()
                .map(|(n, (k, c))| {
                    let p = c.probability.unwrap_or(0.0);
                    // a name people can read: the place number and where it lies from place 1
                    let whereabouts = match first {
                        Some((la, lo)) if n > 0 => {
                            let (dist, dir) = offset_from(la, lo, c.lat, c.lon);
                            format!("{} {dir} of place 1", fmt_dist_short(dist))
                        }
                        _ => format!("{:.3}, {:.3}", c.lat, c.lon),
                    };
                    let sel = k == self.loc_selected;
                    let hover = t.hover;
                    div()
                        .id(("place", k))
                        .flex()
                        .items_center()
                        .gap(px(theme::SP2))
                        .h(px(theme::ROW_H))
                        .px(px(theme::PANEL_PX))
                        .text_size(px(theme::TEXT_SM))
                        .cursor_pointer()
                        .when(sel, |d| d.bg(t.selected))
                        .when(!sel, |d| d.hover(move |s| s.bg(hover)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.loc_selected = k;
                            cx.notify();
                        }))
                        .child(div().w(px(56.)).flex_none().text_color(t.text).child(format!("Place {}", n + 1)))
                        .child(div().flex_1().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child(whereabouts))
                        .child(mono(fmt_prob(p)).w(px(40.)).flex().justify_end().text_size(px(theme::TEXT_XS)).text_color(t.text_muted))
                        .child(div().w(px(50.)).h(px(4.)).rounded(px(2.)).bg(t.border_strong).child(div().h_full().rounded(px(2.)).w(relative(p as f32)).bg(t.warn)))
                        .into_any_element()
                })
                .collect();
            Some(
                div()
                    .flex()
                    .flex_col()
                    .child(section_header(t, "Possible places", Some(label_xs(t, "and their chances").into_any_element())))
                    .children(rows)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(theme::SP2))
                            .h(px(theme::ROW_H))
                            .px(px(theme::PANEL_PX))
                            .text_size(px(theme::TEXT_SM))
                            .child(div().flex_1().text_color(t.text_muted).child("Somewhere else"))
                            .child(mono(fmt_prob(a.elsewhere)).w(px(40.)).flex().justify_end().text_size(px(theme::TEXT_XS)).text_color(t.text_muted))
                            .child(div().w(px(50.)).h(px(4.)).rounded(px(2.)).bg(t.border_strong).child(div().h_full().rounded(px(2.)).w(relative(a.elsewhere as f32)).bg(t.text_faint))),
                    )
                    .into_any_element(),
            )
        });
        let hints: Option<AnyElement> = ambiguous.as_ref().filter(|a| !a.hints.is_empty()).map(|a| {
            div()
                .px(px(theme::PANEL_PX))
                .pt(px(theme::SP2))
                .flex()
                .flex_col()
                .gap(px(1.))
                .text_size(px(theme::TEXT_XS))
                .child(div().text_color(t.text_muted).child("To narrow it down"))
                .children(a.hints.iter().map(|h| div().text_color(t.text_faint).child(format!("· {}", capitalize(h)))))
                .into_any_element()
        });
        let rect = self.map_rect.clone();
        let overlays = self.render_map_overlays(t, results.as_deref(), cx);
        let summary = if details { summary } else { None };
        let list = if details { list } else { vec![] };
        // the answer: header, verdict or warning, and the map, which takes what is left
        let top = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .child(section_header(t, "Location", badge))
            .children(progress_el)
            .children(problem_el)
            .children(stale)
            .children(card)
            .child(
                div()
                    .id("map")
                    .relative()
                    .flex_1()
                    .min_h(px(MAP_MIN_H))
                    .mx(px(theme::PANEL_PX))
                    .mb(px(theme::SP3))
                    .rounded(px(theme::RADIUS))
                    .overflow_hidden()
                    .border_1()
                    .border_color(t.border)
                    .cursor(if self.map_drag.is_some() { CursorStyle::ClosedHand } else { CursorStyle::OpenHand })
                    .on_scroll_wheel(cx.listener(Self::on_map_scroll))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_map_down))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                            // with the analysis open: test the place under the pointer
                            if this.analysis.is_some() && this.explain.is_some() {
                                this.probe_at(ev.position, cx);
                            }
                        }),
                    )
                    .on_mouse_move(cx.listener(Self::on_map_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_map_up))
                    .child(
                        canvas(
                            move |b, _, _| rect.set(b),
                            move |b, _, window, cx| map::paint(b, &scene, window, cx),
                        )
                        .size_full(),
                    )
                    .children(overlays),
            )
            .into_any_element();
        // the places and numbers behind it (in the right column, which scrolls)
        let has_list = summary.is_some() || places.is_some() || !list.is_empty() || hints.is_some() || !notes.is_empty();
        let n_cand = list.len();
        let cand_header = (summary.is_some() || !list.is_empty())
            .then(|| section_header(t, "Candidates", Some(label_xs(t, format!("{n_cand} places, best first")).into_any_element())));
        let below = div()
            .flex()
            .flex_col()
            .flex_none()
            .when(has_list, |d| d.pb(px(theme::SP3)))
            .when(has_list && places.is_none() && cand_header.is_none(), |d| d.pt(px(theme::SP2)))
            .children(cand_header)
            .children(summary)
            .children(places)
            .children(list)
            .children(hints)
            .children(notes.into_iter().map(|n| div().px(px(theme::PANEL_PX)).pt(px(theme::SP1)).text_size(px(theme::TEXT_XS)).text_color(t.text_faint).child(n)))
            .into_any_element();
        (top, below)
    }

    fn render_status(&mut self, t: &Theme) -> impl IntoElement {
        let left: AnyElement = match self.stage {
            Stage::Idle => div().child("Ready").into_any_element(),
            Stage::Decoding | Stage::Overview | Stage::Detecting => {
                let el = self.stage_started.map_or(0.0, |s| s.elapsed().as_secs_f64());
                let est = self.stage_estimate(self.stage);
                div()
                    .flex()
                    .items_center()
                    .gap(px(theme::SP2))
                    .child(div().w(px(6.)).h(px(6.)).rounded(px(theme::RADIUS_SM)).bg(t.accent))
                    .child(self.stage.label())
                    .map(|d| match est {
                        Some(e) => d
                            .child(div().w(px(120.)).child(progress_bar(t, (el / e.max(0.5)).min(0.97) as f32)))
                            .child(mono(format!("{} / est. {}", fmt_dur(el), fmt_dur(e))).text_color(t.text_faint)),
                        None => d.child(mono(fmt_dur(el)).text_color(t.text_faint)),
                    })
                    .into_any_element()
            }
            Stage::Done => {
                let total: f64 = self.stage_times.iter().map(|x| x.1).sum();
                div()
                    .flex()
                    .gap(px(theme::SP2))
                    .items_center()
                    .child(icon(Icon::Check, 11.0, t.ok))
                    .child(match self.detections.len() {
                        1 => "1 fly-by".to_string(),
                        n => format!("{n} fly-bys"),
                    })
                    .child(mono(format!("analysed in {}", fmt_dur(total))).text_color(t.text_faint))
                    .children(self.stats.as_ref().filter(|_| self.details).map(|s| {
                        mono(format!("{} tracks → {} candidates → {} lines", s.tracks, s.candidates, s.accepted_lines)).text_color(t.text_faint)
                    }))
                    .into_any_element()
            }
            Stage::Failed => {
                let e = self.error.clone().unwrap_or_else(|| "Failed".into());
                // One line: the full, multi-line message is in the progress card.
                let first = e.split_whitespace().collect::<Vec<_>>().join(" ");
                div()
                    .flex()
                    .items_center()
                    .gap(px(theme::SP2))
                    .min_w(px(0.))
                    .overflow_hidden()
                    .child(icon(Icon::Close, 11.0, t.danger))
                    .child(
                        div()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(t.danger)
                            .child(format!("{} failed: {first}", self.failed_stage.unwrap_or(Stage::Failed).label())),
                    )
                    .into_any_element()
            }
        };
        let right = format!(
            "{} · FFT {} · {}c = {:.0} m/s",
            if self.whiten { "whitened" } else { "raw dB" },
            self.n_fft,
            "",
            DEFAULT_C
        );
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(px(theme::STATUS_H))
            .px(px(theme::PANEL_PX))
            .bg(t.surface)
            .border_t_1()
            .border_color(t.border)
            .text_size(px(theme::TEXT_XS))
            .text_color(t.text_muted)
            .gap(px(theme::SP4))
            .overflow_hidden()
            .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(left))
            .when(self.details, |d| d.child(mono(right).flex_none().text_color(t.text_faint)))
            .child(self.updater.clone())
    }
}

fn field_compact(t: &Theme, label: &str, input: &Entity<TextInput>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(theme::SP2))
        .child(div().w(px(124.)).flex_none().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(theme::TEXT_SM)).text_color(t.text_muted).child(label.to_string()))
        .child(div().flex_1().min_w(px(0.)).child(input.clone()))
}

fn space_row(t: &Theme, k: &str, v: String) -> impl IntoElement {
    div()
        .flex()
        .justify_between()
        .items_center()
        .h(px(theme::KV_H))
        .text_size(px(theme::TEXT_SM))
        .child(div().text_color(t.text_muted).child(k.to_string()))
        .child(mono(v).text_size(px(theme::TEXT_XS)).text_color(t.text))
}

fn fmt_speedup(x: f64) -> String {
    if x >= 1e6 {
        format!("{:.1}M", x / 1e6)
    } else if x >= 1e4 {
        format!("{:.0}k", x / 1e3)
    } else if x >= 10.0 {
        format!("{:.0}", x)
    } else {
        format!("{:.1}", x)
    }
}

fn fmt_ratio(r: f64) -> String {
    if (r - r.round()).abs() < 0.01 { format!("{:.0}", r) } else { format!("{r:.2}") }
}

fn draw_time_long(s: f64) -> String {
    let total = s.round() as i64;
    format!("{}:{:02}:{:02}", total / 3600, (total / 60) % 60, total % 60)
}

fn progress_bar(t: &Theme, frac: f32) -> impl IntoElement {
    div()
        .w_full()
        .h(px(4.))
        .rounded(px(2.))
        .bg(t.border_strong)
        .child(div().h_full().rounded(px(2.)).w(relative(frac.clamp(0.0, 1.0))).bg(t.accent))
}

/// A bar for work of unknown length: a segment sweeping back and forth.
fn indeterminate_bar(t: &Theme, elapsed_s: f64) -> impl IntoElement {
    let phase = (elapsed_s / 1.6).fract();
    let x = if phase < 0.5 { phase * 2.0 } else { 2.0 - phase * 2.0 };
    div()
        .relative()
        .w_full()
        .h(px(4.))
        .rounded(px(2.))
        .bg(t.border_strong)
        .child(div().absolute().top(px(0.)).h_full().w(relative(0.25)).left(relative((x * 0.75) as f32)).rounded(px(2.)).bg(t.accent))
}

/// A probability for people: "38%", "<1%", ">99%".
fn fmt_prob(p: f64) -> String {
    map::fmt_chance(p)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn fmt_dist_short(m: f64) -> String {
    if m >= 10_000.0 {
        format!("{:.0} km", m / 1000.0)
    } else if m >= 1000.0 {
        format!("{:.1} km", m / 1000.0)
    } else {
        format!("{m:.0} m")
    }
}

/// Linear-style signal bars: brightness of the fit (contrast), hollow when
/// the ranges are open.
fn confidence_glyph(t: &Theme, d: &Detection) -> impl IntoElement {
    let level = if d.contrast >= 15.0 { 3 } else if d.contrast >= 10.0 { 2 } else { 1 };
    let color = if d.likely_ground_vehicle { t.warn } else if d.confident { t.text } else { t.text_muted };
    div().flex().items_end().gap(px(2.)).h(px(12.)).children((0..3).map(move |k| {
        let on = k < level;
        div()
            .w(px(3.))
            .h(px(4.0 + 4.0 * k as f32))
            .rounded(px(1.))
            .bg(if on { color } else { t.border_strong })
    }))
}

/// A colour and a few words for how much to trust a fly-by.
fn flyby_quality(t: &Theme, d: &Detection) -> (Hsla, &'static str) {
    match d.origin {
        Origin::Traced if d.confident => (t.ok, "traced by hand"),
        Origin::Traced => (t.warn, "traced, loosely measured"),
        _ if d.likely_ground_vehicle => (t.warn, "maybe a car or machine"),
        Origin::Edited if d.confident => (t.ok, "corrected by hand"),
        Origin::Edited => (t.warn, "corrected, uncertain"),
        _ if d.confident && d.contrast >= 10.0 => (t.ok, "clear fly-by"),
        _ if d.confident => (t.ok, "fly-by"),
        _ => (t.warn, "uncertain measurement"),
    }
}

/// A search error for people: a title and what to do.
fn friendly_error(e: &str) -> (String, String) {
    let l = e.to_lowercase();
    if e == "Cancelled." {
        ("Cancelled".into(), "The search was stopped. Press Find location to run it again.".into())
    } else if l.contains("no adsb.lol archive found") {
        (
            "No ADS-B data for this day yet".into(),
            "adsb.lol publishes a day's flight history about a day after the day ends (coverage starts in 2023). Check the date, or try again tomorrow.".into(),
        )
    } else if l.contains("no detections") {
        ("No fly-bys to match".into(), "Trace a fly-by by hand in Edit fly-bys, then search again.".into())
    } else if l.contains("region") && (l.contains("not found") || l.contains("no result") || l.contains("nominatim")) {
        ("The region was not found".into(), format!("Check the spelling of the city, state or country, or give lat,lon,radius_km. ({e})"))
    } else if l.contains("download") || l.contains("http") || l.contains("connect") || l.contains("timed out") || l.contains("dns") || l.contains("stopped") {
        ("Download failed".into(), format!("The ADS-B history could not be downloaded. Check the internet connection and try again; what was already downloaded is kept. ({e})"))
    } else {
        ("The search failed".into(), e.to_string())
    }
}

fn flag_row(t: &Theme, ok: bool, title: &str, ok_text: &str, bad_text: Option<&str>) -> impl IntoElement {
    div()
        .flex()
        .gap(px(theme::SP2))
        .py(px(theme::SP1))
        .child(
            div().pt(px(2.)).child(if ok { icon(Icon::Check, 12.0, t.ok).into_any_element() } else { icon(Icon::Warn, 12.0, t.warn).into_any_element() }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(theme::TEXT_SM)).text_color(t.text).child(if ok { title.to_string() } else { format!("Not {}", title.to_lowercase()) }))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(t.text_muted).child(if ok { ok_text.to_string() } else { bad_text.unwrap_or("").to_string() })),
        )
}

/// Value with its plausible range on a track spanning `scale`.
fn range_bar(t: &Theme, value: f64, range: (f64, f64), scale: (f64, f64), log: bool, clipped: bool) -> impl IntoElement {
    let map = |x: f64| -> f32 {
        let (a, b) = scale;
        let u = if log { (x.max(a).ln() - a.ln()) / (b.ln() - a.ln()) } else { (x - a) / (b - a) };
        u.clamp(0.0, 1.0) as f32
    };
    let (l, r, v) = (map(range.0), map(range.1), map(value));
    let color = if clipped { t.warn } else { t.accent };
    div()
        .relative()
        .h(px(10.))
        .my(px(theme::SP1))
        .child(div().absolute().left(px(0.)).right(px(0.)).top(px(4.)).h(px(2.)).rounded(px(1.)).bg(t.border_strong))
        .child(div().absolute().left(relative(l)).w(relative((r - l).max(0.004))).top(px(3.)).h(px(4.)).rounded(px(2.)).bg(color.opacity(0.45)))
        .child(div().absolute().left(relative(v)).ml(px(-1.)).top(px(0.)).w(px(2.)).h(px(10.)).bg(color))
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::theme(cx);
        self.request_analysis(cx);
        for img in self.drop_images.drain(..) {
            let _ = window.drop_image(img);
        }
        self.ensure_tile(window.scale_factor(), cx);
        let title = match &self.file {
            Some(f) => format!("{} — Overhead", f.name),
            None => "Overhead".to_string(),
        };
        if title != self.last_title {
            window.set_window_title(&title);
            self.last_title = title;
        }

        let decorations = window.window_decorations();
        let inset = px(8.);
        let rounding = px(8.);
        // the shadow inset around a floating client-decorated window (both sides)
        let frame = match decorations {
            Decorations::Client { tiling } if !tiling.is_tiled() => {
                window.set_client_inset(inset);
                2.0 * f32::from(inset)
            }
            _ => {
                window.set_client_inset(px(0.));
                0.0
            }
        };
        let client = matches!(decorations, Decorations::Client { .. });

        // panel sizes for this window: the window size now minus the title bar,
        // status bar and borders as measured on the last frame (the body itself
        // is measured only after layout, so using it would lag one frame behind
        // a resize, a snap or a maximize); the first frame estimates them
        let vs = window.viewport_size();
        let (mw, mh) = self.body_margin.get().unwrap_or((0.0, theme::TITLE_H + theme::STATUS_H));
        let (bw, bh) = ((f32::from(vs.width) - frame - mw).max(0.0), (f32::from(vs.height) - frame - mh).max(0.0));
        let fit = self.layout.fitted(bw, bh, self.detections.len());
        let spectro = self.spectro_shown();
        // without the spectrogram the map is the hero, in the middle
        let map_center = self.map_center();

        let left_w = if self.layout.sidebar_open { fit.sidebar_w } else { layout::RAIL_W };
        let center_w = bw - left_w - fit.inspector_w;
        self.center_w = center_w;

        let titlebar = self.render_titlebar(&t, window, cx).into_any_element();
        let sidebar = if self.layout.sidebar_open {
            self.render_sidebar(&t, fit.sidebar_w, cx).into_any_element()
        } else {
            self.render_rail(&t, cx).into_any_element()
        };
        let toolbar = (spectro && self.file.is_some()).then(|| self.render_toolbar(&t, cx).into_any_element());
        let edit_bar = self.render_edit_bar(&t, cx);
        let spec = (!map_center).then(|| self.render_spectrogram(&t, window, cx));
        // simple view without the spectrogram: the map gets the whole middle and
        // the fly-bys move to the right column (the detailed table needs the width)
        let table_right = self.table_right();
        let right_table_h = self.layout.right_table(self.detections.len(), bh);
        // a narrow middle (small window, wide panels) drops the "what" column too,
        // so the aircraft names are not cut
        let compact = table_right || center_w < 560.0;
        let table = self.render_table(&t, if table_right { right_table_h } else { fit.table_h }, compact, cx);
        // no recording: the drop card gets the whole middle (an empty table under
        // it would only repeat "open a recording" and cut the card on small windows)
        let (table_center, table_right) = if table_right {
            (None, Some(table))
        } else if self.file.is_none() {
            (None, None)
        } else {
            (Some(table), None)
        };
        let (loc_top, loc_below) = self.render_location(&t, window, cx);
        let (center_loc, right_loc) = if map_center { (Some(loc_top), None) } else { (None, Some(loc_top)) };
        // right column, below the map: the edit tools, the places / candidates, the selected fly-by
        let mut below: Vec<AnyElement> = vec![];
        if self.edit.is_some() {
            below.push(self.render_edit_panel(&t, cx));
        }
        below.push(loc_below);
        below.push(if self.details { self.render_detail(&t, cx) } else { self.render_detail_simple(&t) });
        let status = self.render_status(&t).into_any_element();

        // also next to the folded rail: dragging it out unfolds the form
        let split_sidebar = self.splitter(&t, Split::Sidebar, cx);
        let split_inspector = self.splitter(&t, Split::Inspector, cx);
        // the analysis drawer takes the table's place in the middle (its first tab goes back to the table)
        let drawer = (self.analysis.is_some() && self.explain.is_some())
            .then(|| self.render_analysis(&t, fit.analysis_h, table_center.is_some(), cx));
        let timeline = if self.file.as_ref().is_some_and(|f| f.overview.is_some()) { self.render_timeline_bar(&t, cx) } else { None };
        let split_table = if drawer.is_some() {
            Some(self.splitter(&t, Split::Drawer, cx))
        } else {
            table_center.is_some().then(|| self.splitter(&t, Split::Table, cx))
        };
        let table_center = if drawer.is_some() { drawer } else { table_center };
        let split_right = (right_loc.is_some() || table_right.is_some()).then(|| self.splitter(&t, Split::Right, cx));
        let (body_cell, center_cell, right_cell) = (self.body_rect.clone(), self.center_rect.clone(), self.right_rect.clone());
        let body_margin = self.body_margin.clone();

        let center = div()
            .id("center")
            .key_context("Spectro")
            .track_focus(&self.nav_focus)
            .on_action(cx.listener(Self::act_next))
            .on_action(cx.listener(Self::act_prev))
            .on_action(cx.listener(Self::act_clear))
            .on_action(cx.listener(Self::act_zoom_all))
            .on_action(cx.listener(Self::act_zoom_sel))
            .on_action(cx.listener(Self::act_whiten))
            .on_action(cx.listener(Self::act_curves))
            .on_action(cx.listener(Self::act_edit))
            .on_action(cx.listener(Self::act_undo))
            .on_action(cx.listener(Self::act_delete))
            .on_action(cx.listener(Self::act_remove_point))
            .on_action(cx.listener(Self::act_fit_trace))
            .on_action(cx.listener(Self::act_play))
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .overflow_hidden()
            .child(canvas(move |b, _, _| center_cell.set(b), |_, _, _, _| {}).absolute().size_full())
            .children(toolbar)
            .children(edit_bar)
            .children(spec)
            .children(center_loc.map(|l| div().flex().flex_col().flex_1().min_h(px(0.)).bg(t.panel_bg).child(l)))
            .children(timeline)
            .children(split_table)
            .children(table_center);
        let right = div()
            .id("right")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(fit.inspector_w))
            .h_full()
            .overflow_hidden()
            .bg(t.panel_bg)
            .child(canvas(move |b, _, _| right_cell.set(b), |_, _, _, _| {}).absolute().size_full())
            .children(right_loc.map(|l| div().flex().flex_col().flex_none().h(px(fit.right_top_h)).child(l)))
            .children(table_right)
            .children(split_right)
            .child(div().id("right-scroll").flex().flex_col().flex_1().min_h(px(0.)).overflow_y_scroll().children(below));

        let content = div()
            .id("content")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::act_open))
            .on_action(cx.listener(Self::act_theme))
            .on_action(cx.listener(Self::act_quit))
            .on_action(cx.listener(Self::act_find))
            .on_action(cx.listener(Self::act_details))
            .on_action(cx.listener(Self::act_spectrogram))
            .on_action(cx.listener(Self::act_sidebar))
            .on_action(cx.listener(Self::act_analysis))
            .on_action(cx.listener(Self::act_focus_next))
            .on_action(cx.listener(Self::act_focus_prev))
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if let Some(p) = paths.paths().first() {
                    this.open_path(p.clone(), cx);
                }
            }))
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(t.accent_soft))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        canvas(
                            move |b, window, _| {
                                body_cell.set(b);
                                let vs = window.viewport_size();
                                body_margin.set(Some((
                                    f32::from(vs.width - b.size.width) - frame,
                                    f32::from(vs.height - b.size.height) - frame,
                                )));
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(sidebar)
                    .child(split_sidebar)
                    .child(center)
                    .child(split_inspector)
                    .child(right),
            )
            .child(status);

        // the title bar is outside the focusable content: a click on it must
        // not be taken by focus handling, or Windows never starts the move
        let shell = div()
            .id("shell")
            .flex()
            .flex_col()
            .size_full()
            .bg(t.app_bg)
            .text_color(t.text)
            .font_family(theme::SANS)
            .text_size(px(theme::TEXT))
            .line_height(relative(theme::LINE))
            .cursor(CursorStyle::Arrow)
            .when(client, |d| d.on_mouse_move(|_, _, cx| cx.stop_propagation()))
            .on_drag_move(cx.listener(|this, e: &DragMoveEvent<DragSplit>, window, cx| {
                let split = e.drag(cx).0;
                this.split_dragging = Some(split);
                this.drag_split(split, e.event.position);
                let cursor = if matches!(split, Split::Sidebar | Split::Inspector) { CursorStyle::ResizeLeftRight } else { CursorStyle::ResizeUpDown };
                cx.set_active_drag_cursor_style(cursor, window);
                this.schedule_layout_save(cx);
                cx.notify();
            }))
            .child(titlebar)
            .child(content);

        div()
            .id("window")
            .size_full()
            .bg(gpui::transparent_black())
            .map(|d| match decorations {
                Decorations::Server => d.child(shell),
                Decorations::Client { tiling } => d
                    .child(
                        canvas(
                            |_b, window, _| {
                                window.insert_hitbox(
                                    Bounds::new(point(px(0.), px(0.)), window.bounds().size),
                                    gpui::HitboxBehavior::Normal,
                                )
                            },
                            move |_b, hitbox, window, _| {
                                let mouse = window.mouse_position();
                                // the window as it is now: `window_bounds()` of a
                                // maximized window is its restored size
                                let size = window.bounds().size;
                                if let Some(edge) = resize_edge(mouse, inset, size, tiling) {
                                    window.set_cursor_style(edge_cursor(edge), &hitbox);
                                }
                            },
                        )
                        .size_full()
                        .absolute(),
                    )
                    .when(!tiling.is_tiled(), |d| d.p(inset))
                    .on_mouse_move(|_, window, _| window.refresh())
                    .on_mouse_down(MouseButton::Left, move |e, window, _| {
                        let size = window.bounds().size;
                        if let Some(edge) = resize_edge(e.position, inset, size, tiling) {
                            window.start_window_resize(edge)
                        }
                    })
                    .child(
                        div()
                            .size_full()
                            .overflow_hidden()
                            .when(!tiling.is_tiled(), |d| {
                                d.rounded(rounding).border_1().border_color(t.border_strong).shadow(vec![gpui::BoxShadow {
                                    color: t.shadow,
                                    blur_radius: inset / 1.5,
                                    spread_radius: px(0.),
                                    offset: point(px(0.), px(1.)),
                                }])
                            })
                            .child(shell),
                    ),
            })
    }
}

/// The edge or corner under `pos`, if any. A maximized window (tiled on
/// every side) has none, and a window snapped to one side keeps only the
/// edges away from that side; a press there would otherwise start a resize
/// the compositor refuses, and the click under it would be lost.
fn resize_edge(pos: Point<Pixels>, inset: Pixels, size: Size<Pixels>, tiling: gpui::Tiling) -> Option<ResizeEdge> {
    let e = inset;
    let edge = if pos.y < e && pos.x < e {
        ResizeEdge::TopLeft
    } else if pos.y < e && pos.x > size.width - e {
        ResizeEdge::TopRight
    } else if pos.y < e {
        ResizeEdge::Top
    } else if pos.y > size.height - e && pos.x < e {
        ResizeEdge::BottomLeft
    } else if pos.y > size.height - e && pos.x > size.width - e {
        ResizeEdge::BottomRight
    } else if pos.y > size.height - e {
        ResizeEdge::Bottom
    } else if pos.x < e {
        ResizeEdge::Left
    } else if pos.x > size.width - e {
        ResizeEdge::Right
    } else {
        return None;
    };
    let blocked = match edge {
        ResizeEdge::Top => tiling.top,
        ResizeEdge::Bottom => tiling.bottom,
        ResizeEdge::Left => tiling.left,
        ResizeEdge::Right => tiling.right,
        ResizeEdge::TopLeft => tiling.top || tiling.left,
        ResizeEdge::TopRight => tiling.top || tiling.right,
        ResizeEdge::BottomLeft => tiling.bottom || tiling.left,
        ResizeEdge::BottomRight => tiling.bottom || tiling.right,
    };
    (!blocked).then_some(edge)
}

fn edge_cursor(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

/// A fly-by file: a plain list of detections, or `{"detections": [...],
/// "cuts": [600.5, {"at": 1200, ...}]}` (the form `overhead-adsb locate` reads).
fn parse_detections(text: &str) -> anyhow::Result<(Vec<Detection>, Option<Vec<f64>>)> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    if v.is_array() {
        return Ok((serde_json::from_value(v)?, None));
    }
    let dets: Vec<Detection> = serde_json::from_value(v.get("detections").cloned().unwrap_or_default())?;
    let cuts = v.get("cuts").and_then(|c| c.as_array()).map(|a| {
        a.iter().filter_map(|c| c.as_f64().or_else(|| c.get("at").and_then(|x| x.as_f64()))).collect::<Vec<f64>>()
    });
    Ok((dets, cuts))
}

/// Ground distance (m) and compass direction (8 points) from one place to another.
fn offset_from(lat0: f64, lon0: f64, lat: f64, lon: f64) -> (f64, &'static str) {
    let north = (lat - lat0) * 111_195.0;
    let east = (lon - lon0) * 111_195.0 * lat0.to_radians().cos();
    let dist = north.hypot(east);
    let deg = east.atan2(north).to_degrees().rem_euclid(360.0);
    const DIRS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    (dist, DIRS[((deg + 22.5) / 45.0) as usize % 8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_edges_only_where_the_window_can_grow() {
        let size = Size { width: px(1600.), height: px(928.) };
        let free = gpui::Tiling::default();
        let p = |x: f32, y: f32| point(px(x), px(y));
        assert!(matches!(resize_edge(p(800., 925.), px(8.), size, free), Some(ResizeEdge::Bottom)));
        assert!(matches!(resize_edge(p(1597., 3.), px(8.), size, free), Some(ResizeEdge::TopRight)));
        assert!(resize_edge(p(800., 400.), px(8.), size, free).is_none());
        // maximized: no edge anywhere, so a click near the border stays a click
        let max = gpui::Tiling::tiled();
        assert!(resize_edge(p(800., 925.), px(8.), size, max).is_none());
        assert!(resize_edge(p(2., 2.), px(8.), size, max).is_none());
        // snapped to the left: only the right edge resizes
        let left = gpui::Tiling { left: true, top: true, bottom: true, right: false };
        assert!(resize_edge(p(2., 400.), px(8.), size, left).is_none());
        assert!(matches!(resize_edge(p(1597., 400.), px(8.), size, left), Some(ResizeEdge::Right)));
        assert!(resize_edge(p(1597., 925.), px(8.), size, left).is_none());
    }

    #[test]
    fn detections_load_with_and_without_cuts() {
        let (d, c) = parse_detections("[]").unwrap();
        assert!(d.is_empty() && c.is_none());
        let (d, c) = parse_detections(r#"{"detections": [], "cuts": [600.5, {"at": 1200, "start_unix": 1723965000}]}"#).unwrap();
        assert!(d.is_empty());
        assert_eq!(c, Some(vec![600.5, 1200.0]));
        // what save_dets writes round-trips
        let json = serde_json::to_string(&serde_json::json!({ "detections": Vec::<Detection>::new(), "cuts": [10.0, 20.0] })).unwrap();
        assert_eq!(parse_detections(&json).unwrap().1, Some(vec![10.0, 20.0]));
        assert!(parse_detections("{").is_err());
    }
}
