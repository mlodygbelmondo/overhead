"""scripts/evidence/run_locate.py for the long recordings: same modes and inputs (all from meta.json), this
track's binary and the province regions of the new recordings.

usage: python3 -I run_locate.py OUTDIR MODE ID [ID...]     MODE: start | hours | day | oracle | exact | both
(exact uses EXACT_START below: the start implied by the ADS-B pairing of cache_hits.py, a best case.
both: for a recording whose source states two start times (meta.json alt_local_starts), --start at the midpoint
of the earliest and latest stated time, --start-tol = half their span + the stated uncertainty, so the window
covers every stated reading and no ADS-B-informed choice is made.)
"""
import os
import sys
from datetime import datetime, timedelta

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "evidence"))
import run_locate as rl  # noqa: E402
DATA = os.environ.get("OVERHEAD_DATA") or os.environ.get("DOPPLER_DATA") or os.path.expanduser("~/overhead-data")

rl.B = os.environ.get("OVERHEAD_ADSB", f"{DATA}/target-longrec/release/overhead-adsb")
rl.REGION.update({
    "fs749484": "Noord-Brabant, Netherlands",
    "fs864478": "Zeeland, Netherlands",
    "fs794275": "Zeeland, Netherlands",
    "fs801327": "Noord-Brabant, Netherlands",
    "fs796004": "Noord-Brabant, Netherlands",
})
rl.EXACT_START.update({
    # start at which cache_hits.py pairs the most detections one-to-one with ADS-B passes (+-15 s)
    "fs864478": "2026-07-24T12:55:48Z",
    "fs794275": "2024-08-18T07:24:13Z",
    "fs801327": "2025-04-23T07:28:45Z",
    "fs749484": "2024-07-30T11:06:37Z",
})

_args_for = rl.args_for


def args_for(m, mode):
    if mode != "both":
        return _args_for(m, mode)
    off = m["utc_offset"][:3] + ":" + m["utc_offset"][3:]
    ts = sorted(datetime.fromisoformat(t) for t in [m["local_start"], *m.get("alt_local_starts", [])])
    mid = ts[0] + (ts[-1] - ts[0]) / 2
    tol = int((ts[-1] - ts[0]).total_seconds() / 2 + m["start_uncertainty_s"])
    return ["--day", m["local_start"][:10], "--region", rl.REGION[m["id"]],
            "--start", mid.strftime("%Y-%m-%dT%H:%M:%S") + off, "--start-tol", str(max(120, tol))]


rl.args_for = args_for

if __name__ == "__main__":
    rl.main()
