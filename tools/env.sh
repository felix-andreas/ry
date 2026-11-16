# Sets up a shell for working on ry: `source tools/env.sh` (or `. tools/env.sh`). It is idempotent
# and prints one line saying what it found. It does not touch the checked-in cargo configuration.
#
# - GNU R: the `R` on PATH, else the oracle's reference build in /opt/R/<version>
#   (tools/reference-r.sh), put on PATH. The oracle, libr's tests against GNU R headers and the
#   docs and scenario tests that compare with GNU R all look for `R` on PATH.
# - mold: on Linux, when mold is installed and no rustc wrapper is configured, RUSTC_WRAPPER runs
#   rustc through tools/mold-rustc, which links Linux targets with mold instead of rust-lld. Cargo
#   leaves the wrapper out of its fingerprints, so turning it on or off rebuilds nothing.
# - ry's base R: reports whether target/base-r, which a debug `ry` finds by itself, is built.

ry_env_root=$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)
ry_env_notes=()

if ! command -v R >/dev/null 2>&1; then
	ry_env_r_bin=/opt/R/$(cat "$ry_env_root/tools/base-r/VERSION")/bin
	if [ -x "$ry_env_r_bin/R" ]; then
		export PATH="$ry_env_r_bin:$PATH"
	fi
fi
if command -v R >/dev/null 2>&1; then
	ry_env_notes+=("GNU R $(R --version 2>/dev/null | sed -n '1s/^R version \([^ ]*\).*/\1/p') at $(dirname "$(command -v R)")")
else
	ry_env_notes+=("no GNU R (sudo tools/reference-r.sh installs it)")
fi

if [ -f "$ry_env_root/target/base-r/library/base/R/base" ]; then
	ry_env_notes+=("base R built")
else
	ry_env_notes+=("no target/base-r (build it: cargo build -p libr -p rcmd && tools/base-r/build.sh target/base-r target/debug/ry-rcmd target/debug/libR.so)")
fi

if [ "${RUSTC_WRAPPER:-}" = "$ry_env_root/tools/mold-rustc" ]; then
	ry_env_notes+=("mold linker")
elif [ -n "${RUSTC_WRAPPER:-}" ] || grep -qs 'rustc-wrapper' "${CARGO_HOME:-$HOME/.cargo}/config.toml"; then
	ry_env_notes+=("rust-lld linker (a rustc wrapper is already configured)")
elif [ "$(uname -s)" != Linux ]; then
	ry_env_notes+=("the platform's default linker")
elif command -v mold >/dev/null 2>&1; then
	export RUSTC_WRAPPER="$ry_env_root/tools/mold-rustc"
	ry_env_notes+=("mold linker")
else
	ry_env_notes+=("rust-lld linker (install mold for faster debug links)")
fi

(
	IFS=';'
	printf 'ry env: %s\n' "${ry_env_notes[*]}" | sed 's/;/; /g'
)
unset ry_env_root ry_env_notes ry_env_r_bin
