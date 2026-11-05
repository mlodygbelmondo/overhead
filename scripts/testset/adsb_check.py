"""Stream one adsb.lol globe_history day and list aircraft that passed close
to given recording sites during given time windows.

usage: python3 -I adsb_check.py SITES.json DAY [--radius-km 25] [--out FILE]
       python3 -I adsb_check.py --recompute HITS.json SITES.json [--out FILE]

SITES.json: [{"id": "xc941260", "lat": .., "lon": .., "elev_m": .., "geoid_m": ..,
              "t0": "2024-03-13T15:40:00Z", "t1": "2024-03-13T16:00:00Z"}, ...]
DAY: YYYY-MM-DD (UTC day of the archive; prod release, falls back to staging)

Nothing is stored on disk: the release parts are streamed through tarfile.
Per site, prints every aircraft with a trace point inside the radius and
window, with its closest point of approach (CPA).

Altitude: readsb trace points carry the barometric (pressure) altitude at
index 3 and, for ADS-B aircraft, the geometric (GNSS) altitude at index 10,
which readsb documents as height above the WGS84 ellipsoid. Pressure
altitude is not true altitude (it is off by ~27 ft per hPa of QNH away from
1013.25, often several hundred feet), so the height above the site is taken
from, in order of preference:
  geom      the point's geometric altitude;
  trace     baro + this aircraft's median (geom - baro) in the window;
  site      baro + the median (geom - baro) of all aircraft below 15 000 ft
            near the site in the window (a local QNH estimate);
  baro      raw pressure altitude (no geometric data at all).
Geometric heights are converted to height above mean sea level with the
site's geoid undulation `geoid_m` (EGM96; catalog.py has the values), then
the site elevation (MSL) is subtracted. 'ground' counts as 0 m AGL.

--recompute redoes the CPAs of an existing hits file for new site positions
(e.g. a corrected location) from the stored trace points, without streaming.
Only aircraft whose points were stored (CPA < 8 km) are kept.
"""

import argparse
import gzip
import io
import json
import math
import statistics
import subprocess
import sys
import tarfile
import urllib.request
from datetime import datetime, timezone

UA = "overhead research"


class Concat:
    """File-like object over the concatenated release parts.

    curl does the download (Python's SSL reads in tarfile-sized chunks are
    an order of magnitude slower); a large buffer sits in between.
    """

    def __init__(self, urls):
        print(f"  streaming {len(urls)} part(s) via curl", file=sys.stderr)
        self.proc = subprocess.Popen(["curl", "-sfL", *urls], stdout=subprocess.PIPE, bufsize=0)
        self.buf = io.BufferedReader(self.proc.stdout, buffer_size=16 << 20)
        self.read_total = 0

    def read(self, n=-1):
        out = self.buf.read(n)
        self.read_total += len(out)
        return out


def release_assets(day):
    y = day[:4]
    tag_day = day.replace("-", ".")
    for kind in ("prod", "staging"):
        tag = f"v{tag_day}-planes-readsb-{kind}-0"
        api = f"https://api.github.com/repos/adsblol/globe_history_{y}/releases/tags/{tag}"
        try:
            with urllib.request.urlopen(urllib.request.Request(api, headers={"User-Agent": UA}), timeout=60) as r:
                rel = json.load(r)
        except Exception:
            continue
        assets = sorted(a["browser_download_url"] for a in rel["assets"] if ".tar" in a["name"])
        if assets:
            return tag, assets
    raise SystemExit(f"no release for {day}")


def haversine_m(lat1, lon1, lat2, lon2):
    r = 6371000.0
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp, dl = p2 - p1, math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * r * math.asin(math.sqrt(a))


def enu(s, lat, lon, agl):
    """Local east/north/up metres relative to the site (flat-earth, fine within ~30 km)."""
    e = math.radians(lon - s["lon"]) * 6371000.0 * math.cos(math.radians(s["lat"]))
    n = math.radians(lat - s["lat"]) * 6371000.0
    return e, n, agl


