"""Score the long-recording locate runs against the truth (true place from meta.json, truth flights = the
ADS-B passes paired with detections in adsb_hits.json).

usage: python3 -I analyze.py RUNDIR [--json OUT]
"""
import os
import json
import math
import sys
from pathlib import Path
DATA = os.environ.get("OVERHEAD_DATA") or os.environ.get("DOPPLER_DATA") or os.path.expanduser("~/overhead-data")

T = Path(f"{DATA}/testset")
MODES = ["exact", "start", "both", "hours", "day"]


def hours_searched(d):
    """Length of the start windows locate actually searched (h) and its UTC offsets (from the output JSON)."""
    from datetime import datetime
    tot = 0.0
    for a, b in d.get("start_windows_utc") or [d["start_window_utc"]]:
        f = lambda x: datetime.fromisoformat(x.replace("Z", "+00:00"))  # noqa: E731
        tot += (f(b) - f(a)).total_seconds() / 3600
    return tot, d.get("utc_offsets_h") or []


def hav(lat1, lon1, lat2, lon2):
    r = 6371008.8
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp, dl = p2 - p1, math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * r * math.asin(math.sqrt(a))


def main():
    rd = Path(sys.argv[1])
    rows = []
    for run in sorted(rd.glob("*.log")):
        if not run.with_suffix(".time").exists():
            continue  # still running
        rid, mode = run.stem.rsplit("-", 1)
        rdir = T / rid if (T / rid / "meta.json").exists() else T / "candidates" / rid
        m = json.loads((rdir / "meta.json").read_text())
        hits = json.loads((rdir / "adsb_hits.json").read_text())
        truth = {d["assigned"]["hex"] for d in hits["pairing"]["detections"] if d.get("assigned")}
        wall, code = (run.with_suffix(".time").read_text().split() + ["?", "?"])[:2]
        out = {"id": rid, "mode": mode, "wall_s": wall, "exit": code, "truth_hex": sorted(truth)}
        js = run.with_suffix(".json")
        if not js.exists():
            out["error"] = run.read_text(errors="replace").strip().splitlines()[-1]
            rows.append(out)
            continue
        d = json.loads(js.read_text())
        out["search_s"] = round(d["stats"]["seconds"], 1)
        out["window_h"], out["utc_offsets_h"] = hours_searched(d)
        out["flights"] = d["candidate_flights"]
        out["detections_used"] = d["detections_used"]
        cands = d["candidates"]
        if not cands:
            out["error"] = "no candidate"
            rows.append(out)
            continue
        c = cands[0]
        out.update(lat=c["lat"], lon=c["lon"], err_m=round(hav(m["lat"], m["lon"], c["lat"], c["lon"])),
                   score=round(c["score"], 2), margin=round(c["margin"], 2), matched=c["matched"],
                   start=c["start_utc"],
                   flights1=[{"det": f["detection"], "reg": f.get("reg") or f.get("callsign") or f["hex"],
                              "type": f.get("type_code"), "truth": f["hex"] in truth,
                              "curve": round(f.get("curve_score") or 0, 2)} for f in c["flights"]])
        out["truth_in_1"] = sum(1 for f in c["flights"] if f["hex"] in truth)
        near = [k for k in cands if hav(m["lat"], m["lon"], k["lat"], k["lon"]) <= 2000]
        out["rank_2km"] = near[0]["rank"] if near else None
        if near:
            k = near[0]
            out["best_2km"] = {"rank": k["rank"], "err_m": round(hav(m["lat"], m["lon"], k["lat"], k["lon"])),
                               "score": round(k["score"], 2), "matched": k["matched"],
                               "truth": sum(1 for f in k["flights"] if f["hex"] in truth)}
        rows.append(out)
    if "--md" in sys.argv:
        names = {"exact": "(a) ADS-B start ±20 s (best case)", "start": "(a') stated start ±3 min",
                 "both": "(a'') both stated times ±23 min", "hours": "(b) hours", "day": "(c) day only"}

        def label(r):
            n = names.get(r["mode"], r["mode"])
            if r["mode"] in ("hours", "day") and "window_h" in r:
                offs = "/".join(f"{o:+.0f}" for o in r["utc_offsets_h"])
                n += f" ({r['window_h']:.0f} h searched, UTC {offs})"
            return n
        order = {k: i for i, k in enumerate(MODES)}
        for r in sorted(rows, key=lambda r: (r["id"], order.get(r["mode"], 9))):
            if "error" in r:
                print(f"| {r['id']} | {label(r)} | failed: {r['error']} | | | | | | {r['wall_s']} s |")
                continue
            fl = "; ".join(f"{'**' if f['truth'] else ''}{f['reg']} {f['type'] or '?'}{'**' if f['truth'] else ''} "
                           f"(det {f['det']}, curve {f['curve']:.2f})" for f in r["flights1"])
            b = r.get("best_2km")
            bs = (f"#{b['rank']}, {b['err_m']} m, score {b['score']:.2f}, {b['truth']} paired flight(s)" if b else "none in top 10")
            ok = " **located**" if r["err_m"] <= 2000 else ""
            print(f"| {r['id']} | {label(r)} | [{r['err_m'] / 1000:.1f} km]"
                  f"(https://www.openstreetmap.org/?mlat={r['lat']:.5f}&mlon={r['lon']:.5f}#map=13/{r['lat']:.5f}/{r['lon']:.5f}){ok} | "
                  f"{r['score']:.2f} / {r['margin']:.2f} | {fl} | {r['truth_in_1']}/{len(r['truth_hex'])} | {bs} | "
                  f"{r['flights']} | {r['search_s']:.0f} s / {float(r['wall_s']):.0f} s |")
        return
    for r in rows:
        if "error" in r:
            print(f"{r['id']:9} {r['mode']:6} FAILED {r['error']} ({r['wall_s']} s)")
            continue
        fl = "; ".join(f"{f['reg']} {f['type'] or '?'} det{f['det']} c{f['curve']}{' *' if f['truth'] else ''}"
                       for f in r["flights1"])
        print(f"{r['id']:9} {r['mode']:6} err {r['err_m'] / 1000:7.1f} km score {r['score']:5.2f} margin "
              f"{r['margin']:4.2f} matched {r['matched']} truth-in-#1 {r['truth_in_1']}/{len(r['truth_hex'])} "
              f"rank<=2km {r['rank_2km']} flights {r['flights']} search {r['search_s']} s wall {r['wall_s']} s | {fl}")
    if "--json" in sys.argv:
        json.dump(rows, open(sys.argv[sys.argv.index("--json") + 1], "w"), indent=1)


if __name__ == "__main__":
    main()
