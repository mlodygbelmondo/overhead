"""Print title, channel, dates and description of YouTube videos (yt-dlp).

usage: python3 -I yt_meta.py [--search 'ytsearch20:query'] [ID ...]
"""

import json
import os
import subprocess
import sys

YTDLP = os.path.expanduser("~/.local/bin/yt-dlp")


def meta(vid):
    out = subprocess.run([YTDLP, "-j", "--skip-download", f"https://www.youtube.com/watch?v={vid}"],
                         capture_output=True, text=True)
    return json.loads(out.stdout) if out.returncode == 0 and out.stdout else None


def main():
    args = sys.argv[1:]
    if args and args[0] == "--search":
        out = subprocess.run([YTDLP, "--flat-playlist", "-j", args[1]], capture_output=True, text=True)
        for ln in out.stdout.splitlines():
            d = json.loads(ln)
            print(d.get("id"), d.get("duration"), d.get("channel"), "|", (d.get("title") or "")[:100])
        return
    for vid in args:
        d = meta(vid)
        if not d:
            print("====", vid, "unavailable")
            continue
        print("====", d["id"], d.get("channel"), "upload", d.get("upload_date"), "dur", d.get("duration"),
              "licence", d.get("license"), "loc", d.get("location"))
        print(d.get("title"))
        print((d.get("description") or "")[:900])


if __name__ == "__main__":
    main()
