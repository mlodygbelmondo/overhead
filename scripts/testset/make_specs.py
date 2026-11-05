"""Render spectrograms for test-set recordings.

usage: python3 -I make_specs.py OVERHEAD_BIN [--fmax HZ] [--t0 S --t1 S] DIR [DIR ...]

Writes DIR/spec_overview.png (whole file, or the --t0/--t1 window) and, if
DIR/detect.json has detections, DIR/spec_det<N>.png with the fitted curve
overlaid on a window around each detection.
"""

import argparse
import glob
import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from run_detect import audio_of  # noqa: E402


def run(args):
    subprocess.run(args, check=True, capture_output=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("exe")
    ap.add_argument("dirs", nargs="+")
    ap.add_argument("--fmax", type=float, default=600)
    ap.add_argument("--fmin", type=float, default=0)
    ap.add_argument("--t0", type=float)
    ap.add_argument("--t1", type=float)
    ap.add_argument("--nfft", type=int, default=8192)
    ap.add_argument("--suffix", default="overview")
    a = ap.parse_args()
    for d in a.dirs:
        au = audio_of(d)
        if not au:
            continue
        cmd = [a.exe, "spec", au, "--fmin", str(a.fmin), "--fmax", str(a.fmax), "--nfft", str(a.nfft), "--whiten",
               "--grid-s", "10", "-o", os.path.join(d, f"spec_{a.suffix}.png")]
        if a.t0 is not None:
            cmd += ["--t0", str(a.t0)]
        if a.t1 is not None:
            cmd += ["--t1", str(a.t1)]
        run(cmd)
        det_path = os.path.join(d, "detect.json")
        if a.suffix != "overview" or not os.path.exists(det_path):
            continue
        for old in glob.glob(os.path.join(d, "spec_det*.png")):
            os.remove(old)
        with open(det_path) as f:
            dets = json.load(f)
        for i, det in enumerate(dets):
            fb = det["flyby"]
            top = max(det["harmonics"]) * fb["f0"] * 1.4
            run([a.exe, "spec", au, "--t0", str(max(0, fb["tc"] - 60)), "--t1", str(fb["tc"] + 60),
                 "--fmin", "0", "--fmax", str(max(200, min(1000, top))), "--whiten", "--grid-s", "10",
                 "--curve", f"{fb['f0']},{fb['v']},{fb['d']},{fb['tc']}",
                 "-o", os.path.join(d, f"spec_det{i}.png")])
        print(d, "ok", len(dets))


if __name__ == "__main__":
    main()
