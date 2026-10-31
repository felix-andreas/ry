#!/bin/sh
# Regenerates graphics_layout_r46.txt, the structure layouts of GNU R 4.6's graphics headers
# that test_headers.rs holds ry's headers to.
#
# usage: generate_graphics_layout.sh R46_INCLUDE [FALLBACK_INCLUDE]
#
# R46_INCLUDE holds R 4.6's Rinternals.h and R_ext/. FALLBACK_INCLUDE (default
# /usr/share/R/include) supplies any header R46_INCLUDE lacks, such as the generated Rconfig.h.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
r46=$1
fallback=${2:-/usr/share/R/include}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cc -std=c11 -DPROBE_LOCK -I"$r46" -I"$fallback" "$here/graphics_layout.c" -o "$work/probe"
"$work/probe" > "$here/graphics_layout_r46.txt"
