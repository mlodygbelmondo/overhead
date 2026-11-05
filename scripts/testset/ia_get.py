"""Download radio aporee recordings from the Internet Archive mirror.

usage: python3 -I ia_get.py OUTROOT aporee_LOC_SND [...]

For each item writes OUTROOT/<item>/ia.json (archive.org metadata: lat, lon,
date with timezone, licence, description) and audio.<ext> (the largest
audio file of the item).
"""

import json
import os
import sys
import urllib.parse
import urllib.request

UA = "overhead research"
AUDIO = (".mp3", ".flac", ".wav", ".ogg")


def get(url):
    with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": UA}), timeout=300) as r:
        return r.read()


def main():
    root = sys.argv[1]
    for item in sys.argv[2:]:
        meta = json.loads(get(f"https://archive.org/metadata/{item}"))
        files = [f for f in meta["files"] if f["name"].lower().endswith(AUDIO) and f.get("source") != "derivative"]
        if not files:
            files = [f for f in meta["files"] if f["name"].lower().endswith(AUDIO)]
        f = max(files, key=lambda f: int(f.get("size", 0)))
        d = os.path.join(root, item)
        os.makedirs(d, exist_ok=True)
        ext = os.path.splitext(f["name"])[1].lower()
        audio = os.path.join(d, "audio" + ext)
        if not os.path.exists(audio):
            data = get(f"https://archive.org/download/{item}/{urllib.parse.quote(f['name'])}")
            with open(audio, "wb") as fh:
                fh.write(data)
        info = dict(meta["metadata"])
        info["source_file"] = f["name"]
        info["length_s"] = f.get("length")
        info["audio_file"] = os.path.basename(audio)
        info["url"] = f"https://archive.org/details/{item}"
        loc, snd = item.split("_")[1:3]
        info["aporee_url"] = f"https://aporee.org/maps/?loc={loc}&snd={snd}"
        with open(os.path.join(d, "ia.json"), "w") as fh:
            json.dump(info, fh, indent=2, ensure_ascii=False)
        print(item, info.get("date"), info.get("timezone"), info.get("latitude"), info.get("longitude"),
              f.get("length"), info.get("licenseurl"))


if __name__ == "__main__":
    main()
