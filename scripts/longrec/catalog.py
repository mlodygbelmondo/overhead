"""Long real recordings with several aircraft fly-bys: the curated list; writes each meta.json in the test-set
layout (same keys as scripts/testset/catalog.py).

usage: python3 -I catalog.py TESTSET_DIR [--copy DIR] [ID ...]

--copy DIR also writes a copy of each meta.json to DIR/<id>.meta.json (committed in docs/testset/long/).

Elevation and geoid come from `overhead-adsb terrain` (SRTM + EGM96). Audio is downloaded separately
(pget.py for Freesound HQ previews) and never committed.
"""
import json
import os
import re
import subprocess
import sys
from datetime import datetime
from zoneinfo import ZoneInfo
DATA = os.environ.get("OVERHEAD_DATA") or os.environ.get("DOPPLER_DATA") or os.path.expanduser("~/overhead-data")

ADSB = os.environ.get("OVERHEAD_ADSB", f"{DATA}/target-longrec/release/overhead-adsb")

KB = {"recordist": "klankbeeld", "licence": "CC BY 4.0", "licence_url": "https://creativecommons.org/licenses/by/4.0/",
      "source": "Freesound (HQ MP3 preview; the original needs a login)", "timezone": "Europe/Amsterdam",
      "country": "Netherlands", "audio_file": "audio.mp3"}

