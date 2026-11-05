"""The curated real-recording test set: one entry per recording.

usage:
  python3 -I catalog.py meta  ROOT          write ROOT/<id>/meta.json for every entry
  python3 -I catalog.py sites ROOT OUTDIR   write OUTDIR/sites_<UTC day>.json for adsb_check.py

ROOT is the test-set directory (~/overhead-data/testset). Each entry's audio
lives in ROOT/<id>/<audio_file>. Times are what the source states; the
uncertainty is our judgement of how loosely the source gives it.
"""

import glob
import json
import os
import sys
from datetime import datetime, timedelta, timezone
from zoneinfo import ZoneInfo

ENTRIES = [
    {
        "id": "aporee_68715_79706",
        "short": "Mechlin (Śrem)",
        "notes": "No aircraft below 6 km slant within 20 km in adsb.lol during the window: the helicopter did "
                 "not broadcast ADS-B (or was not received). Useful as a hard case: a clear fly-by that the "
                 "matcher must leave unmatched.",
        "source": "radio aporee (Internet Archive mirror)",
        "source_url": "https://aporee.org/maps/?loc=68715&snd=79706",
        "archive_url": "https://archive.org/details/aporee_68715_79706",
        "title": "Mechlin, gmina Śrem - morning ambience at the river",
        "recordist": "maciej janasik",
        "licence": "CC BY-NC 3.0",
        "licence_url": "https://creativecommons.org/licenses/by-nc/3.0/",
        "lat": 52.099139698773, "lon": 17.092804576342, "elev_m": 67, "geoid_m": 36.3,
        "location_uncertainty_m": 100, "location_note": "radio aporee map pin placed by the recordist.",
        "place": "Mechlin, gmina Śrem, powiat śremski",
        "region": "Wielkopolskie (Greater Poland)", "country": "Poland",
        "timezone": "Europe/Warsaw",
        "local_start": "2025-06-29T09:19:00",
        "start_uncertainty_s": 600,
        "time_note": "Date and time from the radio aporee map entry (minute precision, start of recording assumed).",
        "audible": "River and oak-wood ambience; 'a small helicopter arrives and flies directly over the "
                   "recorder' (recordist). Helicopter clearly audible from about 9:40 to 11:30 of the file, "
                   "closest at about 10:29.",
        "audio_file": "audio.mp3",
    },
    {
        "id": "fs858688",
        "short": "Kampina (Boxtel)",
        "adsb_match": {"reg": "PH-TRG", "det": 0,
                       "note": "Cessna 172 at 1475 ft pressure altitude (~460 m above the site after the QNH correction), "
                               "2.6 km slant from the recordist's GPS position, heard ~7 s before the detector's tc; "
                               "the only low aircraft within 8 km in the window."},
        "source": "Freesound (HQ preview MP3; the original needs a login)",
        "source_url": "https://freesound.org/people/klankbeeld/sounds/858688/",
        "title": "prop plane birds fens Kampina Netherlands 1140 am 250718_1173",
        "recordist": "klankbeeld",
        "licence": "CC BY 4.0",
        "licence_url": "https://creativecommons.org/licenses/by/4.0/",
        "lat": 51.57401, "lon": 5.27708, "elev_m": 10, "geoid_m": 44.0,
        "location_uncertainty_m": 50, "location_note": "Recordist's GPS from the description ('Zandbergse vennen ... GPS 51.57401 5.27708'); OSM puts Zandbergsvennen at the same spot. The Freesound map pin (51.5787, 5.2573) is ~1.5 km west and is not used.",
        "place": "Zandbergse vennen, Kampina, Van Gogh National Park, Boxtel",
        "region": "Noord-Brabant", "country": "Netherlands",
        "timezone": "Europe/Amsterdam",
        "local_start": "2025-07-18T11:40:00",
        "start_uncertainty_s": 180,
        "time_note": "Date conflict: the title says '1140 am 250718' (18 July) but the description's 'Date/time' "
                     "field says 'July 17, 2025, 11:40 am' (as does the sister file fs858591). ADS-B decides it (same local times, "
                     "+-8 min): on the 17th nothing passes within 6 km slant during this file, and during "
                     "fs858591 only an MD902 helicopter (D-HMDX, 1.5 km) that is heard ~10 min after that file "
                     "ends, while on the 18th a Cessna 172 and a Robin DR400 pass at the right moments. So the 18th is used; the "
                     "17th in the description looks like a copy slip. The time of day agrees in title and description.",
        "alt_dates": ["2025-07-17"],
        "audible": "'A small prop plane flying over' (recordist): strong propeller harmonics over the "
                   "first ~80 s, closest around 0:30-0:40.",
        "audio_file": "audio.mp3",
    },
    {
        "id": "fs858591",
        "short": "Kampina (Boxtel)",
        "adsb_match": {"reg": "PH-HLR", "det": 0,
                       "note": "Robin DR400 at 2300 ft pressure altitude (~690 m above the site), 0.8 km slant from the "
                               "recordist's GPS position, the only light aircraft within 5 km. Its CPA is heard 1:04 "
                               "into the file, 41 s before the detector's tc (1:45), well inside the +-3 min start "
                               "uncertainty. Some of its trace points report ~420 kt ground speed (a decoding "
                               "artefact); the speed flown between points is ~117 kt. (An EC135, PH-LLN, passed 8 km "
                               "away and would be heard at ~1:47; too far to give these strong lines.)"},
        "source": "Freesound (HQ preview MP3; the original needs a login)",
        "source_url": "https://freesound.org/people/klankbeeld/sounds/858591/",
        "title": "prop plane voice insects birds fens Kampina Netherlands 1243 pm 250718_1173",
        "recordist": "klankbeeld",
        "licence": "CC BY 4.0",
        "licence_url": "https://creativecommons.org/licenses/by/4.0/",
        "lat": 51.57401, "lon": 5.27708, "elev_m": 10, "geoid_m": 44.0,
        "location_uncertainty_m": 50, "location_note": "Recordist's GPS from the description ('Zandbergse vennen ... GPS 51.57401 5.27708'); OSM puts Zandbergsvennen at the same spot. The Freesound map pin (51.5787, 5.2573) is ~1.5 km west and is not used.",
        "place": "Zandbergse vennen, Kampina, Van Gogh National Park, Boxtel",
        "region": "Noord-Brabant", "country": "Netherlands",
        "timezone": "Europe/Amsterdam",
        "local_start": "2025-07-18T12:43:00",
        "start_uncertainty_s": 180,
        "time_note": "Time from title and description (12:43). Date conflict as in fs858688: title 18 July, "
                     "description 'July 17, 2025'; ADS-B supports the 18th (see fs858688).",
        "alt_dates": ["2025-07-17"],
        "audible": "'A prop plane over fens and heathlands' (recordist): two clear Doppler lines, closest "
                   "around 1:45.",
        "audio_file": "audio.mp3",
    },
    {
        "id": "xc941260",
        "short": "Seattle (Wedgwood)",
        "adsb_match": {"reg": "N486AS", "det": None,
                       "note": "Alaska Airlines Boeing 737-900 on approach at 4425 ft passing almost overhead (0.05 km "
                               "horizontal, 1.3 km slant). So the 'airplane flyover' is an airliner, not a propeller aircraft; the "
                               "detector does not pick up its line."},
        "source": "xeno-canto",
        "source_url": "https://xeno-canto.org/941260",
        "title": "XC941260 Soundscape, Wedgwood, Seattle",
        "recordist": "Whitney Neufeld-Kaiser",
        "licence": "CC BY-NC-SA 4.0",
        "licence_url": "https://creativecommons.org/licenses/by-nc-sa/4.0/",
        "lat": 47.6766, "lon": -122.294, "elev_m": 100, "geoid_m": -22.4,
        "location_uncertainty_m": 300, "location_note": "xeno-canto coordinates (4 decimals) given by the recordist.",
        "place": "Wedgwood, Seattle, King County",
        "region": "Washington", "country": "United States",
        "timezone": "America/Los_Angeles",
        "local_start": "2024-03-13T08:46:00",
        "start_uncertainty_s": 300,
        "time_note": "xeno-canto date/time field (minute precision; xeno-canto asks for the start time). "
                     "Beginning and end were trimmed by the recordist, so the true start may be a little later.",
        "audible": "'An annoyingly loud airplane flyover in the middle of the recording' (recordist): one "
                   "strong falling Doppler line with ground-reflection interference from ~0:50 to ~1:50.",
        "audio_file": "audio.wav",
    },
    {
        "id": "aporee_62753_72191",
        "short": "Edgemere, Queens NY",
        "adsb_candidates": [{"reg": "N913DU", "verdict": "rejected",
                             "note": "Delta B739 on approach to JFK, heard ~2 s from the detector's tc. Rejected: "
                                     "approach jets pass at this same ~2.2 km slant every 2-3 min (9 in the window), "
                                     "so a timing coincidence within the +-10 min uncertainty is expected, and the "
                                     "sound is a helicopter (see notes)."}],
        "notes": "The fly-by is a helicopter, not an airliner: it shows a dense harmonic comb (14+ lines below 300 Hz) "
                 "whose spacing falls from 21.6 Hz (approach) to 14.7 Hz (recede), i.e. a rest spacing of ~17.5 Hz "
                 "and ~65 m/s (scripts/testset/comb.py). That is a main-rotor blade-passing comb; jet fan tones of an "
                 "airliner on approach lie far higher and are not a 17 Hz comb. No helicopter appears in adsb.lol "
                 "within 6 km in the window, so treat this like Mechlin: a clear fly-by the matcher must leave "
                 "unmatched (or at most match to an unknown non-ADS-B aircraft). An earlier version of this page "
                 "attributed it to the B739; that was wrong.",
        "source": "radio aporee (Internet Archive mirror)",
        "source_url": "https://aporee.org/maps/?loc=62753&snd=72191",
        "archive_url": "https://archive.org/details/aporee_62753_72191",
        "title": "Edgemere, Queens, New York - After bathing time",
        "recordist": "Peter Stets",
        "licence": "Public Domain Mark 1.0",
        "licence_url": "https://creativecommons.org/publicdomain/mark/1.0/",
        "lat": 40.590734589389, "lon": -73.767042983697, "elev_m": 0, "geoid_m": -32.7,
        "location_uncertainty_m": 100, "location_note": "radio aporee map pin placed by the recordist.",
        "place": "Edgemere beach, Queens, New York City",
        "region": "New York", "country": "United States",
        "timezone": "America/New_York",
        "local_start": "2023-07-10T18:11:00",
        "start_uncertainty_s": 600,
        "time_note": "Date and time from the radio aporee map entry.",
        "audible": "Waves, siren, 'a helicopter which flew along the shore and some planes from JFK' "
                   "(recordist). Helicopter harmonics very clear from ~0:20 to ~1:40, closest ~1:07.",
        "audio_file": "audio.mp3",
    },
    {
        "id": "aporee_64975_75055",
        "short": "Jersey City waterfront",
        "adsb_candidates": [{"reg": "N821SS", "verdict": "ambiguous",
                             "note": "Sikorsky S-76, 0.9 km slant, heard 1:43 into the file at the stated start, "
                                     "the nearest in time to the loud pass (Doppler midpoint ~2:18). But in the "
                                     "+-10 min window there are 11 helicopter passes within 2 km slant (Hudson "
                                     "corridor): 6 by Bell 206s, 4 by Bell 407s (N38BL, N403TD, N84BL, N401TD) and "
                                     "this S-76, any of which fits if the start is off by a few minutes. The rotor comb (rest ~22 Hz, see notes) points to a 4-blade rotor "
                                     "(S-76 ~19.5 Hz, B407 ~27.5 Hz by published rotor speed) and argues against the "
                                     "2-blade Bell 206 / R44 (~13 Hz), but cannot pick one aircraft. Not ground "
                                     "truth."}],
        "notes": "Rotor comb (scripts/testset/comb.py): spacing 26.9 Hz on approach, ~18.9 Hz receding, so ~22 Hz at "
                 "rest and ~60 m/s. ADS-B was checked on both UTC days (the window crosses midnight). The detector "
                 "misses this very clear, fast, close pass.",
        "source": "radio aporee (Internet Archive mirror)",
        "source_url": "https://aporee.org/maps/?loc=64975&snd=75055",
        "archive_url": "https://archive.org/details/aporee_64975_75055",
        "title": "3 2nd St, Jersey City - Waves near Harborside and Hudson River Waterfront walkway",
        "recordist": "rsj49",
        "licence": "CC BY-SA 3.0",
        "licence_url": "https://creativecommons.org/licenses/by-sa/3.0/",
        "lat": 40.72087479385, "lon": -74.03134603807, "elev_m": 0, "geoid_m": -32.8,
        "location_uncertainty_m": 100, "location_note": "radio aporee map pin; the title gives the street address (3 2nd St).",
        "place": "Hudson River Waterfront walkway near Harborside light-rail station, Jersey City",
        "region": "New Jersey", "country": "United States",
        "timezone": "America/New_York",
        "local_start": "2024-08-11T19:50:00",
        "start_uncertainty_s": 600,
        "time_note": "Date and time from the radio aporee map entry.",
        "audible": "Waves, pedestrians; 'a helicopter can be heard passing overhead' (recordist). Very "
                   "clear, fast and close helicopter fly-by from ~1:30 to ~2:20, closest ~2:05.",
        "audio_file": "audio.mp3",
    },
    {
        "id": "aporee_60944_74426",
        "notes": "ADS-B: only airliners above 10 000 ft within 20 km in the window; the helicopter is not in "
                 "adsb.lol. A hard case for the matcher (clear fly-by, nothing to match).",
        "short": "Kirchmöser, Brandenburg an der Havel",
        "source": "radio aporee fieldradio stream (Internet Archive mirror)",
        "source_url": "https://aporee.org/maps/?loc=60944&snd=74426",
        "archive_url": "https://archive.org/details/aporee_60944_74426",
        "title": "Wusterauer Anger, Kirchmöser Ost - garden ambience, morning rain, helicopter",
        "recordist": "radio aporee fieldradio (fixed remote microphone)",
        "licence": "CC BY-SA 3.0",
        "licence_url": "https://creativecommons.org/licenses/by-sa/3.0/",
        "lat": 52.383656, "lon": 12.440769, "elev_m": 32, "geoid_m": 41.1,
        "location_uncertainty_m": 100, "location_note": "radio aporee map pin of the fixed fieldradio microphone.",
        "place": "Wusterauer Anger, Kirchmöser, Brandenburg an der Havel",
        "region": "Brandenburg", "country": "Germany",
        "timezone": "Europe/Berlin",
        "local_start": "2024-06-15T09:19:00",
        "start_uncertainty_s": 300,
        "time_note": "Time from the title and map entry of a 10-minute excerpt of a live remote-microphone stream.",
        "audible": "Garden ambience, rain, a helicopter (title). Clear Doppler fly-by with harmonics, "
                   "closest ~4:49; a second weaker one later.",
        "audio_file": "audio.mp3",
    },
    {
        "id": "aporee_66516_76914",
        "short": "Berlin-Neukölln",
        "adsb_match": {"reg": "D-EMFM", "det": 0,
                       "note": "Cessna 152 at 1500 ft pressure altitude, ~510 m above the site after the QNH correction "
                               "(that morning geometric altitudes ran ~475 ft above pressure altitudes, i.e. high "
                               "pressure), 0.6 km slant, heard 8:50 into the file, 17 s after the detector's tc. So the fan of lines at "
                               "~8:33 is this light aircraft, not the helicopter of the title (an EC145 D-HYAS passed "
                               "4 km away ~3 min before the file starts). The detector flags it 'ground?' because "
                               "its fitted speed (39 m/s) is low."},
        "source": "radio aporee fieldradio stream (Internet Archive mirror)",
        "source_url": "https://aporee.org/maps/?loc=66516&snd=76914",
        "archive_url": "https://archive.org/details/aporee_66516_76914",
        "title": "Emmauswald, Neukölln, Berlin - urban forest ambience, w/ goshawk, churchbells, helicopter",
        "recordist": "radio aporee fieldradio (fixed remote microphone)",
        "licence": "CC BY-SA 3.0",
        "licence_url": "https://creativecommons.org/licenses/by-sa/3.0/",
        "lat": 52.462273, "lon": 13.424262, "elev_m": 53, "geoid_m": 39.7,
        "location_uncertainty_m": 100, "location_note": "radio aporee map pin of the fixed fieldradio microphone.",
        "place": "Emmauswald, Neukölln, Berlin",
        "region": "Berlin", "country": "Germany",
        "timezone": "Europe/Berlin",
        "local_start": "2025-02-09T09:58:00",
        "start_uncertainty_s": 300,
        "time_note": "Time from the title and map entry of a 10-minute excerpt of a live remote-microphone stream.",
        "audible": "Urban forest, goshawk, church bells, a helicopter (title). Helicopter harmonics fan "
                   "out around 8:20-8:45, closest ~8:33.",
        "audio_file": "audio.mp3",
    },
    {
        "id": "xc1152845",
        "notes": "ADS-B (+-1 h window): the only low traffic near the site is a Citation jet (D-CXLS, 1.9 km, "
                 "10:00 UTC) and an unregistered French address at 83 kt, 1550 ft, 8.4 km (09:41 UTC), which "
                 "is slow enough to be a helicopter but too far for the loud pass in the file. No convincing match: "
                 "treat as a hard case where the helicopter is not in ADS-B.",
        "short": "Mondeville (Caen)",
        "source": "xeno-canto",
        "source_url": "https://xeno-canto.org/1152845",
        "title": "XC1152845 Marsh Warbler, parc du Biez, Mondeville",
        "recordist": "Guillaume DEBOUT",
        "licence": "CC BY-NC-SA 4.0",
        "licence_url": "https://creativecommons.org/licenses/by-nc-sa/4.0/",
        "lat": 49.1815, "lon": -0.3151, "elev_m": 10, "geoid_m": 47.1,
        "location_uncertainty_m": 300, "location_note": "xeno-canto coordinates (4 decimals) given by the recordist.",
        "place": "Parc du Biez, Mondeville (near Caen)",
        "region": "Calvados, Normandie", "country": "France",
        "timezone": "Europe/Paris",
        "local_start": "2026-07-04T12:00:00",
        "start_uncertainty_s": 3600,
        "time_note": "xeno-canto time is a round 12:00, so treat it as 'around midday' (we assume +-1 h).",
        "audible": "Urban park, traffic, 'hélicoptère' (recordist). Clear helicopter fly-by, closest ~2:28.",
        "audio_file": "audio.mp3",
    },
    {
        "id": "xc883367",
        "short": "Dünserberg",
        "adsb_candidates": [{"hex": "3e6f61", "verdict": "speculative",
                             "note": "Unregistered German address passing 0.6 km slant (~525 m above the site) at 11:35 UTC, which "
                                     "needs the stated 12:00 local start to be ~33 min early. Not supported: its "
                                     "trace (~100 kt climbing to ~8350 ft pressure altitude, then a turn and a steady 75-85 kt descent) "
                                     "looks like a light aircraft or motor glider rather than a helicopter, the "
                                     "detector puts the source at ~2.7 km (4.8x further), and the slow change of the "
                                     "comb over ~2 min also fits a distant source. Another unregistered target "
                                     "(3ff878, 76 kt, ~6800 ft) passed 11 km away at 10:59. Not ground truth."}],
        "notes": "A helicopter by its sound: a harmonic comb with ~21 Hz spacing at rest (23 Hz -> 19 Hz from 0:40 to "
                 "2:55, scripts/testset/comb.py). No convincing ADS-B source; treat as unmatched.",
        "source": "xeno-canto",
        "source_url": "https://xeno-canto.org/883367",
        "title": "XC883367 Soundscape, Dünserberg (mit Hubschrauber)",
        "recordist": "Georg Amann",
        "licence": "CC BY-NC-SA 4.0",
        "licence_url": "https://creativecommons.org/licenses/by-nc-sa/4.0/",
        "lat": 47.2365, "lon": 9.7341, "elev_m": 1600, "geoid_m": 47.1,
        "location_uncertainty_m": 500, "location_note": "xeno-canto coordinates (4 decimals); the locality is the whole Gemeinde.",
        "place": "Gemeinde Dünserberg, Feldkirch district",
        "region": "Vorarlberg", "country": "Austria",
        "timezone": "Europe/Vienna",
        "local_start": "2024-03-20T12:00:00",
        "start_uncertainty_s": 3600,
        "time_note": "xeno-canto time is a round 12:00; treat as 'around midday' (+-1 h assumed).",
        "audible": "Subalpine spruce forest, 'helicopter' (remarks; the file name says 'mit Hubschrauber'). "
                   "Helicopter harmonics from ~1:00 to ~3:30, closest ~2:26. Mountain site, 1600 m.",
        "audio_file": "audio.mp3",
    },
    {
        "id": "fs847111",
        "adsb_match": {"reg": "D-HAIB", "det": 0, "caveat": "only if the file starts ~8 min after the stated time",
                       "note": "Robinson R44 helicopter at 2275 ft pressure / 2750 ft geometric altitude (~380 m above "
                               "the site), 1.6 km slant, the only low aircraft within 6 km in the window. Its CPA is heard 12:02 after the stated 13:30 start, while the "
                               "recordist marks the helicopter at 03:00 and the detector puts it at 3:39, so the "
                               "file most likely starts ~13:38 local (it is 33:43 long, not the stated hour, so a start "
                               "after 13:30 is consistent with the description). This is a match only under that "
                               "8-minute shift; another R44, D-HTAD, passed 4.4 km away ~22 min before 13:30. The "
                               "27:45 'large military aircraft' is not in ADS-B."},
        "short": "Altenthann (Regensburg)",
        "source": "Freesound (HQ preview MP3; the original needs a login)",
        "source_url": "https://freesound.org/people/TSP-Talk/sounds/847111/",
        "title": "Altenthann - Midday Rural Village Ambience with Birds, Children, Distant Traffic & "
                 "Aircraft Events (03 Mar 2026)",
        "recordist": "TSP-Talk",
        "licence": "CC0 1.0",
        "licence_url": "https://creativecommons.org/publicdomain/zero/1.0/",
        "lat": 49.08138, "lon": 12.3007, "elev_m": 400, "geoid_m": 46.3,
        "location_uncertainty_m": 1500, "location_note": "Freesound map pin only; it lies ~1.3 km south of the village centre that Nominatim gives (49.0931, 12.3038), and the description just says 'Altenthann'.",
        "place": "Altenthann (93177), Regensburg district (Freesound geotag)",
        "region": "Bavaria", "country": "Germany",
        "timezone": "Europe/Berlin",
        "local_start": "2026-03-03T13:30:00",
        "start_uncertainty_s": 900,
        "time_note": "Description: 'recorded on 03 March 2026 between 13:30-14:30 CET'. The file is only 33:43 "
                     "long, so it can start anywhere from 13:30 to ~13:56; we use 13:30 +-15 min. The only ADS-B "
                     "candidate needs a start of ~13:38.",
        "audible": "Village ambience; recordist marks '03:00 a distinctly audible low-altitude "
                   "aircraft, likely a small helicopter' and '27:45 a large military aircraft flying overhead'. "
                   "The helicopter is very clear (closest ~3:40); the second event is broadband and weaker.",
        "notes": "The description says 13:30-14:30 but the file is 33:43 long, so it is either an excerpt or "
                 "the end time is loose; the start time is what matters here.",
        "audio_file": "audio.mp3",
    },
]

