#!/usr/bin/env bash
# Installs a release archive into a scratch directory and checks that what a user runs first works:
# the version, `ry check`, `ry fmt`, the language server's handshake, `ry run` on ry's runtime with
# its built-in base R, and, where GNU R is installed, `ry R`. With RY_SMOKE_PACKAGES set, it also
# builds a compiled package with `ry install` and loads one that GNU R installed from CRAN.
#
#   tools/smoke-test.sh release/0.4.0-beta/ry-x86_64-unknown-linux-gnu.tar.gz
set -euo pipefail

archive=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
tools=$(cd "$(dirname "$0")" && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/ry-smoke.XXXXXX")
trap 'rm -rf "$scratch"' EXIT

failures=0
pass() { printf 'ok    %s\n' "$1"; }
fail() {
	printf 'FAIL  %s\n' "$1" >&2
	failures=$((failures + 1))
}

mkdir -p "$scratch/install" "$scratch/project"
case "$archive" in
*.zip) (cd "$scratch/install" && unzip -q "$archive") ;;
*) tar -xzf "$archive" -C "$scratch/install" ;;
esac
ry="$scratch/install/ry"
[ -x "$ry.exe" ] && ry="$ry.exe"

# ry.exe and GNU R on Windows read Windows paths, not Git Bash's /tmp/... spelling.
native_path() {
	if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s' "$1"; fi
}

if version=$("$ry" --version); then pass "ry --version: $version"; else fail "ry --version"; fi

# The runtime and the R front end are built into `ry`, which extracts the runtime on first use.
contents=$(cd "$scratch/install" && LC_ALL=C ls | tr '\n' ' ')
case "$contents" in
"LICENSE THIRD_PARTY_NOTICES ry " | "LICENSE THIRD_PARTY_NOTICES ry.exe ") pass "the archive holds ry alone" ;;
*) fail "the archive holds more than ry: $contents" ;;
esac
XDG_CACHE_HOME=$(native_path "$scratch/cache")
export XDG_CACHE_HOME

cd "$scratch/project"
printf 'f <- function(rate) {\n  rate * ratee\n}\n' >bug.R
output=$("$ry" check bug.R 2>&1) && status=0 || status=$?
if [ "$status" -eq 1 ] && grep -q 'ratee' <<<"$output"; then
	pass "ry check reports an unresolved name"
else
	fail "ry check (exit $status): $output"
fi

printf 'x<-c(1,2)\n' >messy.R
if "$ry" fmt messy.R 2>/dev/null && [ "$(cat messy.R)" = "x <- c(1, 2)" ]; then
	pass "ry fmt"
else
	fail "ry fmt wrote: $(cat messy.R)"
fi

# macOS ships no `timeout`; Homebrew's coreutils names it `gtimeout`, and perl is everywhere.
with_timeout() {
	if command -v timeout >/dev/null 2>&1; then
		timeout "$@"
	elif command -v gtimeout >/dev/null 2>&1; then
		gtimeout "$@"
	else
		perl -e 'alarm shift; exec @ARGV' "$@"
	fi
}

# A minimal LSP session: initialize, then (as LSP requires) only after its reply, initialized,
# shutdown and exit. The reply must announce capabilities. Polling the output file instead of using
# coproc keeps this working on macOS's bash 3.2, and `cat` writes it because the server's stdout
# must be a pipe.
lsp_message() { printf 'Content-Length: %d\r\n\r\n%s' "${#1}" "$1"; }
{
	lsp_message '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}'
	for _ in $(seq 1 150); do
		grep -q '"capabilities"' lsp.out 2>/dev/null && break
		sleep 0.2
	done
	lsp_message '{"jsonrpc":"2.0","method":"initialized","params":{}}'
	lsp_message '{"jsonrpc":"2.0","id":2,"method":"shutdown"}'
	lsp_message '{"jsonrpc":"2.0","method":"exit"}'
} | with_timeout 60 "$ry" server 2>/dev/null | cat >lsp.out || true
if grep -q '"capabilities"' lsp.out; then pass "ry server answers initialize"; else fail "ry server: $(cat lsp.out)"; fi

# ry's runtime runs on the base R built into `ry`, so these steps hide any GNU R: no R_HOME, and
# nothing on PATH that ry could ask.
run_ry() { env -u R_HOME "$ry" "$@"; }
printf 'cat(sum(1:10), "\\n")\n' >sum.R
output=$(run_ry run sum.R 2>&1 | tr -d '\r') || true
if [ "$output" = "55 " ]; then pass "ry run on ry's runtime"; else fail "ry run: $output"; fi
if ls "$scratch/cache/ry/runtime/"*/*R.* >/dev/null 2>&1; then
	pass "ry extracted its runtime into the cache"
else
	fail "ry extracted no runtime into $scratch/cache/ry/runtime"
fi
printf 'cat(as.character(getRversion()), file.exists(file.path(R.home(), "library", "stats", "help")), "\\n")\n' >base.R
output=$(run_ry run base.R 2>&1 | tr -d '\r') || true
expected="$(cat "$tools/base-r/VERSION") TRUE "
if [ "$output" = "$expected" ] && ls -d "$scratch/cache/ry/base/"*/library/base >/dev/null 2>&1; then
	pass "ry run uses the base R built into ry"
else
	fail "ry run base R: $output"