def interpolated_cpa(s, pts):
    """Closest point of approach, linearly interpolating between trace points
    (traces can be sparse on straight legs). Also gives the time the sound
    emitted at CPA reaches the site (still air, 343 m/s)."""
    best = None
    xyz = [enu(s, q[1], q[2], q[8]) for q in pts]
    for i in range(len(pts)):
        cands = [(pts[i][0], xyz[i])]
        if i + 1 < len(pts) and pts[i + 1][0] - pts[i][0] <= 120:
            (t0, a), (t1, b) = (pts[i][0], xyz[i]), (pts[i + 1][0], xyz[i + 1])
            d = [b[k] - a[k] for k in range(3)]
            dd = sum(x * x for x in d)
            if dd > 0:
                u = max(0.0, min(1.0, -sum(a[k] * d[k] for k in range(3)) / dd))
                cands.append((t0 + u * (t1 - t0), tuple(a[k] + u * d[k] for k in range(3))))
        for t, p in cands:
            r = math.sqrt(p[0] ** 2 + p[1] ** 2 + p[2] ** 2)
            if best is None or r < best[1]:
                best = (t, r, p, i)
    t, r, p, i = best
    q = pts[i]
    # Ground speed from the flown path over +-30 s around the CPA: the reported speed of some trace points
    # is unreliable (e.g. a Robin DR400 alternating between ~110 kt and ~420 kt).
    near = [k for k in range(len(pts)) if abs(pts[k][0] - t) <= 30]
    gs_pos = None
    if len(near) >= 2 and pts[near[-1]][0] - pts[near[0]][0] >= 10:
        path = sum(haversine_m(pts[k][1], pts[k][2], pts[k + 1][1], pts[k + 1][2]) for k in near[:-1])
        gs_pos = round(path / (pts[near[-1]][0] - pts[near[0]][0]) / 0.514444, 1)
    return {
        "t": iso(t), "slant_m": round(r), "horiz_m": round(math.hypot(p[0], p[1])),
        "agl_m": round(p[2]), "alt_ft": q[3], "geom_ft": q[4], "alt_ref": q[9],
        "gs_kt": q[6], "gs_pos_kt": gs_pos, "track": q[7],
        "heard_t": iso(t + r / 343.0),
        "nearest_point_gap_s": round(min(abs(x[0] - t) for x in pts), 1),
    }


FT = 0.3048
SITE_QNH_MAX_FT = 15000


def num(x):
    return isinstance(x, (int, float)) and not isinstance(x, bool)


def geom_of(p):
    return p[10] if len(p) > 10 and num(p[10]) else None


def with_agl(s, raw, site_off_ft):
    """raw: (t, lat, lon, baro, geom, h, gs, track) -> + (agl_m, ref)."""
    diffs = [q[4] - q[3] for q in raw if num(q[3]) and q[4] is not None]
    trace_off = statistics.median(diffs) if len(diffs) >= 3 else None
    out = []
    for q in raw:
        baro, geom = q[3], q[4]
        if baro == "ground":
            agl, ref = 0.0, "ground"
        elif geom is not None:
            agl, ref = geom * FT - s.get("geoid_m", 0.0) - s.get("elev_m", 0), "geom"
        elif num(baro) and trace_off is not None:
            agl, ref = (baro + trace_off) * FT - s.get("geoid_m", 0.0) - s.get("elev_m", 0), "trace"
        elif num(baro) and site_off_ft is not None:
            agl, ref = (baro + site_off_ft) * FT - s.get("geoid_m", 0.0) - s.get("elev_m", 0), "site"
        elif num(baro):
            agl, ref = baro * FT - s.get("elev_m", 0), "baro"
        else:
            agl, ref = 0.0, "none"
        out.append(tuple(q) + (max(0.0, agl), ref))
    return out


def finish(sites, raw_hits, site_diffs):
    """Compute CPAs once the per-site QNH offset is known."""
    hits, qnh = {}, {}
    for s in sites:
        d = site_diffs.get(s["id"], [])
        off = statistics.median(d) if len(d) >= 20 else None
        qnh[s["id"]] = {"geom_minus_baro_ft": off, "samples": len(d)}
        out = []
        for meta, raw, inside in raw_hits.get(s["id"], []):
            pts = with_agl(s, raw, off)
            cpa = interpolated_cpa(s, pts)
            hit = dict(meta, first_in_radius=iso(inside[0]), last_in_radius=iso(inside[-1]), cpa=cpa)
            if cpa["slant_m"] < 8000:
                hit["points"] = [[round(q[0], 1), q[1], q[2], q[3], q[6], q[7], q[4]] for q in raw]
            out.append(hit)
        out.sort(key=lambda h: h["cpa"]["slant_m"])
        hits[s["id"]] = out
    return hits, qnh


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


