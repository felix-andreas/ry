#!/usr/bin/env bash
# Runs the table benchmark: generates the data if needed, then runs the same questions under
# data.table and under the ry engine, one after the other so they never share the CPU.
#
# Usage: tools/bench/table/run.sh <data-dir> [rows]
#   rows defaults to 1e7, which is db-benchmark's 0.5 GB size; 1e8 is the 5 GB size.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
dir="$1"
rows="${2:-1e7}"

if [ ! -f "$dir/groupby.csv" ]; then
  Rscript "$here/datagen.R" "$dir" "$rows"
fi
(cd "$root" && cargo build --release -p table --bench db_benchmark)
binary="$(ls -t "$root"/target/release/deps/db_benchmark-* | grep -v '\.d$' | grep -v '\.o$' | head -1)"

echo "== data.table"
Rscript "$here/datatable.R" "$dir"
echo "== ry"
"$binary" "$dir"
