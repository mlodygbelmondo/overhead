"""YouTube search (yt-dlp, flat): id, duration, live status, channel, title per hit.

usage: python3 -I yt_search.py N 'query' ['query' ...]
"""
import subprocess
import sys

FMT = "%(id)s|%(duration)s|%(live_status)s|%(channel)s|%(title)s"


def main():
    n = int(sys.argv[1])
    for q in sys.argv[2:]:
        print(f"## {q}", flush=True)
        r = subprocess.run(["yt-dlp", "--flat-playlist", "--print", FMT, f"ytsearch{n}:{q}"],
                           capture_output=True, text=True, timeout=180)
        print(r.stdout, end="", flush=True)


if __name__ == "__main__":
    main()
