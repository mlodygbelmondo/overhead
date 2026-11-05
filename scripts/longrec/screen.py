"""ADS-B pre-screen of candidate long recordings: how many propeller/rotor passes came close during the stated
window? Writes a minimal meta.json per candidate into OUTDIR/<id>/ and runs cache_hits.py on it.

usage: python3 -I screen.py OUTDIR [ID ...]     (default: all CANDIDATES)
"""
import json
import os
import subprocess
import sys
from datetime import datetime
from zoneinfo import ZoneInfo

HERE = os.path.dirname(os.path.abspath(__file__))

# id: (lat, lon, local start, IANA zone, duration s, start uncertainty s)
CANDIDATES = {
    "fs749484": (51.703972, 4.62306, "2024-07-30T13:06:00", "Europe/Amsterdam", 4739, 300),
    "fs776236": (51.65651, 5.22905, "2024-03-18T12:00:00", "Europe/Amsterdam", 3650, 300),
    "fs700161": (51.68001, 5.27109, "2023-08-10T14:04:00", "Europe/Amsterdam", 3646, 300),
    "fs816300": (51.80599, 5.33527, "2025-07-15T14:10:00", "Europe/Amsterdam", 3580, 300),
    "fs834345": (51.71588, 5.19204, "2025-03-20T14:00:00", "Europe/Amsterdam", 3661, 600),
    "fs864478": (51.43994, 3.58148, "2026-07-24T14:55:00", "Europe/Amsterdam", 3600, 300),
    "fs825930": (47.24703, 4.82899, "2025-09-09T14:50:00", "Europe/Paris", 10301, 300),
    "fs688082": (51.54782, 3.45187, "2023-05-19T15:46:00", "Europe/Amsterdam", 6784, 300),
    "fs794275": (51.43263, 4.15276, "2024-08-18T09:23:00", "Europe/Amsterdam", 7107, 300),
    "fs846103": (51.71812, 4.69615, "2025-04-28T13:38:00", "Europe/Amsterdam", 5840, 300),
    "fs829598": (51.81458, 5.17456, "2025-09-01T14:57:00", "Europe/Amsterdam", 3600, 300),
    "fs801327": (51.71823, 4.71655, "2025-04-23T10:01:00", "Europe/Amsterdam", 3707, 600),
    "fs796004": (51.68882, 5.23562, "2025-03-27T13:02:00", "Europe/Amsterdam", 3246, 300),
    "fs843953": (51.79027, 5.37270, "2026-01-28T14:55:00", "Europe/Amsterdam", 2575, 300),
    "fs853666": (51.70626, 4.61943, "2025-05-01T14:39:00", "Europe/Amsterdam", 1800, 900),
    "fs808023": (51.63881, 5.10545, "2025-05-21T12:45:00", "Europe/Amsterdam", 2724, 300),
    "fs835182": (51.65408, 5.08539, "2025-04-04T09:53:00", "Europe/Amsterdam", 3757, 300),
    "fs792625": (51.65600, 5.31921, "2025-03-06T11:27:00", "Europe/Amsterdam", 3255, 3900),
    "fs853167": (51.71060, 4.70547, "2025-04-26T07:51:00", "Europe/Amsterdam", 3600, 300),
    "fs854034": (51.71672, 4.72869, "2024-07-27T14:31:00", "Europe/Amsterdam", 2100, 300),
    "fs865699": (51.81627, 5.28613, "2026-08-02T15:38:00", "Europe/Amsterdam", 1980, 300),
    "fs811306": (51.69465, 5.31844, "2025-06-02T19:35:00", "Europe/Amsterdam", 3968, 300),
    "fs835530": (51.74062, 5.23052, "2025-04-09T19:31:00", "Europe/Amsterdam", 3241, 3900),
    "fs837958": (51.81027, 5.24714, "2025-11-09T09:57:00", "Europe/Amsterdam", 5282, 300),
    "fs785812": (51.73883, 5.23097, "2023-04-05T09:28:00", "Europe/Amsterdam", 3231, 300),
    "fs686522": (51.71848, 4.71733, "2023-04-27T09:54:00", "Europe/Amsterdam", 2775, 300),
}


def main():
    out = sys.argv[1]
    ids = sys.argv[2:] or list(CANDIDATES)
    for cid in ids:
        lat, lon, local, tz, dur, unc = CANDIDATES[cid]
        dt = datetime.fromisoformat(local).replace(tzinfo=ZoneInfo(tz))
        utc = dt.astimezone(ZoneInfo("UTC")).strftime("%Y-%m-%dT%H:%M:%SZ")
        d = os.path.join(out, cid)
        os.makedirs(d, exist_ok=True)
        meta = {"id": cid, "lat": lat, "lon": lon, "elev_m": 0, "geoid_m": 45.0, "utc_start": utc,
                "duration_s": dur, "start_uncertainty_s": unc}
        p = os.path.join(d, "meta.json")
        json.dump(meta, open(p, "w"), indent=1)
        r = subprocess.run([sys.executable, "-I", os.path.join(HERE, "cache_hits.py"), p, "--radius-km", "5",
                            "--out", os.path.join(d, "screen.json")], capture_output=True, text=True)
        print(r.stdout + r.stderr[-500:], flush=True)


if __name__ == "__main__":
    main()
