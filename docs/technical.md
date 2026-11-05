# Technical reference

Everything about how Overhead works and every option it has: the stage-1
detector, the ADS-B matcher, the app's panels, the command lines and the
validation. For what the project is and how to use it, start with the
[README](../README.md). The method follows
[this video](https://www.youtube.com/watch?v=UytYrhYLOKI).

Stage 1: automatic detection and measurement of every
aircraft fly-by in a recording. Each one gives you:

- `f0`: the rest frequency of the line,
- `v`: speed,
- `d`: slant distance at closest approach,
- `tc`: the recording time at which the sound from closest approach is heard,
- uncertainty ranges for these.

Stage 2 (`overhead-adsb`) matches these against historical ADS-B tracks
(adsb.lol) and returns ranked locations. It needs the recording **day** (or a
2–3 day range). An approximate time window and region are optional but make
the search much faster and much less ambiguous. See [Stage 2](#stage-2-ads-b-matching-overhead-adsb).

## Crates

| crate | what |
|---|---|
| `overhead-core` | Decoding, spectrograms, the retarded-time Doppler model, fitting and detection, cut suggestions (`cuts`), listening with a band boost (`listen`). No UI. |
| `overhead-cli` | Command line: `info`, `spec`, `fit`, `trace`, `detect`, `synth`, `cuts`, `listen`. |
| `overhead-bench` | Realistic synthetic recordings with ground truth, and an evaluator for the detector. |
| `overhead-adsb` | Stage 2: adsb.lol history download/cache, matching fly-bys to flights and ground points, synthetic end-to-end validation. |
| `overhead-app` | GPUI desktop app: spectrogram, fly-by table, ADS-B location search with an OpenStreetMap map (see [App](#app-overhead)). |

## Usage

```sh
cargo build --release

# every fly-by in a file (wav/flac/mp3/ogg/m4a natively, anything else via ffmpeg)
./target/release/overhead-cli detect recording.webm > flybys.json

# look at a window, optionally with model curves "f0,v,d,tc" overlaid
./target/release/overhead-cli spec recording.webm --t0 2050 --t1 2280 --fmin 60 --fmax 220 \
    --whiten --curve 113.07,139.1,6229,2155.41 -o window.png

# benchmark the detector
./target/release/overhead-bench run --out /tmp/overhead-bench --count 10 --seed 1 --difficulty medium --jobs 8
```

Each detection carries flags:

- `confident`: false when a parameter range ran off the edge of its scan,
- `likely_ground_vehicle`: possibly a car, a machine or a bird, or just an
  uncertain fit; not proof of a ground source. Set when the pass is slow and
  close (under 36 m/s within 450 m, or under 45 m/s within 200 m: slow
  aircraft such as helicopters pass a few hundred metres away, road traffic
  rarely exceeds 130 km/h), or measured so loosely (v range over 2×,
  or d range over 4× and wider than 700 m) that slow and close cannot be
  ruled out (the ranges' low ends under 45 m/s within 450 m). Distance
  alone never sets it: aircraft near airfields pass closer than 200 m.

Both are kept rather than dropped, so that ADS-B matching can decide.

`harmonics` lists every line heard on the fly-by as a multiple of `f0`. After
a pass is fitted on its strongest line, stage 1 scans the spectrogram for
every other line that follows the same Doppler shape (the "comb",
`overhead_core::comb`), rejects level tones the curve merely crosses (mains
hum), and picks the fundamental that explains the most lines. That
fundamental may be one that is not heard at all, or lie below the strongest
line: a propeller at 87.5 Hz heard only at its 3rd, 4th, 7th and 10th
harmonic comes out as `f0` 87.4, `harmonics` [3, 4, 7, 10], not as a single
611.7 Hz line. Lines that are no multiple are kept as measured ratios
(twin engines, turbine plus propeller). The pass is then refitted on all
lines together, and the refit is kept only when it is brighter and
consistent with the single-line fit. ADS-B matching scores every listed line,
so more lines make a correct match stand out more. Measured effect: see
[docs/evidence.md](evidence.md#harmonic-comb-2026-11-05).

Analysis assumes still air at about 20 °C (c = 343 m/s).

## App (`overhead`)

```sh
cargo build --release -p overhead-app
./target/release/overhead recording.wav --day 2026-08-15 --start 16:48 --city Warszawa --locate
```

Open a recording (Ctrl+O or drop it on the window); fly-bys are detected
automatically. Fill in the day (required; a 2–3 day range if unsure) and, if
you can, the exact start or approximate hours, the UTC offset or zone
(`+02:00`, `Europe/Warsaw`; empty = the region's zone on that day from the time
zone database) and the region (city, voivodeship/state, country, joined into
one place-name query; `lat,lon[,radius_km]` in the city field also works).
"Find location" (Ctrl+Enter) runs overhead-adsb's `locate` in the background
with these inputs, the same search as `overhead-adsb locate`:

- Progress is shown in the location panel; the first search on a new day
  downloads and indexes its adsb.lol archive (≈3.8 GB read, ≈0.9 GB kept,
  a minute or more), with a progress bar from the size of the kept data.
  Cancel (or opening another file) stops a search at once before the
  matcher; a search already matching finishes in the background and its
  result is dropped (the matcher has no cancellation hook).
- The result is one sentence above the map ("Recorded here, give or take
  139 m: 5 of the 7 fly-bys fit aircraft that passed here."; when unsure it
  gives the chance the shaded area holds, e.g. "Not found for sure: the
  shaded spots fit best, but together they have only a 4 % chance…"; with
  another place clicked it describes that place), with the coordinates, **Copy**, and
  **Google Maps**, **Street View** and **OpenStreetMap** buttons. On the map
  one pin marks the best place, with the circle it is likely within; an
  ambiguous answer also shades the possible area. Other places are shown
  only as "Place N · chance" behind the **Other possible places (N)** switch
  (on by itself for an ambiguous answer with at most three of them), and
  "Back to the best place" returns from one you clicked. The aircraft heard
  are drawn with their tracks and a dot labelled "CALLSIGN · heard m:ss"
  where each was when it was heard, and a plane at the timeline cursor.
  A **Legend** (collapsible; folded by itself on a small map) names every
  mark. **Zoom to result**, **Possible area** and **Search area** reframe the
  map, + and − zoom it, scroll zooms, drag pans. Before a search the map
  frames the region typed in the form. Tiles are OpenStreetMap's, drawn in
  grey so their own colours do not read as marks (cached in
  `~/overhead-data/tiles/osm`, at most two connections, attribution shown;
  `--no-basemap` for a plain grid).
- The fly-by table's ADS-B column and the fly-by detail show the flight
  matched to each detection at the selected candidate (callsign or
  registration, type, class, ICAO hex, modelled heard CPA and its offset
  from the detection, slant range, ground speed, height, curve score).
- Below the map: the probability of the selected place, the margin to the
  best place more than 2 km away (below 1 = ambiguous), uncertainty, fitted
  start time (UTC), speed of sound, terrain height, silence violations, the
  UTC offsets used and the matcher's warnings. Each candidate in the list
  shows its place's probability (`≈1` = within 2 km of candidate 1, the
  same place).
- When the answer is ambiguous (margin of #1 below 1), #1 is not presented
  as the answer: the sentence says so, the pin is "Best guess · chance",
  the map frames and shades the possible area (grid cells, amber, opacity
  by probability; in Details labelled with their probability, and the card
  adds the probability of the best place, the best 10 and "somewhere
  else"); and a short list says what would narrow it down (exact start,
  smaller region, a longer recording with more aircraft). See
  [How sure is the answer](#how-sure-is-the-answer).
- **Analysis** (the button on the result, Ctrl+Shift+A) opens a drawer under
  the map or spectrogram with the views of the reference video, computed in
  the background from the search's session:
  - *Places*: every distinct place with its chance, fly-bys fitted, score
    and silent aircraft. A is the selected place, pick B to compare: the
    curves their aircraft would make are drawn dashed on the spectrogram
    (A green, B pink, labelled with the aircraft and its curve score; the
    spectrogram opens by itself for it), with a fly-by by fly-by table.
    Right-click the map to test any other place with A's aircraft.
  - Closing the analysis (×, "Close analysis" or its "Fly-bys" tab) takes
    its marks off the map (heat map, compared and tested place) and hides
    the spectrogram again if only the comparison opened it.
  - *Heat maps*: the score of every place drawn on the map, for all fly-bys
    together or one fly-by alone, over the whole search (the matcher's
    cells) or around a place (100 m grid).
  - *Scores*: the scatter of one fly-by's score against another's (or the
    total) for every place the search refined, coloured by the pair of
    flights given to the two fly-bys, the best of each pair ringed.
  - *Silence*: the aircraft that passed close to the place while recording
    and whether each is heard in the recording.
  - *Timing*: detected against modelled times of every fly-by, the offset
    and the gaps between fly-bys, per part when there are cuts.
- **Listening**: the bar under the map or spectrogram is the recording's
  timeline (fly-bys, cuts, the spectrogram's window, the position). Click
  it to move the cursor; the play button (Space on the spectrogram) plays
  from the cursor, which then moves on the timeline, the spectrogram and the
  map. **Boost aircraft sound** raises a band and lowers the rest; its
  settings (the arrow) take the band from the selected fly-by or as typed,
  and the boost and volume in 3 dB steps, live while playing.
- **Cuts** (the scissors): the audio's suggested cuts are dashed on the
  timeline and listed; click one to take it, double-click the timeline (or
  "Cut at the cursor") to add one, drag a cut to move it, right-click or ×
  to remove it. Each part after a cut gets its own start time; "Search
  again" appears when the cuts changed. Cuts are drawn on the spectrogram,
  undone with the edits (Undo in the Cuts panel or Ctrl+Z, also outside
  Edit fly-bys; a drag is one step), and saved with the fly-bys (Edit fly-bys → Save…
  writes `{"detections": [...], "cuts": [...]}`, which `overhead-adsb
  locate --detections` also reads; Load… takes both forms). `--cut T`
  starts with a cut.

`--sample-data` replaces the search with invented, clearly labelled results
(debug only). `--analysis TAB`, `--compare N`, `--others` and `--cut T` open
those views from the command line. `overhead --help` lists all options.

### Window and layout

- The window moves, resizes and maximizes like any other: drag the title
  bar (Aero snap and Snap Layouts on Windows), double-click it to maximize
  or restore, resize from any edge or corner; minimize, maximize/restore and
  close are at the right end of the title bar. On Windows these are the
  system's own (the title bar and buttons are reported to Windows by hit
  testing) and drawn with Windows' own glyphs (Segoe Fluent Icons, or
  Segoe MDL2 Assets on Windows 10), on Linux the app asks the compositor.
- **Recent recordings**: the start screen lists the last recordings opened
  (also under **Open** in the title bar). A click opens one again with the
  day, time and region typed for it. Only that is kept, with its length,
  the number of fly-bys and the last answer, in `history.json` in the
  settings folder (at most 12; no audio or ADS-B). A file that was moved
  or deleted shows as not found; the × on a row forgets it.
- Every divider between panels can be dragged: the input form | the middle
  | the right column, the spectrogram (or map) | the fly-by table, and the
  map | what is below it in the right column. Double-click a divider for its
  default size. Dragging the form's divider far to the left folds the form
  to a narrow rail (also the panel button at the left of the title bar,
  Ctrl+B); the rail's search button runs Find location.
- **Spectrogram** in the title bar (Ctrl+Shift+S) shows or hides the
  spectrogram. Hidden (the default) the map is the hero: it takes the
  middle, with the verdict above it, and the fly-by table moves to the
  right column next to the selected fly-by and the possible places (in
  Details the wide table stays under the map). The spectrogram comes back
  by itself in Edit fly-bys and when no fly-by was found (tracing by hand
  happens there), without changing the saved choice.
- Panel sizes, folded panels, the spectrogram choice and the window place,
  size and maximized state are kept between runs in `layout.json` in the
  settings folder (`%APPDATA%\Overhead`, `~/.config/overhead`; an existing
  folder of the old name, `%APPDATA%\Doppler` or `~/.config/doppler`, is
  renamed to it on the first start). A saved place
  that is no longer on any screen (an unplugged monitor) is moved back
  onto the first one. `--default-layout` starts with the defaults,
  `--size WxH` with that window size, `--spectrogram` / `--no-spectrogram`
  overrides the choice for the run.

The window behaviour is checked on a real Windows desktop by the
`ui-smoke` job of [`.github/workflows/windows.yml`](../.github/workflows/windows.yml)
(runs when the workflow is started by hand: `gh workflow run windows.yml
--ref <branch>`): it starts the exe built in the same run with a generated
one-minute recording and, with real mouse input
([`scripts/windows-ui-smoke/smoke.py`](../scripts/windows-ui-smoke/smoke.py)),
drags and double-clicks the title bar, resizes from the edges and a corner,
uses the three buttons, drags a divider, snaps the window to the left half,
closes it and starts it again; window rectangles (`results.json`) and
screenshots are the `windows-ui-smoke` artifact.

### Updates

Every push to main is built on Windows by
[`.github/workflows/windows.yml`](../.github/workflows/windows.yml) (static CRT)
with its build number (the workflow run number) and short commit baked in,
and published as a prerelease `build-<N>` with `overhead.exe` and
`latest.json` (`{build, commit, date}`); only the last 10 `build-*` releases
are kept (other releases such as `preview-1` are left alone). The status bar
shows the build (`build 14 (ab12cd3)`, or `dev build` for local builds);
`overhead --version` prints it.

The Windows app updates itself (`crates/overhead-app/src/update.rs`) from
the public repository's releases, with no token, account or setup. Each
start checks the latest releases in the background (an unauthenticated
`GET https://api.github.com/repos/mlodygbelmondo/overhead/releases`); a
newer build's exe is downloaded (the asset redirect is followed by hand,
API headers go to `api.github.com` only), checked for size and the `MZ`
header and saved next to the running exe as `overhead.new.exe`. The status
bar then shows "Update ready (build N) — Restart": the running exe is
renamed to `overhead.old.exe`, the new one moved into place and started
with the same arguments; the new version deletes the old one once it has
run for 20 s (until then a build that dies at start can be undone by
renaming `overhead.old.exe` back). If anything fails the current exe stays
(or is put back) and the status bar says why in a few words ("Retry" next
to it); this includes GitHub's rate limit for unauthenticated requests (60
an hour per address), which a few starts an hour never reach. "Check for
updates" checks again by hand. Dev builds never check; on Linux and macOS a
CI build only reports a newer build when `OVERHEAD_UPDATE_CHECK=1`.

The workflow needs nothing but the `github.token` of its run (`contents:
write` to publish the release).

## Stage 2: ADS-B matching (`overhead-adsb`)

```sh
cargo build --release -p overhead-adsb

# fully automatic: detect fly-bys, fetch the day(s), search, rank
./target/release/overhead-adsb locate recording.wav --day 2026-08-15 --region "Mazowieckie" -o result.json

# more inputs = faster and less ambiguous
./target/release/overhead-adsb locate rec.wav --day 2026-08-15 --hours 13-17 --tz +02:00 --region "Warszawa"
./target/release/overhead-adsb locate rec.wav --day 2026-08-15 --start "2026-08-15T14:48:00+02:00" --start-tol 120 \
    --region 52.3,21.0,30          # lat,lon,radius_km (or lat_min,lon_min,lat_max,lon_max, or a place name)
./target/release/overhead-adsb locate --detections flybys.json --duration 2400 --day 2026-08-15 --region Poland

# data management
./target/release/overhead-adsb release 2026-08-15          # where the archive is, how big
./target/release/overhead-adsb fetch 2026-08-14 2026-08-15 # download + index (≈1 min per day here)
./target/release/overhead-adsb tracks --day 2026-08-15 --region Warszawa --utc-hours 12-14

# synthetic end-to-end validation on a real ADS-B day (see below)
./target/release/overhead-adsb e2e --day 2026-08-15 --area 51.6,20.0,53.0,22.2 --cases 6 --seed 1 --time start
```

Inputs: `--day` is required (`--day-end` for a 2–3 day range, at most 2 days
after `--day`). Optional:

- `--start` (with `--start-tol`, default ±120 s): RFC 3339 with an offset
  (absolute), or local `HH:MM[:SS]` / `YYYY-MM-DD HH:MM`.
- `--hours` (local, e.g. `13-17`; ±30 min slack; applied on every day of a
  range).
- `--tz`: an offset (`+02:00`) or an IANA zone (`Europe/Warsaw`). Without it
  the zone is looked up for the region in the time zone database (`tzf-rs`
  boundaries + `chrono-tz`, DST included, every zone the region touches); a
  local `--start` then gets one start window per possible offset. Without a
  region every offset in use worldwide is tried (one 4-minute window each for
  a local `--start`, the hull for a day).
- `--region`: a place name (OpenStreetMap Nominatim, cached in
  `geocode-v2.json`), `lat,lon[,radius_km]` or `lat_min,lon_min,lat_max,lon_max`.
  A place name stands for its main part only: the piece of its outline that
  holds the place's point, plus every piece within 300 km of a piece kept
  (coastal islands, Corsica, the Balearics, Shetland). Overseas parts are
  left out, so `Netherlands` is the European Netherlands (not a box reaching
  Bonaire), `France` is without French Guiana, `Norway` is without Svalbard.

Without a region every candidate flight of the day worldwide is searched. That
works but is slow; over large areas (more than 40 one-degree tiles with
traffic) the coarse pass search uses coarse terrain (Terrarium zoom-7 tiles,
≈1 km pixels, a few hundred MB for a continent) and only the candidates found
are scored on SRTM, so no-region searches no longer download SRTM for the
whole world. At most 64 detections are matched (the confident ones and then
the highest-contrast ones; a warning says so).

Output (stderr summary, JSON on stdout or `-o`): ranked candidates with
latitude/longitude, terrain height, an uncertainty ellipse (RMS radius, major
and minor axes, azimuth) from the score surface around the optimum, the
recording start time (UTC), the fitted ground speed of sound, and for every
matched detection the flight (ICAO hex, registration, type, class, callsign),
its closest approach and heard time (UTC and recording time), slant range,
speed, height above ground and curve score of the pass that was scored (the
heard closest approach nearest the detection's `tc`); plus "silence
violations" (prop aircraft that should have been heard at that point but no
detection explains), the UTC offsets used and where they came from, the
start windows searched, and warnings (dropped detections, terrain tiles
that failed to download).

### Historical data: adsb.lol `globe_history`

- One GitHub release per UTC day and readsb instance in
  `adsblol/globe_history_<year>` (repos for 2023, 2024, 2025 (archived) and
  2026), tag `v<YYYY>.<MM>.<DD>-planes-readsb-prod-0` (2023 also has
  `prod-1`; `staging-*` and `mlatonly-*` releases are other pods and are not
  used). Coverage starts 2023-03-10 (test pods from 2023-02-16); a day appears
  about a day after it ends. Global, wherever adsb.lol has feeders (dense in
  Europe and the US). Licence: ODbL.
- Each release is one plain tar split into 2 GB parts (`.tar.aa`, `.tar.ab`):
  3.1–3.8 GB per day in 2025–2026. Content: `heatmap/*.bin.ttf` (48 half-hour
  position snapshots, ≈0.76 GB), small `acas/` files, and
  `traces/<last two hex digits>/trace_full_<icao hex>.json`, one gzipped
  full-day readsb trace per aircraft (≈75 000 per day). Trace points are
  `[dt, lat, lon, alt_baro_ft|"ground", gs_kt, track, flags, baro_rate,
  details, source, alt_geom_ft, geom_rate, ias, roll]`.
- There is no official per-region or per-aircraft historical API
  (`api.adsb.lol` is live only; the web `globe_history` path answers 403),
  and the tar has no index and is ordered by hex, not by place or time. So a
  day is read once: `fetch` skips the heatmaps with a few range requests, then
  streams the trace section with 8 parallel HTTP range requests (each worker
  syncs to the next tar header in its slice), reads each trace's type code
  from its first few hundred decompressed bytes, indexes jets only, and keeps
  propeller, rotor and unknown-type aircraft verbatim with their airborne
  bounding box and time span. Measured for 2026-08-15: 3.76 GB read in 58 s,
  75 674 aircraft, 43 748 kept (0.87 GB). Every later query for any region of
  that day is local. Cache: `$OVERHEAD_DATA/adsb/<day>/` (default
  `~/overhead-data/adsb`), terrain tiles in `~/overhead-data/dem`. A day is
  only cached when every slice of the stream was read to its end and the
  last one reached the tar's end-of-archive block; a truncated download or a
  release whose parts are still uploading is an error, not a silently
  incomplete cache.
- Aircraft class comes from the ICAO type designator (ICAO Doc 8643
  description such as `L2P`, table from `wiedehopf/tar1090-db`, embedded in
  `crates/overhead-adsb/data/aircraft_types.tsv`), falling back to the ADS-B
  emitter category (`A7` = rotorcraft). Like the reference method, only known
  jets are removed; unknown types stay candidates.
- Terrain: SRTM 1" tiles from the AWS Terrain Tiles bucket (`skadi`), reduced
  to 3" and cached (missing tiles = sea = 0 m). SRTM heights are above the
  EGM96 geoid while ADS-B geometric altitude is above the WGS84 ellipsoid, so
  the geoid undulation is added (EGM96 on a 1° grid embedded in the crate,
  0.5 m RMS from the full model; +31 m at Warsaw, +43 m in the Netherlands,
  −30 m at Phoenix). A tile that fails to download is reported in the
  output's warnings instead of silently becoming 0 m.

### Method

Follows the video, with these changes:

1. **Flights as smooth trajectories.** Each track becomes ECEF cubic Hermite
   segments using the reported positions *and* velocities (ground speed,
   track, vertical rate), split at gaps over 120 s, ground contact and new
   legs. Position jumps, altitude spikes and impossible velocities are
   cleaned per aircraft class. Geometric (GNSS) altitude is used where
   present, otherwise barometric altitude shifted by the aircraft's median
   geometric−baro offset; aircraft that never report a geometric altitude
   (about half of the light aircraft) get the regional median offset of the
   other aircraft below 3000 m in the search box and time (QNH, temperature
   and geoid together), else the geoid undulation. Multilateration (MLAT)
   reports scatter by ~100–150 m from report to report and carry no usable
   velocity, which made the modelled speed of an MLAT-tracked aircraft swing
   between ~30 and ~80 m/s (the Kampina C172 of docs/evidence.md flew a
   steady 52). Every MLAT knot is therefore replaced by a local quadratic
   least-squares fit of the positions around it (tricube weights, GNSS knots
   4×). A quadratic's slope is biased low in a turn (≈ ω²W²/10 over ±W, 20 %
   for a ±40 s window in a standard-rate turn, while its residual still looks
   fine), so the fit is corrected for the constant-rate level turn that
   explains its own acceleration: the fit's weights applied to that circle
   give the bias, which is removed (exact for a steady turn, nothing in
   straight flight). The window is the widest of ±90/60/40/25 s whose RMS
   residual stays within 150 m (the MLAT scatter) and whose turn correction
   is at most 15 %; with none (turns tighter than ~5°/s, segment ends) the
   knot stays as reported. On a noise-free circular MLAT track at 52 m/s the
   modelled speed stays within 52.0–52.1 m/s up to 3°/s (raw knots:
   50.5–52.8); with 100 m scatter 51.4–52.5 straight and 44.4–57.2 at 3°/s
   (raw: 34.7–69.3 and 36.3–70.4).
2. **Passes.** For every candidate flight and every 200 m ground cell near its
   path: every closest approach (circuits around an airfield give several),
   with slant range to the cell (terrain + 1.5 m microphone), speed and
   emission time; heard CPA = emission + range / c. Cells are enumerated per
   3 s track step only at lateral offsets where some detection's slant range
   (or the silence distance) is possible, so the whole corridor is never laid.
3. **Per-detection filter.** Speed and range must fit the detection's ranges,
   widened for the speed of sound (±7 %), wind (±12 m/s, see the
   limitations) and stage-1 distance errors (×0.6–1.6). This is only the
   outer gate of the search; how well the speed and distance fit is weighed
   softly (step 7).
4. **Combination by time offsets.** The detections' `tc` differences are
   exact, the absolute start `S` is not. At each cell every compatible pass
   implies `S = heard − tc`; the `S` most detections agree on (each detection
   its own pass, within its `tc` uncertainty + 4 s) gives the hypothesis.
   Unmatched detections (false positives, distant aircraft) are not penalised.
5. **Silence.** A propeller/rotor aircraft that passed the place while the
   recording ran, with no detection within 30 s, costs the place by how
   likely stage 1 was to detect it (`overhead_adsb::silence`): `p_detect`
   from the slant range at closest approach, the aircraft class and the
   recording's background then, and the cost is
   `min(0.2, 0.15 · −ln(1 − p_detect))` (0.15 = half the ambiguity
   temperature, which reads score differences as log-likelihoods; the cap
   keeps one unheard flight below a fifth of a matched detection, because
   the model rests on 22 real passes and stage 1 also misses close passes
   for reasons it does not model). For a piston single
   `p_detect` = 0.95 / (1 + (d / 4 km)^4): unheard within ≈ 2.9 km it costs
   0.2, at 3 km 0.19, at 4 km 0.10, at 5 km 0.05, beyond ≈ 5.7 km nothing
   (twins and turboprops carry 1.4× as far; helicopters `p_max` 0.6 at half the range, since stage 1 does not search
   the main rotor's band; unknown types, often microlights or gliders,
   `p_max` 0.3). A background more than 3 dB above the recording's median
   floor in 60–1000 Hz (measured 1–10 min around the pass, so the aircraft
   itself does not count) shortens the range by 20·log10. A detection heard
   30–90 s from the pass masks it in part (`p_detect` falls linearly to 0
   at 30 s), a pass under 30 m above the ground or slower than 20 m/s
   (taxiing, landing roll) costs nothing, and a flight the hypothesis
   matches is not held against it for its other passes (circuits). Each
   flight costs its most expensive pass. The coarse search counts passes
   within 1 km, the ranking every pass that costs at least 0.03. The
   silence ranks but never raises a margin: the margin is the smaller of
   the one with and the one without it (as for the soft evidence).
   Calibrated on the real recordings only (see below); `--quiet-d D` gives
   the old flat term (0.75 per prop aircraft unheard within `D`).

   *Calibration.* Every pass of a candidate aircraft within 6 km of the true
   place of the 7 recordings with an ADS-B ground truth (Oosterschelde,
   Amer, Vlissingen, both Kampina clips, Berlin, Moerdijk) at the exact
   start (`crates/overhead-adsb/examples/silence_cal.rs`, table and fit
   `scripts/silence/calibrate.py`; passes of one aircraft less than 120 s
   apart taken as one), against "stage 1 has a detection within 30 s":

   | slant | piston single | twin | turboprop | helicopter | unknown type |
   |---|---|---|---|---|---|
   | 0.5–1 km | 3/3 | – | – | 1/1 | – |
   | 1–1.5 km | 2/2 | – | – | 0/1 | – |
   | 1.5–2 km | – | 1/1 | 1/1 | – | 0/1 |
   | 2–3 km | 3/3 | – | – | – | – |
   | 3–4 km | 1/2 | – | – | – | – |
   | 4–5 km | 1/1 | 1/1 | – | – | – |
   | 5–6 km | 0/3 | – | 0/1 | 1/1 (a chance detection) | – |

   Engine aircraft: a logistic in log distance with `p_max` 0.95 fits best
   at `d50` ≈ 4.0 km for any slope from 3 to 10 (log-likelihood −5.5 to
   −4.2); slope 4 was taken (a ±6 dB spread of source level and
   propagation). The background did not predict detection on these
   recordings (every pass between −6.7 and +2.5 dB of its recording's
   median floor;
   with a 20·log10 range scaling the fit got worse, −7.5 against −5.0), so
   only a clearly louder background counts. Before, the silence distance
   was half the median stage-1 distance (700–2000 m), stage-1 distances run
   short, and the term fired at 1 of the 490 reported candidates of the
   real runs.
6. **Curve sieve.** Hypotheses are grouped by flight assignment. The best
   2500 groups by coarse score get a cheap curve score (step 7 at stride 4)
   at their best cell, with the start that their passes imply there; the
   best 300 of those get a small start/`c0` scan and the best 80 the full
   refinement. (Before, only the best 300 groups by coarse score reached any
   curve check, and the true assignment was often ranked far below that.)
7. **Full Doppler-curve scoring** of the best hypotheses (by flight
   assignment): for each matched detection the heard curve of the *real*
   flight geometry is computed with retarded time and a speed of sound that
   falls with height (≈2 m/s per km of source height for the ray, 3.9 m/s
   per km at the source), laid on the whitened spectrogram around the
   detection with all its harmonics, sweeping `f0` ±1.2 %. The score is
   relative to the stage-1 straight-line fit on the same map (1.0 = as good;
   real geometry often does better). `S` is shared, so every detection after
   the first is locked to the flight geometry ("combined locked scoring").
   Observer east/north, `S` and the ground speed of sound `c0` (prior
   340 ± 9 m/s, i.e. −15…+35 °C) are optimised with Nelder–Mead (with a
   soft wall at the edge of the search region). A match
   counts only above 0.45 (chance alignments reach 0.1–0.5) and is capped at
   3.0 (a detection whose stage-1 fit was poor has a tiny reference, so a
   ratio of 10+ on one bright line must not outweigh several good matches),
   and only fully
   if the flight's modelled heard CPA falls within the detection's `tc`
   uncertainty + 10 s (beyond that it fades out with a 5 s Gaussian), so
   one stretch of a curve cannot be stretched over several detections.
   The track is only scored where it is known: from the first to the last
   ADS-B report of its segment plus 10 s (about one report interval). A
   heard closest approach beyond that does not count for the timing lock,
   and Doppler samples beyond it count at the noise floor, so a flight known
   over only part of the detection's window cannot score as if that part
   were all of it.
   **Soft speed/distance evidence.** A match's share is multiplied by how
   well the speed `v` and slant range `d` of the scored pass fit stage 1's
   (`vd_evidence` in the JSON, shown in the summary when below 1; 0.3–1).
   Stage 1 on a single line pins little more than `v²/d` (the curve's
   central slope), and its `d` is often far too short, so inside the ranges
   and anywhere along the `v²/d` ridge nothing is taken off, nor for a pass
   faster than the speed range. What costs is what is rare for true flights:
   a pass slower than the estimate by more than 1.5 half-widths of the speed
   range below it (never narrower than 10 %: speed of sound, wind), or far
   off the ridge: its log ratio below −2.5 for a detection with closed
   ranges, below −1.5 for an unconfident one, above +0.7 for either
   (Gaussian beyond those limits, 0.5 half-widths and 0.75 in log `v²/d`;
   floor 0.3). Calibrated as a likelihood ratio on the synthetic end-to-end
   detections: every true detection's stage-1 estimate against its true
   pass (157), against the flights matched at the wrong places among the
   reported candidates (544), by the logistic loss of "true" given the
   evidence (1.386 without it, 1.327 with it; the first shape, the ridge
   from −1.5 for every detection and a floor of 0.1, reached 1.346 and was
   worse than nothing on confident detections, whose true passes lie 2.5
   below the ridge as often as wrong ones).
   The evidence weighs an assignment: which pass a detection takes in a
   cell, the re-assignment (step 8) and the final ranking. It does not
   decide which hypotheses are refined (sieve, quick check and wide-scan
   picks rank by the curve score alone), it does not move the position
   (refinement, wide scan and uncertainty use the curve score alone: pulling
   the point towards stage 1's often too short distance moved three right
   answers 120–180 m further off), and it never raises a margin (step 11).
8. **Re-assignment.** At the refined point, every detection is re-matched to
   whichever flight pass heard within its time tolerance gives the best curve
   times its speed/distance evidence (no speed/distance filter, so a badly
   measured stage-1 `v`/`d` cannot hide the right aircraft), then refined
   again. The new assignment replaces the old one when it is better both
   with and without the evidence; when the two disagree, both are kept.
9. **Wide scan.** The refinement only moves a few hundred metres, but the
   cell a group was seeded from can be kilometres from where its curves fit
   best. The 16 best distinct assignments are therefore also scanned over
   ±5 km in 250 m steps (start implied at each point by the assigned passes),
   and the two best points found are refined like the others.
10. **Repeated passes of one flight** (`overhead_adsb::repeat`). When the
   start is free, one aircraft flying circuits or out and back can explain
   several detections at a wrong place: once one of its passes fits, a
   later pass with the same geometry fits a second detection at little
   extra chance (a circuit repeats it every lap; along a line flown out and
   back some point has any time difference between the two passes). So a
   flight's matches are taken strongest first, and each further one counts
   `1 − 0.6 · dependence` of its share. The dependence on the stronger
   match is the product of four similarities at the closest approaches:
   track axis `cos²` of the angle between the tracks (parallel and reversed
   alike: the Doppler curve does not tell the direction), side of the
   observer `max(0, cos)²` of the angle between the directions to the
   aircraft, distance (Gaussian in log slant ratio, σ 0.3) and time
   `1 / (1 + (Δt / 30 min)⁴)`. Crossing tracks, the other side of the
   observer, clearly different distances or passes an hour apart count
   fully. The discount is in the ranking and in both margin scores, and it
   never raises a margin (the margin is also measured without it, the
   smaller is kept). Reported per matched flight as `repeat` (the detection
   it repeats, the dependence and the factor) and as "repeats det N (×f)"
   in the summary. On River Amer with hours or the day, the 12 km wrong #1
   matched PH-PXA (EC135) twice, out and back past it 14 min apart on the
   same side at 549 and 710 m: dependence 0.54, its margin 0.51 → 0.22 (the
   nearest the test set came to a confident wrong answer). Vlissingen's
   PH-BAR is matched at 872 and 2565 m (independent, 0.03 unchanged);
   Oosterschelde's 5 distinct aircraft keep 2.66. On the synthetic cases no
   error moved by more than 10 m; true circuits at the true place cost some
   margin (Prague case 01 7.39 → 4.66, Warsaw case 02 2.43 → 1.85), no
   confident answer was lost. The weight 0.6 is not fitted: a repeat with
   the same geometry counts 40 %.
11. Rank by curve score (with the soft evidence) minus silence penalties and the repeat discount, drop near-duplicates
   (500 m), estimate the uncertainty from the score surface (±600 m grid)
   plus a floor, and report each matched flight with the pass that was
   actually scored (the heard closest approach nearest the detection's
   `tc`; before, a ±30 s window could report a neighbouring, closer pass of
   circling traffic). The margin is measured against every refined
   hypothesis more than 2 km away, not only the reported top 10, both with
   the soft evidence and without it, and the smaller of the two is
   reported: the evidence ranks, but it was calibrated as a match
   likelihood, not for confidence, and discounting a wrong place's
   competitor must not make that wrong place look sure (Moerdijk harbour,
   stated start: with the evidence alone the 8.4 km wrong #1 went from
   margin 0.76 to 1.04).

Without audio (`--detections` only), each detection's own fitted curve stands
in for the spectrogram (a Gaussian kernel in log-frequency, σ = 1 % (it was 0.4 %, which matched almost nothing)).

### Validation (synthetic end-to-end on real ADS-B)

`overhead-adsb e2e` takes a real adsb.lol day, picks a real ground point under
real propeller/rotor traffic (a real pass at 350–3000 m during the recording
and at least two passes within 3 km), renders 40 minutes of what a microphone
there would hear from **every** real candidate flight within 15 km
(overhead-bench's retarded-time propagation, spherical spreading, air
absorption, ground reflection, excess and grazing attenuation, tone families
of the aircraft class, ambient noise 14–22 dB/Hz at 100 Hz), runs the stage-1
detector on it and then the full stage-2 search with only the inputs a user
would have. The rendered truth deliberately differs from what the matcher
sees: Catmull–Rom tangents instead of reported velocities, a per-flight
altitude bias (σ 20 m), slow horizontal wander (σ 15 m), timestamp jitter
(±0.5 s) and a random air temperature 0–30 °C (c = 331–349 m/s) while stage 1
assumes 343 m/s. Not modelled: wind, temperature gradients, terrain shielding.

Each case states what the user would know: `start` = start time ±2 min,
`hours` = the local day and a 4-hour window, `day` = only the local day
(searches 2026-08-14 21:30Z to 2026-08-16 00:30Z, three archives). The region is
always the whole test area (Warsaw 51.6–53.0 N, 20.0–22.2 E, about
150 × 155 km; Rhein-Main 49.6–50.4 N, 8.0–9.4 E; Phoenix 33.4–33.9 N,
112.6–111.6 W). Real ADS-B day 2026-08-15. Recordings are 40 min long.
"margin" is the score gap from #1 to the best candidate more than 2 km away.
"flights ok" counts correctly identified flights among the reported matches
that correspond to a true pass. Search times were measured while other
heavy jobs were running on the machine (load average 50–100), so they are
pessimistic.

Results (all from the binary at the last commit; JSON for every case under
`~/overhead-data/e2e/final/<run>/`):

| run | case | passes < 5 km | detections (true) | error | truth rank | margin | flights ok | search |
|---|---|---|---|---|---|---|---|---|
| Warsaw, start | 00 | 3 | 2 (2) | 284 m | 1 | 0.13 | 2/2 | 63 s |
| | 01 | 8 | 10 (8) | 47 m | 1 | 6.79 | 6/6 | 248 s |
| | 02 | 7 | 7 (4) | 84 m | 1 | 5.97 | 3/4 | 83 s |
| | 03 | 10 | 10 (5) | 108 m | 1 | 2.58 | 4/4 | 129 s |
| | 04 | 4 | 7 (3) | 39 m | 1 | 5.41 | 1/4 | 83 s |
| | 05 | 7 | 7 (5) | 74 m | 1 | 4.01 | 3/3 | 28 s |
| Warsaw, hours | 00 | 3 | 2 (2) | **47.4 km** | 5 | 0.21 | 0/1 | 53 s |
| | 01 | | | 48 m | 1 | 5.20 | 6/6 | 112 s |
| | 02 | | | 84 m | 1 | 5.66 | 3/4 | 35 s |
| | 03 | | | 64 m | 1 | 2.78 | 4/4 | 61 s |
| | 04 | | | **13.6 km** | – | 0.31 | 0/3 | 95 s |
| | 05 | | | 74 m | 1 | 2.63 | 3/3 | 70 s |
| Warsaw, day | 00 | | | **61.4 km** | 2 | 0.00 | 0/2 | 24 s |
| | 01 | | | 47 m | 1 | 5.20 | 6/6 | 60 s |
| | 02 | | | 84 m | 1 | 5.60 | 3/4 | 36 s |
| | 03 | | | **80.9 km** | 2 | 0.01 | 0/3 | 85 s |
| | 04 | | | **24.9 km** | – | 0.16 | 0/4 | 40 s |
| | 05 | | | **19.5 km** | – | 0.61 | 0/2 | 80 s |
| Rhein-Main, hours | 00 | 32 | 22 (16) | 119 m | 1 | 10.09 | 8/9 | 82 s |
| | 01 | 9 | 6 (3) | **40.5 km** | – | 1.53 | 0/3 | 58 s |
| | 02 | 3 | 14 (5) | **40.7 km** | – | 0.34 | 0/6 | 133 s |
| | 03 | 12 | 9 (6) | 146 m | 1 | 1.79 | 3/3 | 45 s |
| Phoenix, start | 00 | 3 | 14 (3) | 498 m | 1 | 1.23 | 2/5 | 94 s |
| | 01 | 94 | 29 (27) | 14 m | 1 | 34.29 | 26/26 | 832 s |
| | 02 | 10 | 26 (9) | **36.6 km** | – | 0.03 | 0/11 | 122 s |
| | 03 | 8 | 14 (6) | 74 m | 1 | 3.35 | 4/8 | 92 s |

Summary: 17 of 26 runs within 500 m (median error of those 74 m, 14–498 m;
recording start recovered to within ±1 s in all but two of them, which were off by 3.0 and 3.2 s).
By time knowledge: start known 9/10, a 4-hour window 6/10, day only 2/6.
The margin works as a confidence flag: with margin ≥ 1, 16 of 17 runs were right
(the exception is Rhein-Main 01, margin 1.53); with margin < 1, 1 of 9 was right.

Also checked: the CLI on the Warsaw case 01 recording (`locate rec.wav --day
2026-08-15 --hours 16-17 --tz +02:00 --region Warszawa`, Nominatim box
plus 15 km) gives 52.29474, 21.00286 ±38 m (36 m from the truth), start
−0.3 s, margin 6.34, with SP-TPF (AAT3), SPAWY, SP-MLS (SR20) and SP-KOG
(C152) among the 6 matched flights.

Why the failures fail:

- **Warsaw 00 (hours, day).** All the evidence is two detections of one EC30
  helicopter, and the second is badly measured (v 26 m/s against a true
  47 m/s). One good detection fits equally well with many light aircraft
  anywhere in the area and time window. With the start known it is found, but
  barely (margin 0.13).
- **Warsaw 03 (day).** A PZ3T circling near the recorder explains four
  detections. An RALL flying circuits 80 km away and 4.3 h earlier explains three
  of them just as well (4.662 against 4.654 for the truth, ranked 2nd).
  A single aircraft circling is weak evidence once the start time is free.
- **Warsaw 04 (hours, day).** Stage 1 measured both CRUZ passes badly
  (114 m against a true 1307 m; 305 m and 76 m/s against 4915 m and 45 m/s).
  Both fall outside the coarse speed/distance windows, so the true assignment
  never reaches the curve stage. With the start known, re-assignment (which has
  no speed/distance filter) finds it (39 m). The CRUZ ADS-B track also has bad
  speed reports (≈100 m/s).
- **Warsaw 05 (day).** At the true cell the coarse stage settles on a wrong
  start with more chance matches (refined 0.74). Correct with a 4-hour window.
- **Rhein-Main 01.** Stage 1 missed the closest pass (an EC55 at 390 m) and
  mis-measured the others (for example 76 m/s for a hovering EC35). There was
  nothing correct to match.
- **Rhein-Main 02.** A recall problem in dense traffic: 444 candidate flights and
  94 641 assignment groups. The true assignment refines to 6.48 against
  the winner's 1.92, but its coarse score (3.09) leaves it outside the 300
  groups that get the curve check. Raising that number eightfold did not
  recover it and doubled the search time, so the change was not kept.
- **Phoenix 02.** A busy airfield attracts chance matches: next to Deer Valley
  (94 passes within 5 km in case 01) enough of 26 detections line up with
  circuit traffic to score 15.0, against 13.7 for the truth 37 km away.

Also tried and not kept: keeping the best three start times per ground cell
instead of one. Day mode was unchanged (2/6).

**Effect of the MLAT smoothing and the track bounds (branch `wf3/match`).**
On the *stored* development audio (24 cases, audio and detections reused)
the first version of this branch gave **8/24 within 500 m, against 10/24 for
main**; on Warsaw start + Rhein-Main hours (9 cases) the turn-corrected
smoothing gives 5/9 against 7/9 for main (all its margin ≥ 1 answers, 4/4,
correct; main 5/6). That audio was rendered with the truth drawn through the
*raw* MLAT reports, so their 100–150 m scatter is real motion in it (speed
swinging 30–80 m/s), and a matcher that smooths it away no longer follows the
rendered aircraft; on real recordings that scatter is a measurement error
(the Kampina C172 is not found at all without the smoothing, see
docs/evidence.md).

The renderer no longer shares the matcher's smoothing:
`e2e::load_area_flights` builds the flights with `smooth_mlat: false` and
draws the MLAT truth with its own smoother (Gaussian-weighted local cubic,
σ = 15 s, `truth_smooth_mlat`), while the matcher still reads the raw
reports from the archive. On the same two runs re-rendered that way (same
seeds and observers, new audio in `~/overhead-data/match/e2e-truth2/`) both
main and this branch get **6/9 within 500 m**; margin ≥ 1: 5/5 correct for
the branch, 4/4 for main; margin ≥ 0.5: 6/7 and 5/6, the one wrong answer
being 529 m (main: 548 m) off on the same case. The largest margin of an
answer more than 1 km off is 0.17 (main 0.22). The rest of the development
set was not re-rendered.

### Search speed

Measured with 6 threads (`taskset -c 0-5`, `RAYON_NUM_THREADS=6`), main and
this build one after the other while other jobs shared the machine, so
treat the times as indicative. "Search" is the matcher alone; the CLI wall
time also includes decoding and stage-1 detection (about 50 s for 40
minutes of audio). `OVERHEAD_TIMING=1` prints the time per search phase.

| case | before (search) | after (search) | speed-up |
|---|---|---|---|
| Warsaw start, cases 00–04 (e2e) | 20.7 / 38.4 / 19.9 / 37.0 / 83.4 s | 4.8 / 6.8 / 3.6 / 6.5 / 14.4 s | 4.3–5.8× |
| NL start, cases 00–01 (e2e) | 18.6 / 77.8 s | 4.9 / 17.7 s | 3.8–4.4× |
| Rhein-Main 4-hour window, cases 00–01 (e2e) | 101.4 / 58.4 s | 24.7 / 17.4 s | 3.4–4.1× |
| Warsaw day only, case 01 (e2e) | 58.0 s | 12.0 s | 4.8× |
| CLI `--start 16:48 --region Warszawa` | 88.5 s (149 s wall) | 14.0 s (63 s wall) | 6.3× |
| CLI `--region Poland`, day only, 948 flights | 336 s (397 s wall) | 87 s (126 s wall) | 3.9× |

Every one of these 12 runs gives exactly the same output as main (the
whole result JSON with the timing fields removed: every candidate, flight,
margin and uncertainty number). The full development set was not re-run
against main.

Where the time went and what changed (all of it leaves results
bit-identical):

- The Doppler-curve score (a ±1.2 % sweep of the rest frequency over the
  whitened spectrogram, 49 values) was 85 % of a known-start search. Each
  sample called libm `floor` (baseline x86-64 has no SSE4.1). Now the
  sweep runs column by column with plain truncating casts and, where the
  CPU has it, an AVX2 kernel that does the same IEEE operations four sweep
  values at a time (a unit test checks it gives the same bits).
- Track lookups (`Segment::state`) scanned the knots from the start; they
  use a binary search now. Knot bounding boxes skip far segments in the
  silence check and the re-assignment.
- Cell evaluation only looks at the passes heard within each detection's
  tolerance and skips start times that cannot beat the best one.
- Tried and not kept: a coarse-to-fine wide scan (500 m grid, then 250 m
  around the 8 best points) saved under 7 % and changed secondary
  candidates and margins in 6 of 9 cases (one wrong case went from 32 to
  55 km off); and stopping the Nelder–Mead refinement at a score
  tolerance. The refinement runs to its iteration limit almost every time
  (146 of 160 iterations on average) and a tolerance of 1e-5 halves a
  known-start search, but it changes which hypothesis wins in close cases
  (NL start case 01 went from 237 m to 765 m at 1e-5 and 1e-7).
- Left as it was: over a whole country the pass search and cell
  evaluation dominate, and in the CLI stage-1 detection does.

A second round (hill climb, 2026-11-05) made `locate` faster again for
detections with many lines (the harmonic comb scores every line), with the
same answers:

- An AVX-512 sweep kernel where the CPU has AVX-512F/VL/DQ (exact division
  without dividing, map rows kept in registers), else the AVX2 one.
  `OVERHEAD_NO_AVX512=1` falls back to AVX2.
- The Newton iterations that find a hypothesis's heard passes run four
  passes at a time in vector registers (AVX2), each starting from the
  previous time's state. `OVERHEAD_NO_LANES=1` runs them one at a time.
- Matches whose timing alone keeps them below the minimum curve score are
  pruned before scoring.
- In the pass search: cell positions cached per tile, a sample's cells
  sorted through a bitmap, the lattice computed eight points at a time,
  and only the passes loud enough to cost anything scanned for silence.
- MP3 audio is decoded in parallel chunks. Each chunk is first decoded over
  the packets before it (the decoder's bit reservoir), and its first
  packets are checked against the end of the previous chunk; if they
  disagree, the file is decoded in one piece again. The decimator computes
  eight outputs at a time in the same order. Both give the same bits as
  before (unit tests).
- `OVERHEAD_TIMING=1` also prints the time for audio, flights, observed
  maps and the background measurement.

Measured on the 54 real runs of docs/evidence.md (4 cores each, four runs
at a time): `locate` wall time 1537 s → 1098 s in total, median
17.7 s → 12.8 s, worst 179.5 s → 112.7 s (Oosterschelde, exact start); per
run 0.75× at the median, 1.07× at worst (Hudson River, stated start,
6.1 → 6.5 s). The 33 synthetic end-to-end searches: 2703 s → 1825 s in total, median 73.9 s → 44.6 s.
Decoding the 118-minute Oosterschelde MP3 on 4 cores: 10.6 s → 3.7 s. All
50 real result files with a candidate are identical apart from the run
time, and the 46 detector-benchmark detection files are byte-identical.

### How sure is the answer

A location is not all-or-nothing. With one fly-by, many places in a
province fit about equally well (another light aircraft elsewhere can copy
the speed, distance and time of one Doppler curve); with several fly-bys
that must all fit one place and one start, the answer becomes unique
(Oosterschelde, 7 detections: margin 2.57, 54 m). `locate` now says which
case it is, without changing the ranking, the scores or the margins
(checked: 45 of 46 real runs and 5 of 5 synthetic runs give exactly the
same candidates and statistics as before; the 46th read a day archive
that was cached only after the earlier run).

The JSON output gains `ambiguity` (and each candidate `probability` and
`same_place_as`); the stderr summary gains one line and, when ambiguous,
the best places and the hints:

```
ambiguous: 52 places fit similarly (margin 0.06); probability of the best 1/3/10 places 3/8/18 %, elsewhere 59 %; 54 distinct places, plausible area 51 cells of 2.0 km
  place  1   3.1 %  52.45018,13.37020  score 1.12  D-MZAP C42  (#1)
  ...
  to narrow it down: give the exact start time (to the minute)
```

- **Places.** Every refined hypothesis that matches at least one
  detection (≈100–200 per search, not just the reported 10) is grouped
  into distinct places, best first: a hypothesis within 2 km of a better
  place belongs to it. Up to 30 are listed (`places`: position, score,
  probability, flights, the candidate rank in that place).
- **Probabilities.** The score is read as a log-likelihood at a
  temperature: `p_i = exp((s_i − s_best)/T) / Z`, with
  `Z = Σ exp((s_j − s_best)/T) + exp((s_else − s_best)/T)` over all
  places. `s_else` is "somewhere the search did not refine" (a chance place
  elsewhere, or no ADS-B source at all). It takes most of the mass when
  the best score is low (one fly-by scores ≈0.5–1.3) and vanishes for
  several matched fly-bys (score 3+). `T = 0.30`, `s_else = 2.0`.
- **Plausible area** (`area`): the places binned into grid cells (1/40 of
  the search box's larger side, 2–10 km), keeping cells with a place of
  at least 5 % of the best place's probability, each with its total
  probability.
- **Margin.** The margin of #1 is the smallest of several versions: with
  and without the soft speed/distance evidence, with and without the
  repeat discount (method steps 7, 10 and 11), and the silence cost only
  ranks. None of these terms can raise a margin; they can lower it. When
  #1 by the full score is not #1 by the curve score alone, the margin is
  slightly negative (Vlissingen with the stated start: right place,
  margin −0.07; synthetic Kraków case 03: wrong place, −0.29). A negative
  margin reads like a margin near 0: ambiguous.
- **Verdict**: "confident" when the margin of #1 is at least 1 (the rule
  the margin was validated with), else "ambiguous: N places fit
  similarly" (N = distinct places within 1.0 of the best score), plus the
  probability of the best 1/3/5/10 places, the "elsewhere" probability,
  the effective number of places (exp of the entropy) and hints (exact
  start when more than 15 min of start times were searched, a smaller
  region when the box is over 40 km, a longer recording when #1 matches
  fewer than 3 fly-bys).

**Calibration.** `T` and `s_else` were fitted by maximum likelihood of
"the place within 2 km of the truth, else elsewhere" on 84 runs: the 46
real runs of docs/evidence.md's final run (cached detections, every
mode; 28 with an ADS-B source, 18 without) and 38 synthetic e2e runs with
stored audio (Warsaw start/day, Rhein-Main hours, NL start/hours, and the
held-out Kraków start/day, Prague hours, UK start). Real alone gives
T = 0.29, s_else = 2.0; synthetic alone T = 0.47, s_else = 3.0; using one
set's fit on the other loses 0.05 (synthetic → real) and 0.18 (real →
synthetic) nats per run. Without the elsewhere term the likelihood is far
worse (−385 against −140), because the truth is often not among the
places found at all (real: 23 of 46 runs; synthetic: 27 of 38).
Reliability of #1 at the chosen values (runs grouped by the probability
given to #1):

| p(#1) | runs | #1 right (≤ 2 km) |
|---|---|---|
| < 5 % | 40 | 3 (8 %) |
| 5–20 % | 13 | 4 (31 %) |
| 20–40 % | 4 | 1 (25 %) |
| 40–60 % | 1 | 0 |
| 80–95 % | 3 | 2 (67 %) |
| ≥ 95 % | 23 | 23 (100 %) |

Over every listed place (3300 predictions) the low bins agree (< 5 %:
predicted 0.7 %, right 0.6 %; 5–20 %: predicted 9.3 %, right 10.6 %).
**The calibration is weak**: the runs are bimodal (several fly-bys:
confident and right; one fly-by: hopeless), so the 20–95 % range rests on
eight runs; #1 is under-confident at the low end (8 % and 31 % right
where 2 % and 11 % are given: its rank carries information the softmax
ignores); and one synthetic run is confidently wrong (Kraków day case 03:
92 % on a place 21 km off, margin 0.89). The "elsewhere" share is driven
partly by the 18 real recordings that have no ADS-B source at all.

**On the real recordings** (docs/evidence.md, final run; `#1 ≤ 2 km`
counts only the top answer, the other columns the set):

| | runs | #1 ≤ 2 km | truth ≤ 2 km of a listed place | truth in the plausible area | mean probability ≤ 2 km of the truth | confident (right) |
|---|---|---|---|---|---|---|
| with an ADS-B source | 28 | 9 | 18 | 20 | 0.16 | 4 (4) |
| no ADS-B source | 18 | 0 | 1 | 4 | 0.00 | 0 |

All 24 ambiguous runs with a source are flagged as such (none presents
#1 as the answer); the truth is inside the plausible area in 16 of them
(#1 is right in 5), at the price of large areas for one fly-by (4–36 km²
for Berlin with an exact or stated start, 40–1400 km² in a Dutch
province, up to 3500 km² in Bavaria). The recordings without a source
get no confident answer and an elsewhere share of 17–93 %. Synthetic (38 runs): confident 21, all right; of the
17 ambiguous, #1 right in 3 and the truth in the plausible area in 5
(their failures are mostly recall: the true assignment never reached the
refinement, so it is not among the places).

These tables are from the final run of main 3b3464b. The current numbers
(after the harmonic comb and the hill climb of 2026-11-05) are in
docs/evidence.md: on the 54 real runs 10 within 500 m, 14 within 2 km, 4
confident and all right, the highest margin on a wrong answer 0.64
(Moerdijk harbour, stated start, 8.4 km off); on 33 synthetic cases 22
confident and all right, the highest margin on a wrong answer 0.23.

### Tracing a curve by hand

A curve traced by hand (the app's Edit fly-bys → Trace, or
`overhead-cli trace --point t,f ...`) is a detection with a `trace` field: the
clicked `[t, f]` points. The Doppler model is fitted to the points
(`overhead_core::edit::fit_trace`, with ranges), and the matcher scores a
candidate's modelled curve by its distance to the points instead of by
spectrogram brightness (`Observed::Trace` in `overhead-adsb/src/score.rs`), as
in the reference video for a line too faint to score: the mean over the
points of exp(-½·d²), d the distance from the point to the curve across it
in (t / 1.5 s, ln f / 1.2 %), with the same ±1.2 % rest-frequency sweep as
the spectrogram score. Detections without a trace are scored as before
(the locate output for them is unchanged). `overhead-adsb locate --detections
FILE` takes the edited JSON the app saves.

Calibration on the synthetic development cases (`e2e-fix/dev`, s4-00..03,
start ±120 s, 35 s off): every detection was replaced by 4–10 points read
off the rendered line (the ridge of `overhead-cli fit --ridge` at −40…+40 s
around the closest approach), either for all detections or only for those
near a true pass (a person tracing only real aircraft). Error of #1:

| case | detections + recording | traces, all | traces, true lines only |
|---|---|---|---|
| s4-01 | 752 m, confident | 1117 m, ambiguous | 501 m, confident |
| s4-03 | 798 m, confident | 1232 m, ambiguous | 746 m, ambiguous |
| s4-00 | 3.4 km (wrong)¹ | 5.3 km | 42 km |
| s4-02 | 12.5 km (wrong)¹ | 43 km | 2.9 km |

¹ from the development run's summary (start mode), not rerun here.

The first version (frequency distance only, 0.7 %) placed s4-01 1.26 km
off with 3 aircraft matched and 92 % "elsewhere"; it now matches 6 true
aircraft. Widths 0.4–1.2 % and 0.5–3 s were tried: a narrower kernel
matched fewer of the true passes, a longer time scale more, but placed the
answer less precisely. A traced curve counts at most about as much as a
well-fitted detection (score ≤ 1), so a few traced curves alone rarely give
a confident answer; the temperature and "elsewhere" score were fitted on
spectrogram scores only. Real recordings: Berlin aporee_66516_76914 traced
by hand gives D-EMFM and a place 45 m from the detection-based answer with
the exact start (170 m with the start ±5 min; day only: wrong). Seattle
xc941260 cannot work: the aircraft on its visible line (N486AS, a B739 jet)
is not among the kept ADS-B tracks (propeller, rotor and unknown types only).

### Limitations and open issues

- **Wind is not modelled.** For a microphone at rest on the ground, wind
  does not change the source speed that matters (that is the ground speed,
  which ADS-B reports); it changes the effective speed of sound along each
  ray, c + w·n̂, which differs between approach and recession. A single
  fitted `c0` cannot absorb that asymmetry; it is only covered by the speed
  tolerance (±12 m/s) and the `f0` sweep, and it biases the position in
  strong wind. The synthetic test has no wind, so this is untested.
- **What the synthetic test cannot see.** The rendered truth is built from
  the same cleaned ADS-B tracks the matcher uses, perturbed (interpolant,
  altitude bias σ 20 m / 40 m for pressure-only aircraft, 15 m wander plus
  60 m on multilateration stretches, ±0.5 s timing, random temperature).
  Real errors can be larger: aircraft without a transponder or outside
  coverage are silent here but audible in reality (they cost detections and
  can trigger false silence violations), jets are never rendered, and
  nothing flies during ADS-B gaps. The geoid and regional pressure-altitude
  corrections are applied in both the matcher and the rendering, so their
  residual errors (≈0.5 m and a few tens of metres) are only represented by
  the altitude bias.
- **Ambiguity.** Day-only searches over a large area are often ambiguous,
  and busy airfields attract chance matches (the score has no correction
  for how many passes a place has; only one aircraft's repeated passes
  with the same geometry count less, step 10 of the method, not many
  different aircraft around one airfield). Check the margin and the `ambiguity`
  section (see [How sure is the answer](#how-sure-is-the-answer)), give
  hours or a smaller region when possible, or look at the ranked
  alternatives. The probabilities only cover places the search refined;
  a true place that never reached the refinement is in "elsewhere".
- **Silence calibration.** The detection probability behind the silence
  cost (method step 5) is fitted on 22 passes at the true places of 7 of
  the real test recordings, the same recordings that judge the method, so
  its effect on them is partly in-sample. The unknown-type value
  (`p_max` 0.3) rests on one pass (Vlissingen, 1.6 km, not heard); with
  0.95 instead, Vlissingen with the stated start is wrong again (29.6 km).
  The app's Silence view still lists passes within twice the model radius
  and shows neither `p_detect` nor the cost.
- **Repeated passes.** The repeat discount compares a match only with the
  most similar stronger match of the same flight; it does not add up, so
  an aircraft flying many laps still gains at least 0.4 of a match's
  share per lap. River
  Amer with hours or the day is still 12 km wrong (margin 0.22).
- **Uncertainty.** The radius is the spread of the curve score around the
  optimum plus an empirical floor (calibrated on the development cases); it
  describes the precision of a correct answer, not the chance that the
  answer is the wrong place, which the margin indicates.
- The archive for a day appears about a day after it ends, and each day costs
  ≈3.8 GB of download (once). Without a region every candidate flight
  worldwide is searched, which is slow (coarse terrain is used for that
  search, so terrain downloads stay at a few hundred MB).

## Everything from the reference video

Every step of [the video](https://www.youtube.com/watch?v=UytYrhYLOKI)
(the times are minutes:seconds into the video), where it lives here, and how to do it. "App"
means it is in the desktop app's interface; "API" means a Rust function a
front end can call (the app holds the data, but there is no screen for it
yet); "CLI" a command line.

| Video | What | Here | How |
|---|---|---|---|
| 01:38–01:57 | Spectrogram; zoom to the right band, raise the resolution | `overhead_core::spectrogram`, app spectrogram | App: scroll/drag to zoom; CLI: `overhead-cli spec --fmin 60 --fmax 400 --nfft 16384` |
| 02:30–02:46 | Find the Doppler curves and their harmonics | `overhead_core::detect` (stage 1): `f0`, `v`, `d`, `tc`, ranges, `harmonics` | App: automatic on open (fly-by table); CLI: `overhead-cli detect` |
| 02:46–03:39 | ADS-B for every flight of the day | `overhead-adsb` cache of adsb.lol `globe_history` | Automatic in `locate` / the app; `overhead-adsb fetch DAY` |
| 03:39–04:01, 26:54 | Boost the frequencies to hear the propeller | `overhead_core::listen` (band boost, gain, limiter, resampling), `overhead-app/src/audio.rs` (`audio::Player`: play/pause/seek, position callback, cpal) | App: play button and Boost aircraft sound on the timeline; CLI: `overhead-cli listen rec.mp3 --t0 1440 --t1 1500 --band 100-400 -o boosted.wav`; API: `Player::new(audio)`, `set_settings(ListenSettings::boost(100.0, 400.0))`, `play_from(t, None)`, `on_position(cb)` |
| 04:01–04:18 | Drop jets by type code | `overhead-adsb` aircraft classes: jets are not kept (propeller, rotor and unknown types are) | Automatic |
| 04:36–07:01 | 200 m grid, CPA, delayed (heard) CPA, slant distance with a DEM | Method steps 2 (passes per 200 m cell, heard CPA = emission + range / c, SRTM terrain) | Automatic |
| 07:01–07:55 | Per point: the flights it heard and when; the 920 s ± 5 s offset filter | Method step 4 (the start `S` most detections agree on, within `tc` uncertainty + 4 s) | Automatic |
| 08:57–10:38 | Speed and CPA distance from the curve, with generous ranges | Stage-1 fit and its `ranges`; `overhead-cli fit`; app: Edit fly-bys | Automatic; App: drag a fly-by's curve or trace it |
| 10:38–11:32 | Per-event speed filter, points at the measured slant distance | Method step 3 | Automatic |
| 11:32–12:57 | Airports; the silence filter (flights the place would have heard but did not) | Method step 5 (each silent flight costs by how likely it was to be heard); **silence per candidate with times**: `Session::silence` / `--explain` → `candidates[].silence` | App: Analysis → Silence; CLI: `locate --explain`; API: `session.silence(&hypothesis, Some(radius_m))` |
| 13:47–18:04 | Model the exact curve of each real flight with retarded time | Method step 7 (`matcher::Search::curve_scores`); **modelled curves of any flight assignment, with harmonics**: `Session::model_curves` / `--explain` → `candidates[].curves` | App: Analysis → Places (curves on the spectrogram, right-click the map to test a place); CLI: `locate --explain` (top 3, `--explain-top N`); API: `session.model_curves(&h)`, `session.check(&h)` for a hand-picked place, start and flights |
| 18:21–19:40 | Score by brightness under the curve; sweep `f0` and the time offset | Method step 7 (`f0` ±1.2 %, start and `c0` optimised) | Automatic |
| 19:40–20:15 | Combined locked scoring | One start per recording (per part with cuts), method step 7 | Automatic |
| 20:15–21:08 | 200 m is too coarse: 100 m around the survivors | Continuous Nelder–Mead refinement and the ±5 km wide scan (steps 7–9) | Automatic |
| 21:08–22:27 | Scatter of event-1 vs event-2 scores; group by flight pair; best per pair | **`Session::event_scores`** / `--explain` → `event_scores.points` (every refined hypothesis, per-detection score and flight) and `best_per_assignment` | App: Analysis → Scores; CLI: `locate --explain`; API: `session.event_scores()` |
| 21:53–22:27 | Spectrograms with the modelled curves of the top points | `candidates[].curves` (polylines `[t_rec, f]` per harmonic) | App: Analysis → Places, pick A and B (`overhead-cli spec --curve` takes a straight-line model only) |
| 22:27–22:59 | Heat map per event and combined, finer, for the found flights | **`Session::score_maps`** / `--explain` → `score_maps.coarse` (the matcher's own cells over the whole search) and `score_maps.fine` (`--map-radius`, `--map-step`: per-detection layers and the combined locked layer) | App: Analysis → Heat maps; CLI: `locate --explain --map-radius 2500 --map-step 100`; API: `session.score_maps(&opts)`, `session.fine_map_for(&h, &opts)` |
| 22:59–24:07 | One faint curve, a known time: trace it by hand, score by distance to the line | `--start` with a small `--start-tol`; traced detections (`Observed::Trace`, see [Tracing a curve by hand](#tracing-a-curve-by-hand)) | App: Edit fly-bys → Trace; CLI: `overhead-cli trace --point t,f ...` |
| 24:07–25:10 | Spatial vs temporal constraints; same place from two methods | `--region`, `--hours`, `--start`; the ambiguity report ([How sure is the answer](#how-sure-is-the-answer)) | App and CLI |
| 25:10–26:20 | Open the coordinate in Google Maps / Street View | **`overhead_adsb::links`** (`google_maps`, `street_view`, `openstreetmap`, `for_place`) / `--explain` → `candidates[].links` | App: the buttons on the result; CLI: `locate --explain`; API: `links::for_place(lat, lon)` |
| 27:12–28:20 | Cuts defeat the time offset | **Cuts**: each part after a cut gets its own start (`matcher::Cuts`, `locate::Cut`); cuts found from the fly-bys (`locate::find_cuts`) and suggested from the audio (`overhead_core::cuts::suggest`) | App: Cuts on the timeline (suggested cuts, add, move, remove, Search again; Analysis → Timing per part); CLI: `locate --cut 1834.5 --cut 3600@2024-08-18T10:30:00+02:00`, `--find-cuts`, `--auto-cuts`, `overhead-cli cuts rec.mp3`; detections file `{"detections": [...], "cuts": [...]}` |

Not reproduced: the phone-call test (27:29–28:04) is about recording
conditions, not a step of the search; the 15 000-point counts of the
video's filters have no direct equivalent (our filters are not separate
passes over a grid).

### Explaining a result (`locate --explain`)

`overhead-adsb locate ... --explain` adds an `explain` object to the JSON;
everything else in the output is unchanged (checked on Oosterschelde: the
output with `--explain`, minus `explain` and timing, is identical to the
output without). It costs about 2 s on Oosterschelde (7 detections, 13 123
coarse cells, a 51 × 51 fine map) and makes the JSON large (≈17 MB there,
3.4 MB of it the score maps, most of the rest curve polylines). The same
data is available in the app through `LocationResults::session`
(`overhead_adsb::explain::Session`, kept by `locate::locate_session`); its
detection indices refer to the detections that were matched (`index` maps
them to the app's list).

- `score_maps.coarse`: `cell_m` (larger side of the search box / 150,
  200 m–5 km) and per cell `lat`, `lon`, `combined` (coarse match quality
  of the best hypothesis there, minus its silence cost),
  `matched`, `flights` (`[detection, "REG TYPE"]`) and `per_detection`
  (best coarse score of the cell for each detection alone, with its flight,
  or `null`). Cells where no flight explains any detection are absent.
- `score_maps.fine` (around candidate 1, for its flights): `lat0`, `lon0`
  (south-west corner), `dlat`, `dlon`, `rows`, `cols`, `step_m`; one layer
  per matched detection (its curve score alone, its own best start at every
  point: "score event 1 only") and `combined` (all of them with one locked
  start: the matcher's objective); each layer has `values` (row-major from
  the south-west, `null` where the pass is not known), `max`, `best_lat`,
  `best_lon`.
- `event_scores`: `points` (every refined hypothesis: `lat`, `lon`,
  `total`, `starts`, `c0`, `per_detection` [`flight`, `score`, `f0_scale`]
  or `null`, `assignment` text, `candidate_rank`, `silence_violations`,
  `hypotheses`: how many refinements converged on that point, which is
  listed once),
  `best_per_assignment` (the best point of each distinct assignment of
  flights to detections, i.e. the video's "best per flight pair") and
  `min_score` (0.45: a per-detection score below it does not count).
- `candidates[]` (the best `--explain-top`, default 3): `hypothesis` (place,
  part starts, `c0`, flights with their closest-approach time), `curves`
  (per detection: flight, `score`, `f0`, `heard_cpa_rec`, `lines` per
  harmonic as pieces of `[t_rec s, f Hz]` every 0.25 s where the track is
  known), `silence` (`radius_m` = `quiet_d_m`, the range within which an
  unheard pass costs anything (2 × `quiet_d_m` with `--quiet-d`); every
  pass of a candidate flight within it during the recording: flight, part,
  heard time (recording and UTC), CPA time, slant range, `status` =
  `matched`, `near_detection`, `silent` or `edge`, `p_detect`, `cost` and
  `counts_against` = silent and costing something; `violations` = distinct
  flights counting against it, the same number as the candidate's
  `violations`, and `penalty` = what they cost) and `links`.

Oosterschelde (`fs794275`, start ±3 min), shortened:

```json
"explain": {
  "score_maps": {
    "coarse": { "cell_m": 514.3, "cells": [
      { "lat": 51.4335, "lon": 4.1535, "combined": 4.7566, "matched": 5,
        "flights": [[0, "9H-WIT PC12"], [3, "PH-WKB P28A"], [4, "ES-ILM DA62"], [5, "PH-HJG TB20"], [6, "PH-ZYS DA50"]],
        "per_detection": [[0.9866, "9H-WIT PC12"], null, null, [0.9901, "PH-WKB P28A"], [0.9947, "ES-ILM DA62"], [0.9998, "PH-HJG TB20"], [0.9317, "PH-ZYS DA50"]] },
      "... 13122 more" ] },
    "fine": { "candidate": 1, "lat0": 51.410587, "lon0": 4.116383, "dlat": 0.0009, "dlon": 0.001443,
      "rows": 51, "cols": 51, "step_m": 100.0, "seconds": 1.85,
      "per_detection": [ { "detection": 0, "flights": ["9H-WIT PC12"], "values": "[2601 values]",
                           "max": 0.962, "best_lat": 51.45378, "best_lon": 4.15535 }, "... 4 more" ],
      "combined": { "detection": null, "flights": ["9H-WIT PC12", "PH-WKB P28A", "ES-ILM DA62", "PH-HJG TB20", "PH-ZYS DA50"],
                    "values": "[2601 values]", "max": 4.607, "best_lat": 51.43308, "best_lon": 4.15247 } } },
  "event_scores": {
    "detections": [ { "index": 0, "tc": 1476.43, "f0": 139.08 }, "... 6 more" ],
    "points": "[101 points]",
    "best_per_assignment": [
      { "lat": 51.433, "lon": 4.152, "total": 4.54, "starts": [1723965854.006], "c0": 348.58,
        "per_detection": [ { "flight": { "hex": "4d220e", "reg": "9H-WIT", "type_code": "PC12", "class": "turboprop", "callsign": "9HWIT" },
                             "score": 0.93, "f0_scale": 1.01 }, null, null, { "...": "PH-WKB 1.105" },
                           { "...": "ES-ILM 1.0" }, { "...": "PH-HJG 0.979" }, { "...": "PH-ZYS 0.809" } ],
        "assignment": "det 0: 9H-WIT PC12 · det 3: PH-WKB P28A · det 4: ES-ILM DA62 · det 5: PH-HJG TB20 · det 6: PH-ZYS DA50",
        "candidate_rank": 1, "silence_violations": 0 },
      "... 10 more" ],
    "min_score": 0.45 },
  "candidates": [ {
    "rank": 1,
    "hypothesis": { "lat": 51.43308, "lon": 4.15247, "starts": [1723965854.00565], "c0": 348.58,
      "flights": [ { "detection": 0, "flight": "4d220e", "t_cpa_unix": 1723967300.33 }, "... 4 more" ] },
    "curves": [ { "detection": 0, "flight": { "reg": "9H-WIT", "...": "" }, "score": 0.93, "f0": 140.401, "f0_scale": 1.01,
                  "heard_cpa_rec": 1473.12, "lines": [ { "harmonic": 1.0, "pieces": [[[1080.3, 183.88], [1080.55, 184.02], "... 3154 more"]] } ] },
                "... 4 more" ],
    "silence": { "radius_m": 3539.3, "quiet_d_m": 1769.7, "violations": 0, "passes": [
      { "flight": { "reg": "PH-WKB", "...": "" }, "part": 0, "heard_rec": 4938.8, "heard_utc": "2024-08-18T08:46:32.8Z",
        "cpa_utc": "2024-08-18T08:46:26.3Z", "slant_m": 2229.7, "status": "matched", "detection": 3, "counts_against": false } ] },
    "links": {
      "google_maps": "https://www.google.com/maps/search/?api=1&query=51.433083,4.152467",
      "street_view": "https://www.google.com/maps/@?api=1&map_action=pano&viewpoint=51.433083,4.152467",
      "openstreetmap": "https://www.openstreetmap.org/?mlat=51.433083&mlon=4.152467#map=16/51.433083/4.152467" } },
    "... 2 more" ]
}
```

The per-detection maxima of the fine map lie up to 2.3 km from the place
(9H-WIT alone fits best 2.3 km north): one fly-by constrains a band, the
combined locked score a point, as in the video's heat maps.

### Recordings with cuts

A cut (material removed and the rest joined) breaks the time offsets
between fly-bys on either side of it. With cuts, the recording is split into
parts at the cut points and every part gets its own start; within a part the
timing stays locked as before. Without cuts nothing changes (all reference
runs identical).

- `locate --cut T` (recording seconds; repeat or comma-separate): the part
  after `T` may start anywhere from the earliest possible start to the end
  of the possible day(s), or at most `--cut-gap-max G` seconds later per
  cut. `--cut T@START` (START as for `--start`, ±`--start-tol`) gives the
  absolute time that part's recording time 0 corresponds to, i.e. when the
  part started minus `T`. With an exact `--start` only the first part is
  anchored unless a cut has its own start.
- A detections file may be `{"detections": [...], "cuts": [600.5,
  {"at": 1200, "start_unix": 1723965000, "start_tol_s": 30}]}`
  (`locate::read_detections`; a plain list still works).
- The output gains `cuts` (each cut and the start window of the part after
  it) and each candidate `part_starts_unix` (`null` for a part none of its
  matched detections is in) and `start_part`. All are absent without cuts.
  A candidate's `start_unix` / `start_utc` is the first *known* part start:
  when `start_part` is not 0, no fly-by before the first cut matched, the
  recording's own start is unknown, and `start_unix` is the time zero of a
  later part (it can lie far outside the start window given; the CLI says
  so next to the start, and the app then shows no recording start).
- `--cut` warns about a cut inside a detection (that fly-by is split; it
  counts as after the cut if its `tc` is, and the part it opens has a free
  start, which can turn an exact-start search into a day-wide one) and
  ignores, with a warning, a cut outside the recording.
- `locate --find-cuts` finds cuts from the fly-bys themselves. For the
  search a cut only matters through which fly-bys end up on either side of
  it, so it tries a cut in every gap between two consecutive detections
  (one search each, with that part's start free) and keeps the gap that
  makes the most fly-bys fit, if it adds at least one matched fly-by and
  raises the best score by at least 0.6 (`locate::CUT_SCORE_GAIN_MIN`; one
  well-fitting fly-by adds about 0.8–1.1);
  then it tries the other gaps again (`--find-cuts-max`, default 2). The
  JSON gains `cut_search`: `baseline_score`, `baseline_matched`, every
  `tests[]` entry (`at`, `after_detection`, `before_detection`, `score`,
  `matched`, `lat`, `lon`, `score_gain`, `matched_gain`, `round`,
  `accepted`) and `suggested[]` (`at`, `range`: where the cut can be, since
  every gap in it gives the same answer; `audio_hint`: the strongest
  waveform evidence inside that range, `[t, strength]`). It is slow: one
  search per gap and round (the Oosterschelde cut test below: 11 searches,
  about 10 minutes on 4 cores). API: `locate::find_cuts(&input, max_cuts)`.
  Measured: on the Oosterschelde recording with 600 s removed at 2000 s
  (start ±3 min), the search without cuts gives 0.89 with 1 fly-by (24 km
  off); the gaps between detection 0 and 3 each give 4.61 with 5 (+3.72),
  the cut is taken with range 1839–4209 s (detections 1 and 2 fit nothing
  either way; the real cut at 2000 s is inside) and #1 is 51.43303,
  4.15276, 45 m from the truth, 5 matched, margin 1.79; a second round
  finds nothing (+0.36 with one more fly-by at most). The `audio_hint` in
  that range is a background change at 3189.3 s (strength 1.62), not the
  splice, which comes right after it (2000.0 s, strength 1.6). On the unedited recording every gap gives at most +0.05
  and no extra fly-by, so nothing is suggested.
- `overhead-cli cuts rec.mp3` suggests cuts (JSON, strongest first): digital
  silence (`dropout`), a waveform jump at one sample (`discontinuity`) and
  an abrupt, lasting change of the background spectrum (`change`), each
  with a `strength` in units of its threshold. A waveform jump's strength
  is weighted (0.2–4×) by how much it is one lasting step of the waveform
  level (what a hard splice leaves) rather than a swing that returns (a
  natural click), and suggestions below 0.5 are dropped by the defaults
  (`min_strength`; 0 with `--sensitive`). `locate --auto-cuts` uses all
  suggestions that are not inside a detection. Accept or remove them by
  passing the real ones with `--cut` instead; `--find-cuts` (above) is the
  stronger test.

How well the suggestions work: on synthetic audio a splice between two
backgrounds (about 11 dB apart, different spectra) is found at the exact place, a 0.1 s digital
silence is found, and steady ambience with a fly-by gives nothing. Material
removed from one steady background leaves only a waveform jump, typically
15–40 × the local sample-to-sample scale for a jump of 3–5 standard
deviations of the background; natural clicks, wind and handling noise on
the 14 test recordings reach 175 ×. The defaults (jump 60 ×, change 8) are
therefore conservative and find such splices only with `--sensitive` (jump
15 ×, change 4), which also flags many natural sounds. With the defaults the
14 test recordings (none known to be cut) still get 0–20 suggestions each
(median 1), mostly bird calls, voices and gusts that start abruptly and
last several seconds; on Berlin (`aporee_66516_76914`) a call at 468.5 s is
suggested, and `--auto-cuts` there frees the start of the only detection,
which makes the start-window search no better than a day-only one (#1
becomes the day-only answer 52.45018, 13.37020, margin 0.06). Oosterschelde
(start ±3 min) got 4 suggestions before the step weighting (three waveform
jumps, one background change) and gets 1 with it (the background change);
with the earlier 4, `--auto-cuts` still put #1 at the right place (20 m from the
answer without cuts, 5 matched) but the margin dropped from 2.57 to 1.30.
The step weighting took the 14 recordings from 61 to 44 default
suggestions (the natural clicks; background changes are unchanged). Use
`--auto-cuts` only after checking the suggestions.

A real splice inside one steady background is weak evidence in the audio.
On the Oosterschelde recording with 600 s removed at 2000 s (a hard cut, no
crossfade; reviewed in the engine track), the splice's jump is about 24 ×
the local scale and the defaults do not report it. Before the step
weighting `--sensitive` did not list it in its 50; with it, it is 7th of 50
(strength 1.6) and the strongest waveform jump of the recording, ahead of
the natural clicks (which are larger, but swing back). That ranking is one
example, not a calibration (no recording known to be cut was available); a
splice between two similar sample values, or with a crossfade, leaves no
trace at all. `--find-cuts` does not depend on that evidence.

### Listening

`overhead-cli listen rec.mp3 --t0 T0 --t1 T1 --band LO-HI` writes a WAV of the
excerpt with the band raised (`--band-gain`, default +18 dB) and the rest
lowered (`--rest-gain`, −12 dB), an overall gain (default: −20 dBFS RMS)
and a soft limiter, at `--rate` (44.1 kHz). The same processing plays live
in `overhead-app/src/audio.rs` (`audio::Player`, cpal on WASAPI/ALSA/
PulseAudio): `play`, `pause`, `toggle`, `seek`, `play_from(t, end)`,
`set_settings` (takes effect within one output buffer), `position`, and
`on_position(cb)` called about 30 times a second with the position and
whether it is playing. The DSP (4th-order Butterworth band-pass, gain,
limiter, Catmull–Rom resampling, seeking with a 0.15 s pre-roll so the
filters are settled) is in `overhead_core::listen` and unit-tested; the
player has a device test that is skipped where no output device exists.
The app plays through it (the timeline bar, see [App](#app-overhead)).

The analysis audio is 8 kHz, so a player fed with it has nothing above
4 kHz (enough for propeller lines, but it sounds like a telephone).
`audio::load_for_listening(&analysis)` decodes the file again at
`audio::listen_rate` (24 kHz up to about 33 minutes of audio, less for
longer files to stay within 48 M samples, about 190 MB: 1 h gets about
13 kHz, and from about 100 minutes on it stays at 8 kHz, so the 2 h
Oosterschelde recording still plays with nothing above 4 kHz), or returns
the analysis audio when that would not be clearly better; the app's player
uses it. `overhead-cli listen` uses the 8 kHz analysis audio too.

`overhead-cli listen` checks its ranges: `--t0` outside the recording and
`--t1` not after `--t0` are errors; a reversed `--band` is swapped (with a
note), and a band beyond the decoded audio's Nyquist frequency is limited
to it or, if nothing is left, an error. `ListenSettings` bands are
normalised the same way inside `listen::Processor`, so any caller is safe.
