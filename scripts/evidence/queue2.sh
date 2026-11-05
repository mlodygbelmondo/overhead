#!/bin/bash
# Second batch: start/hours on the recordings without ADS-B truth, then day mode on all.
S=$(dirname "$0")/run_locate.py
O=${OVERHEAD_DATA:-$HOME/overhead-data}/evidence/runs
while [ ! -f $O/xc941260-hours.time ]; do sleep 10; done
OTHERS="aporee_68715_79706 aporee_60944_74426 xc1152845 xc883367 aporee_62753_72191 aporee_64975_75055"
python3 -I $S $O start $OTHERS
python3 -I $S $O hours $OTHERS
python3 -I $S $O day fs858688 fs858591 aporee_66516_76914 fs847111 xc941260
python3 -I $S $O day $OTHERS
echo QUEUE2 DONE
