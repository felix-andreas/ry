# rytest

A package written for ry's native-boundary tests. Each routine exercises one part of the C API
contract: allocation and protection under collection pressure, errors and the non-local exits
native code sets up itself, `R_alloc` watermarks, external pointers and finalizers, ALTREP,
cross-package C callables, encodings, coercion, attributes, environments and the random number
generator. `cpp_unwind.cpp` reproduces the pattern cpp11 and Rcpp use to run C++ destructors when
an R error unwinds through C++ frames.

The routines are registered in `R_init_rytest`, and the Rust harness in
`crates/libr/tests/test_native.rs` calls them through `.Call`.