ENTRIES = [
    dict(KB, id="fs749484", short="Moerdijk harbour",
         source_url="https://freesound.org/people/klankbeeld/sounds/749484/",
         title="harbor in summer 0106 PM 240730_0867",
         lat=51.703972, lon=4.62306, location_uncertainty_m=150,
         location_note="Freesound map pin (village harbour of Moerdijk); the description has no GPS line.",
         place="Moerdijk village harbour (Hollands Diep)", region="Noord-Brabant",
         local_start="2024-07-30T13:06:00", start_uncertainty_s=180,
         time_note="Title '0106 PM 240730' (13:06 local, 30 July 2024), minute precision.",
         audible="Harbour, waves, cars, voices. Description: '#4:10 A small propeller plane flies by', "
                 "'#13:24 A small propeller plane flies by', '#33:29 a winged cigar with vacationers and a "
                 "helicopter fly over'.",
         duration_s=4739.2, status="near_miss",
         adsb_note="Does not count: only 2 detections pair with ADS-B (PH-MDZ DA62, PH-SKC C172), and with the same "
                   "search a wrong clock pairs 2-3 too (null test). The recordist's own times fit ADS-B if the file "
                   "starts ~1 min after 13:06: PH-MDZ heard at ~4:21 (#4:10), gyrocopter PH-4U7 at 5.7 km at ~13:22 "
                   "(#13:24), EC135 PH-PXF at 1.4 km at ~33:51 (#33:29); stage 1 finds neither the gyro nor the "
                   "helicopter."),
    dict(KB, id="fs864478", short="Vlissingen sea front",
         source_url="https://freesound.org/people/klankbeeld/sounds/864478/",
         title="coastal riverside Westerschlelde Vlissingen Netherlands 255 pm 260724_0144",
         lat=51.43994, lon=3.58148, location_uncertainty_m=50,
         location_note="Recordist's GPS in the description (51.43994 3.58148), same as the map pin: on the "
                       "basalt sea wall at the mouth of the Westerschelde.",
         place="Vlissingen sea front, Westerschelde", region="Zeeland",
         local_start="2026-07-24T14:55:00", start_uncertainty_s=180,
         time_note="Description 'Date/time: July 24, 2026, 2:55 pm', minute precision.",
         audible="Beach, pilot boats, cargo ships, funfair, bells; aircraft not mentioned in the description.",
         duration_s=3600.0, status="qualified",
         adsb_note="3 detections pair with 3 light aircraft within 2 s at one start (+48 s from the stated 14:55): "
                   "D-EHIP (R300, 0.9 km, at the very start of the file), OO-VMX (P28A, 1.3 km), PH-HLF (C182, "
                   "0.9 km). Stage 1's speeds are low (28-51 m/s against 58-68 m/s), so the pairs agree in time only. "
                   "An unknown-type aircraft (3ffb79, 1.6 km at 3:50) has no detection. Chance check: of 50 wrong "
                   "clocks over +-150 min, 0 pair 3 at +-5 s / 5 km, 2 do at +-15 s / 10 km."),
    dict(KB, id="fs794275", short="Oosterschelde salt marsh (Rilland)",
         source_url="https://freesound.org/people/klankbeeld/sounds/794275/",
         title="mud flats salt marshes Oosterschelde NL LONG 923 AM 240818_0899",
         lat=51.43263, lon=4.15276, location_uncertainty_m=50,
         location_note="Recordist's GPS in the description (51.43263 4.15276), same as the map pin.",
         place="Oosterschelde mud flats and salt marshes near Rilland", region="Zeeland",
         local_start="2024-08-18T09:23:00", start_uncertainty_s=180,
         time_note="Description 'Date/time: August 18, 2024, 09:23 AM', minute precision.",
         audible="Mud flats, gulls, wind; aircraft not annotated in the description.",
         duration_s=7107.3, status="qualified",
         adsb_note="5 detections pair with 5 aircraft within 7 s at one start (+73 s from the stated 09:23); speeds "
                   "and distances agree too (e.g. ES-ILM DA62 83/81 m/s, 4.5/4.4 km; PH-HJG TB20 68/68 m/s, "
                   "3.5/3.6 km). One of them is a PC-12 (9H-WIT) at 6 km height, 9.6 km slant."),
    dict(KB, id="fs796004", short="Moerputten, Vlijmen ('s-Hertogenbosch)",
         source_url="https://freesound.org/people/klankbeeld/sounds/796004/",
         title="swamp meadow Vlijmens Ven Moerputten Netherlands 102 PM 250327_1064",
         lat=51.68882, lon=5.23562, location_uncertainty_m=50,
         location_note="Recordist's GPS in the description (51.68882 5.23562), same as the map pin.",
         place="Moerputten, Gement polder, south of the Venkantbrug, Vlijmen", region="Noord-Brabant",
         local_start="2025-03-27T13:02:00", start_uncertainty_s=180,
         time_note="Date conflict: title '102 PM 250327' (27 March), description 'March 17th 2025, 1:02 PM'. "
                   "ADS-B does not settle it (see adsb_note); 13:02 local, minute precision.",
         alt_dates=["2025-03-17"],
         audible="Marsh, moor frogs, birds, distant highway; aircraft not annotated in the description.",
         duration_s=3246.0, status="near_miss",
         adsb_note="Does not count: a PA-31T (PH-SVY) flew survey lines over the site (8 passes 0.9-3.4 km in the "
                   "window on the 27th), but stage 1 finds none of them; 1 detection pairs (an AH-64, 5.2 km). On the "
                   "17th 2 detections pair with an R44 (OO-SAF). The date stays unresolved."),
    dict(KB, id="fs801327", short="River Amer, Lage Zwaluwe",
         source_url="https://freesound.org/people/klankbeeld/sounds/801327/",
         title="amer river Netherlands in spring 927 AM 250423_1077",
         lat=51.71823, lon=4.71655, location_uncertainty_m=50,
         location_note="Recordist's GPS in the description (51.71823 4.71655), 13 m from the map pin.",
         place="River Amer dyke, Beverpad, Lage Zwaluwe", region="Noord-Brabant",
         local_start="2025-04-23T09:27:00", start_uncertainty_s=180,
         alt_local_starts=["2025-04-23T10:01:00"],
         time_note="Time conflict: title '927 AM 250423', description 'Date/time: April 23 2025, 10:01 AM', and "
                   "the description also says 'on a nice spring afternoon'. The source therefore does not fix the "
                   "start to +-3 min: 09:27, 10:01 and an afternoon reading are all possible from the text alone. "
                   "ADS-B settles it for the title: at 09:27 four detections line up with four flights within "
                   "+-7 s (file start ~09:28:45); with the same +-3 min search 10:01 pairs 1 (+-5 s, 5 km) or 2 "
                   "(+-15 s, 10 km), the afternoon readings 13:27 and 14:01 pair 0 (+-5 s) or 0-2 (+-15 s). The "
                   "title time is used as local_start, so mode (a') is ADS-B-informed; mode 'both' searches "
                   "09:24-10:04 (both stated morning times) without that choice.",
         start_uncertainty_note="+-180 s is around the title time only; taken over both stated times the "
                                "uncertainty is 09:24-10:04 local (+-23 min around 09:44).",
         audible="River, birds (long list with times), ships; aircraft not annotated in the description.",
         duration_s=3707.0, status="qualified",
         adsb_note="4 detections pair with 4 aircraft within 7 s at one start (+105 s from the title's 09:27): "
                   "PH-NSC (DR400, 2.3 km), the Dutch Air Force PC-7s L-08 and L-03 (1.7 km, 50 s apart) and the "
                   "police EC135 PH-PXA (0.7 km; stage 1 53 m/s, 678 m against ADS-B 53 m/s, 694 m). Two pairs agree "
                   "in time only: det 2 / PH-NSC (stage 1 30 m/s, 492 m against 57 m/s, 2268 m) and det 6 / L-03 "
                   "(40 m/s, 470 m against 90 m/s, 1688 m); det 4 / L-08 is in between (75/82 m/s, 652/1673 m). "
                   "The count of 4 rests on timing: 50 wrong clocks over +-150 min pair at most 2 (+-5 s, 5 km) "
                   "or 3 (+-15 s, 10 km)."),
    dict(KB, id="fs846103", short="Marina Lage Zwaluwe (river Amer)",
         source_url="https://freesound.org/people/klankbeeld/sounds/846103/",
         title="marina Lage Zwaluwe at river Amer Netherlands 138 pm 250428_1084",
         lat=51.71812, lon=4.69615, location_uncertainty_m=50,
         location_note="Recordist's GPS in the description (51.71812 4.69615).",
         place="Ameroever marina, Lage Zwaluwe", region="Noord-Brabant",
         local_start="2025-04-28T13:38:00", start_uncertainty_s=180,
         time_note="Title '138 pm 250428' and description 'Date/time: April 28 2025, 1:38 pm', minute precision.",
         audible="Marina, boats, ferry, people talking; aircraft not annotated in the description.",
         duration_s=5840.7, status="rejected",
         adsb_note="Rejected (review round): 6 propeller passes inside the file within 10 km, but all 3.7-8.3 km "
                   "away (PH-BAN A210 x2, PH-GVB C172, PH-BAT A210, D-MMQM J600 x2). Stage 1 finds 4 detections and "
                   "none pairs within +-15 s at any start within +-3 min."),
    dict(KB, id="fs834345", short="Gelderse Waarden polder, Haarsteeg",
         source_url="https://freesound.org/people/klankbeeld/sounds/834345/",
         title="Dutch polder Gelderse Waarden Noord-Brabant Netherlands  2 pm 250320_1061",
         lat=51.71588, lon=5.19204, location_uncertainty_m=50,
         location_note="Recordist's GPS in the description (51.71588 5.19204).",
         place="Polder Gelderse Waarden, Hoge Maasdijk, Haarsteeg / Hedikhuizen", region="Noord-Brabant",
         local_start="2025-03-20T14:00:00", start_uncertainty_s=600,
         time_note="Title '2 pm 250320' and description 'Date/time: March 20 2025, 2 pm': hour stated, not the "
                   "minute, so +-10 min.",
         audible="Birds, distant shipping, 'high altitude planes', bikes, tractors.",
         duration_s=3661.0, status="rejected",
         adsb_note="Rejected (review round): 4 propeller/rotor passes inside the file within 10 km (PA-31 PH-PNY "
                   "survey lines at 3.2-5.6 km, AH-64 Q-40 at 5.0 km). Stage 1 finds 6 detections (all "
                   "unconfident); 2 pair (PH-PNY -12 s, Q-40 +2 s), fewer than 3."),
]


