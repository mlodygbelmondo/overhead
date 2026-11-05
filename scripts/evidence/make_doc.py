"""Write docs/evidence.md from the analysed locate runs.

usage: python3 -I make_doc.py SUMMARY_JSON NOTES_JSON OUT_MD
SUMMARY_JSON: output of analyze.py --json; NOTES_JSON: hand-written per-recording notes
({"<id>": {"why": {"start": "...", ...}, "summary": "...", "figure_caption": "..."}, "_intro": "...", ...}).
"""
import json
import os
import re
import statistics
import sys
from datetime import datetime
from pathlib import Path
DATA = os.environ.get("OVERHEAD_DATA") or os.environ.get("DOPPLER_DATA") or os.path.expanduser("~/overhead-data")

T = Path(f"{DATA}/testset")
MODES = ["exact", "start", "hours", "day"]
MODE_NAME = {"exact": "(a) exact start (ADS-B-implied ±20 s, best case)", "start": "(a') stated start", "hours": "(b) hours (±2 h asked, 5 h searched)", "day": "(c) day only", "oracle": "diagnostic: start + truth ±5 km (±10 km box with the default margin)"}
TRUTH_ORDER = ["fs858688", "fs858591", "aporee_66516_76914", "fs847111", "xc941260"]
OTHER_ORDER = ["aporee_68715_79706", "aporee_60944_74426", "xc1152845", "xc883367", "aporee_62753_72191", "aporee_64975_75055"]


def osm(lat, lon, z=14, text=None):
    t = text or f"{lat:.5f}, {lon:.5f}"
    return f"[{t}](https://www.openstreetmap.org/?mlat={lat:.5f}&mlon={lon:.5f}#map={z}/{lat:.5f}/{lon:.5f})"


def km(e):
    if e is None:
        return "–"
    return f"{e:.0f} m" if e < 1000 else f"{e / 1000:.1f} km"


def flights_str(c, truth_reg):
    out = []
    for f in c["flights"]:
        name = f.get("reg") or f.get("callsign") or f["hex"]
        typ = f.get("type") or "?"
        s = f"{name} {typ}"
        if truth_reg and f.get("reg") == truth_reg:
            s = f"**{s}**"
        if f.get("slant_m") is None:
            s += " [no pass: heard time and range missing, bug 1]"
        out.append(f"{s} (det {f['det']}, curve {f['curve']:.2f})")
    return "; ".join(out)


def cmd_inputs(rid, mode):
    log = Path(os.environ.get("RUNS", f"{DATA}/evidence/runs")) / f"{rid}-{mode}.log"
    if not log.exists():
        return ""
    first = log.read_text().splitlines()[0]
    a = first[first.index("--day"):first.index(" -o ")] if "--day" in first and " -o " in first else first
    # the stage-1 detections file is an input of the run, not of the user
    a = re.sub(r"^--detections \S+ \S+ ", "", a)
    a = re.sub(r"--region (.+?)( --|$)", lambda mm: f'--region "{mm.group(1)}"{mm.group(2)}', a)
    return "`" + a + "`"


def local_time(m):
    return m["local_start"].replace("T", " ")[:16]


