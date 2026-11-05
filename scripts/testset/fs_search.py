"""Search Freesound (public web search, no API key) for geotagged sounds
uploaded since 2023-03 and print the ones whose description mentions a
recording date and time in adsb.lol's coverage period.

usage: python3 -I fs_search.py OUTDIR 'query one' 'query two' ... > hits.jsonl

Sound pages are cached in OUTDIR/fs_<id>.html.
"""

import html
import json
import os
import re
import sys
import time
import urllib.parse
import urllib.request

UA = "Mozilla/5.0 (X11; Linux x86_64) overhead research"
FILTER = "is_geotagged:1 created:[2023-03-01T00:00:00Z TO NOW]"


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    for attempt in range(4):
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                return r.read().decode("utf-8", "replace")
        except Exception:
            time.sleep(2 + 3 * attempt)
    return ""


def search(query, pages=5):
    ids = []
    for pg in range(1, pages + 1):
        q = urllib.parse.urlencode({"q": query, "f": FILTER, "page": pg})
        page = get("https://freesound.org/search/?" + q)
        found = re.findall(r'href="/people/([^/"]+)/sounds/(\d+)/"', page)
        new = [f for f in dict.fromkeys(found) if f not in ids]
        if not new:
            break
        ids += new
    return ids


def text(fragment):
    t = re.sub(r"<br\s*/?>", "\n", fragment)
    t = re.sub(r"</p>", "\n", t)
    return html.unescape(re.sub(r"<[^>]+>", " ", t))


def parse(page, user, sid):
    rec = {"id": int(sid), "user": user, "url": f"https://freesound.org/people/{user}/sounds/{sid}/"}
    m = re.search(r'<div[^>]*id="soundDescriptionSection".*?</div>', page, re.S)
    rec["description"] = re.sub(r"[ \t]+", " ", text(m.group(0))).strip() if m else ""
    m = re.search(r'<h1[^>]*>(.*?)</h1>', page, re.S)
    rec["title"] = text(m.group(1)).strip() if m else ""
    m = re.search(r'data-lat="([-\d.]+)"[^>]*data-lon="([-\d.]+)"', page) or re.search(
        r'data-lon="([-\d.]+)"[^>]*data-lat="([-\d.]+)"', page)
    if m:
        rec["lat"], rec["lon"] = float(m.group(1)), float(m.group(2))
    m = re.search(r'title="Geotag: ([^"]+)"', page)
    rec["geotag_name"] = html.unescape(m.group(1)) if m else None
    m = re.search(r'creativecommons.org/(licenses|publicdomain)/[^"\s]+', page)
    rec["licence"] = "https://" + m.group(0) if m else None
    m = re.search(r'(\d+):(\d\d)\.(\d+)</', page)
    rec["duration_hint"] = m.group(0)[:-2] if m else None
    return rec


DATE_RE = re.compile(r"(20(2[3-6]))[-./](\d{1,2})[-./](\d{1,2})|"
                     r"(\d{1,2})[-./](\d{1,2})[-./](20(2[3-6]))|"
                     r"(january|february|march|april|may|june|july|august|september|october|november|december)"
                     r"\s+\d{1,2}(st|nd|rd|th)?,?\s+20(2[3-6])|"
                     r"\d{1,2}(st|nd|rd|th)?\s+(january|february|march|april|may|june|july|august|september|"
                     r"october|november|december),?\s+20(2[3-6])", re.I)
TIME_RE = re.compile(r"\b([01]?\d|2[0-3])[:.h]([0-5]\d)\b\s*(am|pm|AM|PM|h|uur|Uhr)?")


def main():
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    seen = set()
    for query in sys.argv[2:]:
        ids = search(query)
        print(f"{query}: {len(ids)}", file=sys.stderr)
        for user, sid in ids:
            if sid in seen:
                continue
            seen.add(sid)
            cache = os.path.join(out, f"fs_{sid}.html")
            if os.path.exists(cache):
                with open(cache) as f:
                    page = f.read()
            else:
                page = get(f"https://freesound.org/people/{user}/sounds/{sid}/")
                with open(cache, "w") as f:
                    f.write(page)
                time.sleep(0.5)
            rec = parse(page, user, sid)
            rec["query"] = query
            desc = rec["title"] + "\n" + rec["description"]
            rec["dates"] = [m.group(0) for m in DATE_RE.finditer(desc)]
            rec["times"] = [m.group(0) for m in TIME_RE.finditer(desc)][:5]
            print(json.dumps(rec, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
