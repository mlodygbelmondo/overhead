#!/bin/bash
# Sequential adsb.lol day fetches pinned to cores 12-15. usage: fetch_queue.sh DAY...
B=${OVERHEAD_DATA:-$HOME/overhead-data}/target-evidence/release/overhead-adsb
for d in "$@"; do
  echo "=== $d $(date +%T)"
  taskset -c 12-15 $B fetch $d 2>&1 | tail -5
  echo "=== $d done $(date +%T)"
done
