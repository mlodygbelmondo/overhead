"""Fetch a xeno-canto page through its Anubis proof-of-work gate.

usage: python3 -I xc_fetch.py URL OUTFILE

Cookies are kept in $XC_COOKIES (default ~/overhead-data/scratch/xc.cookies),
so once one challenge has been solved, later pages come straight through.
"""

import hashlib
import http.cookiejar
import json
import os
import re
import sys
import time
import urllib.parse
import urllib.request

UA = "Mozilla/5.0 (X11; Linux x86_64) overhead research"
CJ = os.environ.get("XC_COOKIES", os.path.expanduser("~/overhead-data/scratch/xc.cookies"))

cj = http.cookiejar.MozillaCookieJar(CJ)
try:
    cj.load(ignore_discard=True, ignore_expires=True)
except Exception:
    pass
op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cj))
op.addheaders = [("User-Agent", UA)]


def get(url):
    with op.open(url, timeout=120) as r:
        return r.read()


def fetch(url):
    body = get(url)
    m = re.search(rb'<script id="anubis_challenge" type="application/json">(.*?)</script>', body, re.S)
    if not m:
        cj.save(ignore_discard=True, ignore_expires=True)
        return body
    ch = json.loads(m.group(1))
    c = ch["challenge"]
    diff = ch["rules"]["difficulty"]
    data = c["randomData"]
    t0 = time.time()
    n = 0
    while True:
        h = hashlib.sha256(f"{data}{n}".encode()).hexdigest()
        if h.startswith("0" * diff):
            break
        n += 1
    el = int((time.time() - t0) * 1000) + 50
    u = urllib.parse.urlparse(url)
    redir = u.path + ("?" + u.query if u.query else "")
    q = urllib.parse.urlencode({"id": c["id"], "response": h, "nonce": n, "redir": redir, "elapsedTime": el})
    body = get(f"{u.scheme}://{u.netloc}/.within.website/x/cmd/anubis/api/pass-challenge?" + q)
    cj.save(ignore_discard=True, ignore_expires=True)
    if b"anubis_challenge" in body:
        raise SystemExit("challenge not passed")
    return body


if __name__ == "__main__":
    out = fetch(sys.argv[1])
    with open(sys.argv[2], "wb") as f:
        f.write(out)
    print(len(out), "bytes")
