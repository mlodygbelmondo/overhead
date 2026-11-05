"""Summarise locate runs against the test-set truth.

usage: python3 -I analyze.py RUNDIR [--json OUT]
Prints one line per run and writes a JSON summary (for make_doc.py).
"""
import os
import json
import math
import sys
from pathlib import Path
DATA = os.environ.get("OVERHEAD_DATA") or os.environ.get("DOPPLER_DATA") or os.path.expanduser("~/overhead-data")

T = Path(f"{DATA}/testset")


def hav(lat1, lon1, lat2, lon2):
    r = 6371008.8
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp, dl = p2 - p1, math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * r * math.asin(math.sqrt(a))


def summarise(run: Path, m: dict):
    rid, mode = run.stem.rsplit("-", 1)
    tfile = run.with_suffix(".time")
    wall, code = (tfile.read_text().split() if tfile.exists() else ("nan", "nan"))
    out = {"id": rid, "mode": mode, "wall_s": float(wall), "exit": int(code) if code != "nan" else None}
    log = run.with_suffix(".log").read_text(errors="replace")
    out["log_tail"] = log.strip().splitlines()[-1] if log.strip() else ""
    truth_reg = (m.get("adsb_match") or {}).get("reg")
    out["truth_reg"] = truth_reg
    if not run.exists():
        out["error"] = out["log_tail"]
        return out
    d = json.loads(run.read_text())
    out["search_s"] = d["stats"]["seconds"]
    out["flights"] = d["candidate_flights"]
    out["detections_used"] = d["detections_used"]
    out["region_bbox"] = d["search_box"]
    out["days"] = d["days"]
    out["start_window"] = d["start_window_utc"]
    out["warnings"] = d["warnings"]
    cands = []
    for c in d["candidates"]:
        err = hav(m["lat"], m["lon"], c["lat"], c["lon"])
        regs = [f.get("reg") or f.get("callsign") or f["hex"] for f in c["flights"]]
        cands.append({
            "rank": c["rank"], "lat": c["lat"], "lon": c["lon"], "err_m": err, "score": c["score"],
            "margin": c["margin"], "matched": c["matched"], "start_utc": c["start_utc"], "c0": c["c0"],
            "radius_m": c["uncertainty"]["radius_m"],
            "flights": [{
                "det": f["detection"], "tc": f["tc"], "hex": f["hex"], "reg": f.get("reg"), "type": f.get("type_code"),
                "callsign": f.get("callsign"), "heard_utc": f.get("heard_utc"), "heard_rec": f.get("heard_rec"),
                "slant_m": f.get("slant_m"), "speed_ms": f.get("speed_ms"), "h_m": f.get("height_agl_m"),
                "curve": f.get("curve_score"), "f0_scale": f.get("f0_scale"),
            } for f in c["flights"]],
            "regs": regs,
            "violations": len(c["violations"]),
        })
    out["candidates"] = cands
    if not cands:
        out["error"] = f"no candidate (none of {d['candidate_flights']} flights reached the curve-score threshold)"
    if cands:
        out["top"] = cands[0]
        out["err_m"] = cands[0]["err_m"]
        near = [c for c in cands if c["err_m"] < 2000]
        out["truth_rank_2km"] = near[0]["rank"] if near else None
        if truth_reg:
            with_truth = [c for c in cands if truth_reg in c["regs"]]
            out["truth_flight_rank"] = with_truth[0]["rank"] if with_truth else None
            out["truth_flight_in_top"] = truth_reg in cands[0]["regs"]
    return out


def main():
    rd = Path(sys.argv[1])
    res = []
    for run in sorted(rd.glob("*.log")):
        rid = run.stem.rsplit("-", 1)[0]
        m = json.loads((T / rid / "meta.json").read_text())
        res.append(summarise(run.with_suffix(".json"), m))
    for r in res:
        if "top" in r:
            t = r["top"]
            print(f"{r['id']:20s} {r['mode']:6s} err {r['err_m']/1000:8.2f} km  score {t['score']:.2f} margin {t['margin']:.2f} "
                  f"flights {','.join(map(str, t['regs']))}  truth@2km rank {r['truth_rank_2km']}  truth flight rank "
                  f"{r.get('truth_flight_rank')}  search {r['search_s']:.0f} s wall {r['wall_s']:.0f} s  ({r['flights']} flights)")
        else:
            print(f"{r['id']:20s} {r['mode']:6s} FAILED: {r.get('error', r['log_tail'])}")
    if "--json" in sys.argv:
        Path(sys.argv[sys.argv.index("--json") + 1]).write_text(json.dumps(res, indent=1))


if __name__ == "__main__":
    main()
