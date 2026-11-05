//! overhead (overhead-app): desktop UI for finding where a recording was made from the
//! Doppler curves of passing aircraft.

mod adsb;
mod analysis;
mod audio;
mod draw;
mod explain_data;
mod heat;
mod history;
mod icons;
mod layout;
mod location;
mod map;
mod query;
mod spectro;
mod text_input;
mod timeline;
mod theme;
mod tiles;
mod update;
mod workspace;

use std::borrow::Cow;
use std::path::PathBuf;

use gpui::{App, Application, Bounds, TitlebarOptions, WindowDecorations, WindowOptions, prelude::*, px, size};

use workspace::{Options, Workspace};

const USAGE: &str = "usage: overhead [RECORDING] [options]

  --light                 start with the light theme
  --details               show the expert details (rest frequency, flags, FFT controls, numbers)
  --edit                  open Edit fly-bys when detection ends
  --day YYYY-MM-DD        recording day (required for the location search)
  --day-end YYYY-MM-DD    last day of a 2-3 day range
  --start HH:MM[:SS]      exact local start time
  --hours H1-H2           approximate local hours, e.g. 14-17
  --utc +01:00|ZONE       UTC offset or IANA zone (Europe/Warsaw) of the times above
                          (default: the region's zone on that day, from the time zone database)
  --city / --state / --country NAME   approximate region (a place name, or lat,lon[,radius_km] as city)
  --locate                run the location search as soon as detection ends
  --select N              select fly-by N (1-based) when detection ends
  --candidate N           select location candidate N (1-based) when the search ends
  --map-area              frame the whole search area on the map (default: the location)
  --no-basemap            no OpenStreetMap tiles (plain grid; no network for the map)
  --sample-data           debug: invented location results instead of the ADS-B search
  --zoom T0:T1            initial time window (s)
  --fzoom F0:F1           initial frequency window (Hz)
  --cursor T              initial timeline cursor (s)
  --detections FILE.json  load detections from `overhead-cli detect` output instead of detecting
  --size WxH              window size in logical pixels (instead of the size kept from the last run)
  --spectrogram           show the spectrogram (also kept from the last run; Ctrl+Shift+S)
  --no-spectrogram        hide it: the map takes its place
  --default-layout        start with the default panel sizes and window (ignore the saved layout)
  --cut T                 a cut in the recording at T s (repeat for more; each part gets its own start)
  --analysis TAB          open the analysis when the search ends: places, heat, scores, silence, timing
  --compare N             compare place N (as the analysis numbers them) with the selected one
  --others                show the other possible places on the map
  --version               print the build number";

/// Smallest window (logical px). Narrower than half of a 1920 px screen, so
/// Aero snap / tiling can put it side by side with another window.
const MIN_W: f32 = 940.0;
const MIN_H: f32 = 600.0;

fn parse_args() -> (Options, Option<(f32, f32)>) {
    let mut o = Options::default();
    let mut win = None;
    let mut args = std::env::args().skip(1);
    let need = |a: &mut dyn Iterator<Item = String>, flag: &str| -> String {
        a.next().unwrap_or_else(|| {
            eprintln!("{flag} needs a value\n\n{USAGE}");
            std::process::exit(2)
        })
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--version" => {
                println!("overhead {}", update::build_label());
                std::process::exit(0);
            }
            "--light" => o.light = true,
            "--details" => o.details = true,
            "--edit" => o.edit = true,
            "--spectrogram" => o.spectrogram = Some(true),
            "--no-spectrogram" => o.spectrogram = Some(false),
            "--default-layout" => o.default_layout = true,
            "--day" => o.day = Some(need(&mut args, &a)),
            "--day-end" => o.day_end = Some(need(&mut args, &a)),
            "--start" => o.start = Some(need(&mut args, &a)),
            "--hours" => {
                let v = need(&mut args, &a);
                let (x, y) = v.split_once('-').unwrap_or((&v, ""));
                o.hours = Some((x.to_string(), y.to_string()));
            }
            "--utc" => o.utc = Some(need(&mut args, &a)),
            "--city" => o.city = Some(need(&mut args, &a)),
            "--state" => o.state = Some(need(&mut args, &a)),
            "--country" => o.country = Some(need(&mut args, &a)),
            "--locate" => o.locate = true,
            "--sample-data" => o.sample_data = true,
            "--no-basemap" => o.no_basemap = true,
            "--map-area" => o.map_area = true,
            "--candidate" => o.candidate = need(&mut args, &a).parse().ok(),
            "--select" => o.select = need(&mut args, &a).parse().ok(),
            "--cursor" => o.cursor = need(&mut args, &a).parse().ok(),
            "--zoom" => {
                let v = need(&mut args, &a);
                if let Some((x, y)) = v.split_once(':') {
                    o.zoom = x.parse().ok().zip(y.parse().ok());
                }
            }
            "--fzoom" => {
                let v = need(&mut args, &a);
                if let Some((x, y)) = v.split_once(':') {
                    o.fzoom = x.parse().ok().zip(y.parse().ok());
                }
            }
            "--cut" => {
                for v in need(&mut args, &a).split(',') {
                    if let Ok(t) = v.trim().parse::<f64>() {
                        o.cuts.push(t);
                    }
                }
            }
            "--analysis" => o.analysis = Some(need(&mut args, &a)),
            "--compare" => o.compare = need(&mut args, &a).parse().ok(),
            "--others" => o.others = true,
            "--detections" => o.detections_json = Some(PathBuf::from(need(&mut args, &a))),
            "--size" => {
                let v = need(&mut args, &a);
                if let Some((w, h)) = v.split_once('x')
                    && let (Ok(w), Ok(h)) = (w.parse(), h.parse())
                {
                    win = Some((w, h));
                }
            }
            s if s.starts_with("--") => {
                eprintln!("unknown option {s}\n\n{USAGE}");
                std::process::exit(2);
            }
            _ => o.file = Some(PathBuf::from(a)),
        }
    }
    (o, win)
}

