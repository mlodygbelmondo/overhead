#!/bin/bash
# Stage 1 ('overhead-cli detect', defaults, as in scripts/evidence/detect_all.sh) on the given test-set recordings,
# pinned to the given cores; writes <id>/detect.json and <id>/detect.stderr.txt.
# usage: detect.sh CORES ID [ID ...]
CORES=$1; shift
D=${OVERHEAD_BIN:-${OVERHEAD_DATA:-$HOME/overhead-data}/target-longrec/release/overhead-cli}
T=${OVERHEAD_DATA:-$HOME/overhead-data}/testset
export RAYON_NUM_THREADS=4
for id in "$@"; do
  a=$(ls "$T/$id"/audio.* | head -1)
  s=$(date +%s)
  taskset -c "$CORES" "$D" detect "$a" > "$T/$id/detect.json" 2> "$T/$id/detect.stderr.txt"
  e=$(date +%s)
  n=$(python3 -I -c "import json,sys; print(len(json.load(open(sys.argv[1]))))" "$T/$id/detect.json")
  echo "$id: $n detections in $((e - s)) s"
done
