//! `overhead-adsb`: geolocate a recording from its aircraft Doppler curves and
//! historical ADS-B (adsb.lol).

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use clap::{Parser, Subcommand};
use overhead_adsb::e2e::{self, E2eConfig, TimeMode};
use overhead_adsb::locate::{self, LocateInput};
use overhead_adsb::matcher::{MatchConfig, utc};
use overhead_adsb::region::{self, TimeSpec};
use overhead_adsb::{archive, cache, geo};
use overhead_core::audio;
use overhead_core::detect::{self, DetectConfig, Detection};

#[derive(Parser)]
#[command(name = "overhead-adsb", about = "Match Doppler fly-bys against historical ADS-B (adsb.lol) to locate a recording")]
struct Cli {
    /// Cache directory (default: $OVERHEAD_DATA/adsb or ~/overhead-data/adsb).
    #[arg(long, global = true)]
    cache: Option<PathBuf>,
    /// Parallel HTTP range streams when downloading a day.
    #[arg(long, global = true, default_value_t = 8)]
    streams: usize,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args, Clone)]
struct WhenWhere {
    /// Recording day (local date of the place), YYYY-MM-DD. Required.
    #[arg(long)]
    day: NaiveDate,
    /// Last day of a 2-3 day range (inclusive, at most 2 days after --day).
    #[arg(long)]
    day_end: Option<NaiveDate>,
    /// Start time of the recording: RFC 3339 ("2026-08-15T14:30:00+02:00"),
    /// or local "HH:MM[:SS]" / "YYYY-MM-DD HH:MM" (local clock, see --tz).
    #[arg(long)]
    start: Option<String>,
    /// Uncertainty of --start (s).
    #[arg(long, default_value_t = 120.0)]
    start_tol: f64,
    /// Approximate local hours of the start, e.g. "13-17" or "13:30-15:00" (±30 min slack; on each day of a range).
    #[arg(long)]
    hours: Option<String>,
    /// Time zone of the local clock: an offset ("+02:00") or an IANA zone
    /// ("Europe/Warsaw"). Default: the zone(s) in force in --region on that day
    /// (time zone database, DST included); without a region every offset is tried.
    #[arg(long)]
    tz: Option<String>,
    /// Approximate region: a place name (city, voivodeship/state, country;
    /// looked up on OpenStreetMap), "lat,lon[,radius_km]" or "lat_min,lon_min,lat_max,lon_max".
    #[arg(long)]
    region: Option<String>,
    /// Extra margin around a named region (km).
    #[arg(long, default_value_t = 5.0)]
    region_margin_km: f64,
}

