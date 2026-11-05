//! `overhead-bench`: generate synthetic fly-by scenes and score the detector.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use overhead_bench::chain::Codec;
use overhead_bench::eval::{self, ScenarioResult};
use overhead_bench::rng::Rng;
use overhead_bench::scenario::{self, Difficulty, GENERATOR_VERSION, ScenarioConfig, Truth};
use overhead_core::audio;
use overhead_core::detect::{self, DetectConfig, Detection};

#[derive(Parser)]
#[command(name = "overhead-bench", about = "Synthetic benchmark for the fly-by detector")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate one scenario (`<name>.wav` + `<name>.truth.json`).
    Gen {
        #[arg(long, default_value = "/tmp/overhead-bench")]
        out: PathBuf,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long, value_enum, default_value_t = Difficulty::Medium)]
        difficulty: Difficulty,
        /// Duration override (s).
        #[arg(long)]
        duration: Option<f64>,
        #[arg(long, default_value_t = 8000)]
        rate: u32,
        #[arg(long, value_enum)]
        codec: Option<Codec>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Generate N scenarios, run the detector on each and report.
    Run {
        #[arg(long, default_value = "/tmp/overhead-bench")]
        out: PathBuf,
        #[arg(long, default_value_t = 10)]
        count: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long, value_enum, default_value_t = Difficulty::Medium)]
        difficulty: Difficulty,
        #[arg(long, default_value_t = 8000)]
        rate: u32,
        /// Scenarios processed concurrently.
        #[arg(long, default_value_t = 2)]
        jobs: usize,
        /// Reuse existing recordings whose truth matches the generator version.
        #[arg(long)]
        reuse: bool,
        /// Matching tolerance on heard CPA time (s).
        #[arg(long, default_value_t = 3.0)]
        tol: f64,
    },
    /// Run the detector on the real 1-hour recording and check the two known fly-bys.
    Real {
        #[arg(long, default_value = "/tmp/yt_4CxVxuMVkMc/rec.webm")]
        file: PathBuf,
        #[arg(long, default_value = "/tmp/overhead-bench")]
        out: PathBuf,
    },
}

fn scenario_seed(seed: u64, i: usize, d: Difficulty) -> u64 {
    Rng::new(seed.wrapping_mul(0x100_0000_01B3) ^ ((i as u64) << 8) ^ d as u64).next_u64()
}

fn diff_name(d: Difficulty) -> &'static str {
    match d {
        Difficulty::Easy => "easy",
        Difficulty::Medium => "medium",
        Difficulty::Hard => "hard",
    }
}

/// Run the detector, converting a panic into an error.
fn run_detect(path: &Path) -> Result<(Vec<Detection>, f64)> {
    let audio = audio::load(path, audio::DEFAULT_TARGET_RATE)?;
    let t = Instant::now();
    let dets = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| detect::detect(&audio, &DetectConfig::default())))
        .map_err(|e| {
            let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()));
            anyhow::anyhow!("detect panicked: {}", msg.unwrap_or_default())
        })?;
    Ok((dets, t.elapsed().as_secs_f64()))
}

fn load_truth(path: &Path) -> Option<Truth> {
    let t: Truth = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    (t.version == GENERATOR_VERSION).then_some(t)
}

fn print_truth(t: &Truth) {
    eprintln!(
        "{}: {:.1} min, {} Hz, codec {:?}, AGC {}, clipped {}, {} sources, synth {:.1} s",
        t.name,
        t.duration / 60.0,
        t.sample_rate,
        t.codec,
        t.agc.is_some(),
        t.clipped_samples,
        t.sources.len(),
        t.synth_seconds
    );
    for a in &t.aircraft {
        eprintln!(
            "  #{:<2} {:<10} tc {:7.1} s  f0 {:6.1} Hz  v {:5.1} m/s  d {:6.0} m  alt {:5.0} m  SNR {:5.1} dB{}{}",
            a.id,
            format!("{:?}", a.kind).to_lowercase(),
            a.flyby.tc,
            a.flyby.f0,
            a.flyby.v,
            a.flyby.d,
            a.altitude_agl,
            a.snr_db,
            if a.expected_detectable { "  detectable" } else { "" },
            if a.maneuvers.is_empty() { "" } else if a.maneuver_near_cpa { "  maneuver@CPA" } else { "  maneuvering" }
        );
    }
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Gen { out, seed, difficulty, duration, rate, codec, name } => {
            let name = name.unwrap_or_else(|| format!("{}-{seed}", diff_name(difficulty)));
            let cfg = ScenarioConfig { seed, difficulty, sample_rate: rate, duration, codec };
            let g = scenario::generate(&cfg, &name)?;
            let wav = scenario::write(&out, &g)?;
            print_truth(&g.truth);
            println!("{}", wav.display());
        }
        Cmd::Run { out, count, seed, difficulty, rate, jobs, reuse, tol } => {
            std::fs::create_dir_all(&out)?;
            let results: Mutex<Vec<(usize, ScenarioResult)>> = Mutex::new(Vec::new());
            let next = std::sync::atomic::AtomicUsize::new(0);
            std::thread::scope(|s| {
                for _ in 0..jobs.max(1).min(count.max(1)) {
                    s.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if i >= count {
                                break;
                            }
                            let name = format!("{}-{seed}-{i:03}", diff_name(difficulty));
                            let r = process(&out, &name, scenario_seed(seed, i, difficulty), difficulty, rate, reuse, tol);
                            let r = r.unwrap_or_else(|e| ScenarioResult {
                                name: name.clone(),
                                error: Some(format!("{e:#}")),
                                ..Default::default()
                            });
                            eprintln!(
                                "[{name}] aircraft {} (detectable {}), detections {}, TP {}, FP {}{}",
                                r.n_aircraft,
                                r.n_detectable,
                                r.n_detections,
                                r.matches.len(),
                                r.false_positives.len(),
                                r.error.as_deref().map(|e| format!(", ERROR {e}")).unwrap_or_default()
                            );
                            results.lock().unwrap().push((i, r));
                        }
                    });
                }
            });
            let mut results = results.into_inner().unwrap();
            results.sort_by_key(|r| r.0);
            let results: Vec<ScenarioResult> = results.into_iter().map(|r| r.1).collect();
            let summary = eval::summarize(&results);
            let title = format!("overhead-bench: {} × {}, seed {seed}", count, diff_name(difficulty));
            let md = eval::markdown(&title, &results, &summary);
            let stem = format!("results-{}-{seed}", diff_name(difficulty));
            std::fs::write(out.join(format!("{stem}.md")), &md)?;
            std::fs::write(
                out.join(format!("{stem}.json")),
                serde_json::to_string_pretty(&serde_json::json!({ "summary": summary, "scenarios": results }))?,
            )?;
            println!("{md}");
            eprintln!("wrote {}", out.join(format!("{stem}.{{md,json}}")).display());
        }
        Cmd::Real { file, out } => real(&file, &out)?,
    }
    Ok(())
}

