"""Build docs/testset.md from the catalog, detector output and ADS-B hits.

usage: python3 -I make_doc.py ROOT ADSB_DIR OUT.md

ROOT/<id>/meta.json and ROOT/<id>/detect.json come from catalog.py and
run_detect.py; ADSB_DIR/hits_<day>.json from adsb_check.py.
"""

import glob
import json
import os
import sys
from datetime import datetime, timedelta, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from catalog import ENTRIES, FINDINGS, REJECTED  # noqa: E402
from thumbs import WINDOWS  # noqa: E402

LOW_SLANT_M = 6000  # aircraft further than this are listed only in the JSON


def parse(t):
    return datetime.fromisoformat(t.replace("Z", "+00:00"))


def fmt_s(x):
    m, s = divmod(x, 60)
    return f"{int(m)}:{s:04.1f}"


def osm(lat, lon):
    return f"https://www.openstreetmap.org/?mlat={lat:.5f}&mlon={lon:.5f}#map=15/{lat:.5f}/{lon:.5f}"


def gmaps(lat, lon):
    return f"https://www.google.com/maps?q={lat:.6f},{lon:.6f}"


def load_hits(adsb_dir, rid):
    out, releases = {}, set()
    for path in sorted(glob.glob(os.path.join(adsb_dir, "hits_*.json"))):
        with open(path) as f:
            d = json.load(f)
        for h in d["hits"].get(rid, []):
            releases.add(d["release"])
            key = h["hex"]
            if key not in out or h["cpa"]["slant_m"] < out[key]["cpa"]["slant_m"]:
                out[key] = h
    return sorted(out.values(), key=lambda h: h["cpa"]["slant_m"]), sorted(releases)


def is_match(h, match):
    return bool((match.get("reg") and h.get("reg") == match["reg"]) or
                (match.get("hex") and h["hex"] == match["hex"]))


def ac_name(h):
    bits = [h.get("reg") or h["hex"], h.get("type") or "?"]
    return " ".join(bits)


