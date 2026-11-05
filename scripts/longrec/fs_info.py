"""Freesound sound info: title, duration, map pin (from the geotag modal), licence, full description.

usage: python3 -I fs_info.py CACHEDIR USER/ID [USER/ID...]   (prints one JSON line per sound)
"""
import json
import os
import re
import sys
import time
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "testset"))
import fs_search  # noqa: E402

UA = "Mozilla/5.0 (X11; Linux x86_64) overhead research"


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read().decode("utf-8", "replace")


def cached(path, url):
    if os.path.exists(path):
        return open(path).read()
    page = get(url)
    open(path, "w").write(page)
    time.sleep(0.5)
    return page


def main():
    cache = sys.argv[1]
    os.makedirs(cache, exist_ok=True)
    for arg in sys.argv[2:]:
        user, sid = arg.split("/")
        base = f"https://freesound.org/people/{user}/sounds/{sid}/"
        page = cached(os.path.join(cache, f"fs_{sid}.html"), base)
        rec = fs_search.parse(page, user, sid)
        geo = cached(os.path.join(cache, f"fs_{sid}_geo.html"), base + "geotag/?ajax=1")
        m1 = re.search(r'data-map-center-lat="([-\d.]+)"', geo)
        m2 = re.search(r'data-map-center-lon="([-\d.]+)"', geo)
        if m1 and m2:
            rec["lat"], rec["lon"] = float(m1.group(1)), float(m2.group(1))
        m = re.search(r'"(https://cdn\.freesound\.org/previews/[^"]+-hq\.mp3)"', page)
        rec["preview_hq"] = m.group(1) if m else None
        print(json.dumps(rec, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
