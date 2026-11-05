"""Run `overhead-cli detect` on every test-set recording and summarise.

usage: python3 -I run_detect.py OVERHEAD_BIN DIR [DIR ...]

Each DIR holds one recording (audio.* or the file named in meta.json
"audio_file"). Writes DIR/detect.json and prints one summary line per
detection.
"""

import glob
import json
import os
import subprocess
import sys


def audio_of(d):
    meta = os.path.join(d, "meta.json")
    if os.path.exists(meta):
        with open(meta) as f:
            name = json.load(f).get("audio_file")
        if name:
            return os.path.join(d, name)
    files = sorted(glob.glob(os.path.join(d, "audio.*")))
    return files[0] if files else None


def summary(det):
    fb = det["flyby"]
    r = det["ranges"]
    return (
        f"tc={fb['tc']:8.1f}s [{r['tc'][0]:.0f},{r['tc'][1]:.0f}]  "
        f"f0={fb['f0']:7.1f}Hz  v={fb['v']:5.1f}m/s [{r['v'][0]:.0f},{r['v'][1]:.0f}]  "
        f"d={fb['d']:6.0f}m [{r['d'][0]:.0f},{r['d'][1]:.0f}]  "
        f"contrast={det['contrast']:.1f}  harm={len(det['harmonics'])}  "
        f"{'CONF' if det['confident'] else 'unconf'}{'  GROUND?' if det['likely_ground_vehicle'] else ''}"
    )


def main():
    exe = sys.argv[1]
    for d in sys.argv[2:]:
        a = audio_of(d)
        if not a:
            print(f"{d}: no audio")
            continue
        out = subprocess.run([exe, "detect", a], capture_output=True, text=True)
        if out.returncode != 0:
            print(f"{os.path.basename(d.rstrip('/'))}: detect failed: {out.stderr.strip()[-300:]}")
            continue
        with open(os.path.join(d, "detect.json"), "w") as f:
            f.write(out.stdout)
        with open(os.path.join(d, "detect.stderr.txt"), "w") as f:
            f.write(out.stderr)
        dets = json.loads(out.stdout)
        print(f"{os.path.basename(d.rstrip('/'))}: {len(dets)} detection(s)")
        for det in dets:
            print("   ", summary(det))


if __name__ == "__main__":
    main()
