"""Spectrogram of the matched fly-bys with three curves overlaid.

usage: python3 -I figure.py ID RUN_JSON OUT_JPG [--adsb-shift S]

Panels: one per detection that the ADS-B truth or our #1 candidate uses.
  white dotted : stage-1 fit (straight line, f0/v/d/tc from the detection, c = 343 m/s)
  green        : ADS-B truth flight, real trace geometry seen from the TRUE location,
                 recording start = stated start (+ --adsb-shift), f0 from the detection, c = 340 m/s
  magenta dash : our #1 candidate's matched pass (straight-line approximation from the reported
                 speed, slant range, heard time and f0 scale, c = fitted c0)
"""
import os
import argparse
import json
import math
import subprocess
import tempfile
from datetime import datetime
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont
DATA = os.environ.get("OVERHEAD_DATA") or os.environ.get("DOPPLER_DATA") or os.path.expanduser("~/overhead-data")

DOPPLER = f"{DATA}/target-evidence/release/overhead-cli"
T = Path(f"{DATA}/testset")
W, H = 1000, 300
FT = 0.3048


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


def straight(t, f0, v, d, tc, c):
    tau_c = tc - d / c
    tau = t - d / c
    for _ in range(60):
        r = np.sqrt(d * d + (v * (tau - tau_c)) ** 2)
        tau = t - r / c
    x = v * (tau - tau_c)
    r = np.sqrt(d * d + x * x)
    rdot = v * x / r
    return f0 / (1 + rdot / c)


def adsb_curve(t, hit, m, start_unix, f0, c=340.0):
    pts = [p for p in hit["points"] if isinstance(p[3], (int, float))]
    tt = np.array([p[0] for p in pts])
    lat = np.array([p[1] for p in pts])
    lon = np.array([p[2] for p in pts])
    cpa = hit["cpa"]
    h = []
    for p in pts:
        if p[6] is not None:
            h.append(p[6] * FT - m.get("geoid_m", 0.0) - m.get("elev_m", 0))
        else:
            h.append((p[3] - cpa["alt_ft"]) * FT + cpa["agl_m"] if isinstance(cpa["alt_ft"], (int, float)) else cpa["agl_m"])
    h = np.array(h)
    R = 6371008.8
    e = np.radians(lon - m["lon"]) * math.cos(math.radians(m["lat"])) * R
    n = np.radians(lat - m["lat"]) * R
    # 1 s grid, Gaussian-smoothed (sigma 4 s): trace points are irregular and
    # position-quantised, raw linear interpolation gives a kinked range rate
    tg = np.arange(tt[0], tt[-1], 1.0)
    k = np.exp(-0.5 * (np.arange(-12, 13) / 4.0) ** 2)
    k /= k.sum()

    def smooth(x):
        g = np.interp(tg, tt, x)
        p = np.pad(g, 12, mode="edge")
        return np.convolve(p, k, mode="valid")

    es, ns, hs = smooth(e), smooth(n), smooth(h)

    def rng(tau):
        return np.sqrt(np.interp(tau, tg, es) ** 2 + np.interp(tau, tg, ns) ** 2 + (np.interp(tau, tg, hs) - 1.5) ** 2)

    ta = start_unix + t
    tau = ta - 3000 / c
    for _ in range(40):
        tau = ta - rng(tau) / c
    rdot = (rng(tau + 0.5) - rng(tau - 0.5)) / 1.0
    f = f0 / (1 + rdot / c)
    f[(tau < tt[0]) | (tau > tt[-1])] = np.nan
    return f, rng(tau)


def hit_for(m):
    reg = (m.get("adsb_match") or {}).get("reg")
    if not reg:
        return None
    day = m["utc_start"][:10]
    for f in sorted((T / "adsb").glob("hits_*.json")):
        d = json.loads(f.read_text())
        for h in d["hits"].get(m["id"], []):
            if h.get("reg") == reg and "points" in h:
                return h
    return None