FINDINGS = """
- 11 recordings from 6 countries (Poland, Netherlands, Germany, France, Austria, USA). In all of them
  a fly-by is clearly visible in the spectrogram and audible.
- adsb.lol gives a defensible source (`adsb_match`) for 5 of them: 4 light aircraft or helicopters
  (Kampina C172 and DR400, Berlin C152, Altenthann R44) and 1 airliner on approach (Seattle B739,
  which the detector does not pick up). Altenthann holds only if the file starts ~8 min after the
  stated 13:30, which the description ("13:30-14:30", but the file is 34 min long) allows.
- 3 more have ADS-B candidates that are not ground truth: Jersey City (S-76 N821SS is nearest in time,
  but 11 helicopter passes lie within 2 km inside the +-10 min window; both UTC days were checked),
  Dünserberg (an unregistered target 0.6 km away, but only with a 33 min shift, a ~5x distance mismatch
  and a flight profile that does not look like a helicopter) and Edgemere (a B739 on approach heard
  within 2 s of the detector's tc, rejected: the sound has a ~17.5 Hz rotor comb, and approach jets pass
  at that distance every 2-3 min, so a timing coincidence is expected).
- So 6 clear fly-bys have no usable ADS-B source (Mechlin PL, Kirchmöser DE, Mondeville FR, Edgemere
  US, Jersey City US, Dünserberg AT): cases the matcher must leave unmatched, or match only with low
  confidence, rather than force a wrong match. Helicopters often do not broadcast ADS-B.
- Stated times: for 3 of the 4 prop/heli matches the ADS-B sound arrives within 41 s of the detector's
  tc at the stated start (+7, +41, -17 s); Altenthann needs ~8 min. Round times like 12:00 mean little.
  The pipeline should treat the recording time as a hint with a generous window. The Kampina pair
  also has a date conflict (title 18 July, description 17 July); ADS-B settles it for the 18th.
- Locations: map pins can be well off. The Kampina Freesound pins are ~1.5 km from the recordist's GPS
  (now used), and the Altenthann pin is 1.3 km from the village centre. Each entry now has a
  `location_uncertainty_m`; the pipeline's error should be judged against it.
- Altitudes now use geometric (GNSS) height, or pressure altitude corrected with the local
  geom-baro offset. That offset ranged from -100 ft (New York, July) to +650 ft (Normandy, July) and
  was +475 ft in Berlin (a February high), so raw pressure altitude would have put aircraft up to
  ~150 m too low. For Kampina the offset (+225 ft) fits the recordist's 1015 hPa plus the 44 m
  geoid (expected ~+190 ft), which supports reading readsb's geometric altitude as ellipsoidal.
- Detector: it finds the fly-by in 9 of 11 (misses the Seattle airliner and the fast, close Jersey City
  helicopter). Against the 4 prop/heli matches (table below) fitted speed is -33 % to +22 % of the
  ADS-B ground speed (48/52, 40/60, 39/48, 77/63 m/s) and fitted distance 0.6-1.6x the ADS-B slant CPA
  (2391/2637, 1241/759, 391/619, 1203/1568 m): noisy, with no consistent bias in 4 cases. The
  'likely_ground_vehicle' flag fires on the Berlin C152 and the Mechlin helicopter, so that flag
  must not be used to drop candidates.
- Helicopter recordings show a main-rotor harmonic comb (`scripts/testset/comb.py`): rest spacing
  ~14 Hz at Mechlin (a 2-blade light helicopter would fit), ~17.5 Hz at Edgemere, ~21 Hz at Dünserberg,
  ~22 Hz at Jersey City. Comb spacing is a possible cue for the helicopter type in a later matcher.
- Corrections after an independent review: Kampina location (recordist GPS instead of the map pin),
  Edgemere (B739 rejected), Dünserberg and Jersey City (downgraded to candidates), geometric altitude,
  flown instead of reported ground speed, Jersey City second UTC day, Altenthann +-15 min, Kampina
  date conflict, licence summary, Mondeville recordist. An earlier version claimed 8 of 11 matches.
"""

