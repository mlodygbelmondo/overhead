"""Search xeno-canto (web search, all result pages) and print one JSON object
per recording: number, date, time, coordinates, remarks, licence, audio URL.

usage: python3 -I xc_search.py 'rmk:helicopter' [more queries...] > hits.jsonl

The API v3 needs a personal key, so this scrapes the public search pages
through xc_fetch.py (Anubis proof-of-work gate).
"""

import html
import json
import os
import re
import sys
import urllib.parse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import xc_fetch  # noqa: E402


def strip(s):
    return html.unescape(re.sub(r"<[^>]+>", " ", s)).strip()


def parse(page):
    t = page.find("<table class='results'>")
    if t < 0:
        return []
    out = []
    for row in re.findall(r"<tr(?: class=.new-species.)? ?>(.*?)</tr>", page[t:], re.S):
        cells = re.findall(r"<td[^>]*>(.*?)</td>", row, re.S)
        m_nr = re.search(r'href="https://xeno-canto.org/(\d+)">XC\d+', row)
        if not m_nr or len(cells) < 12:
            continue
        m_loc = re.search(r'location/map\?lat=([-\d.]*)&long=([-\d.]*)&loc=[^"]*">(.*?)</a>', row, re.S)
        m_aud = re.search(r"<audio[^>]*src='([^']+)'", row)
        m_lic = re.search(r'href="(https://creativecommons.org/licenses/[^"]+)"', row)
        m_rmk = re.search(r"<div class='remarks[^']*'>(.*?)</div>", row, re.S)
        rec = {
            "xc": int(m_nr.group(1)),
            "species": strip(cells[1]),
            "length": strip(cells[2]),
            "recordist": strip(cells[3]),
            "date": strip(cells[4]),
            "time": strip(cells[5]),
            "country": strip(cells[6]),
            "lat": float(m_loc.group(1)) if m_loc and m_loc.group(1) else None,
            "lon": float(m_loc.group(2)) if m_loc and m_loc.group(2) else None,
            "loc": strip(m_loc.group(3)) if m_loc else strip(cells[7]),
            "elev": strip(cells[8]),
            "remarks": strip(re.sub(r"<span class='tooltip'.*", "", m_rmk.group(1), flags=re.S)) if m_rmk else "",
            "licence": m_lic.group(1) if m_lic else None,
            "audio": m_aud.group(1) if m_aud else None,
        }
        out.append(rec)
    return out


def search(query):
    seen = set()
    pg = 1
    while True:
        url = "https://xeno-canto.org/explore?" + urllib.parse.urlencode({"query": query, "pg": pg})
        page = xc_fetch.fetch(url).decode("utf-8", "replace")
        recs = parse(page)
        new = [r for r in recs if r["xc"] not in seen]
        if not new:
            break
        for r in new:
            seen.add(r["xc"])
            r["query"] = query
            yield r
        if f"pg={pg + 1}" not in page:
            break
        pg += 1


if __name__ == "__main__":
    for q in sys.argv[1:]:
        n = 0
        for r in search(q):
            print(json.dumps(r, ensure_ascii=False), flush=True)
            n += 1
        print(f"{q}: {n}", file=sys.stderr)
