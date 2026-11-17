#!/usr/bin/env bash
# run_all.sh [script ...]: runs every reproducer on ry and on GNU R and prints one table row per
# pattern: nanoseconds per call on GNU R, on ry, and GNU R's time over ry's.
# Set RY to the ry binary and RSCRIPT to GNU R's Rscript. Both need the packages data.table, xts,
# zoo, zeallot, fastmap and lubridate. Run it on an idle machine.
here=$(cd "$(dirname "$0")" && pwd)
RY=${RY:-ry}; RSCRIPT=${RSCRIPT:-Rscript}
scripts=("$@"); [ ${#scripts[@]} -eq 0 ] && scripts=("$here"/r[0-9]*.R)
printf '%-55s %10s %10s %8s\n' "pattern" "GNU R ns" "ry ns" "GNU/ry"
for script in "${scripts[@]}"; do
  join -t $'\t' <("$RSCRIPT" "$script" 2>/dev/null | grep -P '\t' | sort -t $'\t' -k1,1) \
                <("$RY" run "$script" 2>/dev/null | grep -P '\t' | sort -t $'\t' -k1,1) |
    awk -F'\t' '{printf "%-55s %10d %10d %8.2f\n", $1, $2, $3, $2 / $3}'
done