# Candidates looked at and not used, with the reason.
REJECTED = [
    {"name": "radio aporee 62004/71347 (Reykjavík harbour, wind harp and helicopter, 2023-10-23 16:08)",
     "url": "https://aporee.org/maps/?loc=62004&snd=71347",
     "why": "Superb, very clear helicopter fly-by, but adsb.lol has no aircraft at all within 20 km of "
            "Reykjavík in the window (no feeder coverage there in 2023), so nothing to verify against."},
    {"name": "xeno-canto XC1145145 (Poland, Corn Bunting, 2025-05-24)", "url": "https://xeno-canto.org/1145145",
     "why": "'Passing aircraft' is only a faint broadband rumble; no tonal Doppler line; detector finds nothing."},
    {"name": "xeno-canto XC1093293 (Croatia, Eagle-Owl, 2026-03-05)", "url": "https://xeno-canto.org/1093293",
     "why": "Airplane noise is broadband (jet), no lines; detector finds nothing."},
    {"name": "xeno-canto XC960630 / XC960631 (Frankfurt, 2024-12-25)", "url": "https://xeno-canto.org/960631",
     "why": "Airliners on approach (broadband, ground-reflection fringes) and a 400 Hz high-pass by the recordist; "
            "detections are unconfident. Usable later as a jet test, not for propeller/helicopter lines."},
    {"name": "xeno-canto XC1050957 (Haverleij, NL, military helicopters, 2025-10-27 20:10)",
     "url": "https://xeno-canto.org/1050957",
     "why": "Only the receding half of a helicopter pass (first 30 s); military helicopters often fly without "
            "ADS-B. Checked: nothing in adsb.lol below 6 km slant in the window (nearest a GLF4 at 16 km)."},
    {"name": "xeno-canto XC961711 (Westland, New Zealand, helicopter tours, 2024-11-04)",
     "url": "https://xeno-canto.org/961711",
     "why": "Helicopter lines only while receding (first ~30 s of a 92 s clip); ADS-B feeder coverage of the "
            "South Island West Coast is poor. Checked: adsb.lol has a single airliner at 37 000 ft in the window."},
    {"name": "xeno-canto XC1080406, XC1092456, XC1105861, XC904490, XC905295", "url": "https://xeno-canto.org/1092456",
     "why": "Aircraft mentioned in the remarks but no tonal Doppler line visible in the spectrogram; detector "
            "finds nothing."},
    {"name": "xeno-canto, 12 long unedited Polish soundscapes (XC1136486, XC1148061/2, XC905328, XC905892, "
             "XC1144936-45, XC928277)", "url": "https://xeno-canto.org/1144945",
     "why": "Mined blind for fly-bys (no aircraft in the remarks): the detector found only two slow "
            "'ground?' tracks, nothing aircraft-like."},
    {"name": "xeno-canto XC792401 (Austria, snipe display with airplane noise)", "url": "https://xeno-canto.org/792401",
     "why": "High-pass filtered below 150 Hz by the recordist, which removes the propeller band."},
    {"name": "xeno-canto XC906184 / radio aporee 64294 (Malton, UK, light aircraft take-off)",
     "url": "https://xeno-canto.org/906184",
     "why": "'Time edited extended recording': cut together, so file time does not map to clock time."},
    {"name": "xeno-canto XC941347 (Arizona, low-flying helicopter)", "url": "https://xeno-canto.org/941347",
     "why": "'Interval shortened' (edited); helicopter loiters rather than passes."},
    {"name": "Freesound 870002 (klankbeeld, Bokhoven NL, small prop plane, 2025-06-11)",
     "url": "https://freesound.org/people/klankbeeld/sounds/870002/",
     "why": "Plane is weak and masked by waves and traffic; one unconfident detection; date in the title has a typo "
            "('2500611')."},
    {"name": "Freesound 842369 (klankbeeld, Apache helicopters, 's-Hertogenbosch, 2026-01-16)",
     "url": "https://freesound.org/people/klankbeeld/sounds/842369/",
     "why": "Military helicopters 'flying high'; weak lines, one confident detection only; Apaches rarely "
            "broadcast ADS-B. Checked: no Apache in adsb.lol; a police AW139 (PH-PXZ) passed overhead 6 min "
            "before the stated start, so it is not in the file unless the time is off."},
    {"name": "Freesound 853568 (klankbeeld, distant Chinook, Hedikhuizen NL, 2026-03-03)",
     "url": "https://freesound.org/people/klankbeeld/sounds/853568/",
     "why": "Chinook is distant and masked by a cargo ship's engine; detections unconfident."},
    {"name": "Freesound 834307 (klankbeeld, two far C-130s, 2025-03-20)",
     "url": "https://freesound.org/people/klankbeeld/sounds/834307/",
     "why": "Far military transports; detections unconfident (d ~ 12 km)."},
    {"name": "Freesound 740864 (Brussels police helicopter, 2024-06-11)",
     "url": "https://freesound.org/people/Virgile_Loiseau/sounds/740864/",
     "why": "Helicopter circles/hovers over the city for half an hour rather than passing; start time only "
            "implied by a file reference ('24_06_11_22_01')."},
    {"name": "Freesound 761008 (Nottingham police helicopter), 853223 (Barcelona helicopter), 784764, 801891",
     "url": "https://freesound.org/people/carthny/sounds/761008/",
     "why": "No time of day stated (date only)."},
    {"name": "radio aporee 67246/77796 (Wallingford UK, 'light aircraft and their falling tones')",
     "url": "https://aporee.org/maps/?loc=67246&snd=77796",
     "why": "A soundwalk: the recordist moves along the river, so there is no single true location."},
    {"name": "radio aporee 60721/69782 (Bois Saint-Martin near Lognes aerodrome, light aircraft)",
     "url": "https://aporee.org/maps/?loc=60721&snd=69782",
     "why": "Light aircraft present but weak and partly masked by the nestlings; detections mostly unconfident. "
            "Checked: adsb.lol only has airliners at cruise level there in the window."},
    {"name": "radio aporee 69616/81056 (Voila, Romania, military helicopter)",
     "url": "https://aporee.org/maps/?loc=69616&snd=81056",
     "why": "60 s, normalised, helicopter mostly broadband; detector finds nothing."},
    {"name": "radio aporee 60445/69430 (Braunschweig, whitethroat with helicopter)",
     "url": "https://aporee.org/maps/?loc=60445&snd=69430", "why": "Helicopter too faint; detector finds nothing."},
    {"name": "radio aporee 65363/75502 (Ridgeway, Marlborough UK, light aircraft and motorbikes)",
     "url": "https://aporee.org/maps/?loc=65363&snd=75502",
     "why": "Only slow 'ground?' detections (motorbikes); aircraft faint."},
    {"name": "YouTube 'Citizen Observer Perth' (e.g. JZE1i5mBAuY, k6PiaLJShWo)",
     "url": "https://www.youtube.com/watch?v=JZE1i5mBAuY",
     "why": "Exact date, time and registration, but the filming location is not stated."},
    {"name": "YouTube spotting channels (e.g. 32TFS Aviation)", "url": "https://www.youtube.com/watch?v=8BF8vCIgCBM",
     "why": "Edited compilations (cuts break the timeline), and the dated ones found were before 2023."},
    {"name": "GBIF mirror of xeno-canto; xeno-canto API v3", "url": "https://api.gbif.org/v1/occurrence/search",
     "why": "GBIF returned 503 'too busy' all session; xeno-canto API v3 needs a personal key, so the public "
            "search pages were scraped instead."},
]


