#!/usr/bin/env bash
# Builds ry for TARGET with profile-guided optimization (PGO) of every crate, and on x86_64 Linux
# with llvm-bolt installed, lays out libR's code with BOLT on top. The result is what
# `just package` ships: `$CARGO_TARGET_DIR/TARGET/release/ry` with the optimized runtime and base R
# embedded, plus the runtime and its start-up image under `$CARGO_TARGET_DIR/layout/TARGET/runtime`.
#
#   1. An instrumented build (`-Cprofile-generate` in RUSTFLAGS, so every crate linked into libR
#      and ry is instrumented, dependencies included) builds base R and runs
#      tools/layout/train.py, writing raw profiles.
#   2. The release build uses the merged profile (`-Cprofile-use`) with the same flags for every
#      crate. LLVM reports a function whose code no longer matches its profile with a warning
#      ("hash mismatch", "profile data may be out of date", clang's -Wbackend-plugin group), and
#      any such warning fails this script: a stale profile optimizes hot code as cold.
#   3. BOLT instruments that libR, the training runs again, and BOLT rewrites libR with its hot
#      functions packed together and each function's cold blocks split off.
#
# usage: tools/layout/optimize.sh TARGET
#   RY_LAYOUT_BASE  an existing base R tree to reuse for training instead of building one with
#                   the instrumented runtime (its start-up image is rewritten for each runtime)
#   RY_LAYOUT_BOLT  `off` skips BOLT even where it is available
#   LLVM_BOLT       the llvm-bolt to use (default: llvm-bolt, then llvm-bolt-18 on PATH)
set -euo pipefail
target=$1
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
target_root=$(mkdir -p "${CARGO_TARGET_DIR:-target}" && cd "${CARGO_TARGET_DIR:-target}" && pwd)
work="$target_root/layout/$target"
build="$target_root/$target/release"
instrumented="$work/instrumented/$target/release"
optimized="$work/optimized/$target/release"
host=$(rustc -vV | sed -n 's/^host: //p')
tools="$(rustc --print sysroot)/lib/rustlib/$host/bin"
profdata="$tools/llvm-profdata"
[ -x "$profdata" ] || { echo "error: $profdata is missing; install the llvm-tools rustup component" >&2; exit 1; }

case "$target" in
*windows*) library=R.dll rcmd=ry-rcmd.exe ry=ry.exe ;;
*apple*) library=libR.dylib rcmd=ry-rcmd ry=ry ;;
*) library=libR.so rcmd=ry-rcmd ry=ry ;;
esac

bolt=""
if [ "$target" = x86_64-unknown-linux-gnu ] && [ "${RY_LAYOUT_BOLT:-}" != off ]; then
	for candidate in "${LLVM_BOLT:-}" llvm-bolt llvm-bolt-18 /usr/lib/llvm-18/bin/llvm-bolt; do
		if [ -n "$candidate" ] && command -v "$candidate" > /dev/null; then
			bolt=$(command -v "$candidate")
			break
		fi
	done
	[ -n "$bolt" ] || echo "optimize: llvm-bolt not found, so libR gets PGO without BOLT" >&2
fi
merge_fdata="$(dirname "${bolt:-/nonexistent}")/merge-fdata"

# Sets up base R for RUNTIME (a libR) in DIRECTORY, built with RCMD or copied from
# RY_LAYOUT_BASE, with a start-up image for RUNTIME.
base_r() {
	local directory=$1 rcmd=$2 runtime=$3
	if [ -n "${RY_LAYOUT_BASE:-}" ]; then
		rm -rf "$directory"
		cp -a "$RY_LAYOUT_BASE" "$directory"
		"$rcmd" start-image "$directory" --libr "$runtime" > /dev/null
	else
		tools/base-r/build.sh "$directory" "$rcmd" "$runtime"
	fi
}

rm -rf "$work/pgo-raw" "$work/bolt-raw" "$work/runtime" "$work/bolt-instrumented" "$work/build.log"
mkdir -p "$work/pgo-raw" "$work/bolt-raw" "$work/runtime"

# Builds the runtime (libR, ry-rcmd) and then ry, each in its own cargo invocation exactly as a
# plain release build does, so the instrumented and the optimized build compile the same units.
# The profile flags go in RY_LAYOUT_RUSTFLAGS, which tools/layout/rustc-wrapper.sh adds, because
# RUSTFLAGS would change every crate's symbol names between the two builds. Both builds keep the
# release profile's strip off for the same reason: the two must differ in the profile flags only.
# Each build has its own target directory, since a change of the wrapper's flags does not make
# cargo rebuild.
# Cargo unifies features per invocation, and `interp` and `jit` compile differently when built
# beside `ry-lang`, so a profile collected from a combined build would not match the shipped
# runtime. Separate invocations also keep the two fat-LTO links, about 14 GB for `ry` alone with
# instrumentation, from running at once.
build() {
	export RUSTC_WRAPPER="$root/tools/layout/rustc-wrapper.sh" CARGO_PROFILE_RELEASE_STRIP=false
	cargo build --release --locked --target "$target" -p libr -p rcmd 2> >(tee -a "$work/build.log" >&2)
	cargo build --release --locked --target "$target" -p ry-lang 2> >(tee -a "$work/build.log" >&2)
}