def iso(t):
    return datetime.fromtimestamp(t, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def print_hits(hits, radius_km, qnh):
    for sid, hs in hits.items():
        q = qnh.get(sid, {})
        print(f"== {sid}: {len(hs)} aircraft within {radius_km} km; site geom-baro offset "
              f"{q.get('geom_minus_baro_ft')} ft from {q.get('samples')} points")
        for h in hs[:15]:
            c = h["cpa"]
            print(f"   {h['hex']} {h['reg'] or '-':8} {h['type'] or '-':5} CPA {c['t']} heard {c['heard_t'][11:]} "
                  f"slant {c['slant_m'] / 1000:5.1f} km horiz {c['horiz_m'] / 1000:5.1f} km alt {c['alt_ft']} ft "
                  f"geom {c['geom_ft']} ft ({c['alt_ref']}) gs {c['gs_kt']} kt")


def recompute(hits_path, sites_path, out):
    """New CPAs for (possibly moved) sites from the trace points stored in a hits file."""
    with open(hits_path) as f:
        old = json.load(f)
    with open(sites_path) as f:
        sites = json.load(f)
    raw_hits, site_diffs = {}, {}
    for s in sites:
        src = old["hits"].get(s["id"], [])
        lst, far = [], []
        for h in src:
            if "points" not in h:
                far.append(h)  # no stored points (CPA >= 8 km): kept unchanged
                continue
            raw = []
            for p in h["points"]:
                geom = p[6] if len(p) > 6 and num(p[6]) else None
                raw.append((p[0], p[1], p[2], p[3], geom, haversine_m(s["lat"], s["lon"], p[1], p[2]), p[4], p[5]))
            inside = [ts(h["first_in_radius"]), ts(h["last_in_radius"])]
            meta = {k: h.get(k) for k in ("hex", "reg", "type", "desc")}
            lst.append((meta, raw, inside))
        raw_hits[s["id"]] = lst
        s["_far"] = far
        q = old.get("qnh", {}).get(s["id"], {})
        if q.get("geom_minus_baro_ft") is not None:
            site_diffs[s["id"]] = [q["geom_minus_baro_ft"]] * max(20, q.get("samples", 0))
    hits, qnh = finish(sites, raw_hits, site_diffs)
    for s in sites:
        hits[s["id"]] += s.pop("_far")
    result = {"release": old["release"], "radius_km": old["radius_km"], "sites": sites, "hits": hits, "qnh": qnh,
              "recomputed_from": hits_path}
    if out:
        with open(out, "w") as f:
            json.dump(result, f, indent=1)
    print_hits(hits, old["radius_km"], qnh)


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--recompute":
        ap = argparse.ArgumentParser()
        ap.add_argument("--recompute", dest="hits")
        ap.add_argument("sites")
        ap.add_argument("--out")
        a = ap.parse_args()
        return recompute(a.hits, a.sites, a.out)
    ap = argparse.ArgumentParser()
    ap.add_argument("sites")
    ap.add_argument("day")
    ap.add_argument("--radius-km", type=float, default=25)
    ap.add_argument("--out")
    a = ap.parse_args()
    with open(a.sites) as f:
        sites = json.load(f)
    for s in sites:
        s["_t0"], s["_t1"] = ts(s["t0"]), ts(s["t1"])
    tag, urls = release_assets(a.day)
    print(f"{tag}: {len(urls)} part(s)", file=sys.stderr)
    rad = a.radius_km * 1000
    lat_pad = a.radius_km / 111.0
    raw_hits = {s["id"]: [] for s in sites}
    site_diffs = {s["id"]: [] for s in sites}
    n_traces = 0
    stream = Concat(urls)
    with tarfile.open(fileobj=stream, mode="r|") as tar:
        for m in tar:
            if not m.isfile() or "/traces/" not in "/" + m.name.lstrip("./") or "trace_full_" not in m.name:
                continue
            raw = tar.extractfile(m).read()
            try:
                data = json.loads(gzip.decompress(raw) if raw[:2] == b"\x1f\x8b" else raw)
            except Exception:
                continue
            n_traces += 1
            if n_traces % 20000 == 0:
                print(f"  {n_traces} traces, {stream.read_total / 1e9:.2f} GB", file=sys.stderr)
            base = data.get("timestamp", 0)
            trace = data.get("trace") or []
            if not trace:
                continue
            for s in sites:
                if base + trace[-1][0] < s["_t0"] or base + trace[0][0] > s["_t1"]:
                    continue
                pts = []
                for p in trace:
                    t = base + p[0]
                    if t < s["_t0"] - 120 or t > s["_t1"] + 120:
                        continue
                    lat, lon = p[1], p[2]
                    if lat is None or abs(lat - s["lat"]) > lat_pad * 1.5:
                        continue
                    h = haversine_m(s["lat"], s["lon"], lat, lon)
                    if h > rad * 1.5:
                        continue
                    pts.append((t, lat, lon, p[3], geom_of(p), h, p[4], p[5]))
                inside = [q[0] for q in pts if s["_t0"] <= q[0] <= s["_t1"] and q[5] <= rad]
                if not inside:
                    continue
                site_diffs[s["id"]] += [q[4] - q[3] for q in pts if num(q[3]) and q[4] is not None
                                        and q[3] < SITE_QNH_MAX_FT and q[5] <= rad]
                meta = {"hex": data.get("icao"), "reg": data.get("r"), "type": data.get("t"), "desc": data.get("desc")}
                raw_hits[s["id"]].append((meta, pts, inside))
    print(f"done: {n_traces} traces", file=sys.stderr)
    for s in sites:
        s.pop("_t0"), s.pop("_t1")
    hits, qnh = finish(sites, raw_hits, site_diffs)
    result = {"release": tag, "radius_km": a.radius_km, "sites": sites, "hits": hits, "qnh": qnh}
    if a.out:
        with open(a.out, "w") as f:
            json.dump(result, f, indent=1)
    print_hits(hits, a.radius_km, qnh)


if __name__ == "__main__":
    main()