def utc_window(e, pad_s=300):
    tz = ZoneInfo(e["timezone"])
    start = datetime.fromisoformat(e["local_start"]).replace(tzinfo=tz)
    dur = e.get("duration_s") or 0
    unc = e["start_uncertainty_s"]
    t0 = (start - timedelta(seconds=unc + pad_s)).astimezone(timezone.utc)
    t1 = (start + timedelta(seconds=dur + unc + pad_s)).astimezone(timezone.utc)
    return start, t0, t1


def duration_of(path):
    import subprocess
    out = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path],
                         capture_output=True, text=True)
    try:
        return float(out.stdout.strip())
    except ValueError:
        return None


def main():
    cmd, root = sys.argv[1], sys.argv[2]
    by_day = {}
    for e in ENTRIES:
        d = os.path.join(root, e["id"])
        found = sorted(glob.glob(os.path.join(d, "audio.*")))
        if found:
            e["audio_file"] = os.path.basename(found[0])
        audio = os.path.join(d, e["audio_file"])
        if os.path.exists(audio):
            e["duration_s"] = round(duration_of(audio) or 0, 1)
        start, t0, t1 = utc_window(e)
        e["utc_start"] = start.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        e["utc_offset"] = start.strftime("%z")
        if cmd == "meta":
            if not os.path.isdir(d):
                print("missing dir", d)
                continue
            meta_path = os.path.join(d, "meta.json")
            old = {}
            if os.path.exists(meta_path):
                with open(meta_path) as f:
                    old = json.load(f)
            meta = dict(e)
            for k in ("adsb", "detector"):
                if k in old:
                    meta[k] = old[k]
            with open(meta_path, "w") as f:
                json.dump(meta, f, indent=2, ensure_ascii=False)
            print("wrote", meta_path)
        elif cmd == "sites":
            if e["lat"] is None:
                continue
            site = {"id": e["id"], "lat": e["lat"], "lon": e["lon"], "elev_m": e["elev_m"],
                    "geoid_m": e.get("geoid_m", 0.0)}
            for day in sorted({t0.strftime("%Y-%m-%d"), t1.strftime("%Y-%m-%d")}):
                by_day.setdefault(day, []).append(dict(site, t0=t0.strftime("%Y-%m-%dT%H:%M:%SZ"),
                                                       t1=t1.strftime("%Y-%m-%dT%H:%M:%SZ")))
            # Alternative dates (conflicting sources): same local time on another day, id "<id>@<date>".
            for alt in e.get("alt_dates", []):
                shift = datetime.fromisoformat(alt).date() - start.date()
                a0, a1 = t0 + shift, t1 + shift
                for day in sorted({a0.strftime("%Y-%m-%d"), a1.strftime("%Y-%m-%d")}):
                    by_day.setdefault(day, []).append(dict(site, id=f"{e['id']}@{alt}",
                                                           t0=a0.strftime("%Y-%m-%dT%H:%M:%SZ"),
                                                           t1=a1.strftime("%Y-%m-%dT%H:%M:%SZ")))
    if cmd == "sites":
        # Merge into existing files: entries of this catalog are replaced, any other (hand-added) sites kept.
        out = sys.argv[3]
        os.makedirs(out, exist_ok=True)
        for day, sites in sorted(by_day.items()):
            path = os.path.join(out, f"sites_{day}.json")
            ids = {x["id"] for x in sites}
            keep = []
            if os.path.exists(path):
                with open(path) as f:
                    keep = [x for x in json.load(f) if x["id"] not in ids]
            merged = sites + keep
            changed = True
            if os.path.exists(path):
                with open(path) as f:
                    changed = json.load(f) != merged
            if changed:
                with open(path, "w") as f:
                    json.dump(merged, f, indent=1)
            print(day, "changed" if changed else "same", [x["id"] for x in merged])

if __name__ == "__main__":
    main()
