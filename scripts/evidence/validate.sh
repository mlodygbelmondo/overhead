#!/usr/bin/env bash
# Run the whole real test set (every recording, every input mode) with one
# overhead-adsb binary, for before/after comparisons.
#
# usage: validate.sh BIN OUTDIR [JOBS]
#   BIN     overhead-adsb binary to test
#   OUTDIR  results: <id>-<mode>.{json,log,time}, summary.json, summary.txt
#   JOBS    runs at a time (default 4, each with 4 threads)
#
# The recordings and what locate is told about each are those of
# scripts/longrec/run_locate.py (docs/evidence.md: exact / stated start /
# hours / day, region = province or state + country). Stage 1 runs inside
# locate, so the binary's own detector is used. Compare two result folders
# with compare.py.
set -euo pipefail
BIN=$(realpath "$1")
OUT=$(realpath -m "$2")
JOBS=${3:-4}
HERE=$(cd "$(dirname "$0")" && pwd)
DATA=${OVERHEAD_DATA:-${DOPPLER_DATA:-$HOME/overhead-data}}
mkdir -p "$OUT"

EXACT="fs858688 fs858591 aporee_66516_76914 fs847111 fs864478 fs794275 fs801327 fs749484"
jobs=()
for dir in "$DATA"/testset/*/; do
    id=$(basename "$dir")
    [ -f "$dir/meta.json" ] || continue
    audio=$(python3 -I -c "import json,sys; print(json.load(open(sys.argv[1])).get('audio_file',''))" "$dir/meta.json")
    [ -n "$audio" ] && [ -f "$dir/$audio" ] || continue
    for mode in exact start hours day; do
        if [ "$mode" = exact ] && ! grep -qw "$id" <<<"$EXACT"; then continue; fi
        [ -f "$OUT/$id-$mode.time" ] || jobs+=("$mode $id")
    done
done
echo "${#jobs[@]} runs to do in $OUT"
printf '%s\n' "${jobs[@]}" | OVERHEAD_ADSB="$BIN" CORES="0-$(($(nproc) - 1))" THREADS=4 \
    xargs -P "$JOBS" -L 1 bash -c 'python3 -I "$0/../longrec/run_locate.py" "$1" $2 $3' "$HERE" "$OUT"
python3 -I "$HERE/analyze.py" "$OUT" --json "$OUT/summary.json" >"$OUT/summary.txt"
echo "summary: $OUT/summary.txt"
