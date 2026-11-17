#!/usr/bin/env bash
# run.sh: runs app_workload.R on GNU R, on ry, and on ry with its profile summary switched on.
# It prints the seconds per section and the total for each engine, and two ratios per section:
# GNU R's time over ry's, and what the profile summary adds to ry's time.
# Set RY to the ry binary and RSCRIPT to GNU R's Rscript. Run it on an idle machine.
here=$(cd "$(dirname "$0")" && pwd)
RY=${RY:-ry}; RSCRIPT=${RSCRIPT:-Rscript}
out=$(mktemp -d)
seconds() { grep -E '^[a-z]+ +[0-9.]+' | awk '{print $1, $2}' | sort; }
"$RSCRIPT" "$here/app_workload.R" 2>/dev/null | seconds > "$out/gnu"
"$RY" run "$here/app_workload.R" 2>/dev/null | seconds > "$out/ry"
RY_PROFILE_SUMMARY="$out/summary.md" "$RY" run "$here/app_workload.R" 2>/dev/null | seconds > "$out/profiled"
printf '%-14s %8s %8s %13s %7s %13s\n' section "GNU R s" "ry s" "ry+summary s" "GNU/ry" "summary cost"
join "$out/gnu" "$out/ry" | join - "$out/profiled" | sort -k1,1 |
  awk '{printf "%-14s %8.2f %8.2f %13.2f %7.2f %12.0f%%\n", $1, $2, $3, $4, $2 / $3, 100 * ($4 / $3 - 1)}'
rm -r "$out"