fn process(out: &Path, name: &str, seed: u64, difficulty: Difficulty, rate: u32, reuse: bool, tol: f64) -> Result<ScenarioResult> {
    let wav = out.join(format!("{name}.wav"));
    let truth_path = out.join(format!("{name}.truth.json"));
    let cached = if reuse && wav.exists() { load_truth(&truth_path).filter(|t| t.seed == seed) } else { None };
    let truth = match cached {
        Some(t) => t,
        None => {
            let cfg = ScenarioConfig { seed, difficulty, sample_rate: rate, duration: None, codec: None };
            let g = scenario::generate(&cfg, name)?;
            scenario::write(out, &g)?;
            g.truth
        }
    };
    print_truth(&truth);
    let (dets, secs) = run_detect(&wav).with_context(|| format!("detect on {}", wav.display()))?;
    let mut r = eval::evaluate(&truth, &dets, tol);
    r.detect_seconds = secs;
    Ok(r)
}

/// Reference fits of the two fly-bys in the real recording (heard CPA time, v, d, f0).
const REAL_REFS: [(f64, f64, f64, f64); 2] = [(2155.8, 140.5, 6427.0, 112.8), (3073.2, 94.7, 2338.0, 133.3)];

fn real(file: &Path, out: &Path) -> Result<()> {
    if !file.exists() {
        println!("{} not found; skipping real-recording check", file.display());
        return Ok(());
    }
    std::fs::create_dir_all(out)?;
    let (dets, secs) = run_detect(file)?;
    let mut md = format!("# Real recording: {}\n\ndetect took {secs:.1} s, {} detections\n\n", file.display(), dets.len());
    md += "| reference tc | found | det tc | v (ref) | d (ref) | f0 (ref) | contrast |\n|---|---|---|---|---|---|---|\n";
    let mut used = vec![false; dets.len()];
    let mut found = 0;
    for (tc, v, d, f0) in REAL_REFS {
        let best = dets
            .iter()
            .enumerate()
            .filter(|(_, x)| (x.flyby.tc - tc).abs() <= 3.0)
            .min_by(|a, b| (a.1.flyby.tc - tc).abs().total_cmp(&(b.1.flyby.tc - tc).abs()));
        match best {
            Some((i, x)) => {
                used[i] = true;
                found += 1;
                let p = x.flyby;
                md += &format!(
                    "| {tc:.1} | yes | {:.1} | {:.1} ({v}) | {:.0} ({d}) | {:.1} ({f0}) | {:.1} |\n",
                    p.tc, p.v, p.d, p.f0, x.contrast
                );
            }
            None => {
                // report the nearest detection within 10 s as a near miss
                let near = dets
                    .iter()
                    .enumerate()
                    .filter(|(_, x)| (x.flyby.tc - tc).abs() <= 10.0)
                    .min_by(|a, b| (a.1.flyby.tc - tc).abs().total_cmp(&(b.1.flyby.tc - tc).abs()));
                match near {
                    Some((i, x)) => {
                        used[i] = true;
                        let p = x.flyby;
                        md += &format!(
                            "| {tc:.1} | **no** (nearest Δtc {:+.1} s) | {:.1} | {:.1} ({v}) | {:.0} ({d}) | {:.1} ({f0}) | {:.1} |\n",
                            p.tc - tc, p.tc, p.v, p.d, p.f0, x.contrast
                        );
                    }
                    None => md += &format!("| {tc:.1} | **no** | | ({v}) | ({d}) | ({f0}) | |\n"),
                }
            }
        }
    }
    md += &format!("\n{found}/{} reference fly-bys found.\n\n## Other detections\n\n", REAL_REFS.len());
    md += "| tc (s) | f0 (Hz) | v (m/s) | d (m) | contrast | other lines |\n|---|---|---|---|---|---|\n";
    for (x, _) in dets.iter().zip(&used).filter(|(_, u)| !**u) {
        let p = x.flyby;
        md += &format!(
            "| {:.1} | {:.1} | {:.1} | {:.0} | {:.1} | {:?} |\n",
            p.tc, p.f0, p.v, p.d, x.contrast, x.other_lines.iter().map(|f| (f * 10.0).round() / 10.0).collect::<Vec<_>>()
        );
    }
    std::fs::write(out.join("real.md"), &md)?;
    std::fs::write(out.join("real.json"), serde_json::to_string_pretty(&dets)?)?;
    println!("{md}");
    Ok(())
}
