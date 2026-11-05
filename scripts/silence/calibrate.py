"""Silence calibration table and fit (README, Method step 5) from the output of
crates/overhead-adsb/examples/silence_cal.rs.

usage: python3 -I calibrate.py DIR     (DIR/<id>.jsonl, one per ground-truth recording, from
       silence_cal AUDIO DETECTIONS LAT LON START_UNIX 6000 > DIR/<id>.jsonl at the exact start)

Passes of one aircraft less than 120 s apart are taken as one (the closest; detected if any of them
has a detection within 30 s), passes in the first or last 45 s of the recording are left out.
"""
import glob
import json
import math
import sys


def load(files):
    out = []
    for f in files:
        ps = sorted((json.loads(l) for l in open(f)), key=lambda p: (p["hex"], p["heard_rec"]))
        groups = []
        for p in ps:
            if not p["inside"]:
                continue
            if groups and groups[-1][-1]["hex"] == p["hex"] and p["heard_rec"] - groups[-1][-1]["heard_rec"] < 120:
                groups[-1].append(p)
            else:
                groups.append([p])
        for g in groups:
            best = min(g, key=lambda p: p["slant_m"])
            out.append(dict(best, detected=any(p["near_det"] is not None for p in g), rec=f.split("/")[-1][:-6]))
    return out


rows = load(sorted(glob.glob(sys.argv[1] + "/*.jsonl")))
cls = ["piston_single", "piston_twin", "turboprop", "rotor", "other"]
bins = [0, 500, 1000, 1500, 2000, 3000, 4000, 5000, 6000]
print("| slant | " + " | ".join(cls) + " |")
print("|---" * (len(cls) + 1) + "|")
for a, b in zip(bins, bins[1:]):
    cells = []
    for c in cls:
        s = [r for r in rows if a <= r["slant_m"] < b and r["loudness"] == c]
        cells.append(f"{sum(r['detected'] for r in s)}/{len(s)}" if s else "-")
    print(f"| {a/1000:.1f}-{b/1000:.1f} km | " + " | ".join(cells) + " |")

# engine aircraft: p = p_max / (1 + (d / (d50 * range))^k), optionally with the range shortened by the background
RANGE = {"piston_single": 1.0, "piston_twin": 1.4, "turboprop": 1.4}
eng = [r for r in rows if r["loudness"] in RANGE]


def ll(d50, k, pmax, bg):
    s = 0.0
    for r in eng:
        de = r["slant_m"] / RANGE[r["loudness"]]
        if bg and r.get("floor_rel_db") is not None:
            de *= 10 ** (max(-3.0, min(12.0, r["floor_rel_db"])) / 20)
        p = min(max(pmax / (1 + (de / d50) ** k), 1e-6), 1 - 1e-6)
        s += math.log(p if r["detected"] else 1 - p)
    return s


print(f"\nengine aircraft: {len(eng)} passes, {sum(r['detected'] for r in eng)} detected")
for bg in (False, True):
    for k in (3, 4, 6, 10):
        best = max((ll(d, k, 0.95, bg), d) for d in range(2000, 8001, 100))
        print(f"p_max 0.95, k {k}, background {'on' if bg else 'off'}: log-likelihood {best[0]:.2f} at d50 {best[1]} m")
