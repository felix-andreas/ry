#!/usr/bin/env bash
# Builds ry's base R tree from GNU R's pinned source release: downloads the tarball named in
# VERSION, checks it against SHA256SUMS, applies patches/*.patch in order, and runs
# `ry-rcmd build-base`, which builds the base packages on ry's own interpreter.
#
# usage: tools/base-r/build.sh OUTPUT RY_RCMD LIBR
#   R_SOURCE_TARBALL  use this tarball instead of downloading it (it is still checked)
#   R_SOURCE_CACHE    where downloads are kept (default: target/base-r-source)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
output=$1 rcmd=$2 libr=$3
version=$(tr -d '[:space:]' < "$here/VERSION")
name="R-$version.tar.gz"
checksum=$(awk -v name="$name" '$2 == name { print $1 }' "$here/SHA256SUMS")
[ -n "$checksum" ] || { echo "SHA256SUMS has no entry for $name" >&2; exit 1; }
cache=${R_SOURCE_CACHE:-$root/target/base-r-source}
mkdir -p "$cache"
tarball=${R_SOURCE_TARBALL:-$cache/$name}
if [ ! -f "$tarball" ]; then
	major=${version%%.*}
	curl -fL --retry 4 -o "$tarball.part" "https://cran.r-project.org/src/base/R-$major/$name"
	mv "$tarball.part" "$tarball"
fi
if command -v sha256sum > /dev/null; then
	actual=$(sha256sum "$tarball" | cut -d' ' -f1)
else
	actual=$(shasum -a 256 "$tarball" | cut -d' ' -f1)
fi
if [ "$actual" != "$checksum" ]; then
	echo "$tarball has SHA-256 $actual, but SHA256SUMS pins $checksum" >&2
	exit 1
fi
source_root=$(mktemp -d "$cache/source.XXXXXX")
trap 'rm -rf "$source_root"' EXIT
# The recommended packages and the test suite are the release's only symlinks, which tar cannot
# create on Windows, and the build needs neither.
tar -xzf "$tarball" -C "$source_root" --exclude "R-$version/src/library/Recommended" --exclude "R-$version/tests"
source="$source_root/R-$version"
for patch in "$here"/patches/*.patch; do
	[ -e "$patch" ] || continue
	patch -d "$source" -p1 --quiet < "$patch"
done
"$rcmd" build-base "$source" "$output" --include "$root/crates/libr/include" --libr "$libr" --sha256 "$checksum"
