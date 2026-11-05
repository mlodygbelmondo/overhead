"""Render a small JPEG spectrogram of the fly-by window of each test-set
recording, for docs/testset.md.

usage: python3 -I thumbs.py OVERHEAD_BIN ROOT OUTDIR

Windows are listed below (seconds into the file, max frequency). The fitted
detector curve nearest the window centre is overlaid when there is one.
"""

import json
import os
import subprocess
import sys

WINDOWS = {
    "aporee_68715_79706": (540, 720, 600),
    "fs858688": (0, 120, 600),
    "fs858591": (40, 165, 600),
    "xc941260": (20, 150, 600),
    "aporee_62753_72191": (0, 150, 600),
    "aporee_64975_75055": (70, 200, 800),
    "aporee_60944_74426": (220, 360, 600),
    "aporee_66516_76914": (450, 580, 600),
    "xc1152845": (90, 210, 600),
    "xc883367": (60, 230, 600),
    "fs847111": (150, 300, 600),
}


def main():
    exe, root, out = sys.argv[1:4]
    os.makedirs(out, exist_ok=True)
    for rid, (t0, t1, fmax) in WINDOWS.items():
        d = os.path.join(root, rid)
        with open(os.path.join(d, "meta.json")) as f:
            meta = json.load(f)
        audio = os.path.join(d, meta["audio_file"])
        cmd = [exe, "spec", audio, "--t0", str(t0), "--t1", str(t1), "--fmin", "0", "--fmax", str(fmax),
               "--whiten", "--width", "1000", "--height", "360", "--grid-s", "10", "--grid-hz", "100"]
        det_path = os.path.join(d, "detect.json")
        if os.path.exists(det_path):
            with open(det_path) as f:
                dets = json.load(f)
            mid = (t0 + t1) / 2
            inside = [x for x in dets if t0 <= x["flyby"]["tc"] <= t1]
            if inside:
                fb = min(inside, key=lambda x: abs(x["flyby"]["tc"] - mid))["flyby"]
                cmd += ["--curve", f"{fb['f0']},{fb['v']},{fb['d']},{fb['tc']}"]
        png = os.path.join(d, "thumb.png")
        subprocess.run(cmd + ["-o", png], check=True, capture_output=True)
        jpg = os.path.join(out, f"{rid}.jpg")
        subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", png, "-q:v", "7", jpg], check=True)
        print(jpg, os.path.getsize(jpg))


if __name__ == "__main__":
    main()
