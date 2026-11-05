"""ADS-B passes over a recording site from the overhead-adsb day cache, and their match to stage-1 detections.

usage: python3 -I cache_hits.py META.json [--cache DIR] [--radius-km 8] [--detect DETECT.json]
                                [--start UTC_ISO] [--tol 20] [--out HITS.json]

META.json is a test-set meta.json (lat, lon, elev_m, geoid_m, utc_start, duration_s, start_uncertainty_s).
The site window is [utc_start - uncertainty - 2 min, utc_start + duration + uncertainty + 2 min].

Unlike scripts/testset/adsb_check.py (which streams the whole 3-4 GB day and keeps one closest approach per
aircraft), this reads the traces `overhead-adsb fetch` keeps in ~/overhead-data/adsb/<day>/kept.bin, i.e. only
propeller, rotor and unknown-type aircraft (jets are indexed but not kept), and splits each trace into separate
passes (an aircraft flying circuits passes several times in a long recording). Altitudes, QNH correction and the
interpolated CPA are those of adsb_check.py.

With --detect, every detection is paired with the passes whose heard-CPA time (CPA + slant / 343 m/s) falls
near its tc. The recording start is unknown to a few minutes, so the script first searches the start offset
(within the stated uncertainty) that pairs the most detections with a pass within --tol seconds, preferring
close passes, and prints the pairing at that offset; --start fixes the start instead.
"""
import argparse
import gzip
import json
import math
import os
import statistics
import sys
from datetime import datetime, timedelta, timezone

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "testset"))
import adsb_check as ac  # noqa: E402

FT = 0.3048


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


def days_between(t0, t1):
    d = datetime.fromtimestamp(t0, timezone.utc).date()
    end = datetime.fromtimestamp(t1, timezone.utc).date()
    out = []
    while d <= end:
        out.append(d.isoformat())
        d += timedelta(days=1)
    return out


def load_traces(cache, day, site, t0, t1, rad):
    """Kept traces of one day that come within 1.5 * rad of the site during [t0, t1]."""
    dirp = os.path.join(cache, day)
    lat_pad = rad * 1.5 / 111000.0
    lon_pad = lat_pad / max(0.1, math.cos(math.radians(site["lat"])))
    out = []
    with open(os.path.join(dirp, "kept.bin"), "rb") as kept, open(os.path.join(dirp, "index.jsonl")) as idx:
        for line in idx:
            e = json.loads(line)
            if not e.get("kept") or e["t1"] < t0 - 120 or e["t0"] > t1 + 120:
                continue
            b = e["bbox"]
            if (b["lat_max"] < site["lat"] - lat_pad or b["lat_min"] > site["lat"] + lat_pad
                    or b["lon_max"] < site["lon"] - lon_pad or b["lon_min"] > site["lon"] + lon_pad):
                continue
            off, ln = e["kept"]
            kept.seek(off)
            raw = kept.read(ln)
            try:
                data = json.loads(gzip.decompress(raw) if raw[:2] == b"\x1f\x8b" else raw)
            except Exception:
                continue
            out.append((e, data))
    return out


def passes_of(site, pts, rad):
    """Split one aircraft's points (with AGL) into passes: local minima of slant range separated by a rise of
    at least max(1.5 km, 50 %) or a gap > 5 min."""
    xyz = [ac.enu(site, q[1], q[2], q[8]) for q in pts]
    r = [math.sqrt(x * x + y * y + z * z) for x, y, z in xyz]
    n = len(pts)
    if n == 0:
        return []
    # walk the trace, tracking the running minimum; when the range rises well above it, track the peak;
    # when it then falls well below the peak, a new pass starts at the peak.
    cuts = [0]
    state, mn, pk, pk_i = "down", r[0], r[0], 0
    for i in range(1, n):
        if pts[i][0] - pts[i - 1][0] > 300:
            cuts.append(i)
            state, mn, pk, pk_i = "down", r[i], r[i], i
            continue
        rise = max(1500.0, 0.5 * mn)
        if state == "down":
            mn = min(mn, r[i])
            if r[i] > mn + rise:
                state, pk, pk_i = "up", r[i], i
        else:
            if r[i] > pk:
                pk, pk_i = r[i], i
            if r[i] < pk - max(1500.0, 0.5 * r[i]):
                cuts.append(pk_i)
                state, mn = "down", r[i]
    cuts.append(n)
    out = []
    for a, b in zip(cuts[:-1], cuts[1:]):
        seg = pts[a:b]
        if len(seg) < 2:
            continue
        cpa = ac.interpolated_cpa(site, seg)
        if cpa["slant_m"] <= rad:
            cpa["first_t"] = ac.iso(seg[0][0])
            cpa["last_t"] = ac.iso(seg[-1][0])
            cpa["n_points"] = len(seg)
            out.append(cpa)
    return out


