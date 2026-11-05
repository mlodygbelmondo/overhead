"""Markdown tables for docs/testset.md (section 'Long recordings'): per recording, stage-1 detections against
the ADS-B passes they pair with (adsb_hits.json from cache_hits.py), and the passes no detection took.

usage: python3 -I doc_tables.py ID [ID ...]
"""
import os
import json
import sys
from datetime import datetime
from pathlib import Path
DATA = os.environ.get("OVERHEAD_DATA") or os.environ.get("DOPPLER_DATA") or os.path.expanduser("~/overhead-data")

T = Path(f"{DATA}/testset")
KT = 0.514444


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


def mmss(t):
    s = "-" if t < 0 else ""
    t = abs(t)
    return f"{s}{int(t // 60)}:{t % 60:04.1f}"


def main():
    for rid in sys.argv[1:]:
        h = json.loads((T / rid / "adsb_hits.json").read_text())
        p = h["pairing"]
        start = ts(p["start"])
        print(f"#### {rid}\n")
        print(f"Start at which the detections pair best: **{p['start']}** ({p['offset_s']:+d} s from the stated "
              f"start); {p['matched']} of {len(p['detections'])} detections pair one-to-one with an ADS-B pass "
              f"heard within ±{p['tol_s']:.0f} s.\n")
        print("| det | tc | f0 Hz | v m/s | d m | conf | ADS-B pass (heard − tc) | ADS-B gs m/s | slant m | AGL m |")
        print("|---|---|---|---|---|---|---|---|---|---|")
        used = set()
        for d in p["detections"]:
            a = d.get("assigned")
            if a:
                used.add((a["hex"], a["heard_t"]))
                gs = f"{a['gs_kt'] * KT:.0f}" if a.get("gs_kt") else "–"
                src = f"**{a['reg'] or a['hex']}** {a['type'] or '?'} ({-a['dt_s']:+.0f} s)"
                rest = f"{gs} | {a['slant_m']} | {a['agl_m']}"
            else:
                src, rest = "–", "– | – | –"
            print(f"| {d['det']} | {mmss(d['tc'])} | {d['f0']:.0f} | {d['v']:.0f} | {d['d']:.0f} | "
                  f"{'yes' if d['confident'] else 'no'} | {src} | {rest} |")
        others = [q for q in h["passes"] if (q["hex"], q["cpa"]["heard_t"]) not in used
                  and q["cpa"]["slant_m"] <= 5000 and 0 <= ts(q["cpa"]["heard_t"]) - start <= T_dur(rid)]
        if others:
            print("\nADS-B passes within 5 km slant inside the file that no detection took: " + "; ".join(
                f"{q['reg'] or q['hex']} {q['type'] or '?'} at {mmss(ts(q['cpa']['heard_t']) - start)} "
                f"({q['cpa']['slant_m'] / 1000:.1f} km)" for q in others) + ".")
        print()


def T_dur(rid):
    return json.loads((T / rid / "meta.json").read_text())["duration_s"]


if __name__ == "__main__":
    main()
