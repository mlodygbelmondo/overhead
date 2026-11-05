"""Download Freesound sounds (HQ preview, no login needed) with metadata.

usage: python3 -I fs_get.py OUTROOT USER/ID [USER/ID ...]

For each sound writes OUTROOT/fs<ID>/:
  fs.json     title, description, geotag, licence, duration, preview URL
  audio.mp3   the HQ preview (128 kbit/s MP3; originals need a login)
"""

import html
import json
import os
import re
import sys
import urllib.request

UA = "Mozilla/5.0 (X11; Linux x86_64) overhead research"


def get(url, binary=False):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=120) as r:
        data = r.read()
    return data if binary else data.decode("utf-8", "replace")


def text(fragment):
    t = re.sub(r"<br\s*/?>", "\n", fragment)
    t = re.sub(r"</p>", "\n", t)
    t = html.unescape(re.sub(r"<[^>]+>", " ", t))
    return re.sub(r"[ \t]+", " ", t).strip()


def main():
    root = sys.argv[1]
    for spec in sys.argv[2:]:
        user, sid = spec.split("/")
        url = f"https://freesound.org/people/{user}/sounds/{sid}/"
        page = get(url)
        info = {"id": int(sid), "user": user, "url": url}
        m = re.search(r'<div[^>]*id="soundDescriptionSection".*?</div>', page, re.S)
        info["description"] = text(m.group(0)) if m else ""
        m = re.search(r"<h1[^>]*>(.*?)</h1>", page, re.S)
        info["title"] = text(m.group(1)) if m else ""
        m = re.search(r'creativecommons.org/(licenses|publicdomain)/[^"\s]+', page)
        info["licence_url"] = "https://" + m.group(0) if m else None
        m = re.search(r'duration="([\d.]+)"', page)
        info["duration_s"] = float(m.group(1)) if m else None
        m = re.search(r'title="Geotag: ([^"]+)"', page)
        info["geotag_name"] = html.unescape(m.group(1)) if m else None
        try:
            g = get(url + "geotag/?ajax=1")
            la = re.search(r'data-map-center-lat="([-\d.]+)"', g)
            lo = re.search(r'data-map-center-lon="([-\d.]+)"', g)
            if la and lo:
                info["lat"], info["lon"] = float(la.group(1)), float(lo.group(1))
        except Exception as e:  # noqa: BLE001
            info["geotag_error"] = str(e)
        m = re.search(r"https://cdn.freesound.org/previews/[^\"']+-hq\.mp3", page)
        info["preview_url"] = m.group(0) if m else None
        d = os.path.join(root, f"fs{sid}")
        os.makedirs(d, exist_ok=True)
        if info["preview_url"] and not os.path.exists(os.path.join(d, "audio.mp3")):
            with open(os.path.join(d, "audio.mp3"), "wb") as f:
                f.write(get(info["preview_url"], binary=True))
        info["audio_file"] = "audio.mp3"
        with open(os.path.join(d, "fs.json"), "w") as f:
            json.dump(info, f, indent=2, ensure_ascii=False)
        print(sid, info.get("lat"), info.get("lon"), info.get("duration_s"), info["title"][:80])


if __name__ == "__main__":
    main()