def terrain(lat, lon):
    r = subprocess.run([ADSB, "terrain", str(lat), str(lon)], capture_output=True, text=True, timeout=300)
    m = re.search(r"SRTM ([-\d.]+) m.*geoid N ([-\d.]+) m", r.stdout + r.stderr)
    return (round(float(m.group(1))), float(m.group(2))) if m else (0, 0.0)


def main():
    root = sys.argv[1]
    args = sys.argv[2:]
    copy = None
    if args[:1] == ["--copy"]:
        copy, args = args[1], args[2:]
    ids = set(args)
    for e in ENTRIES:
        if ids and e["id"] not in ids:
            continue
        m = dict(e)
        local = datetime.fromisoformat(m["local_start"]).replace(tzinfo=ZoneInfo(m["timezone"]))
        off = local.strftime("%z")
        m["utc_start"] = local.astimezone(ZoneInfo("UTC")).strftime("%Y-%m-%dT%H:%M:%SZ")
        m["utc_offset"] = off
        sub = "" if m["status"] == "qualified" else "candidates"  # near misses go with the rejected candidates
        p = os.path.join(root, sub, m["id"], "meta.json")
        old = json.load(open(p)) if os.path.exists(p) else {}
        if "elev_m" not in m:
            if "elev_m" in old and old.get("lat") == m["lat"] and old.get("lon") == m["lon"]:
                m["elev_m"], m["geoid_m"] = old["elev_m"], old["geoid_m"]
            else:
                m["elev_m"], m["geoid_m"] = terrain(m["lat"], m["lon"])
        hp = os.path.join(root, sub, m["id"], "adsb_hits.json")
        if os.path.exists(hp):  # ADS-B ground truth from cache_hits.py (pairing at a common start offset)
            pr = json.load(open(hp))["pairing"]
            m["adsb_start_utc"] = pr["start"]
            m["adsb_matches"] = [dict(d["assigned"], det=d["det"], tc=round(d["tc"], 1))
                                 for d in pr["detections"] if d.get("assigned")]
        for k in ("adsb_note",):  # written by hand after the ADS-B check
            if k in old and k not in m:
                m[k] = old[k]
        os.makedirs(os.path.dirname(p), exist_ok=True)
        json.dump(m, open(p, "w"), indent=1, ensure_ascii=False)
        if copy:
            os.makedirs(copy, exist_ok=True)
            json.dump(m, open(os.path.join(copy, m["id"] + ".meta.json"), "w"), indent=1, ensure_ascii=False)
        print(p, m["utc_start"], m["elev_m"], m["geoid_m"])


if __name__ == "__main__":
    main()