/// Minimal stderr logger: `OVERHEAD_LOG=info|debug|warn` (default warn).
struct StderrLog(log::LevelFilter);

impl log::Log for StderrLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        // the MP3 decoder warns about every chunk a parallel decode starts in
        // the middle of the stream (its bit reservoir is still empty there);
        // those packets are dropped, so its warnings only show with debug
        if m.target().starts_with("symphonia") && m.level() > log::Level::Error && self.0 < log::LevelFilter::Debug {
            return false;
        }
        m.level() <= self.0
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("[{:5}] {}: {}", r.level(), r.target(), r.args());
        }
    }
    fn flush(&self) {}
}

fn init_log() {
    let level = match std::env::var("OVERHEAD_LOG").as_deref() {
        Ok("trace") => log::LevelFilter::Trace,
        Ok("debug") => log::LevelFilter::Debug,
        Ok("info") => log::LevelFilter::Info,
        Ok("error") => log::LevelFilter::Error,
        _ => log::LevelFilter::Warn,
    };
    if log::set_boxed_logger(Box::new(StderrLog(level))).is_ok() {
        log::set_max_level(level);
    }
}

fn main() {
    init_log();
    let (mut opts, win) = parse_args();
    let mut lay = if opts.default_layout { layout::Layout::default() } else { layout::load() };
    if let Some(v) = opts.spectrogram {
        lay.spectrogram = v;
    }
    // what the command line asks to look at needs the spectrogram: shown for
    // this run only (the saved choice is kept for the next one)
    opts.spectrogram_once = opts.spectrogram.is_none() && (opts.edit || opts.zoom.is_some() || opts.fzoom.is_some());
    opts.layout = lay;
    Application::new().run(move |cx: &mut App| {
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf").as_slice()),
            ])
            .expect("bundled fonts");
        cx.set_global(if opts.light { theme::Theme::light() } else { theme::Theme::dark() });
        text_input::bind_keys(cx);
        workspace::bind_keys(cx);
        cx.on_action(|_: &workspace::Quit, cx| cx.quit());

        let (w, h) = win.unwrap_or((1600.0, 960.0));
        let fallback = Bounds::centered(None, size(px(w), px(h)), cx);
        let screens: Vec<_> = cx.displays().iter().map(|d| d.bounds()).collect();
        let bounds = if win.is_some() || opts.default_layout {
            gpui::WindowBounds::Windowed(fallback)
        } else {
            layout::restore_bounds(lay.window, &screens, fallback, (MIN_W, MIN_H))
        };
        log::info!("window opens at {bounds:?}");
        let opts = opts.clone();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(bounds),
                titlebar: Some(TitlebarOptions { title: Some("Overhead".into()), appears_transparent: true, traffic_light_position: None }),
                window_decorations: Some(WindowDecorations::Client),
                app_id: Some("overhead".into()),
                window_min_size: Some(size(px(MIN_W), px(MIN_H))),
                ..Default::default()
            },
            move |window, cx| cx.new(|cx| Workspace::new(opts, window, cx)),
        )
        .expect("open window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
