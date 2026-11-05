"""Print archive.org metadata summaries (date, audio length, place, description) for items.

usage: python3 -I ia_meta.py CACHEDIR ITEM [ITEM...]
"""
import json
import os
import sys
import urllib.request

UA = "overhead research"


def main():
    cache = sys.argv[1]
    os.makedirs(cache, exist_ok=True)
    for item in sys.argv[2:]:
        p = os.path.join(cache, f"meta_{item}.json")
        if not os.path.exists(p):
            req = urllib.request.Request(f"https://archive.org/metadata/{item}", headers={"User-Agent": UA})
            with urllib.request.urlopen(req, timeout=60) as r:
                open(p, "wb").write(r.read())
        m = json.load(open(p))
        md = m.get("metadata", {})
        ln = [f.get("length") for f in m.get("files", []) if f.get("length")]
        geo = md.get("coverage") or (md.get("latitude"), md.get("longitude"))
        desc = str(md.get("description"))[:400].replace("\n", " ")
        print(item, md.get("date"), ln[:1], geo, "|", md.get("title"), "|", desc, flush=True)


if __name__ == "__main__":
    main()