# Fails when the build log shows a profile that does not match the code. A hash mismatch or a
# corrupt profile fails outright. A function with no profile record is expected now and then:
# LTO drops a function the instrumented binary never calls, together with its record. Many
# missing records mean the two builds named their functions differently, as they do when the
# profile flags reach cargo's metadata hash, so more than one percent of the profile's records
# fails the build.
check_profile() {
	local mismatches missing records
	mismatches=$(grep -E 'hash mismatch|profile data may be (out of date|corrupt)|function control flow change|-Wbackend-plugin' "$work/build.log" || true)
	if [ -n "$mismatches" ]; then
		printf '%s\n' "$mismatches" >&2
		echo "error: the profile does not match the code it was applied to (see above)" >&2
		exit 1
	fi
	missing=$(grep -c 'no profile data available' "$work/build.log" || true)
	records=$("$profdata" show "$work/pgo.profdata" | sed -n 's/^Total functions: //p')
	echo "optimize: $missing functions without a profile record, against $records records" >&2
	if [ $((missing * 100)) -gt "$records" ]; then
		echo "error: the profile names no record for $missing functions; the two builds named them differently" >&2
		exit 1
	fi
}

echo "optimize: instrumented build" >&2
RY_LAYOUT_RUSTFLAGS="-Cprofile-generate=$work/pgo-raw" CARGO_TARGET_DIR="$work/instrumented" build
# `%4m` merges each binary's counters into a pool of four files as processes exit, instead of
# one file per process.
export LLVM_PROFILE_FILE="$work/pgo-raw/%4m.profraw"
echo "optimize: base R and training on the instrumented build" >&2
base_r "$work/instrumented/$target/base-r" "$instrumented/$rcmd" "$instrumented/$library"
RY_BASE_R="$work/instrumented/$target/base-r" RY_LIBR="$instrumented/$library" \
	python3 -I tools/layout/train.py "$instrumented/$ry"
unset LLVM_PROFILE_FILE
"$profdata" merge -o "$work/pgo.profdata" "$work/pgo-raw"

# A function the profile has no record for means the two builds compiled different code, so
# LLVM's warning for it fails the build like a hash mismatch does.
flags="-Cprofile-use=$work/pgo.profdata -Cllvm-args=-pgo-warn-missing-function"
# BOLT moves functions only in a binary that keeps its relocations, and needs the symbol table,
# so stripping waits until after it.
[ -z "$bolt" ] || flags="$flags -Clink-arg=-Wl,--emit-relocs"
export RY_LAYOUT_RUSTFLAGS="$flags" CARGO_TARGET_DIR="$work/optimized"
echo "optimize: profile-guided release build" >&2
: > "$work/build.log"
build
check_profile

runtime="$work/runtime/$library"
cp "$optimized/$library" "$runtime"
for sibling in libRblas.so libRlapack.so; do
	[ ! -e "$optimized/$sibling" ] || cp "$optimized/$sibling" "$work/runtime/"
done
base="$target_root/$target/base-r"
if [ -z "${RY_LAYOUT_BASE:-}" ]; then
	RY_LAYOUT_BASE="$work/instrumented/$target/base-r"
fi
base_r "$base" "$optimized/$rcmd" "$optimized/$library"

if [ -n "$bolt" ]; then
	echo "optimize: BOLT instrumentation and training" >&2
	mkdir -p "$work/bolt-instrumented"
	cp "$work/runtime/"* "$work/bolt-instrumented/"
	# Instrumenting all of libR's 100 MB of code takes more than 14 GB, so BOLT instruments only
	# the functions the PGO training ran. Its final pass lays out only profiled functions anyway.
	python3 -I tools/layout/hot_functions.py "$profdata" "$work/pgo.profdata" "$optimized/$library" > "$work/hot.txt"
	"$bolt" "$optimized/$library" -o "$work/bolt-instrumented/$library" -instrument \
		-funcs-file-no-regex="$work/hot.txt" \
		-instrumentation-file="$work/bolt-raw/libR.fdata" -instrumentation-file-append-pid
	# The image is tied to the runtime's identity, not its layout, so the base R tree's image
	# serves the instrumented copy too.
	RY_BASE_R="$base" RY_LIBR="$work/bolt-instrumented/$library" python3 -I tools/layout/train.py "$optimized/$ry"
	"$merge_fdata" "$work/bolt-raw/"*.fdata > "$work/libR.fdata"
	"$bolt" "$optimized/$library" -o "$runtime" -data="$work/libR.fdata" \
		-reorder-blocks=ext-tsp -reorder-functions=hfsort+ -split-functions -split-all-cold \
		-split-eh -icf=1 -use-gnu-stack -dyno-stats
fi
# `--emit-relocs` leaves static relocation sections, which keep the symbol table alive through a
# plain strip and double the file. Only those go: the dynamic relocations stay.
objcopy --remove-relocations='*' "$runtime"
strip --strip-unneeded "$runtime"
"$optimized/$rcmd" start-image "$base" --libr "$runtime" > /dev/null

echo "optimize: ry with the runtime and base R embedded" >&2
RY_EMBED_RUNTIME="$runtime" RY_EMBED_BASE="$base" \
	cargo build --release --locked --target "$target" -p ry-lang 2> >(tee -a "$work/build.log" >&2)
check_profile
strip "$optimized/$ry"
# `just package` and the bench recipes read the release directory, so the optimized binaries
# replace whatever a plain build left there.
mkdir -p "$build"
cp "$optimized/$ry" "$build/$ry"
cp "$runtime" "$build/$library"
cp "$optimized/$rcmd" "$build/$rcmd"
echo "$build/$ry"