def draw_curve(dr, ts_, fs, t0, t1, fmin, fmax, colour, style, width=2):
    xs = (ts_ - t0) / (t1 - t0) * W
    ys = (fmax - fs) / (fmax - fmin) * H
    seg = []
    for i, (x, y) in enumerate(zip(xs, ys)):
        ok = np.isfinite(y) and 0 <= y < H
        on = style == "solid" or (style == "dash" and (i // 6) % 2 == 0) or (style == "dot" and i % 4 == 0)
        if ok and on:
            seg.append((x, y))
        else:
            if len(seg) > 1:
                dr.line(seg, fill=colour, width=width)
            elif len(seg) == 1 and style == "dot":
                dr.point(seg[0], fill=colour)
            seg = []
    if len(seg) > 1:
        dr.line(seg, fill=colour, width=width)


def panel(audio, group, m, hit, start_unix, ours_by_det, dur, label):
    """group: list of (index, detection) close in time; drawn in one window."""
    tcs = [d["flyby"]["tc"] for _, d in group]
    f0s = [d["flyby"]["f0"] for _, d in group]
    tcm = sum(tcs) / len(tcs)
    t0, t1 = max(0.0, min(tcs) - 70), min(dur, max(tcs) + 70)
    fams = []
    for _, d in group:
        harm = sorted(set([1.0] + [float(x) for x in d.get("harmonics", [1.0])]))
        fams.append(([k for k in harm if k <= 4] or [1.0]) if len(group) == 1 else [1.0])
    fmin = max(10.0, 0.75 * min(f0s))
    fmax = min(1200.0, 1.3 * max(f * max(h) for f, h in zip(f0s, fams)))
    with tempfile.TemporaryDirectory() as td:
        png = Path(td) / "s.png"
        subprocess.run([DOPPLER, "spec", str(audio), "--t0", f"{t0}", "--t1", f"{t1}", "--fmin", f"{fmin}",
                        "--fmax", f"{fmax}", "--whiten", "--width", str(W), "--height", str(H), "--grid-s", "10",
                        "--grid-hz", str(50 if fmax - fmin < 400 else 100), "-o", str(png)],
                       check=True, capture_output=True)
        img = Image.open(png).convert("RGB")
    dr = ImageDraw.Draw(img)
    tt = np.linspace(t0, t1, W)
    shift = None
    if hit is not None:
        heard = ts(hit["cpa"]["heard_t"]) - start_unix
        shift = heard - tcm  # start correction that puts the ADS-B heard CPA on the detection(s)
    for (i, det), fam in zip(group, fams):
        f0, tc = det["flyby"]["f0"], det["flyby"]["tc"]
        ours = ours_by_det.get(i)
        for k in fam:
            draw_curve(dr, tt, straight(tt, k * f0, det["flyby"]["v"], det["flyby"]["d"], tc, 343.0), t0, t1, fmin, fmax,
                       (255, 255, 255), "dot", 1)
            if hit is not None:
                fa, _ = adsb_curve(tt, hit, m, start_unix, k * f0)
                draw_curve(dr, tt, fa, t0, t1, fmin, fmax, (60, 230, 90), "solid", 2)
                if abs(shift) > 2:
                    fa, _ = adsb_curve(tt, hit, m, start_unix + shift, k * f0)
                    draw_curve(dr, tt, fa, t0, t1, fmin, fmax, (0, 220, 255), "dash", 2)
            if ours is not None and ours.get("heard_rec") is not None and math.isfinite(ours.get("slant_m") or float("nan")):
                fo = straight(tt, k * f0 * ours["f0_scale"], ours["speed_ms"], ours["slant_m"], ours["heard_rec"], ours["c0"])
                draw_curve(dr, tt, fo, t0, t1, fmin, fmax, (255, 60, 220), "dash", 2)
    lines = [label + f"   file {t0:.0f}-{t1:.0f} s, {fmin:.0f}-{fmax:.0f} Hz"]
    if hit is not None:
        lines.append(f"green solid: ADS-B {hit['reg']} {hit.get('type', '')} seen from the true place, stated start; "
                     f"cyan dashes: same, start moved by {shift:+.0f} s")
    font = ImageFont.load_default(size=13)
    strip = Image.new("RGB", (W, 6 + 17 * len(lines)), (20, 20, 24))
    sd = ImageDraw.Draw(strip)
    for j, tx in enumerate(lines):
        sd.text((6, 3 + 17 * j), tx, fill=(230, 230, 230), font=font)
    out = Image.new("RGB", (W, H + strip.height))
    out.paste(strip, (0, 0))
    out.paste(img, (0, strip.height))
    return out


def panel_nodet(audio, m, hit, start_unix, f0s, dur):
    """No stage-1 detection: window around the ADS-B heard CPA (stated start), ADS-B curves for
    hand-picked rest frequencies f0s (there is no detection to take f0 from)."""
    heard = ts(hit["cpa"]["heard_t"]) - start_unix
    t0, t1 = max(0.0, heard - 75), min(dur, heard + 60)
    fmin, fmax = 0.6 * min(f0s), min(1200.0, 1.6 * max(f0s))
    with tempfile.TemporaryDirectory() as td:
        png = Path(td) / "s.png"
        subprocess.run([DOPPLER, "spec", str(audio), "--t0", f"{t0}", "--t1", f"{t1}", "--fmin", f"{fmin}",
                        "--fmax", f"{fmax}", "--whiten", "--width", str(W), "--height", str(H), "--grid-s", "10",
                        "--grid-hz", "50", "-o", str(png)], check=True, capture_output=True)
        img = Image.open(png).convert("RGB")
    dr = ImageDraw.Draw(img)
    tt = np.linspace(t0, t1, W)
    for f0 in f0s:
        fa, _ = adsb_curve(tt, hit, m, start_unix, f0)
        draw_curve(dr, tt, fa, t0, t1, fmin, fmax, (60, 230, 90), "dash", 2)
    x = (heard - t0) / (t1 - t0) * W
    dr.line([(x, 0), (x, H)], fill=(255, 255, 255), width=1)
    lines = [f"no stage-1 detection   file {t0:.0f}-{t1:.0f} s, {fmin:.0f}-{fmax:.0f} Hz   white line: ADS-B heard CPA at {heard:.0f} s (stated start)",
             f"green dashes: ADS-B {hit['reg']} {hit.get('type', '')} seen from the true place, stated start, rest frequency "
             + ", ".join(f"{f:.0f}" for f in f0s) + " Hz (picked by hand: no detection gives f0)"]
    font = ImageFont.load_default(size=13)
    strip = Image.new("RGB", (W, 6 + 17 * len(lines)), (20, 20, 24))
    sd = ImageDraw.Draw(strip)
    for j, tx in enumerate(lines):
        sd.text((6, 3 + 17 * j), tx, fill=(230, 230, 230), font=font)
    out = Image.new("RGB", (W, H + strip.height))
    out.paste(strip, (0, 0))
    out.paste(img, (0, strip.height))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("id")
    ap.add_argument("run")
    ap.add_argument("out")
    ap.add_argument("--f0", type=float, nargs="*", help="rest frequencies for a recording with no detection")
    a = ap.parse_args()
    m = json.loads((T / a.id / "meta.json").read_text())
    audio = T / a.id / m["audio_file"]
    run = json.loads(Path(a.run).read_text()) if Path(a.run).exists() else None
    det_file = Path(f"{DATA}/evidence/detect") / f"{a.id}.json"
    dets = json.loads(det_file.read_text())
    hit = hit_for(m)
    start_unix = ts(m["utc_start"])
    if not dets:
        if hit is None or not a.f0:
            raise SystemExit("no detections (give --f0 to draw the ADS-B truth curve anyway)")
        panel_nodet(audio, m, hit, start_unix, a.f0, m["duration_s"]).save(a.out, quality=80)
        print(a.out)
        return
    top = run["candidates"][0] if run and run["candidates"] else None
    ours_by_det = {}
    if top:
        for f in top["flights"]:
            ours_by_det[f["detection"]] = dict(f, c0=top["c0"])
    # ADS-B truth detection: the detection nearest in time to the truth flight's heard CPA (stated start)
    truth_det = None
    if hit is not None:
        heard = ts(hit["cpa"]["heard_t"]) - start_unix
        truth_det = min(range(len(dets)), key=lambda i: abs(dets[i]["flyby"]["tc"] - heard))
    show = sorted(set(([truth_det] if truth_det is not None else []) + list(ours_by_det))) or [0]
    # detections within 30 s of a shown one share its panel
    groups = []
    for i in show:
        tc = dets[i]["flyby"]["tc"]
        near = [j for j in range(len(dets)) if abs(dets[j]["flyby"]["tc"] - tc) < 30]
        if not any(set(near) & set(g) for g in groups):
            groups.append(near)
    panels = []
    for g in groups:
        lab = " + ".join(f"det {i} tc {dets[i]['flyby']['tc']:.1f} s" for i in g) + " | white dots: stage-1 fit"
        ours = [ours_by_det[i] for i in g if i in ours_by_det]
        if ours:
            lab += " | magenta: our #1 = " + ", ".join(f"{o.get('reg') or o.get('callsign') or o['hex']} {o.get('type_code') or ''}".strip() for o in ours)
        h = hit if truth_det in g else None
        panels.append(panel(audio, [(i, dets[i]) for i in g], m, h, start_unix, ours_by_det, m["duration_s"], lab))
    img = Image.new("RGB", (W, sum(p.height for p in panels)))
    y = 0
    for p in panels:
        img.paste(p, (0, y))
        y += p.height
    img.save(a.out, quality=80)
    print(a.out, img.size)


if __name__ == "__main__":
    main()