#[derive(Subcommand)]
enum Cmd {
    /// Download and index UTC days of adsb.lol history (~3-4 GB read per day, ~0.9 GB kept).
    Fetch {
        /// UTC day(s), YYYY-MM-DD.
        days: Vec<NaiveDate>,
    },
    /// Terrain at a point: SRTM and coarse (Terrarium) height above sea level, geoid undulation.
    #[command(allow_negative_numbers = true)]
    Terrain { lat: f64, lon: f64 },
    /// Show where adsb.lol archives for a day are and how big they are.
    Release { day: NaiveDate },
    /// List candidate (propeller/rotor/unknown) flights in a region and time.
    Tracks {
        #[arg(long)]
        day: NaiveDate,
        #[arg(long)]
        region: String,
        /// UTC hours "HH-HH".
        #[arg(long, default_value = "0-24")]
        utc_hours: String,
    },
    /// Locate a recording: detect fly-bys (or read them) and match them against ADS-B.
    Locate {
        /// Recording (any format overhead-core can read). Used for detection and spectrogram scoring.
        recording: Option<PathBuf>,
        /// Detections JSON from `overhead-cli detect` instead of running detection.
        #[arg(long)]
        detections: Option<PathBuf>,
        /// Recording length (s) when only detections are given.
        #[arg(long)]
        duration: Option<f64>,
        #[command(flatten)]
        when: WhenWhere,
        /// Only use these detections (indices, comma separated).
        #[arg(long, value_delimiter = ',')]
        only: Vec<usize>,
        /// Drop detections flagged as likely ground vehicles.
        #[arg(long)]
        no_ground: bool,
        #[arg(long, default_value_t = 10)]
        top: usize,
        /// Use a flat terrain height (m) instead of downloading SRTM.
        #[arg(long)]
        flat_ground: Option<f64>,
        /// Flat silence term instead of the detection-probability one: every
        /// prop aircraft passing closer than this (m) unheard costs 0.75.
        #[arg(long)]
        quiet_d: Option<f64>,
        /// Write the full JSON result here (default: stdout).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// A cut in the recording at T s (material removed there): the
        /// detections after it get their own start. "T" (that part's start
        /// free) or "T@START" with START as for --start (when that part
        /// started, ±--start-tol). Repeat or comma-separate for several.
        #[arg(long = "cut", value_delimiter = ',')]
        cuts: Vec<String>,
        /// Most material removed at one cut (s); default: up to the end of the possible day(s).
        #[arg(long)]
        cut_gap_max: Option<f64>,
        /// Detect likely cuts in the recording, print them, and use them as --cut.
        #[arg(long)]
        auto_cuts: bool,
        /// Find cuts from the fly-bys: try a cut in every gap between two
        /// detections (one search each) and keep those that make clearly
        /// more fly-bys fit together; adds `cut_search` to the JSON.
        #[arg(long)]
        find_cuts: bool,
        /// Most cuts --find-cuts adds.
        #[arg(long, default_value_t = 2)]
        find_cuts_max: usize,
        /// Add score maps, per-fly-by scores, modelled curves, silence and
        /// map links to the JSON ("explain"); the rest of the output is unchanged.
        #[arg(long)]
        explain: bool,
        /// Candidates explained (curves, silence, links) with --explain.
        #[arg(long, default_value_t = 3)]
        explain_top: usize,
        /// Fine score maps with --explain: half width and spacing (m).
        #[arg(long, default_value_t = 2500.0)]
        map_radius: f64,
        #[arg(long, default_value_t = 100.0)]
        map_step: f64,
    },
    /// Synthetic end-to-end test on a real ADS-B day.
    E2e {
        #[arg(long)]
        day: NaiveDate,
        /// Area where observers are placed: place name or numeric box (see locate --region).
        #[arg(long)]
        area: String,
        /// Search region given to the matcher (default: the area).
        #[arg(long)]
        search: Option<String>,
        #[arg(long, default_value_t = 5)]
        cases: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Recording length (s).
        #[arg(long, default_value_t = 2400.0)]
        duration: f64,
        /// Minimum passes closer than --close-d during the recording.
        #[arg(long, default_value_t = 2)]
        min_close: usize,
        #[arg(long, default_value_t = 3000.0)]
        close_d: f64,
        #[arg(long, value_enum, default_value_t = TimeMode::Day)]
        time: TimeMode,
        /// Ambient noise floor range at 100 Hz (dB re 20 µPa²/Hz), "lo,hi".
        #[arg(long, default_value = "14,22")]
        noise: String,
        /// UTC hours in which recording starts are drawn, "lo,hi".
        #[arg(long, default_value = "6,17")]
        hours_utc: String,
        /// Output directory (default: $OVERHEAD_DATA/e2e or ~/overhead-data/e2e).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Only run these case indices.
        #[arg(long, value_delimiter = ',')]
        only: Vec<usize>,
        /// Reuse rendered audio, truth and detections from an earlier run in --out.
        #[arg(long)]
        reuse: bool,
        /// Match from the detections only (as `locate --detections`), without spectrogram scoring.
        #[arg(long)]
        detections_only: bool,
        /// Write results to this directory instead of --out (audio is still read from --out with --reuse).
        #[arg(long)]
        results: Option<PathBuf>,
        /// Only render the audio and run stage 1 (for a later --reuse).
        #[arg(long)]
        render_only: bool,
    },
}

