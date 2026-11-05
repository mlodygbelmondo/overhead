"""YouTube video metadata (yt-dlp -j): title, duration, live status, stream start (release_timestamp), upload date,
licence, description.

usage: python3 -I yt_info.py CACHEDIR ID [ID ...]
"""
import json
import os
import subprocess
import sys
from datetime import datetime, timezone


def main():
    cache = sys.argv[1]
    os.makedirs(cache, exist_ok=True)
    for vid in sys.argv[2:]:
        p = os.path.join(cache, f"yt_{vid}.json")
        if not os.path.exists(p):
            r = subprocess.run(["yt-dlp", "-j", "--skip-download", f"https://www.youtube.com/watch?v={vid}"],
                               capture_output=True, text=True, timeout=300)
            if r.returncode != 0:
                print(vid, "ERROR", r.stderr[-300:])
                continue
            open(p, "w").write(r.stdout)
        m = json.load(open(p))
        rt = m.get("release_timestamp")
        rts = datetime.fromtimestamp(rt, timezone.utc).isoformat() if rt else None
        print(f"===== {vid} | {m.get('channel')} | {m.get('title')}")
        print(f"duration {m.get('duration')} s, live_status {m.get('live_status')}, release {rts}, "
              f"upload {m.get('upload_date')}, licence {m.get('license')}")
        print((m.get("description") or "")[:1200])


if __name__ == "__main__":
    main()
