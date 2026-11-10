//! Command-line access to overhead-core: render spectrograms, fit fly-bys,
//! generate synthetic test recordings.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use overhead_core::fit::{self, FitConfig, Region};
use overhead_core::model::{DEFAULT_C, Flyby};
use overhead_core::spectrogram::{self, SpecRequest};
use overhead_core::synth::{self, SynthFlyby};
use overhead_core::{audio, background, colormap, cuts, detect, listen};

#[derive(Parser)]
#[command(name = "overhead-cli", about = "Aircraft Doppler-curve analysis")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print basic info about a recording.
    Info { file: PathBuf },
    /// Render a spectrogram window to PNG.
    Spec {
        file: PathBuf,
        #[arg(long, default_value_t = 0.0)]
        t0: f64,
        /// End time (s); defaults to end of file.
        #[arg(long)]
        t1: Option<f64>,
        #[arg(long, default_value_t = 0.0)]
        fmin: f64,
        #[arg(long, default_value_t = 1000.0)]
        fmax: f64,
        #[arg(long, default_value_t = 8192)]
        nfft: usize,
        #[arg(long, default_value_t = 1600)]
        width: u32,
        #[arg(long, default_value_t = 600)]
        height: u32,
        /// Remove stationary lines and broadband bursts.
        #[arg(long)]
        whiten: bool,
        /// Draw the map the ADS-B matcher scores a fly-by's candidates on
        /// instead: z-scores 0-6 with level tones (hum, machines) taken out.
        /// Set OVERHEAD_KEEP_LEVEL_TONES=1 to see it with them.
        #[arg(long)]
        score_map: bool,
        /// With --score-map: also write the map's numbers to this file, one
        /// JSON line (t_start, dt, f_min, df, cols, rows, baseline) and then
        /// the values as little-endian f32, column by column.
        #[arg(long, requires = "score_map")]
        dump: Option<PathBuf>,
        /// Overlay a model curve "f0,v,d,tc" (repeatable).
        #[arg(long = "curve", value_parser = parse_flyby)]
        curves: Vec<Flyby>,
        /// Vertical grid line every N seconds.
        #[arg(long, default_value_t = 60.0)]
        grid_s: f64,
        /// Horizontal grid line every N Hz.
        #[arg(long, default_value_t = 50.0)]
        grid_hz: f64,
        #[arg(short, long, default_value = "spectrogram.png")]
        out: PathBuf,
    },
    /// Fit a fly-by inside a time/frequency box; prints JSON.
    Fit {
        file: PathBuf,
        #[arg(long)]
        t0: f64,
        #[arg(long)]
        t1: f64,
        #[arg(long)]
        fmin: f64,
        #[arg(long)]
        fmax: f64,
        /// Harmonic multipliers used for scoring, e.g. "1,2".
        #[arg(long, value_delimiter = ',', default_value = "1")]
        harmonics: Vec<f64>,
        #[arg(long, default_value_t = DEFAULT_C)]
        c: f64,
        /// Include the tracked ridge points in the output.
        #[arg(long)]
        ridge: bool,
    },
    /// Fit the Doppler model to points traced by hand along a line; prints a
    /// detection (JSON array of one) for `overhead-adsb locate --detections`.
    /// Points as "t,f t,f ..." (recording seconds, Hz), at least 4, ideally
    /// 3-4 on each side of the closest approach.
    Trace {
        /// The points, e.g. "100,121.5 110,119 ...".
        #[arg(long, num_args = 1.., value_parser = parse_point)]
        points: Vec<[f64; 2]>,
        #[arg(long, default_value_t = DEFAULT_C)]
        c: f64,
    },
    /// Automatically detect every fly-by in a recording; prints JSON.
    Detect {
        file: PathBuf,
        #[arg(long, default_value_t = 40.0)]
        fmin: f64,
        #[arg(long, default_value_t = 1000.0)]
        fmax: f64,
        #[arg(long, default_value_t = 6.0)]
        min_contrast: f64,
    },
    /// How busy the background is (talking, music): prints JSON.
    Background { file: PathBuf },
    /// Write a synthetic recording with two fly-bys 920 s apart; prints the truth.
    Synth {
        #[arg(default_value = "synthetic.wav")]
        out: PathBuf,
        #[arg(long, default_value_t = 0.3)]
        noise: f64,
    },
    /// Suggest where the recording was cut (digital silence, waveform jumps,
    /// abrupt background changes); prints JSON for `overhead-adsb locate --cut`.
    Cuts {
        file: PathBuf,
        /// Background change threshold (standard errors, RMS over bands).
        #[arg(long)]
        change_threshold: Option<f64>,
        /// Waveform jump threshold (times the local scale).
        #[arg(long)]
        jump_ratio: Option<f64>,
        #[arg(long)]
        max: Option<usize>,
        /// Start from the sensitive settings (weaker splices, more false alarms).
        #[arg(long)]
        sensitive: bool,
    },
    /// Render part of a recording with a frequency boost to a WAV file (to
    /// hear a faint propeller, as in the reference video).
    Listen {
        file: PathBuf,
        #[arg(long, default_value_t = 0.0)]
        t0: f64,
        /// End (s); default: 60 s after --t0.
        #[arg(long)]
        t1: Option<f64>,
        /// Band to boost, "LO-HI" Hz; default: no boost.
        #[arg(long)]
        band: Option<String>,
        /// Level of the band and of everything else (dB).
        #[arg(long, default_value_t = 18.0)]
        band_gain: f64,
        #[arg(long, default_value_t = -12.0, allow_hyphen_values = true)]
        rest_gain: f64,
        /// Overall gain (dB); default: brings the excerpt to −20 dBFS RMS.
        #[arg(long, allow_hyphen_values = true)]
        gain: Option<f64>,
        #[arg(long, default_value_t = 44100)]
        rate: u32,
        #[arg(short, long, default_value = "listen.wav")]
        out: PathBuf,
    },
}