def collect(meta, cache, rad):
    site = {k: meta[k] for k in ("lat", "lon")}
    site["elev_m"] = meta.get("elev_m", 0)
    site["geoid_m"] = meta.get("geoid_m", 0.0)
    unc = meta.get("start_uncertainty_s", 300)
    t0 = ts(meta["utc_start"]) - unc - 120
    t1 = ts(meta["utc_start"]) + meta["duration_s"] + unc + 120
    traces = []
    for day in days_between(t0 - 600, t1 + 600):
        if not os.path.exists(os.path.join(cache, day, "kept.bin")):
            raise SystemExit(f"day {day} not in the cache: run overhead-adsb fetch {day}")
        traces += load_traces(cache, day, site, t0, t1, rad)
    raw_all, diffs = [], []
    for e, data in traces:
        base = data.get("timestamp", 0)
        pts = []
        for p in data.get("trace") or []:
            t = base + p[0]
            if t < t0 - 300 or t > t1 + 300 or p[1] is None:
                continue
            h = ac.haversine_m(site["lat"], site["lon"], p[1], p[2])
            if h > rad * 1.5:
                continue
            pts.append((t, p[1], p[2], p[3], ac.geom_of(p), h, p[4], p[5]))
        if len(pts) < 2:
            continue
        diffs += [q[4] - q[3] for q in pts if ac.num(q[3]) and q[4] is not None and q[3] < 15000]
        raw_all.append((e, data, pts))
    site_off = statistics.median(diffs) if len(diffs) >= 20 else None
    passes, seen = [], set()
    for e, data, pts in raw_all:
        for cpa in passes_of(site, ac.with_agl(site, pts, site_off), rad):
            # a day can have several releases (prod-0, prod-1) that index the same aircraft twice
            key = (data.get("icao") or e["hex"], cpa["t"][:16])
            if key in seen:
                continue
            seen.add(key)
            passes.append({"hex": data.get("icao") or e["hex"], "reg": data.get("r"), "type": data.get("t"),
                           "desc": data.get("desc"), "class": e.get("class"), "cpa": cpa})
    passes.sort(key=lambda p: p["cpa"]["heard_t"])
    return site, site_off, passes, (t0, t1)


