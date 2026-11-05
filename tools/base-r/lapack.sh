#!/usr/bin/env bash
# Builds GNU R's reference BLAS and LAPACK, `libRblas.so` and `libRlapack.so`, from the R source
# tree SOURCE into the directory of ry's runtime LIBR. ry loads them when the system has no
# `libblas.so.3` or `liblapack.so.3` (`library_candidates` in crates/interp/src/linalg.rs), and a
# release embeds them beside the runtime (crates/repl/build.rs).
#
# A system without LAPACK, such as NixOS with no global library path, usually has no
# `libgfortran.so` either, so the libraries link no Fortran runtime: the one routine they need
# from it, and the error handler `xerbla_` that GNU R leaves to its libR, are in fortran.c. The
# handler lives in libRblas rather than in ry's libR, where an export would interpose on the
# handler of a system BLAS.
#
# usage: tools/base-r/lapack.sh SOURCE LIBR
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
source=$(cd "$1" && pwd)
libr=$2
# macOS uses Accelerate, and Windows GNU R's Rblas.dll and Rlapack.dll.
[ "$(uname -s)" = Linux ] || exit 0
output=$(cd "$(dirname "$libr")" && pwd)
build=$(mktemp -d)
trap 'rm -rf "$build"' EXIT
fc=${FC:-gfortran}
cc=${CC:-cc}
blas="$source/src/extra/blas"
lapack="$source/src/modules/lapack"
cd "$build"
"$fc" -O2 -fPIC -c "$blas/blas.f" "$blas/cmplxblas.f" "$blas/blas2.f90" "$blas/cmplxblas2.f90"
"$cc" -O2 -fPIC -c "$here/fortran.c"
# Each module is compiled before the sources that use it.
"$fc" -O2 -fPIC -c "$lapack/la_constants.f90"
"$fc" -O2 -fPIC -c "$lapack/la_xisnan.f90"
"$fc" -O2 -fPIC -c "$lapack/dlartg.f90" "$lapack/dlassq.f90" "$lapack/zlartg.f90" "$lapack/zlassq.f90"
# GNU R's flag for dlamch, which must see each rounding rather than extended precision.
"$fc" -O2 -fPIC -ffloat-store -c "$lapack/dlamch.f"
"$fc" -O2 -fPIC -c "$lapack/dlapack.f" "$lapack/cmplx.f"
# `-z defs` turns a routine left to a Fortran runtime into a link error rather than a load error
# on the user's machine. libR resolves by soname to the runtime the session already has open.
link=(-shared -static-libgcc -Wl,-z,defs)
"$cc" "${link[@]}" -Wl,-soname,libRblas.so -o "$output/libRblas.so" \
	blas.o cmplxblas.o blas2.o cmplxblas2.o fortran.o -L"$output" -lR -lm
"$cc" "${link[@]}" -Wl,-soname,libRlapack.so -Wl,-rpath,'$ORIGIN' -o "$output/libRlapack.so" \
	la_constants.o la_xisnan.o dlartg.o dlassq.o zlartg.o zlassq.o dlamch.o dlapack.o cmplx.o \
	-L"$output" -lRblas -lm