fn parse_point(s: &str) -> Result<[f64; 2], String> {
    let v: Vec<f64> = s.split(',').map(|x| x.trim().parse::<f64>()).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    match v[..] {
        [t, f] => Ok([t, f]),
        _ => Err(format!("expected t,f, got {s:?}")),
    }
}

fn parse_flyby(s: &str) -> Result<Flyby, String> {
    let v: Vec<f64> = s
        .split(',')
        .map(|x| x.trim().parse::<f64>().map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    match v.as_slice() {
        [f0, v, d, tc] => Ok(Flyby { f0: *f0, v: *v, d: *d, tc: *tc }),
        _ => Err("expected f0,v,d,tc".into()),
    }
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Info { file } => {
            let a = load(&file)?;
            println!(
                "{}: {:.1} s, source {} Hz, analysed at {} Hz via {}",
                file.display(),
                a.duration(),
                a.source_rate,
                a.sample_rate,
                a.decoder
            );
        }
        Cmd::Spec { file, t0, t1, fmin, fmax, nfft, width, height, whiten, score_map, dump, curves, grid_s, grid_hz, out } => {
            let a = load(&file)?;
            let t1 = t1.unwrap_or(a.duration()).min(a.duration());
            if t1 <= t0 {
                bail!("empty time range");
            }
            let spec = spectrogram::compute(
                &a,
                &SpecRequest { t_start: t0, t_end: t1, f_min: fmin, f_max: fmax, columns: width as usize, n_fft: nfft, whiten },
            );
            let (mut lo, mut hi) = spec.quantiles(0.05, 0.998);
            let mut spec = spec;
            if score_map {
                let region = overhead_core::fit::Region { t_start: t0, t_end: t1, f_min: fmin, f_max: fmax };
                let cfg = overhead_core::fit::FitConfig { n_fft: nfft, ..Default::default() };
                let map = overhead_core::fit::region_score_map(&a, &region, &cfg);
                eprintln!("level tones: {:.1} % of the map taken out", 100.0 * map.level_cells as f64 / map.spec.db.len().max(1) as f64);
                spec = map.spec;
                (lo, hi) = (0.0, 6.0);
                if let Some(path) = &dump {
                    use std::io::Write;
                    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
                    writeln!(
                        f,
                        "{}",
                        serde_json::json!({"t_start": spec.t_start, "dt": spec.dt, "f_min": spec.f_min, "df": spec.df, "cols": spec.cols, "rows": spec.rows, "baseline": map.baseline})
                    )?;
                    for v in &spec.db {
                        f.write_all(&v.to_le_bytes())?;
                    }
                }
            }
            let lut = colormap::lut();
            let mut img = image::RgbImage::new(width, height);
            for x in 0..width {
                // the column at this pixel's time (the score map has its own column spacing)
                let t = t0 + (x as f64 + 0.5) / width as f64 * (t1 - t0);
                let col = (((t - spec.t_start) / spec.dt).floor().max(0.0) as usize).min(spec.cols.saturating_sub(1));
                for y in 0..height {
                    let f = fmax - (fmax - fmin) * (y as f64 + 0.5) / height as f64;
                    let v = spec.sample(col, f).unwrap_or(lo);
                    let n = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
                    img.put_pixel(x, y, image::Rgb(lut[(n * 255.0) as usize]));
                }
            }
            let to_x = |t: f64| ((t - t0) / (t1 - t0) * width as f64) as i64;
            let to_y = |f: f64| ((fmax - f) / (fmax - fmin) * height as f64) as i64;
            let mut blend = |x: i64, y: i64, c: [u8; 3], alpha: f32| {
                if x >= 0 && y >= 0 && (x as u32) < width && (y as u32) < height {
                    let p = img.get_pixel_mut(x as u32, y as u32);
                    for k in 0..3 {
                        p.0[k] = (p.0[k] as f32 * (1.0 - alpha) + c[k] as f32 * alpha) as u8;
                    }
                }
            };
            let mut g = (t0 / grid_s).ceil() * grid_s;
            while g < t1 {
                let x = to_x(g);
                (0..height as i64).for_each(|y| blend(x, y, [255, 255, 255], 0.25));
                g += grid_s;
            }
            let mut g = (fmin / grid_hz).ceil() * grid_hz;
            while g < fmax {
                let y = to_y(g);
                (0..width as i64).for_each(|x| blend(x, y, [255, 255, 255], 0.25));
                g += grid_hz;
            }
            for p in &curves {
                for x in 0..width as i64 {
                    let t = t0 + (x as f64 + 0.5) / width as f64 * (t1 - t0);
                    let y = to_y(p.freq_at(t, DEFAULT_C));
                    blend(x, y, [0, 255, 200], 0.8);
                }
            }
            img.save(&out).with_context(|| format!("write {}", out.display()))?;
            println!(
                "{} ({}x{}): t {t0:.1}..{t1:.1} s, f {fmin:.0}..{fmax:.0} Hz, grid {grid_s} s / {grid_hz} Hz, df {:.3} Hz, dt {:.3} s",
                out.display(),
                width,
                height,
                spec.df,
                spec.dt
            );
        }
        Cmd::Fit { file, t0, t1, fmin, fmax, harmonics, c, ridge } => {
            let a = load(&file)?;
            let cfg = FitConfig { c, harmonics, ..FitConfig::default() };
            let region = Region { t_start: t0, t_end: t1, f_min: fmin, f_max: fmax };
            let mut res = fit::fit_region(&a, &region, &cfg).context("fit failed: region too small")?;
            if !ridge {
                res.ridge.clear();
            }
            let p = res.flyby;
            eprintln!(
                "v = {:.1} m/s ({:.0} km/h, {:.0} kt) [{:.1}..{:.1}], d = {:.0} m [{:.0}..{:.0}], tc = {:.2} s [{:.2}..{:.2}], f0 = {:.2} Hz, score {:.3} (noise {:.3})",
                p.v,
                p.v * 3.6,
                p.v * 1.943_844,
                res.ranges.v.0,
                res.ranges.v.1,
                p.d,
                res.ranges.d.0,
                res.ranges.d.1,
                p.tc,
                res.ranges.tc.0,
                res.ranges.tc.1,
                p.f0,
                res.score,
                res.baseline
            );
            println!("{}", serde_json::to_string_pretty(&res)?);
        }
        Cmd::Trace { points, c } => {
            let d = overhead_core::edit::trace_detection(&points, c).map_err(anyhow::Error::msg)?;
            let (p, r) = (d.flyby, d.ranges);
            eprintln!(
                "traced {} points: f0 = {:.2} Hz, v = {:.1} m/s [{:.1}..{:.1}], d = {:.0} m [{:.0}..{:.0}], tc = {:.2} s [{:.2}..{:.2}]{}",
                points.len(),
                p.f0,
                p.v,
                r.v.0,
                r.v.1,
                p.d,
                r.d.0,
                r.d.1,
                p.tc,
                r.tc.0,
                r.tc.1,
                if d.confident { "" } else { " (open ranges)" }
            );
            println!("{}", serde_json::to_string_pretty(&vec![d])?);
        }
        Cmd::Background { file } => {
            let a = load(&file)?;
            let b = background::measure(&a);
            println!("{}", serde_json::to_string_pretty(&b)?);
        }
        Cmd::Detect { file, fmin, fmax, min_contrast } => {
            let a = load(&file)?;
            let cfg = detect::DetectConfig { f_min: fmin, f_max: fmax, min_contrast, ..Default::default() };
            let start = std::time::Instant::now();
            let (found, stats) = detect::detect_with_stats(&a, &cfg);
            eprintln!("{stats:?} in {:.1} s", start.elapsed().as_secs_f64());
            for d in &found {
                let p = d.flyby;
                eprintln!(
                    "tc {:8.2} s  v {:6.1} m/s [{:.0}..{:.0}]  d {:6.0} m [{:.0}..{:.0}]  f0 {:6.2} Hz  contrast {:5.1}  cov {:.2} rms {:.2}{}  lines {:?}",
                    p.tc, p.v, d.ranges.v.0, d.ranges.v.1, p.d, d.ranges.d.0, d.ranges.d.1, p.f0, d.contrast,
                    d.quality.coverage, d.quality.rms_bins,
                    format!(
                        "{}{}",
                        if d.likely_ground_vehicle { "  [ground vehicle?]" } else { "" },
                        if d.confident { "" } else { "  [uncertain]" }
                    ),
                    d.harmonics.iter().map(|h| (h * p.f0 * 10.0).round() / 10.0).collect::<Vec<_>>()
                );
            }
            println!("{}", serde_json::to_string_pretty(&found)?);
        }
        Cmd::Synth { out, noise } => {
            let truth = [
                SynthFlyby {
                    flyby: Flyby { f0: 112.0, v: 72.0, d: 1100.0, tc: 200.0 },
                    harmonics: vec![1.0, 0.5, 0.3],
                    amplitude: 0.03,
                },
                SynthFlyby {
                    flyby: Flyby { f0: 131.0, v: 100.0, d: 2400.0, tc: 1120.0 },
                    harmonics: vec![1.0, 0.6, 0.25],
                    amplitude: 0.05,
                },
            ];
            let a = synth::synthesize(8000.0, 1300.0, &truth, noise, &[(60.0, 0.05), (120.0, 0.03)], 42);
            synth::write_wav(&out, &a)?;
            println!("wrote {}", out.display());
            for s in &truth {
                println!("truth: {}", serde_json::to_string(&s.flyby)?);
            }
        }
        Cmd::Cuts { file, change_threshold, jump_ratio, max, sensitive } => {
            let a = load(&file)?;
            let mut cfg = if sensitive { cuts::CutConfig::sensitive() } else { cuts::CutConfig::default() };
            if let Some(v) = change_threshold {
                cfg.change_threshold = v;
            }
            if let Some(v) = jump_ratio {
                cfg.jump_ratio = v;
            }
            if let Some(v) = max {
                cfg.max = v;
            }
            let t0 = std::time::Instant::now();
            let found = cuts::suggest(&a, &cfg);
            eprintln!("{} suggested cut(s) in {:.0} s of audio ({:.1} s)", found.len(), a.duration(), t0.elapsed().as_secs_f64());
            for s in &found {
                let kinds: Vec<&str> = s.kinds.iter().map(|k| k.label()).collect();
                eprintln!("  {:9.2} s  strength {:5.1}  {}", s.at, s.strength, kinds.join(" + "));
            }
            println!("{}", serde_json::to_string_pretty(&found)?);
        }
        Cmd::Listen { file, t0, t1, band, band_gain, rest_gain, gain, rate, out } => {
            let a = load(&file)?;
            let dur = a.duration();
            if !(t0 >= 0.0 && t0 < dur) {
                bail!("--t0 {t0} s is outside the recording (0 .. {dur:.1} s)");
            }
            if let Some(t1) = t1
                && t1 <= t0
            {
                bail!("--t1 {t1} s must be after --t0 {t0} s");
            }
            let t1 = t1.unwrap_or(t0 + 60.0).min(dur);
            let band = match band {
                Some(b) => {
                    let (lo, hi) = b.split_once('-').context("--band: expected LO-HI")?;
                    let raw = (lo.trim().parse::<f64>().context("--band LO")?, hi.trim().parse::<f64>().context("--band HI")?);
                    if raw.0 > raw.1 {
                        eprintln!("note: --band {b}: low edge above the high edge, using {}-{} Hz", raw.1, raw.0);
                    }
                    let nb = listen::normalized_band(raw, a.sample_rate)
                        .with_context(|| format!("--band {b}: no band left inside 1 .. {:.0} Hz", 0.49 * a.sample_rate))?;
                    if (nb.0 - raw.0.min(raw.1)).abs() > 0.5 || (nb.1 - raw.0.max(raw.1)).abs() > 0.5 {
                        eprintln!("note: --band limited to {:.0}-{:.0} Hz (the decoded audio is {:.0} Hz)", nb.0, nb.1, a.sample_rate);
                    }
                    Some(nb)
                }
                None => None,
            };
            let mut s = listen::ListenSettings { band, band_gain_db: band_gain, rest_gain_db: rest_gain, ..Default::default() };
            s.gain_db = gain.unwrap_or_else(|| listen::suggest_gain_db(&a, t0, t1, s, -20.0));
            let y = listen::render(&a, t0, t1, s, rate as f64);
            synth::write_wav(&out, &audio::Audio::from_samples(y, rate as f64))?;
            eprintln!("wrote {} ({:.1} s, gain {:+.1} dB)", out.display(), t1 - t0, s.gain_db);
        }
    }
    Ok(())
}

fn load(file: &std::path::Path) -> Result<audio::Audio> {
    let start = std::time::Instant::now();
    let a = audio::load(file, audio::DEFAULT_TARGET_RATE)?;
    eprintln!("decoded {} in {:.1} s ({})", file.display(), start.elapsed().as_secs_f64(), a.decoder);
    if a.decode_errors > 0 || a.gap_fill_s > 0.0 || a.non_finite > 0 {
        eprintln!(
            "warning: {} decode errors, {:.2} s of gaps filled with silence, {} non-finite samples zeroed",
            a.decode_errors, a.gap_fill_s, a.non_finite
        );
    }
    if let Some(m) = a.duration_mismatch_s.filter(|m| m.abs() > 0.1) {
        eprintln!("warning: decoded length differs from the declared length by {m:+.2} s; times after damaged data may be shifted");
    }
    Ok(a)
}
