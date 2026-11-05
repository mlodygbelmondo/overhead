#!/bin/bash
# Render docs/evidence/<id>.jpg. Ground-truth recordings: magenta = our #1 in mode (a) exact start;
# the others: our #1 in mode (a') stated start. Seattle has no detection: ADS-B curve with a hand-picked f0.
# usage: figures.sh REPO_DIR RUN_DIR
R=${1:-.}; RUNS=${2:-${OVERHEAD_DATA:-$HOME/overhead-data}/evidence/runs}
mkdir -p "$R/docs/evidence"
F="taskset -c ${CORES:-12-15} python3 -I $R/scripts/evidence/figure.py"
for id in fs858688 fs858591 aporee_66516_76914 fs847111; do
  $F "$id" "$RUNS/$id-exact.json" "$R/docs/evidence/$id.jpg"
done
$F xc941260 "$RUNS/xc941260-exact.json" "$R/docs/evidence/xc941260.jpg" --f0 185
for id in aporee_68715_79706 aporee_60944_74426 xc1152845 xc883367 aporee_62753_72191; do
  $F "$id" "$RUNS/$id-start.json" "$R/docs/evidence/$id.jpg"
done
