"""Run adsb_check.py for every DIR/sites_<day>.json that has no hits file yet.

usage: python3 -I adsb_days.py DIR [--jobs 2] [--radius-km 20]
"""

import argparse
import glob
import os
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))


def run(sites, radius):
    day = os.path.basename(sites)[len("sites_"):-len(".json")]
    out = os.path.join(os.path.dirname(sites), f"hits_{day}.json")
    log = os.path.join(os.path.dirname(sites), f"hits_{day}.log")
    with open(log, "w") as lf:
        r = subprocess.run([sys.executable, "-I", os.path.join(HERE, "adsb_check.py"), sites, day,
                            "--radius-km", str(radius), "--out", out], stdout=lf, stderr=subprocess.STDOUT)
    return day, r.returncode


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dir")
    ap.add_argument("--jobs", type=int, default=2)
    ap.add_argument("--radius-km", type=float, default=20)
    a = ap.parse_args()
    todo = [s for s in sorted(glob.glob(os.path.join(a.dir, "sites_*.json")))
            if not os.path.exists(s.replace("sites_", "hits_"))]
    print("todo:", [os.path.basename(t) for t in todo], flush=True)
    with ThreadPoolExecutor(a.jobs) as ex:
        for day, rc in ex.map(lambda s: run(s, a.radius_km), todo):
            print(day, "exit", rc, flush=True)


if __name__ == "__main__":
    main()
