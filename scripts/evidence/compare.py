"""Compare two validate.sh result folders (before / after).

usage: python3 -I compare.py BEFORE_DIR AFTER_DIR [--md]

Prints the counts the acceptance rules of docs/evidence.md use (answers
within 500 m and 2 km, confident and right, confident and wrong, the highest
margin on a wrong answer) and every run whose answer changed. A run is
"right" when its #1 is within 2 km of the true place, "confident" when its
margin is at least 1.
"""
import json
import sys
from pathlib import Path


def load(d):
    return {(r["id"], r["mode"]): r for r in json.loads((Path(d) / "summary.json").read_text())}


def facts(r):
    if r is None:
        return None
    if "top" not in r:
        return {"err": None, "margin": None, "score": None, "regs": [], "wall": r.get("wall_s")}
    t = r["top"]
    return {"err": r["err_m"], "margin": t["margin"], "score": t["score"], "regs": t["regs"], "wall": r.get("wall_s"), "search": r.get("search_s")}


def counts(runs):
    c = {"runs": 0, "no answer": 0, "within 500 m": 0, "within 2 km": 0, "confident and right": 0, "confident and wrong": 0}
    worst = (None, None)
    wall = []
    for key, r in sorted(runs.items()):
        f = facts(r)
        c["runs"] += 1
        if f["wall"] is not None:
            wall.append(f["wall"])
        if f["err"] is None:
            c["no answer"] += 1
            continue
        right = f["err"] < 2000
        c["within 500 m"] += f["err"] < 500
        c["within 2 km"] += right
        if f["margin"] >= 1:
            c["confident and right" if right else "confident and wrong"] += 1
        if not right and (worst[0] is None or f["margin"] > worst[0]):
            worst = (f["margin"], key)
    wall.sort()
    c["highest margin on a wrong answer"] = f"{worst[0]:.2f} ({worst[1][0]} {worst[1][1]})" if worst[0] is not None else "-"
    c["locate wall time total / median / worst (s)"] = f"{sum(wall):.0f} / {wall[len(wall) // 2]:.1f} / {wall[-1]:.1f}" if wall else "-"
    return c


def show(f):
    if f is None:
        return "not run"
    if f["err"] is None:
        return "no answer"
    e = f["err"]
    dist = f"{e:.0f} m" if e < 2000 else f"{e / 1000:.1f} km"
    mark = " ✅" if e < 500 else " ≈" if e < 2000 else ""
    return f"{dist}{mark}, margin {f['margin']:.2f}, {'/'.join(map(str, f['regs']))}"


def main():
    a, b = load(sys.argv[1]), load(sys.argv[2])
    md = "--md" in sys.argv
    ca, cb = counts(a), counts(b)
    if md:
        print("| | before | after |\n|---|---|---|")
        for k in ca:
            print(f"| {k} | {ca[k]} | {cb[k]} |")
    else:
        for k in ca:
            print(f"{k:45s} {str(ca[k]):>34s} {str(cb[k]):>34s}")
    order = {"exact": 0, "start": 1, "hours": 2, "day": 3}
    changed = []
    for key in sorted(set(a) | set(b), key=lambda k: (k[0], order.get(k[1], 9))):
        fa, fb = facts(a.get(key)), facts(b.get(key))
        if fa is None or fb is None:
            changed.append((key, fa, fb))
            continue
        ea, eb = fa["err"], fb["err"]
        if (ea is None) != (eb is None):
            changed.append((key, fa, fb))
        elif ea is not None and (abs(ea - eb) > 300 or abs(fa["margin"] - fb["margin"]) > 0.1):
            changed.append((key, fa, fb))
    print()
    if md:
        print("| recording | mode | before | after |\n|---|---|---|---|")
    for key, fa, fb in changed:
        if md:
            print(f"| {key[0]} | {key[1]} | {show(fa)} | {show(fb)} |")
        else:
            print(f"{key[0]:20s} {key[1]:6s} {show(fa):60s} -> {show(fb)}")
    print(f"\n{len(changed)} of {len(set(a) | set(b))} runs changed (error by more than 300 m or margin by more than 0.1)")


if __name__ == "__main__":
    main()
