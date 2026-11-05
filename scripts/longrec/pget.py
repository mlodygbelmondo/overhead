"""Parallel HTTP range download (the Freesound preview CDN serves ~80 kB/s per connection).

usage: python3 -I pget.py URL OUTFILE [PARTS]
"""
import concurrent.futures as cf
import os
import sys
import urllib.request

UA = "Mozilla/5.0 (X11; Linux x86_64) overhead research"


def size_of(url):
    req = urllib.request.Request(url, method="HEAD", headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as r:
        return int(r.headers["Content-Length"])


def part(url, a, b):
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Range": f"bytes={a}-{b}"})
    for attempt in range(5):
        try:
            with urllib.request.urlopen(req, timeout=300) as r:
                data = r.read()
            if len(data) == b - a + 1:
                return data
        except Exception:
            pass
    raise RuntimeError(f"range {a}-{b} failed")


def main():
    url, out = sys.argv[1], sys.argv[2]
    n = int(sys.argv[3]) if len(sys.argv) > 3 else 32
    size = size_of(url)
    step = (size + n - 1) // n
    ranges = [(a, min(size, a + step) - 1) for a in range(0, size, step)]
    with cf.ThreadPoolExecutor(n) as ex:
        parts = list(ex.map(lambda r: part(url, *r), ranges))
    tmp = out + ".part"
    with open(tmp, "wb") as f:
        for p in parts:
            f.write(p)
    os.replace(tmp, out)
    print(f"{out}: {size} bytes")


if __name__ == "__main__":
    main()
