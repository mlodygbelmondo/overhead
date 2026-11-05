"""Run the automatic pipeline (detect -> overhead-adsb locate) on test-set recordings.

usage: python3 -I run_locate.py OUTDIR MODE ID [ID...]
MODE: exact | start | hours | day | oracle (diagnostic)

Inputs given to locate, all derived from meta.json (what the recordist states):
  exact: --start <ADS-B-implied exact start (UTC)> --start-tol 20 (only for the ADS-B ground-truth
         recordings: start = heard CPA of the truth flight from the true place minus the detection's tc,
         so this is a best case that borrows the exact time from the very flight we want to find)
  start: --start <stated local start with its UTC offset> --start-tol max(120, stated uncertainty)
  hours: --hours <H-2>-<H+2> local (H = stated start rounded to the hour), zone from the region
  day:   --day only
  every mode: --day <local date> --region "<state/province>, <country>"
The audio file is passed, so locate runs stage 1 itself and scores on the real spectrogram.
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path
DATA = os.environ.get("OVERHEAD_DATA") or os.environ.get("DOPPLER_DATA") or os.path.expanduser("~/overhead-data")

B = os.environ.get("OVERHEAD_ADSB", f"{DATA}/target-evidence/release/overhead-adsb")
T = Path(f"{DATA}/testset")
CORES = os.environ.get("CORES", "12-15")
THREADS = os.environ.get("THREADS", "4")
# optional: stage-1 detections from this directory (<id>.json) instead of running stage 1 again
DETS = os.environ.get("DETS")

# region strings for Nominatim (state/province + country; checked in the output's resolved box)
REGION = {
    "fs858688": "Noord-Brabant, Netherlands",
    "fs858591": "Noord-Brabant, Netherlands",
    "aporee_66516_76914": "Berlin, Germany",
    "fs847111": "Bavaria, Germany",
    "xc941260": "Washington State, United States",
    "aporee_68715_79706": "Wielkopolskie, Poland",
    "aporee_62753_72191": "New York State, United States",
    "aporee_64975_75055": "New Jersey, United States",
    "aporee_60944_74426": "Brandenburg, Germany",
    "xc1152845": "Calvados, France",
    "xc883367": "Vorarlberg, Austria",
}

# ADS-B-implied recording start: hits_<day>.json cpa.heard_t of the truth flight minus stage-1 tc
# (1 s precision; fs858591 det 0 gives 10:42:19, det 1 10:42:18; xc941260 has no detection, so the
# stated start is used: stage 1 finds nothing there in any mode)
EXACT_START = {
    "fs858688": "2025-07-18T09:39:53Z",
    "fs858591": "2025-07-18T10:42:18Z",
    "aporee_66516_76914": "2025-02-09T08:58:17Z",
    "fs847111": "2026-03-03T12:38:23Z",
    "xc941260": "2024-03-13T15:46:00Z",
}


def args_for(m, mode):
    local = m["local_start"]  # YYYY-MM-DDTHH:MM:SS
    off = m["utc_offset"]  # +0200
    off = off[:3] + ":" + off[3:]
    day = local[:10]
    a = ["--day", day, "--region", REGION[m["id"]]]
    if mode == "exact":
        a += ["--start", EXACT_START[m["id"]], "--start-tol", "20"]
    elif mode == "start":
        a += ["--start", local + off, "--start-tol", str(max(120, int(m["start_uncertainty_s"])))]
    elif mode == "hours":
        hh, mm = int(local[11:13]), int(local[14:16])
        h = hh + (1 if mm >= 30 else 0)
        lo, hi = max(0, h - 2), min(24, h + 2)
        a += ["--hours", f"{lo}-{hi}"]
    elif mode == "oracle":
        # diagnostic only: the start as in "start", region = 5 km around the TRUE location
        # (locate adds its default --region-margin of 5 km, so the box is about ±10 km).
        # Tells whether the right flight/place scores as well as the regional winner.
        a = ["--day", day, "--region", f"{m['lat']},{m['lon']},5",
             "--start", local + off, "--start-tol", str(max(120, int(m["start_uncertainty_s"])))]
    elif mode != "day":
        raise SystemExit(f"bad mode {mode}")
    return a


def main():
    out = Path(sys.argv[1])
    mode = sys.argv[2]
    out.mkdir(parents=True, exist_ok=True)
    for rid in sys.argv[3:]:
        m = json.loads((T / rid / "meta.json").read_text())
        audio = T / rid / m["audio_file"]
        a = args_for(m, mode)
        cmd = ["taskset", "-c", CORES, B, "locate", str(audio), *a, "-o", str(out / f"{rid}-{mode}.json")]
        if DETS:
            cmd[5:5] = ["--detections", str(Path(DETS) / f"{rid}.json")]
        env = dict(os.environ, RAYON_NUM_THREADS=THREADS)
        t0 = time.time()
        with open(out / f"{rid}-{mode}.log", "w") as log:
            log.write(" ".join(cmd) + "\n")
            log.flush()
            r = subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT, env=env)
        dt = time.time() - t0
        (out / f"{rid}-{mode}.time").write_text(f"{dt:.1f} {r.returncode}\n")
        print(f"{rid} {mode}: exit {r.returncode} in {dt:.0f} s", flush=True)


if __name__ == "__main__":
    main()