fi
# A matrix product runs in ry's own kernels, and solve() goes through LAPACK and the BLAS it calls:
# the system's on Linux, Accelerate on macOS. Windows has no system LAPACK, so there it runs with
# R_HOME naming GNU R, whose Rlapack.dll and Rblas.dll bind to ry's R.dll.
printf 'cat(sum(matrix(1:6, 2) %%*%% matrix(1:6, 3)), solve(matrix(c(2, 1, 1, 3), 2), c(1, 2)), "\\n")\n' >linalg.R
case "$ry" in
*.exe) [ -z "${R_HOME:-}" ] && command -v R >/dev/null 2>&1 && R_HOME=$(R RHOME) && export R_HOME
	output=$("$ry" run linalg.R 2>&1 | tr -d '\r') || true ;;
*) output=$(run_ry run linalg.R 2>&1 | tr -d '\r') || true ;;
esac
if [ "$output" = "163 0.2 0.6 " ]; then pass "ry run calls BLAS and LAPACK"; else fail "ry run linear algebra: $output"; fi
# A Linux without a system BLAS and LAPACK, such as NixOS, uses the reference libRblas and
# libRlapack that ry carries beside its runtime. A user and mount namespace hides the system's by
# mounting /dev/null over each file the loader cache names.
case "$ry" in
*.exe) ;;
*)
	if [ "$(uname -s)" = Linux ]; then
		if ls "$scratch/cache/ry/runtime/"*/libRlapack.so "$scratch/cache/ry/runtime/"*/libRblas.so >/dev/null 2>&1; then
			pass "ry carries libRblas.so and libRlapack.so"
		else
			fail "ry extracted no libRblas.so and libRlapack.so into $scratch/cache/ry/runtime"
		fi
		if command -v unshare >/dev/null 2>&1 && unshare -rm true 2>/dev/null; then
			system=$(/sbin/ldconfig -p 2>/dev/null | awk '/lib(blas|lapack)\.so/ { print $NF }' | xargs -r realpath -e 2>/dev/null | sort -u | tr '\n' ' ')
			output=$(env -u R_HOME unshare -rm sh -c 'for file in $1; do mount --bind /dev/null "$file"; done; shift; exec "$@"' sh "$system" "$ry" run linalg.R 2>&1 | tr -d '\r') || true
			if [ "$output" = "163 0.2 0.6 " ]; then
				pass "ry run falls back to its own BLAS and LAPACK"
			else
				fail "ry run linear algebra without the system's BLAS and LAPACK: $output"
			fi
		else
			echo "skip  linear algebra without the system's BLAS and LAPACK (no user namespaces)"
		fi
	fi
	;;
esac

# Compiled packages download from CRAN and need a C compiler, so they run only when asked.
if [ -n "${RY_SMOKE_PACKAGES:-}" ]; then
	mkdir -p "$scratch/library"
	library=$(native_path "$scratch/library")
	# `ry install` compiles C code through the R front end that `ry` itself answers.
	mkdir -p "$scratch/twice/R" "$scratch/twice/src"
	printf 'Package: twice\nVersion: 1.0\nTitle: Twice\nDescription: Doubles.\nLicense: MIT\n' >"$scratch/twice/DESCRIPTION"
	printf 'useDynLib(twice, .registration = TRUE)\nexport(twice)\n' >"$scratch/twice/NAMESPACE"
	printf 'twice <- function(x) .Call(C_twice, as.numeric(x))\n' >"$scratch/twice/R/twice.R"
	cat >"$scratch/twice/src/twice.c" <<'EOF'
#include <R.h>
#include <Rinternals.h>
#include <R_ext/Rdynload.h>
static SEXP twice(SEXP x) { return Rf_ScalarReal(2 * REAL(x)[0]); }
static const R_CallMethodDef calls[] = {{"C_twice", (DL_FUNC) &twice, 1}, {NULL, NULL, 0}};
void R_init_twice(DllInfo *info) { R_registerRoutines(info, NULL, calls, NULL, NULL); R_useDynamicSymbols(info, FALSE); }
EOF
	output=$(run_ry install "$(native_path "$scratch/twice")" -l "$library" 2>&1) && status=0 || status=$?
	printf 'library(twice, lib.loc = "%s")\ncat(twice(21), "\\n")\n' "$library" >twice.R
	run=$(run_ry run twice.R 2>&1 | tr -d '\r') || true
	if [ "$status" -eq 0 ] && [ "$run" = "42 " ]; then
		pass "ry install builds a compiled package for ry's runtime"
	else
		fail "ry install (exit $status): $output; ry run: $run"
	fi
fi

if command -v R >/dev/null 2>&1 || [ -n "${R_HOME:-}" ]; then
	output=$("$ry" R sum.R 2>&1 | tr -d '\r') || true
	if [ "$output" = "55 " ]; then pass "ry R on GNU R"; else fail "ry R: $output"; fi
	# A package with compiled code, installed by GNU R (a CRAN binary on macOS and Windows, built
	# from source on Linux), must load under ry's runtime and call its C routines there. On macOS
	# this is what proves ry rebinds the package's libR reference to its own runtime.
	if [ -n "${RY_SMOKE_PACKAGES:-}" ]; then
		Rscript -e "install.packages('jsonlite', lib = '$library', repos = 'https://cloud.r-project.org', quiet = TRUE)" >/dev/null 2>&1 || true
		printf 'library(jsonlite, lib.loc = "%s")\ncat(toJSON(list(a = 1:3)), "\\n")\n' "$library" >package.R
		output=$(run_ry run package.R 2>&1 | tr -d '\r') || true
		if [ "$output" = '{"a":[1,2,3]} ' ]; then
			pass "ry run loads a compiled package GNU R installed"
		else
			fail "ry run with jsonlite: $output"
		fi
	fi
else
	echo "skip  ry R (GNU R is not installed)"
fi

if [ "$failures" -gt 0 ]; then
	echo "$failures smoke test(s) failed" >&2
	exit 1
fi
echo "all smoke tests passed"
