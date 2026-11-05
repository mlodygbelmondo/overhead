# Contributing to Overhead

Thanks for helping. Bug reports, test recordings and code are all welcome.

## Reporting a problem

Open an issue with:

- what you ran (app or command line, with the options) and what happened;
- the recording, or a link to it, if you can share it;
- the day, time and region you entered;
- where it was really recorded, if you know. Recordings with a known place
  are the most useful thing you can give this project: they are how every
  change is tested.

## Building and testing

You need [Rust](https://rustup.rs) (stable). On Linux the app also needs the
system libraries listed in the [README](README.md#linux-build-from-source).

```sh
cargo build --release              # everything
cargo test --release --workspace   # all tests (some are slow in debug builds)
```

The workspace:

| crate | what it does |
|---|---|
| `overhead-core` | Audio decoding, spectrograms, the Doppler model, finding and measuring fly-bys (stage 1). No UI, no network. |
| `overhead-adsb` | Flight history from adsb.lol, terrain, and matching fly-bys to flights to find the place (stage 2). |
| `overhead-app` | The desktop app (GPUI). |
| `overhead-cli` | The `overhead-cli` command: `detect`, `spec`, `fit`, `trace`, `cuts`, `listen`, … |
| `overhead-bench` | Synthetic recordings with known fly-bys, and an evaluator for stage 1. |

[docs/technical.md](docs/technical.md) explains how each part works.

Data (flight history, terrain, map tiles, caches) goes to `$OVERHEAD_DATA`,
or `~/overhead-data` if it is not set. One day of flight history is about
3.8 GB to download and 0.9 GB to keep.

## Measuring a change

Stage 1 and stage 2 have their own test beds. A change to either should be
compared against `main` before it is merged:

- **Stage 1:** `overhead-bench run --out /tmp/bench --count 10 --seed 1 --difficulty medium --jobs 8`
  renders synthetic recordings (aircraft, cars, birds, wind, mains hum) and
  reports recall, precision and the errors of speed, distance and time.
  Use several seeds and difficulties.
- **Stage 2:** `overhead-adsb e2e --day 2026-08-15 --area 51.6,20.0,53.0,22.2 --cases 5 --seed 5 --time start`
  builds recordings from real flight tracks of that day and area and runs
  the whole search on them (`--time hours` and `--time day` for the harder
  modes).
- **Real recordings:** the recordings of [docs/testset.md](docs/testset.md)
  and [docs/evidence.md](docs/evidence.md), with the scripts in
  `scripts/evidence` and `scripts/longrec`.

The rules every merged change has followed so far:

1. No answer may become **confident and wrong**. A wrong place reported as
   certain is the worst thing this tool can do.
2. No recording that was located (within 2 km) may be lost.
3. Report the numbers before and after, on the same machine and inputs.
4. Tune on synthetic data. A change that only moves one real recording's
   answer is probably fitted to that recording.

## Code

- Keep `cargo test --release --workspace` passing. Follow the style of the
  file you are in (the code is not run through `cargo fmt`; lines go up to
  about 160 characters).
- Explain *why* in comments where the reason is not obvious (a measured
  value, a failure it prevents). Comments and docs use plain words.
- New behaviour comes with a test, ideally on synthetic data with a known
  answer.
- Commit messages say what changed for the user or the result, for example
  "overhead-adsb: a pass outside the recording's parts is no longer counted
  as silent".

By contributing you agree that your contribution is licensed under the
[GPL-3.0-or-later](LICENSE), like the rest of the project.
