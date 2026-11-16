# libr

Read this before you change R's C API as an ABI-compatible `libR`. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Traps

- **A change to what `Rinternals.h` inlines bumps `rcmd::NATIVE_ABI`**, or packages ry compiled against the old headers keep running the old inline code; with the bump, `dyn.load` refuses them until they are reinstalled (`.agents/memory/decisions/libr-rcmd.md`, "A package ry compiled records its headers' binary interface").
- **Rust calls native code only through a C trampoline in `native.c`, and API callbacks run from C**, so a `longjmp` or C++ exception never crosses a Rust frame. An exported API function runs its body through `boundary::enter`.
- **Mark a C function that native code calls `RY_EXPORT` in `native.c` (data: `RY_EXPORT_DATA`).** rustc hides C symbols in a cdylib, and `build.rs` exports only the marked ones.
- **ry's headers declare and `libR` exports every name in `crates/libr/tests/headers/r_api_names.txt`** (`.agents/memory/decisions/libr-rcmd.md`, "GNU R's public C API is a checked list, not a measured subset"). A libr test package must also build under gcc 14 and load into GNU R 4.6, so its C declares a non-API entry itself, with a parenthesized name (`SEXP (ATTRIB)(SEXP x);`).
- **`LEVELS` reports growability as GNU R's gp bit `0x20`** (`gp::GROWABLE`): packages built before R 4.6, data.table among them, test the bit and otherwise fail with "attempt to resize a non-resizable vector".
- **An ALTREP header holds no length**: read it with `libr::altrep::length`, never `Sexp::length`, which returns garbage.
- **Native code builds calls with `LCONS`, so argument cells are `LANGSXP`**; `libr::eval::normalize_argument_cells` retypes them before the interpreter walks a call.
- **libR's mimalloc needs `local_dynamic_tls` (the default TLS model fails `dlopen`) and the `v2` allocator** (v3 crashes when a second thread allocates after the first exits).
- **GNU R's default-package libraries (`methods.so`, `stats.so`) can never load on ry's heap**, because they inline R's object layout. Their routines are Rust (`builtin_libraries.rs`).
- **On macOS `dyn.load` opens a re-signed copy of a package binary, rebound to ry's libR** (`macho.rs`; `.agents/memory/decisions/libr-rcmd.md`, "macOS packages bind to ry's libR").
- **The corpus tests skip without `.corpus/library`** (`tools/corpus/install.R`). Re-record `corpus_calls/cases.rds` under `LC_ALL=C.UTF-8`, or string routines record C-locale results.
