"""Print one line per test-set recording from its meta.json."""
import os
import json
import sys
from pathlib import Path
DATA = os.environ.get("OVERHEAD_DATA") or os.environ.get("DOPPLER_DATA") or os.path.expanduser("~/overhead-data")

root = Path(sys.argv[1] if len(sys.argv) > 1 else f"{DATA}/testset")
for p in sorted(root.glob("*/meta.json")):
    m = json.loads(p.read_text())
    det = json.loads((p.parent / "detect.json").read_text())
    dets = det if isinstance(det, list) else det.get("detections", det)
    print(
        m["id"], "|", m.get("short"), "|", m["lat"], m["lon"], "|", m.get("region"), "|", m.get("country"), "|",
        m["timezone"], m["local_start"], m["utc_start"], "±", m["start_uncertainty_s"], "| dur", m["duration_s"],
        "| locunc", m.get("location_uncertainty_m"), "| match", (m.get("adsb_match") or {}).get("reg"),
        "| dets", len(dets) if isinstance(dets, list) else "?", "|", m["audio_file"],
    )