def main():
    root, adsb_dir, out_md = sys.argv[1:4]
    lines = []
    w = lines.append
    w("# Real-recording test set")
    w("")
    w("Recordings with a known place, date and time in which a propeller aircraft or helicopter")
    w("can be heard, for checking the pipeline by hand. All fall inside adsb.lol's history")
    w("(globe_history, February 2023 onwards). The audio is not in the repo: each recording lives in")
    w("`~/overhead-data/testset/<id>/` with `meta.json` (source, licence, true location, local and UTC")
    w("start, uncertainty, notes), `detect.json` (output of `overhead-cli detect`) and the downloaded audio.")
    w("ADS-B matches are in `~/overhead-data/testset/adsb/hits_<day>.json`.")
    w("")
    w("How it was built and how to redo it: `scripts/testset/` (see the end of this page).")
    w("")
    # hand-written section of track wf3/longrec, kept from the existing page (not generated here)
    long_marker = "\n## Long recordings with several fly-bys\n"
    long_tail = ""
    if os.path.exists(out_md):
        old_md = open(out_md).read()
        if long_marker in old_md:
            long_tail = old_md[old_md.index(long_marker):]
    if long_tail:
        w("Three long recordings (1–2 h) with 3–5 ADS-B-verified fly-bys each were added later: see "
          "[Long recordings with several fly-bys](#long-recordings-with-several-fly-bys) at the end.")
        w("")
    w("**Times.** `local start` is what the source states; `UTC start` is converted with the site's")
    w("time zone. The ± is our judgement of how loose the stated time is. `tc` is seconds into the file.")
    w("\"Heard\" times from ADS-B are the closest point of approach (CPA, interpolated between trace")
    w("points) plus slant distance / 343 m/s, i.e. when the sound from the CPA reaches the microphone.")
    w("")
    w("**Altitudes.** Heights above the site use the aircraft's geometric (GNSS) altitude where it")
    w("broadcasts one, converted from the WGS84 ellipsoid to mean sea level with the EGM96 geoid at the")
    w("site; otherwise its barometric altitude corrected by its own or the local median (geom − baro)")
    w("offset, a QNH estimate. The `(from)` column says which: geom, trace, site, baro (uncorrected) or")
    w("ground. Raw pressure altitude is off by ~27 ft per hPa of QNH, often a few hundred feet.")
    w("")
    w("**ADS-B matches.** `adsb_match` in meta.json is set only where one aircraft fits time, distance")
    w("and sound type without stretching the stated start time much. Weaker ideas are listed as")
    w("`adsb_candidates` with a verdict (ambiguous / speculative / rejected) and must not be used as truth.")
    w("")

    rows = []
    sections = []
    cmp_rows = []
    for i, e in enumerate(ENTRIES, 1):
        d = os.path.join(root, e["id"])
        with open(os.path.join(d, "meta.json")) as f:
            meta = json.load(f)
        dets = []
        if os.path.exists(os.path.join(d, "detect.json")):
            with open(os.path.join(d, "detect.json")) as f:
                dets = json.load(f)
        start = parse(meta["utc_start"])
        dur = meta.get("duration_s") or 0
        hits, releases = load_hits(adsb_dir, e["id"])
        low = []
        for h in hits:
            c = h["cpa"]
            if c["slant_m"] > LOW_SLANT_M:
                continue
            off = (parse(c["heard_t"]) - start).total_seconds()
            low.append((h, off))
        match = e.get("adsb_match")
        cands = e.get("adsb_candidates") or []
        match_txt = "–"
        if match:
            mh = next((h for h, _ in low if is_match(h, match)), None)
            if mh:
                c = mh["cpa"]
                off = (parse(c["heard_t"]) - start).total_seconds()
                match_txt = (f"{ac_name(mh)}, CPA {c['slant_m'] / 1000:.1f} km, heard at {fmt_s(off)} in file")
                if match.get("caveat"):
                    match_txt += f" ({match['caveat']})"
            else:
                match_txt = match.get("summary", "–")
        elif cands:
            match_txt = "no defensible match (" + "; ".join(
                f"{c.get('reg') or c.get('hex')}: {c['verdict']}" for c in cands) + ")"
        elif not releases:
            match_txt = "not checked"
        elif not hits:
            match_txt = "no aircraft at all within 20 km (no coverage)"
        elif not low:
            match_txt = f"none: no aircraft within {LOW_SLANT_M / 1000:.0f} km slant"
        else:
            match_txt = "no convincing match (see section)"
        conf = [x for x in dets if x["confident"]]
        det_txt = f"{len(dets)} ({len(conf)} confident)"
        rows.append(f"| {i} | [{e['id']}](#{i}-{e['id']}) | {e.get('short', e['place'].split(',')[0])}, "
                    f"{e['country']} | {e['local_start'].replace('T', ' ')[:16]} {meta['utc_offset'][:3]} | "
                    f"{meta['utc_start'][:16].replace('T', ' ')} ±{e['start_uncertainty_s'] // 60} min | "
                    f"{fmt_s(dur)} | {det_txt} | {match_txt} |")

        s = []
        a = s.append
        a(f"### {i}. {e['id']}")
        a("")
        a(f"**{e['title']}**")
        a("")
        a(f"- Source: <{e['source_url']}> ({e['source']})" + (f", mirror <{e['archive_url']}>" if e.get("archive_url") else ""))
        a(f"- Recordist: {e['recordist']}. Licence: [{e['licence']}]({e['licence_url']})")
        a(f"- **True location**: {e['lat']:.5f}, {e['lon']:.5f} ({e['place']}; {e['region']}; {e['country']}), "
          f"elevation ~{e['elev_m']} m, uncertainty ~{e['location_uncertainty_m']} m. "
          f"[OpenStreetMap]({osm(e['lat'], e['lon'])}) · "
          f"[Google Maps]({gmaps(e['lat'], e['lon'])}). {e.get('location_note', '')}")
        a(f"- **Date/time**: local start {e['local_start'].replace('T', ' ')} ({e['timezone']}, UTC{meta['utc_offset']}) "
          f"= **{meta['utc_start'].replace('T', ' ')}**, uncertainty ±{e['start_uncertainty_s'] // 60} min. "
          f"{e['time_note']}")
        a(f"- Length: {fmt_s(dur)} ({dur:.0f} s). File: `~/overhead-data/testset/{e['id']}/{meta['audio_file']}`")
        a(f"- Audible: {e['audible']}")
        if e.get("notes"):
            a(f"- Notes: {e['notes']}")
        a("")
        img = f"testset/{e['id']}.jpg"
        a(f"![{e['id']} spectrogram around the fly-by]({img})")
        a("")
        if e["id"] in WINDOWS:
            t0, t1, fmax = WINDOWS[e["id"]]
            a(f"*Whitened spectrogram, {fmt_s(t0)}–{fmt_s(t1)} of the file, 0–{fmax} Hz; grid every 10 s and "
              f"100 Hz; thin line = detector fit (lowest line only) when one falls in the window.*")
            a("")
        a("Detector (`overhead-cli detect`, defaults):")
        a("")
        if dets:
            a("| tc (file) | heard (UTC) | f0 Hz | v m/s [range] | d m [range] | lines | contrast | flags |")
            a("|---|---|---|---|---|---|---|---|")
            for x in dets:
                fb, r = x["flyby"], x["ranges"]
                heard = (start + timedelta(seconds=fb["tc"])).strftime("%H:%M:%S")
                flags = ", ".join(f for f, on in [("confident", x["confident"]),
                                                  ("ground?", x["likely_ground_vehicle"])] if on) or "unconfident"
                a(f"| {fmt_s(fb['tc'])} [{fmt_s(r['tc'][0])}–{fmt_s(r['tc'][1])}] | {heard} | {fb['f0']:.1f} | "
                  f"{fb['v']:.0f} [{r['v'][0]:.0f}–{r['v'][1]:.0f}] | {fb['d']:.0f} [{r['d'][0]:.0f}–{r['d'][1]:.0f}] | "
                  f"{len(x['harmonics'])} | {x['contrast']:.1f} | {flags} |")
        else:
            a("No detections.")
        a("")
        if releases:
            a(f"ADS-B ({', '.join(releases)}), aircraft whose interpolated CPA was within "
              f"{LOW_SLANT_M / 1000:.0f} km slant of the site during the window "
              f"(start −{e['start_uncertainty_s'] // 60 + 5} min … end +{e['start_uncertainty_s'] // 60 + 5} min); "
              f"{len(hits)} aircraft were within 20 km in total:")
            a("")
            if low:
                a("| aircraft | hex | CPA (UTC) | heard at (file) | slant km | horiz km | baro ft | geom ft | "
                  "above site m (from) | gs kt reported / flown |")
                a("|---|---|---|---|---|---|---|---|---|---|")
                for h, off in low[:12]:
                    c = h["cpa"]
                    a(f"| {ac_name(h)} | {h['hex']} | {c['t'][11:19]} | {fmt_s(off) if off >= 0 else '−' + fmt_s(-off)} | "
                      f"{c['slant_m'] / 1000:.1f} | {c['horiz_m'] / 1000:.1f} | {c['alt_ft']} | "
                      f"{c.get('geom_ft') if c.get('geom_ft') is not None else '–'} | "
                      f"{c['agl_m']} ({c.get('alt_ref', 'baro')}) | {c['gs_kt']} / {c.get('gs_pos_kt') or '–'} |")
            else:
                a("None.")
        else:
            a("ADS-B: not checked.")
        a("")
        for alt in e.get("alt_dates", []):
            ahits, arel = load_hits(adsb_dir, f"{e['id']}@{alt}")
            if not arel:
                continue
            ashift = datetime.fromisoformat(alt).date() - start.date()
            astart = start + ashift
            alow = [(h, (parse(h["cpa"]["heard_t"]) - astart).total_seconds()) for h in ahits
                    if h["cpa"]["slant_m"] <= LOW_SLANT_M]
            a(f"ADS-B on the alternative date {alt} ({', '.join(arel)}), same local time and window: "
              f"{len(ahits)} aircraft within 20 km, {len(alow)} within {LOW_SLANT_M / 1000:.0f} km slant"
              + (":" if alow else "."))
            a("")
            if alow:
                a("| aircraft | hex | CPA (UTC) | heard at (file) | slant km | above site m | gs kt |")
                a("|---|---|---|---|---|---|---|")
                for h, off in alow[:8]:
                    c = h["cpa"]
                    a(f"| {ac_name(h)} | {h['hex']} | {c['t'][11:19]} | "
                      f"{fmt_s(off) if off >= 0 else '−' + fmt_s(-off)} | {c['slant_m'] / 1000:.1f} | {c['agl_m']} | "
                      f"{c['gs_kt']} |")
                a("")
        for c in cands:
            a(f"**ADS-B candidate {c.get('reg') or c.get('hex')} ({c['verdict']}, not ground truth)**: {c['note']}")
            a("")
        if match:
            a(f"**Most likely source of the fly-by**: {match_txt}. {match.get('note', '')}")
            a("")
            mh = next((h for h, _ in low if is_match(h, match)), None)
            if mh is not None and match.get("det") is not None and match["det"] < len(dets):
                fb = dets[match["det"]]["flyby"]
                c = mh["cpa"]
                off = (parse(c["heard_t"]) - start).total_seconds()
                gs = (c.get("gs_pos_kt") or c["gs_kt"] or 0) * 0.5144
                cmp_rows.append(f"| {i} {e['id']} | {ac_name(mh)} | {fb['tc'] - off:+.0f} | {fb['v']:.0f} / {gs:.0f} | "
                                f"{fb['d']:.0f} / {c['slant_m']} |")
                a(f"Detector vs ADS-B for this fly-by: tc {fmt_s(fb['tc'])} vs heard-CPA {fmt_s(off)} "
                  f"(Δ {fb['tc'] - off:+.0f} s, which also absorbs the error of the stated start time); "
                  f"v {fb['v']:.0f} m/s vs ground speed {gs:.0f} m/s; d {fb['d']:.0f} m vs slant CPA "
                  f"{c['slant_m']} m.")
                a("")
        sections.append("\n".join(s))

    w("## Summary")
    w("")
    w("| # | id | place | local start | UTC start | length | detections | ADS-B: most likely source |")
    w("|---|---|---|---|---|---|---|---|")
    lines.extend(rows)
    w("")
    w("## Findings")
    w("")
    w(FINDINGS.strip())
    w("")
    w("Detector against the ADS-B match (where the detector found the fly-by). Δtc = detector tc minus")
    w("ADS-B heard-CPA time in the file, so it also contains the error of the stated start time.")
    w("ADS-B speed is the ground speed flown along the trace over ±30 s around the CPA (some reported")
    w("speeds are corrupt). It ignores wind; stage 1 assumes still air at 343 m/s.")
    w("")
    w("Only defensible matches (`adsb_match` in meta.json) are listed; candidates are not.")
    w("")
    w("| recording | aircraft | Δtc s | v det / ADS-B gs m/s | d det / ADS-B slant m |")
    w("|---|---|---|---|---|")
    lines.extend(cmp_rows)
    w("")
    w("## Recordings")
    w("")
    lines.append("\n\n".join(sections))
    w("")
    w("## Rejected candidates")
    w("")
    w("| candidate | why not |")
    w("|---|---|")
    for r in REJECTED:
        w(f"| [{r['name']}]({r['url']}) | {r['why']} |")
    w("")
    w("## How this was built")
    w("")
    w(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "README.md")).read().strip())
    w("")
    text = "\n".join(lines)
    if long_tail:
        text = text.rstrip("\n") + "\n" + long_tail
    with open(out_md, "w") as f:
        f.write(text)
    print("wrote", out_md)


if __name__ == "__main__":
    main()
