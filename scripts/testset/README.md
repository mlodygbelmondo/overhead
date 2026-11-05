Sources searched: xeno-canto (remarks containing aircraft words in several languages, recordings
from 2023 on), Freesound (geotagged sounds uploaded since March 2023), radio aporee (through its
Internet Archive mirror, collection `radio-aporee-maps`) and a few YouTube searches. Licences are
Creative Commons or public domain, all allowing research use: CC0 1.0 and Public Domain Mark (1 each),
CC BY 4.0 (2), CC BY-SA 3.0 (3), CC BY-NC 3.0 (1) and CC BY-NC-SA 4.0 (3). None is ND. BY licences need
the recordist credited, which each entry does; each recording's licence is listed above.

All scripts are plain Python 3 (standard library only) plus `ffmpeg`/`ffprobe`, `curl` and
`yt-dlp`. Run them with `python3 -I`.

| script | what it does |
|---|---|
| `xc_fetch.py` | Fetches a xeno-canto page through its Anubis proof-of-work gate (the API v3 needs a key). |
| `xc_search.py` | Runs xeno-canto searches (e.g. `'rmk:helicopter year:">2022"'`) and prints one JSON line per recording. |
| `xc_get.py` | Downloads xeno-canto recordings (original file) and their page metadata. |
| `fs_search.py` / `fs_get.py` | Same for Freesound (public search pages; downloads the HQ MP3 preview, originals need a login). |
| `ia_get.py` | Downloads radio aporee recordings and metadata from archive.org. |
| `yt_meta.py` | Prints YouTube metadata (search or ids). |
| `catalog.py` | The curated list (`ENTRIES`, `REJECTED`); writes each `meta.json` and the ADS-B site windows (merged into existing `sites_<day>.json`, so hand-added sites survive; `alt_dates` add `<id>@<date>` sites for conflicting dates). |
| `run_detect.py` | Runs `overhead-cli detect` on each recording, writes `detect.json`. |
| `make_specs.py` / `thumbs.py` | Spectrogram PNGs for inspection / the JPEGs in `docs/testset/`. |
| `adsb_check.py` | Streams one adsb.lol `globe_history` day (3-4 GB, nothing stored) and lists every aircraft within 20 km of each site in its time window, with an interpolated CPA using geometric altitude (or QNH-corrected baro). `--recompute HITS SITES` redoes the CPAs for moved sites from the stored points. |
| `adsb_days.py` | Runs `adsb_check.py` for every `sites_<day>.json`. |
| `comb.py` | Harmonic (rotor) comb spacing over time, `comb.py AUDIO T0 T1 [FMAX]`; approach and recede spacings give the rest spacing and speed. |
| `make_doc.py` | Writes this page. |

```sh
B=~/overhead-data/target-testset/release/overhead-cli      # cargo build --release -p overhead-cli
T=~/overhead-data/testset
python3 -I scripts/testset/catalog.py meta $T
python3 -I scripts/testset/run_detect.py $B $T/*/
python3 -I scripts/testset/catalog.py sites $T $T/adsb
python3 -I scripts/testset/adsb_days.py $T/adsb --jobs 4   # only days without a hits file
python3 -I scripts/testset/thumbs.py $B $T docs/testset
python3 -I scripts/testset/make_doc.py $T $T/adsb docs/testset.md
```

ADS-B data: adsb.lol globe_history releases on GitHub (ODbL 1.0, see the LICENSE-ODbL.txt inside each
release). Only derived hit lists are kept, in `~/overhead-data/testset/adsb/`.
