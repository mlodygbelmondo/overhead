"""Chance-coincidence check of the detection/ADS-B pairing: redo the offset search of cache_hits.py with the
stated start moved by whole minutes (wrong clocks), where any pairing is by chance. Compares the number of
detections paired at the true stated start with the distribution at shifted starts.

usage: python3 -I null_test.py [--tol S] [--radius-km R] [--wide] ID [ID ...]   (defaults 15 s, 10 km)

--wide: 50 wrong clocks at 6-min steps over +-150 min (|shift| >= 6 min) instead of the 10 default shifts, and
a summary of how many wrong clocks pair at least as many detections as the stated clock.
"""
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cache_hits as ch  # noqa: E402
DATA = os.environ.get("OVERHEAD_DATA") or os.environ.get("DOPPLER_DATA") or os.path.expanduser("~/overhead-data")

T = Path(f"{DATA}/testset")
SHIFTS_MIN = [-40, -32, -24, -16, -8, 8, 16, 24, 32, 40]
TOL = 15.0


def best(dets, passes, st, unc):
    cands = [st + k for k in range(-int(unc), int(unc) + 1)]
    s = max(cands, key=lambda x: (ch.pair(dets, passes, x, TOL)[1], -abs(x - st)))
    return len(ch.pair(dets, passes, s, TOL)[2])


def main():
    global TOL, SHIFTS_MIN
    args = sys.argv[1:]
    rad = 10.0
    while args and args[0].startswith("--"):
        if args[0] == "--tol":
            TOL = float(args[1])
        elif args[0] == "--radius-km":
            rad = float(args[1])
        elif args[0] == "--wide":
            SHIFTS_MIN = [k for k in range(-150, 151, 6) if abs(k) >= 6]
            args = args[1:]
            continue
        args = args[2:]
    for rid in args:
        m = json.loads((T / rid / "meta.json").read_text())
        dets = json.loads((T / rid / "detect.json").read_text())
        wide = dict(m, start_uncertainty_s=m["start_uncertainty_s"] + 60 * max(abs(s) for s in SHIFTS_MIN))
        _, _, passes, _ = ch.collect(wide, f"{DATA}/adsb", rad * 1000)
        st = ch.ts(m["utc_start"])
        unc = m["start_uncertainty_s"]
        real = best(dets, passes, st, unc)
        null = [best(dets, passes, st + 60 * s, unc) for s in SHIFTS_MIN]
        if len(SHIFTS_MIN) > 10:
            ge = sum(1 for n in null if n >= real)
            print(f"{rid} (tol {TOL:.0f} s, {rad:.0f} km): stated start -> {real} paired; {len(null)} wrong clocks "
                  f"(6-min steps, +-150 min): {ge} pair >= {real}, max {max(null)}, counts {null}", flush=True)
        else:
            print(f"{rid} (tol {TOL:.0f} s, {rad:.0f} km): stated start -> {real} paired; shifted by {SHIFTS_MIN} min -> {null}", flush=True)


if __name__ == "__main__":
    main()