def pair(dets, passes, start, tol):
    """For each detection, passes heard within tol s of its tc at this start; returns (pairs, score)."""
    out, score = [], 0.0
    cands = []
    for i, d in enumerate(dets):
        tc = d["flyby"]["tc"]
        best = []
        for k, p in enumerate(passes):
            dt = tc - (ts(p["cpa"]["heard_t"]) - start)
            if abs(dt) <= tol:
                cost = abs(dt) + p["cpa"]["slant_m"] / 500.0
                best.append((cost, dt, p))
                cands.append((cost, i, k, dt))
        best.sort(key=lambda x: x[0])
        out.append((i, d, best))
    # one pass per detection and one detection per pass (greedy on cost): two detections of the same
    # fly-by (e.g. two harmonic families) count once
    used_d, used_p = {}, set()
    for cost, i, k, dt in sorted(cands):
        if i in used_d or k in used_p:
            continue
        used_d[i] = (k, dt)
        used_p.add(k)
        score += 1.0 + 1.0 / (1.0 + passes[k]["cpa"]["slant_m"] / 2000.0) - abs(dt) / (10.0 * tol)
    return out, score, used_d


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("meta")
    ap.add_argument("--cache", default=os.path.expanduser("~/overhead-data/adsb"))
    ap.add_argument("--radius-km", type=float, default=8.0)
    ap.add_argument("--detect")
    ap.add_argument("--start")
    ap.add_argument("--tol", type=float, default=20.0)
    ap.add_argument("--out")
    a = ap.parse_args()
    meta = json.load(open(a.meta))
    rad = a.radius_km * 1000
    site, site_off, passes, (t0, t1) = collect(meta, a.cache, rad)
    st = ts(meta["utc_start"])
    print(f"== {meta['id']}: {len(passes)} prop/rotor/unknown passes within {a.radius_km} km slant, "
          f"{ac.iso(t0)} .. {ac.iso(t1)}; site geom-baro offset {site_off} ft")
    for p in passes:
        c = p["cpa"]
        print(f"   {p['hex']} {p['reg'] or '-':8} {p['type'] or '-':5} {p['class']:7} CPA {c['t'][11:]} heard "
              f"{c['heard_t'][11:]} (file {ts(c['heard_t']) - st:7.1f} s at stated start) slant "
              f"{c['slant_m'] / 1000:5.2f} km horiz {c['horiz_m'] / 1000:5.2f} km agl {c['agl_m']:5d} m "
              f"gs {c['gs_pos_kt'] or c['gs_kt']} kt")
    result = {"meta": a.meta, "site": site, "window": [ac.iso(t0), ac.iso(t1)], "radius_km": a.radius_km,
              "geom_minus_baro_ft": site_off, "passes": passes}
    if a.detect:
        dets = json.load(open(a.detect))
        dets = dets if isinstance(dets, list) else dets.get("detections", [])
        if a.start:
            best_start = ts(a.start)
        else:
            unc = meta.get("start_uncertainty_s", 300)
            cands = [st + k for k in range(-int(unc), int(unc) + 1)]
            best_start = max(cands, key=lambda s: (pair(dets, passes, s, a.tol)[1], -abs(s - st)))
        pairs, score, assigned = pair(dets, passes, best_start, a.tol)
        n = len(assigned)
        print(f"== detections: {len(dets)}; start {ac.iso(best_start)} ({best_start - st:+.0f} s from stated): "
              f"{n} detections paired one-to-one with distinct passes heard within {a.tol:.0f} s")
        rows = []
        for i, d, best in pairs:
            f = d["flyby"]
            line = (f"   det {i:2d} tc {f['tc']:7.1f} f0 {f['f0']:6.1f} v {f['v']:5.1f} d {f['d']:6.0f} "
                    f"{'conf' if d.get('confident') else 'unc '}")
            m = []
            for _, dt, p in best[:3]:
                c = p["cpa"]
                m.append(f"{p['reg'] or p['hex']} {p['type'] or '-'} dt {dt:+.0f} s slant {c['slant_m'] / 1000:.2f} km "
                         f"gs {c['gs_pos_kt'] or c['gs_kt']} kt")
            asg = None
            if i in assigned:
                k, dt = assigned[i]
                p = passes[k]
                asg = {"hex": p["hex"], "reg": p["reg"], "type": p["type"], "class": p["class"], "dt_s": round(dt, 1),
                       "slant_m": p["cpa"]["slant_m"], "horiz_m": p["cpa"]["horiz_m"], "agl_m": p["cpa"]["agl_m"],
                       "gs_kt": p["cpa"]["gs_pos_kt"] or p["cpa"]["gs_kt"], "heard_t": p["cpa"]["heard_t"]}
                line += f" => {p['reg'] or p['hex']} {p['type'] or '-'} dt {dt:+.0f} s |"
            print(line + " | " + ("; ".join(m) if m else "-"))
            rows.append({"det": i, "tc": f["tc"], "f0": f["f0"], "v": f["v"], "d": f["d"],
                         "confident": d.get("confident"), "assigned": asg,
                         "passes": [{"hex": p["hex"], "reg": p["reg"], "type": p["type"], "dt_s": round(dt, 1),
                                     "slant_m": p["cpa"]["slant_m"], "gs_kt": p["cpa"]["gs_pos_kt"] or p["cpa"]["gs_kt"]}
                                    for _, dt, p in best[:3]]})
        result["pairing"] = {"start": ac.iso(best_start), "offset_s": round(best_start - st), "tol_s": a.tol,
                             "matched": n, "detections": rows}
    if a.out:
        json.dump(result, open(a.out, "w"), indent=1)


if __name__ == "__main__":
    main()
