# Sets up a shell for working on ry: `source tools/env.sh` (or `. tools/env.sh`). It is idempotent
# and prints one line saying what it found. It does not touch the checked-in cargo configuration.
#
# - The nix dev shell: where nix is installed (every cloud agent container), bash loads the
#   flake's `default` shell, which carries the Rust toolchain, the reference GNU R, just and mold.
#   `nix print-dev-env` is saved in target/nix-dev-env.sh and sourced, because entering
#   `nix develop` costs about 5 s per command and sourcing the saved file 20 ms. The file is
#   rewritten when a file the shell is built from is newer. `RY_ENV=system` skips
#   this for work that needs the host's libraries (the package corpus, tools/dev-setup.sh).
#   Binaries built in the shell link nix's glibc, so a target directory built outside it rebuilds.
# - GNU R: the `R` on PATH, else the oracle's reference build in /opt/R/<version>
#   (tools/reference-r.sh), put on PATH. The oracle, libr's tests against GNU R headers and the
#   docs and scenario tests that compare with GNU R all look for `R` on PATH.
# - mold: on Linux, when mold is installed and no rustc wrapper is configured, RUSTC_WRAPPER runs
#   rustc through tools/mold-rustc, which links Linux targets with mold instead of rust-lld. Cargo
#   leaves the wrapper out of its fingerprints, so turning it on or off rebuilds nothing.
# - ry's base R: reports whether target/base-r, which a debug `ry` finds by itself, is built.

ry_env_root=$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)
ry_env_notes=()

if [ -n "${BASH_VERSION:-}" ] && [ -z "${IN_NIX_SHELL:-}" ] && [ "${RY_ENV:-}" != system ] && command -v nix >/dev/null 2>&1; then
	ry_env_nix=$ry_env_root/target/nix-dev-env.sh
	ry_env_stale=
	[ -s "$ry_env_nix" ] || ry_env_stale=1
	for ry_env_input in flake.nix flake.lock rust-toolchain.toml tools/base-r/VERSION tools/base-r/SHA256SUMS; do
		[ "$ry_env_root/$ry_env_input" -nt "$ry_env_nix" ] && ry_env_stale=1
	done
	if [ -n "$ry_env_stale" ]; then
		printf 'ry env: preparing the nix dev shell (the first run in a container downloads about 9 GB)\n' >&2
		mkdir -p "$ry_env_root/target"
		# The profile is a garbage-collection root for the shell's store paths, and the temporary
		# file is per process because several commands may start at once.
		if nix print-dev-env --accept-flake-config --profile "$ry_env_root/target/nix-dev-profile" \
			"$ry_env_root" >"$ry_env_nix.$$"; then
			mv -f "$ry_env_nix.$$" "$ry_env_nix"
		else
			rm -f "$ry_env_nix.$$"
			if [ -s "$ry_env_nix" ]; then
				ry_env_notes+=("nix dev shell is OUT OF DATE (nix print-dev-env failed)")
			else
				ry_env_notes+=("nix dev shell FAILED to load, using the host's tools")
			fi
		fi
	fi
	if [ -s "$ry_env_nix" ]; then
		ry_env_tmpdir=${TMPDIR:-}
		. "$ry_env_nix"
		# The saved environment makes a build directory on every load and points TMPDIR at it.
		rmdir "$NIX_BUILD_TOP" 2>/dev/null || true
		unset TMP TEMP TEMPDIR NIX_BUILD_TOP
		if [ -n "$ry_env_tmpdir" ]; then export TMPDIR=$ry_env_tmpdir; else unset TMPDIR; fi
		ry_env_notes+=("nix dev shell")
	fi
fi

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
unset ry_env_root ry_env_notes ry_env_r_bin ry_env_nix ry_env_tmpdir ry_env_stale ry_env_input
