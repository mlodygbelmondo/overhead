#!/bin/bash
# ADS-B ground truth for the long recordings: passes within 10 km slant from the overhead-adsb cache, and
# the one-to-one pairing of stage-1 detections with them (common start offset, +-15 s).
# Writes <id>/adsb_hits.json and <id>/adsb_hits.txt.
# usage: pair_all.sh ID [ID ...]
T=${OVERHEAD_DATA:-$HOME/overhead-data}/testset
H=$(dirname "$(readlink -f "$0")")
for id in "$@"; do
  python3 -I "$H/cache_hits.py" "$T/$id/meta.json" --radius-km 10 --detect "$T/$id/detect.json" --tol 15 \
    --out "$T/$id/adsb_hits.json" > "$T/$id/adsb_hits.txt"
  grep '== detections' "$T/$id/adsb_hits.txt" | sed "s/^/$id /"
done
