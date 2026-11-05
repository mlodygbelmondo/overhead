#!/bin/bash
# All locate modes on the long recordings, one after the other (pinned via run_locate.py, CORES=12-15).
# usage: locate_all.sh OUTDIR ID [ID ...]
OUT=$1; shift
H=$(dirname "$(readlink -f "$0")")
for mode in start hours day exact; do
  python3 -I "$H/run_locate.py" "$OUT" "$mode" "$@"
done
