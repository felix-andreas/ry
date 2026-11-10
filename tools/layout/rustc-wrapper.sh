#!/usr/bin/env bash
# The RUSTC_WRAPPER of tools/layout/optimize.sh: adds RY_LAYOUT_RUSTFLAGS to every rustc call that
# compiles for an explicit target, which skips build scripts and proc-macros.
#
# The flags go through a wrapper and not RUSTFLAGS because cargo hashes RUSTFLAGS into each crate's
# `-C metadata`, which is part of every mangled symbol and codegen-unit name. The instrumented
# build and the optimized build then name the same function differently, LLVM finds no profile
# for it, and nothing warns: the profile silently applies to almost nothing. Through the wrapper
# both builds compile with the same metadata, so the names match.
set -euo pipefail
rustc=$1
shift
# Split on whitespace, so a profile path must not contain a space.
read -ra extra <<< "${RY_LAYOUT_RUSTFLAGS:-}"
case " $* " in
*" --target "*) exec "$rustc" "$@" "${extra[@]}" ;;
*) exec "$rustc" "$@" ;;
esac
