#!/usr/bin/env bash
# Installs a release archive into a scratch directory and checks that what a user runs first works:
# the version, `ry check`, `ry fmt`, the language server's handshake, and, where GNU R is installed,
# `ry run` on ry's runtime and `ry R` on GNU R.
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
"$ry" fmt messy.R 2>/dev/null
if [ "$(cat messy.R)" = "x <- c(1, 2)" ]; then pass "ry fmt"; else fail "ry fmt wrote: $(cat messy.R)"; fi

# A minimal LSP session: initialize, shutdown, exit. The reply must announce capabilities.
lsp_message() { printf 'Content-Length: %d\r\n\r\n%s' "${#1}" "$1"; }
reply=$({
	lsp_message '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}'
	lsp_message '{"jsonrpc":"2.0","method":"initialized","params":{}}'
	lsp_message '{"jsonrpc":"2.0","id":2,"method":"shutdown"}'
	lsp_message '{"jsonrpc":"2.0","method":"exit"}'
} | timeout 30 "$ry" server 2>/dev/null) || true
if grep -q '"capabilities"' <<<"$reply"; then pass "ry server answers initialize"; else fail "ry server: $reply"; fi

if command -v R >/dev/null 2>&1 || [ -n "${R_HOME:-}" ]; then
	printf 'cat(sum(1:10), "\\n")\n' >sum.R
	if [ -e "$scratch/install/libR.so" ] || [ -e "$scratch/install/libR.dylib" ]; then
		output=$("$ry" run sum.R 2>&1) || true
		if [ "$output" = "55 " ]; then pass "ry run on ry's runtime"; else fail "ry run: $output"; fi
	else
		echo "skip  ry run (this archive carries no runtime)"
	fi
	output=$("$ry" R sum.R 2>&1) || true
	if [ "$output" = "55 " ]; then pass "ry R on GNU R"; else fail "ry R: $output"; fi
else
	echo "skip  ry run and ry R (GNU R is not installed)"
fi

if [ "$failures" -gt 0 ]; then
	echo "$failures smoke test(s) failed" >&2
	exit 1
fi
echo "all smoke tests passed"