def main():
    res = json.loads(Path(sys.argv[1]).read_text())
    notes = json.loads(Path(sys.argv[2]).read_text())
    by = {(r["id"], r["mode"]): r for r in res}
    metas = {rid: json.loads((T / rid / "meta.json").read_text()) for rid in TRUTH_ORDER + OTHER_ORDER}
    L = []
    L.append("# Evidence: the automatic pipeline on real recordings\n")
    L.append(notes["_intro"].strip() + "\n")

    # ---------------- summary
    L.append("## Summary\n")
    L.append("Error = distance from the true location to our #1 candidate. \"Right flight\" = the ADS-B ground-truth "
             "aircraft is among the flights matched by #1. Search = the matcher's own time (after detection and loading); "
             "wall = the whole `locate` command (in this run without stage 1: the detections were passed in, see above). Each recording is located once per mode.\n")
    for title, ids in (("Recordings with an ADS-B ground truth in testset.md (5; Altenthann doubtful, see its section)", TRUTH_ORDER),
                       ("Recordings with no ADS-B source (6): a correct answer is \"no confident location\"", OTHER_ORDER)):
        L.append(f"**{title}**\n")
        L.append("| mode | run | ≤ 500 m | ≤ 2 km | ≤ 10 km | right flight in #1 | margin ≥ 1 | median search / wall |")
        L.append("|---|---|---|---|---|---|---|---|")
        for mode in MODES:
            rs = [by.get((i, mode)) for i in ids]
            rs = [r for r in rs if r is not None]
            ok = [r for r in rs if "err_m" in r]
            n = len(rs)
            if n == 0:
                L.append(f"| {MODE_NAME[mode]} | not applicable (no ADS-B flight to take an exact start from) | – | – | – | – | – | – |")
                continue
            c = lambda lim: sum(1 for r in ok if r["err_m"] <= lim)
            rf = sum(1 for r in ok if r.get("truth_flight_in_top"))
            conf = sum(1 for r in ok if r["top"]["margin"] >= 1.0)
            srch = statistics.median([r["search_s"] for r in ok]) if ok else float("nan")
            wall = statistics.median([r["wall_s"] for r in rs]) if rs else float("nan")
            rf_s = f"{rf}/{n}" if ids is TRUTH_ORDER else "–"
            L.append(f"| {MODE_NAME[mode]} | {n} ({n - len(ok)} failed before matching) | {c(500)}/{n} | {c(2000)}/{n} | "
                     f"{c(10000)}/{n} | {rf_s} | {conf}/{n} | {srch:.0f} s / {wall:.0f} s |")
        L.append("")
    L.append(notes["_summary"].strip() + "\n")

    # ---------------- all runs
    L.append("## All runs\n")
    L.append("| recording | mode | #1 candidate | error | score / margin | #1 matched flights (ground truth in bold) | truth rank (≤ 2 km / flight) | search / wall | why |")
    L.append("|---|---|---|---|---|---|---|---|---|")
    for rid in TRUTH_ORDER + OTHER_ORDER:
        m = metas[rid]
        for mode in MODES + ["oracle"]:
            r = by.get((rid, mode))
            if r is None:
                continue
            why = notes.get(rid, {}).get("why", {}).get(mode, "")
            name = f"[{m['short']}](#{rid})"
            if "top" not in r:
                L.append(f"| {name} | {MODE_NAME[mode]} | failed: {r.get('error', r.get('log_tail', ''))} | – | – | – | – | {r['wall_s']:.0f} s wall | {why} |")
                continue
            t = r["top"]
            tr = f"{r['truth_rank_2km'] or '–'} / {r.get('truth_flight_rank') or '–'}" if m.get("adsb_match") else f"{r['truth_rank_2km'] or '–'} / n.a."
            L.append(f"| {name} | {MODE_NAME[mode]} | {osm(t['lat'], t['lon'])} | {km(r['err_m'])} | {t['score']:.2f} / {t['margin']:.2f} | "
                     f"{flights_str(t, m.get('adsb_match', {}).get('reg') if m.get('adsb_match') else None)} | {tr} | "
                     f"{r['search_s']:.0f} s / {r['wall_s']:.0f} s | {why} |")
    L.append("")

    # ---------------- per recording
    L.append("## Recordings\n")
    for rid in TRUTH_ORDER + OTHER_ORDER:
        m = metas[rid]
        n = notes.get(rid, {})
        L.append(f"### {rid}\n")
        L.append(f"**{m['short']}**: {m['title']}\n")
        src = f"<{m['source_url']}>"
        if m.get("archive_url"):
            src += f" (mirror <{m['archive_url']}>)"
        L.append(f"- Source (page with the audio): {src}. Recordist: {m.get('recordist', '?')}, licence {m.get('licence', '?')}.")
        L.append(f"- Place: {m.get('place')}; {m.get('region')}; {m.get('country')}.")
        L.append(f"- True location: {osm(m['lat'], m['lon'], 15)} (± {m.get('location_uncertainty_m')} m; {m.get('location_note', '')})")
        utc = m["utc_start"].replace("T", " ").replace("Z", "Z")
        L.append(f"- Date/time (stated start): **{local_time(m)} local** ({m['timezone']}, UTC{m['utc_offset'][:3]}:{m['utc_offset'][3:]}) = "
                 f"**{utc[:16]}Z**, stated uncertainty ± {m['start_uncertainty_s'] / 60:.0f} min. Length {m['duration_s']:.0f} s.")
        am = m.get("adsb_match")
        if am:
            L.append(f"- ADS-B ground truth: **{am['reg']}**. {am.get('note', '')}" + (f" Caveat: {am['caveat']}." if am.get("caveat") else ""))
        else:
            L.append("- ADS-B ground truth: none (see docs/testset.md). Whatever we match is not the aircraft heard.")
        L.append(f"- Stage 1 (current detector): {n.get('stage1', '')}")
        L.append("")
        L.append("| mode | input given to `locate` | #1 candidate | error | margin | matched flights of #1 | start found (UTC) | search / wall |")
        L.append("|---|---|---|---|---|---|---|---|")
        for mode in MODES + ["oracle"]:
            r = by.get((rid, mode))
            if r is None:
                continue
            inp = n.get("inputs", {}).get(mode) or cmd_inputs(rid, mode)
            if "top" not in r:
                L.append(f"| {MODE_NAME[mode]} | {inp} | failed: {r.get('error', r.get('log_tail', ''))} | – | – | – | – | {r['wall_s']:.0f} s |")
                continue
            t = r["top"]
            L.append(f"| {MODE_NAME[mode]} | {inp} | {osm(t['lat'], t['lon'])} ±{t['radius_m']:.0f} m | {km(r['err_m'])} | {t['margin']:.2f} | "
                     f"{flights_str(t, am['reg'] if am else None)} | {t['start_utc'][11:21]} | {r['search_s']:.0f} s / {r['wall_s']:.0f} s |")
        L.append("")
        fig = Path(sys.argv[3]).parent / "evidence" / f"{rid}.jpg"
        if fig.exists():
            L.append(f"![{rid}: spectrogram with curves](evidence/{rid}.jpg)\n")
            L.append("*" + n.get("figure_caption", "Whitened spectrogram around the fly-by(s). White dots: stage-1 fit. "
                     "Green: the ADS-B ground-truth flight (real trace geometry) seen from the true location, placed with the stated start, "
                     "f0 taken from the detection. Cyan dashes: the same with the start corrected so that its heard CPA falls on the "
                     "detection (the correction is in the image header). Magenta dashes: the pass our #1 candidate matched "
                     + ("in mode (a) exact start" if rid in TRUTH_ORDER else "in mode (a') stated start")
                     + " (straight-line approximation).") + "*\n")
        L.append(n.get("summary", "").strip() + "\n")

    L.append(notes.get("_bugs", "").strip() + "\n")
    L.append(notes.get("_repro", "").strip() + "\n")
    Path(sys.argv[3]).write_text("\n".join(L))
    print("wrote", sys.argv[3])


if __name__ == "__main__":
    main()
