"""Download xeno-canto recordings with their metadata.

usage: python3 -I xc_get.py OUTROOT XC_ID [XC_ID ...]

For each id writes OUTROOT/xc<ID>/:
  xc.json      basic data scraped from the recording page
  audio.<ext>  the original uploaded file (via /<id>/download)
"""

import html
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import xc_fetch  # noqa: E402

FIELDS = [
    "Recordist", "Date", "Time", "Latitude", "Longitude", "Location", "Country",
    "Elevation", "Uploaded", "File type", "Length", "Sampling rate", "Channels",
    "Device", "Microphone", "Automatic recording",
]


def text(page):
    t = re.sub(r"<script.*?</script>", " ", page, flags=re.S)
    t = re.sub(r"<[^>]+>", "\n", t)
    t = html.unescape(t)
    return [ln.strip() for ln in t.splitlines() if ln.strip()]


def parse(page, xc):
    lines = text(page)
    info = {"xc": xc, "url": f"https://xeno-canto.org/{xc}"}
    basic = lines[lines.index("Basic data"):] if "Basic data" in lines else lines
    for i, ln in enumerate(basic):
        for f in FIELDS:
            key = f.lower().replace(" ", "_")
            if key in info:
                continue
            if ln == f and i + 1 < len(basic):
                info[key] = basic[i + 1]
            elif ln.startswith(f + " "):
                info[key] = ln[len(f) + 1:].strip()
    # "Location" also labels the map section; the real one follows Longitude.
    for i, ln in enumerate(lines[:-1]):
        if ln == "Location" and i >= 2 and lines[i - 2] == "Longitude":
            info["location"] = lines[i + 1]
    if "Remarks from the Recordist" in lines:
        i = lines.index("Remarks from the Recordist")
        j = next((k for k in range(i + 1, len(lines)) if lines[k].startswith("Annotations")), i + 2)
        info["remarks"] = "\n".join(lines[i + 1:j])
    m = re.search(r'href="(https?://creativecommons.org/licenses/[^"]+)"', page)
    info["licence_url"] = m.group(1) if m else None
    if "License" in lines:
        info["licence"] = lines[lines.index("License") + 1]
    m = re.search(r"<h1>(.*?)</h1>", page, re.S)
    info["title"] = html.unescape(re.sub(r"<[^>]+>", " ", m.group(1))).strip() if m else None
    return info


def main():
    root = sys.argv[1]
    for xc in sys.argv[2:]:
        xc = int(xc.lstrip("XCxc"))
        d = os.path.join(root, f"xc{xc}")
        os.makedirs(d, exist_ok=True)
        page = xc_fetch.fetch(f"https://xeno-canto.org/{xc}").decode("utf-8", "replace")
        info = parse(page, xc)
        ext = (info.get("file_type") or "mp3").lower()
        audio = os.path.join(d, f"audio.{ext}")
        if not os.path.exists(audio):
            data = xc_fetch.fetch(f"https://xeno-canto.org/{xc}/download")
            with open(audio, "wb") as f:
                f.write(data)
        info["audio_file"] = os.path.basename(audio)
        with open(os.path.join(d, "xc.json"), "w") as f:
            json.dump(info, f, indent=2, ensure_ascii=False)
        print(xc, info.get("date"), info.get("time"), info.get("length"), info.get("licence"), os.path.getsize(audio))


if __name__ == "__main__":
    main()
