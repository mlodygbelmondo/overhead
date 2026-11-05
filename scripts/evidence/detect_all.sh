#!/bin/bash
# Stage 1 with the current binary on every test-set recording (pinned to the given cores).
# usage: detect_all.sh CORES OUTDIR
# EXCLUDE_LONG=1 skips the long recordings of docs/testset.md "Long recordings with several fly-bys" (their
# meta.json has a "status" key; ~4 h of audio).
CORES=${1:-12-15}; OUT=${2:-${OVERHEAD_DATA:-$HOME/overhead-data}/evidence/detect}
D=${OVERHEAD_DATA:-$HOME/overhead-data}/target-evidence/release/overhead-cli
T=${OVERHEAD_DATA:-$HOME/overhead-data}/testset
mkdir -p "$OUT"
export RAYON_NUM_THREADS=4
for dir in "$T"/*/; do
  id=$(basename "$dir")
  [ -f "$dir/meta.json" ] || continue
  [ -n "$EXCLUDE_LONG" ] && grep -q '"status"' "$dir/meta.json" && continue
  a=$(ls "$dir"/audio.* | head -1)
  s=$(date +%s.%N)
  taskset -c "$CORES" "$D" detect "$a" > "$OUT/$id.json" 2> "$OUT/$id.stderr.txt"
  e=$(date +%s.%N)
  echo "$id $(python3 -c "import json,sys; d=json.load(open(sys.argv[1])); d=d if isinstance(d,list) else d.get('detections',[]); print(len(d))" "$OUT/$id.json") detections, $(python3 -c "import sys; print(round(float(sys.argv[2])-float(sys.argv[1]),1))" $s $e) s"
done