/// "T" or "T@START" (START: RFC 3339 with an offset, the absolute start of
/// the part after the cut).
fn parse_cut(s: &str, tol: f64) -> Result<locate::Cut> {
    let (at, start) = match s.split_once('@') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let at: f64 = at.trim().parse().with_context(|| format!("--cut {s}: expected seconds"))?;
    let start_unix = match start {
        Some(b) => match region::parse_start(b)? {
            region::StartTime::Utc(t) => Some(t.timestamp() as f64 - at),
            _ => bail!("--cut {s}: give the part's start as RFC 3339 with an offset (e.g. 2024-08-18T08:10:00Z)"),
        },
        None => None,
    };
    Ok(locate::Cut { at, start_unix, start_tol_s: start_unix.map(|_| tol) })
}

fn pair(s: &str) -> Result<(f64, f64)> {
    let (a, b) = s.split_once([',', '-']).context("expected lo,hi")?;
    Ok((a.trim().parse()?, b.trim().parse()?))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.cache.clone().unwrap_or_else(cache::default_root);
    match cli.cmd {
        Cmd::Fetch { days } => {
            for d in days {
                let m = archive::fetch_day(&root, d, cli.streams)?;
                println!("{}", serde_json::to_string_pretty(&m)?);
            }
        }
        Cmd::Terrain { lat, lon } => {
            let dem = locate::dem_for(&root, None);
            let coarse = dem.coarse_twin().context("coarse DEM")?;
            println!(
                "SRTM {:.1} m  coarse {:.1} m  geoid N {:.1} m  (ellipsoidal {:.1} m)",
                dem.height_msl(lat, lon),
                coarse.height_msl(lat, lon),
                overhead_adsb::geoid::undulation(lat, lon),
                dem.height(lat, lon)
            );
        }
        Cmd::Release { day } => {
            for r in archive::resolve(day)? {
                println!("{} {:.2} GB", r.tag, r.size() as f64 / 1e9);
                for p in &r.parts {
                    println!("  {} {:.2} GB", p.url, p.size as f64 / 1e9);
                }
            }
        }
        Cmd::Tracks { day, region: reg, utc_hours } => {
            let r = region::resolve(&reg, &root, 0.0)?;
            let (h0, h1) = pair(&utc_hours)?;
            let t0 = day.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() as f64;
            let d = cache::Day::ensure(&root, day, cli.streams)?;
            let tracks = d.tracks(&r.bbox, t0 + h0 * 3600.0, t0 + h1 * 3600.0, 9000.0)?;
            println!("{} candidate flights touching {:?}", tracks.len(), r.bbox);
            for t in &tracks {
                let inside: Vec<&overhead_adsb::trace::Point> =
                    t.points.iter().filter(|p| !p.ground && r.bbox.contains(p.lat, p.lon)).collect();
                if inside.is_empty() {
                    continue;
                }
                let alt = inside.iter().filter_map(|p| p.alt_geom.or(p.alt_baro)).fold(f64::INFINITY, f64::min);
                println!(
                    "{} {:8} {:5} {:10?} {:9} pts={:5} {} .. {} min_alt={:.0} m",
                    t.hex,
                    t.reg.clone().unwrap_or_default(),
                    t.type_code.clone().unwrap_or_default(),
                    t.class,
                    t.callsigns.first().map(|c| c.1.clone()).unwrap_or_default(),
                    inside.len(),
                    utc(inside[0].t),
                    utc(inside[inside.len() - 1].t),
                    alt
                );
            }
        }
        Cmd::Locate {
            recording,
            detections,
            duration,
            when,
            only,
            no_ground,
            top,
            flat_ground,
            quiet_d,
            out,
            cuts: cut_args,
            cut_gap_max,
            auto_cuts,
            find_cuts,
            find_cuts_max,
            explain,
            explain_top,
            map_radius,
            map_step,
        } => {
            let t_audio = std::time::Instant::now();
            let audio = match &recording {
                Some(p) => Some(audio::load(p, audio::DEFAULT_TARGET_RATE)?),
                None => None,
            };
            if std::env::var_os("OVERHEAD_TIMING").is_some() {
                eprintln!("[timing] audio {:.2}s", t_audio.elapsed().as_secs_f64());
            }
            let mut file_cuts: Vec<locate::Cut> = Vec::new();
            let mut dets: Vec<Detection> = match (&detections, &audio) {
                (Some(p), _) => {
                    let (d, c) = locate::read_detections(&std::fs::read(p)?).context("detections JSON")?;
                    file_cuts = c;
                    d
                }
                (None, Some(a)) => {
                    eprintln!("detecting fly-bys in {:.0} s of audio ...", a.duration());
                    detect::detect(a, &DetectConfig::default())
                }
                (None, None) => bail!("give a recording and/or --detections"),
            };
            let mut cuts = file_cuts;
            for c in &cut_args {
                cuts.push(parse_cut(c, when.start_tol)?);
            }
            if auto_cuts {
                let a = audio.as_ref().context("--auto-cuts needs the recording")?;
                let mut found = overhead_core::cuts::suggest(a, &overhead_core::cuts::CutConfig::default());
                // a sound that starts or stops inside a fly-by is the fly-by itself
                // (or a cut that ruins that detection anyway)
                found.retain(|s| {
                    let inside = dets.iter().position(|d| s.at > d.region.t_start && s.at < d.region.t_end);
                    if let Some(i) = inside {
                        eprintln!("  ignored suggested cut at {:.2} s ({}): inside detection {i}", s.at, s.kind.label());
                    }
                    inside.is_none()
                });
                for s in &found {
                    eprintln!("  suggested cut at {:.2} s ({}, strength {:.1})", s.at, s.kind.label(), s.strength);
                }
                if found.is_empty() {
                    eprintln!("  no cuts found");
                } else {
                    eprintln!(
                        "  every part after a cut gets its own free start: a wrong cut (a bird call, a gust) weakens the timing check; \
                         check them with `overhead-cli cuts` and pass only the real ones with --cut"
                    );
                }
                cuts.extend(found.iter().map(|s| locate::Cut { at: s.at, start_unix: None, start_tol_s: None }));
            }
            let duration = match (&audio, duration) {
                (Some(a), _) => a.duration(),
                (None, Some(d)) => d,
                (None, None) => dets.iter().map(|d| d.region.t_end).fold(0.0, f64::max) + 60.0,
            };
            if !only.is_empty() {
                dets = only.iter().filter_map(|i| dets.get(*i).cloned()).collect();
            }
            if no_ground {
                dets.retain(|d| !d.likely_ground_vehicle);
            }
            for (i, d) in dets.iter().enumerate() {
                eprintln!(
                    "  det {i}: tc={:.1} s f0={:.1} Hz v={:.0} m/s [{:.0}-{:.0}] d={:.0} m [{:.0}-{:.0}]{}{}",
                    d.flyby.tc,
                    d.flyby.f0,
                    d.flyby.v,
                    d.ranges.v.0,
                    d.ranges.v.1,
                    d.flyby.d,
                    d.ranges.d.0,
                    d.ranges.d.1,
                    if d.confident { "" } else { " (unconfident)" },
                    if d.likely_ground_vehicle { " (ground vehicle?)" } else { "" }
                );
            }
            // cuts that cannot be right: past the end, or inside a fly-by
            cuts.retain(|c| {
                let keep = c.at > 0.0 && c.at < duration;
                if !keep {
                    eprintln!("WARNING: cut at {:.1} s is outside the recording (0 .. {duration:.1} s); ignored", c.at);
                }
                keep
            });
            for c in &cuts {
                if let Some(i) = dets.iter().position(|d| c.at > d.region.t_start && c.at < d.region.t_end) {
                    let d = &dets[i];
                    eprintln!(
                        "WARNING: cut at {:.1} s is inside detection {i} ({:.0}-{:.0} s, tc {:.1} s): that fly-by is split; \
                         it counts as after the cut if its tc is, and the part it opens gets a free start",
                        c.at, d.region.t_start, d.region.t_end, d.flyby.tc
                    );
                }
            }
            let reg = match &when.region {
                Some(r) => Some(region::resolve(r, &root, when.region_margin_km * 1000.0)?),
                None => None,
            };
            let time = TimeSpec::from_inputs(
                when.day,
                when.day_end,
                when.start.as_deref(),
                when.start_tol,
                when.hours.as_deref(),
                when.tz.as_deref(),
                reg.as_ref().map(|r| &r.bbox),
            )?;
            eprintln!("UTC offset(s) {:?} h ({})", time.offsets.iter().map(|o| *o as f64 / 3600.0).collect::<Vec<_>>(), time.offset_source);
            let cfg = MatchConfig { top, quiet_d, ..MatchConfig::default() };
            let inp = LocateInput {
                detections: dets,
                audio: audio.as_ref(),
                duration,
                time,
                region: reg,
                cfg,
                cache_root: root.clone(),
                streams: cli.streams,
                flat_ground,
                truth: None,
                cuts,
                cut_gap_max_s: cut_gap_max,
            };
            let mut inp = inp;
            let cut_search = if find_cuts {
                eprintln!("finding cuts: one search per gap between fly-bys ...");
                let cs = locate::find_cuts(&inp, find_cuts_max)?;
                if cs.suggested.is_empty() {
                    eprintln!("no gap between fly-bys looks like a cut (baseline score {:.2}, {} matched)", cs.baseline_score, cs.baseline_matched);
                }
                for f in &cs.suggested {
                    eprintln!(
                        "cut found from the fly-bys: somewhere between {:.1} and {:.1} s (score {:+.2}, {:+} fly-bys); searched at {:.1} s",
                        f.range.0, f.range.1, f.score_gain, f.matched_gain, f.at
                    );
                    if let Some((t, st)) = f.audio_hint {
                        eprintln!("  strongest waveform evidence in that span: {t:.2} s (strength {st:.2}); a hint for the exact place");
                    }
                    inp.cuts.push(locate::Cut { at: f.at, start_unix: None, start_tol_s: None });
                }
                Some(cs)
            } else {
                None
            };
            let mut res = if explain {
                let (mut res, session) = locate::locate_session(inp)?;
                let t0 = std::time::Instant::now();
                let opt = overhead_adsb::explain::ScoreMapOptions { fine_radius_m: map_radius, fine_step_m: map_step, ..Default::default() };
                res.explain = Some(session.explain(&opt, explain_top));
                eprintln!("explain: {:.1} s", t0.elapsed().as_secs_f64());
                res
            } else {
                locate::locate(inp)?
            };
            res.cut_search = cut_search;
            for c in &res.cuts {
                eprintln!("cut at {:.1} s: the part after it may start {} .. {}", c.at, c.start_window_utc.0, c.start_window_utc.1);
            }
            for c in &res.candidates {
                let start_note = match c.start_part {
                    Some(k) if k > 0 => {
                        format!(" (time zero of part {k}, after the cut at {:.1} s; no fly-by before it matched)", res.cuts[k - 1].at)
                    }
                    _ => String::new(),
                };
                eprintln!(
                    "#{} {:.5},{:.5} ±{:.0} m  score {:.2} (margin {:.2}, {} matched, {} silence violations)  start {}{start_note}  c0 {:.1}",
                    c.rank,
                    c.lat,
                    c.lon,
                    c.uncertainty.radius_m,
                    c.score,
                    c.margin,
                    c.matched,
                    c.violations.len(),
                    c.start_utc,
                    c.c0
                );
                for m in &c.flights {
                    eprintln!(
                        "     det {} tc={:.0}s  {} {} {} {}  heard {}  d={:.0} m v={:.0} m/s h={:.0} m  curve {:.2}{}{}",
                        m.detection,
                        m.tc,
                        m.hex,
                        m.reg.clone().unwrap_or_default(),
                        m.type_code.clone().unwrap_or_default(),
                        m.callsign.clone().unwrap_or_default(),
                        m.heard_utc,
                        m.slant_m,
                        m.speed_ms,
                        m.height_agl_m,
                        m.curve_score,
                        if m.vd_evidence < 0.995 { format!("  v/d evidence {:.2}", m.vd_evidence) } else { String::new() },
                        match &m.repeat {
                            Some(r) if r.factor < 0.995 => format!("  repeats det {} (x{:.2})", r.of_detection, r.factor),
                            _ => String::new(),
                        }
                    );
                }
            }
            if let Some(a) = &res.ambiguity {
                let pct = |k: usize| a.top_mass.iter().find(|m| m.0 == k).map_or(0.0, |m| 100.0 * m.1);
                eprintln!(
                    "{}; probability of the best 1/3/10 places {:.0}/{:.0}/{:.0} %, elsewhere {:.0} %; {} distinct places, plausible area {} cells of {:.1} km",
                    a.summary,
                    pct(1),
                    pct(3),
                    pct(10),
                    100.0 * a.elsewhere,
                    a.total_places,
                    a.area.len(),
                    a.area_cell_m / 1000.0
                );
                if !a.confident {
                    for (i, p) in a.places.iter().take(8).enumerate() {
                        eprintln!(
                            "  place {:2}  {:4.1} %  {:.5},{:.5}  score {:.2}  {}{}",
                            i + 1,
                            100.0 * p.probability,
                            p.lat,
                            p.lon,
                            p.score,
                            p.flights.join(", "),
                            p.candidate_rank.map_or(String::new(), |r| format!("  (#{r})"))
                        );
                    }
                    for h in &a.hints {
                        eprintln!("  to narrow it down: {h}");
                    }
                }
            }
            let json = serde_json::to_string_pretty(&res)?;
            match out {
                Some(p) => std::fs::write(p, json)?,
                None => println!("{json}"),
            }
        }
        Cmd::E2e { day, area, search, cases, seed, duration, min_close, close_d, time, noise, hours_utc, out, only, reuse, detections_only, results: results_dir, render_only } => {
            let out = out.unwrap_or_else(|| overhead_adsb::cache::data_root().join("e2e"));
            let area = region::resolve(&area, &root, 0.0)?.bbox;
            let search = search.map(|s| region::resolve(&s, &root, 0.0).map(|r| r.bbox)).transpose()?;
            let cfg = E2eConfig {
                day,
                area,
                search,
                cases,
                seed,
                duration,
                min_close,
                close_d,
                time_mode: time,
                out: out.clone(),
                cache_root: root.clone(),
                streams: cli.streams,
                sample_rate: 8000,
                noise_db: pair(&noise)?,
                hours_utc: pair(&hours_utc)?,
                cfg: MatchConfig::default(),
                reuse,
                detections_only,
                results: results_dir.clone().unwrap_or_else(|| out.clone()),
                render_only,
            };
            let flights = e2e::load_area_flights(&cfg)?;
            eprintln!("{} candidate flights in the area", flights.len());
            let dem = locate::dem_for(&root, None);
            dem.prefetch(&area.expand(20_000.0));
            let mut results = Vec::new();
            for i in 0..cases {
                if !only.is_empty() && !only.contains(&i) {
                    continue;
                }
                match e2e::run_case(i, &cfg, &flights, &dem) {
                    Ok(Some(r)) => results.push(r),
                    Ok(None) => {}
                    Err(e) => eprintln!("case {i}: {e:#}"),
                }
            }
            let md = e2e::summary_markdown(&results);
            println!("{md}");
            let errs: Vec<f64> = results.iter().filter_map(|r| r.error_m).collect();
            let ok = errs.iter().filter(|e| **e < 500.0).count();
            println!("recovered within 500 m: {ok}/{} cases", results.len());
            let rdir = results_dir.unwrap_or(out);
            std::fs::create_dir_all(&rdir)?;
            std::fs::write(rdir.join(format!("summary-{day}-s{seed}-{time:?}.md")), &md)?;
            let _ = geo::BBox::WORLD;
        }
    }
    Ok(())
}
