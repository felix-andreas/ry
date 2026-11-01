#!/usr/bin/env bash
# Installs a release archive into a scratch directory and checks that what a user runs first works:
# the version, `ry check`, `ry fmt`, the language server's handshake, and, where GNU R is installed,
# `ry run` on ry's runtime and `ry R` on GNU R. With RY_SMOKE_PACKAGES set, it also installs a
# compiled package from CRAN with GNU R and loads it under `ry run`.
#
#   tools/smoke-test.sh release/0.4.0-beta/ry-x86_64-unknown-linux-gnu.tar.gz
set -euo pipefail

archive=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
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

if version=$("$ry" --version); then pass "ry --version: $version"; else fail "ry --version"; fi

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

if command -v R >/dev/null 2>&1 || [ -n "${R_HOME:-}" ]; then
	# `ry run` prefers the usual install paths to the R on PATH, so a runner's preinstalled R
	# could shadow the version the workflow asked for.
	if [ -z "${R_HOME:-}" ]; then
		R_HOME=$(R RHOME)
		export R_HOME
	fi
	printf 'cat(sum(1:10), "\\n")\n' >sum.R
	if [ -e "$scratch/install/libR.so" ] || [ -e "$scratch/install/libR.dylib" ]; then
		output=$("$ry" run sum.R 2>&1 | tr -d '\r') || true
		if [ "$output" = "55 " ]; then pass "ry run on ry's runtime"; else fail "ry run: $output"; fi
	else
		echo "skip  ry run (this archive carries no runtime)"
	fi
	output=$("$ry" R sum.R 2>&1 | tr -d '\r') || true
	if [ "$output" = "55 " ]; then pass "ry R on GNU R"; else fail "ry R: $output"; fi

	# A package with compiled code, installed by GNU R (a CRAN binary on macOS, built from source
	# on Linux), must load under ry's runtime and call its C routines there. On macOS this is
	# what proves ry rebinds the package's libR reference to its own runtime. It downloads from
	# CRAN, so it runs only when asked.
	if [ -n "${RY_SMOKE_PACKAGES:-}" ] && { [ -e "$scratch/install/libR.so" ] || [ -e "$scratch/install/libR.dylib" ]; }; then
		mkdir -p "$scratch/library"
		Rscript -e "install.packages('jsonlite', lib = '$scratch/library', repos = 'https://cloud.r-project.org', quiet = TRUE)" >/dev/null 2>&1 || true
		printf 'library(jsonlite, lib.loc = "%s")\ncat(toJSON(list(a = 1:3)), "\\n")\n' "$scratch/library" >package.R
		output=$("$ry" run package.R 2>&1 | tr -d '\r') || true
		if [ "$output" = '{"a":[1,2,3]} ' ]; then
			pass "ry run loads a compiled package GNU R installed"
		else
			fail "ry run with jsonlite: $output"
		fi
	fi
else
	echo "skip  ry run and ry R (GNU R is not installed)"
fi

if [ "$failures" -gt 0 ]; then
	echo "$failures smoke test(s) failed" >&2
	exit 1
fi
echo "all smoke tests passed"
