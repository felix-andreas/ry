---
title: The R runtime
description: "Design draft: how ry runs R, from the C API up to the JIT, the table type and lazy package loading"
---

This is a design draft: how ry becomes an R implementation of its own, in what order to build it,
and why. Built so far are the object model and collector in `crates/heap`, the C API as `libR.so`
in `crates/libr` (the whole measured surface, ALTREP and connections), package installation in
`crates/rcmd`, an interpreter in `crates/interp` with a tier-0 bytecode (`ry run`),
a first Cranelift JIT in `crates/jit` (section 5, "What tier 1 does today"), and a table engine in
`crates/table`. Not built are direct calls between compiled functions, ry's own base R (base R's R
code still comes from a GNU R installation), and the table type's use from the interpreter. `ry run`
runs on ry's runtime, and `ry R` drives GNU R. The C API measurements in section 2 are real and
reproducible. Where section 4 describes something built, `.agents/memory/decisions.md` ("the runtime's object model, collector and native
boundary") records the settled details.

## 1. The recommendations

The rest of the page argues for each of these, and section 10 orders them into a build plan.

- **Implement R's C API as an ABI-compatible `libR`.** A package's compiled code must load and run
  unchanged, including binaries built against GNU R's headers. This is cheap to promise, because
  since R 4.2.0 the public headers treat `SEXP` as an opaque pointer, so ry's object layout stays
  its own. The surface is the whole public header set of R 4.6, not a subset: the benchmark's
  dependency closure uses 792 distinct API names (plus 150 BLAS and LAPACK routines), and covering
  even half of its compiled packages takes the 495 most widely used ones.
- **Keep the object model close to GNU R where native code can see it,** and free everywhere else.
  Objects that native code touches do not move, `PROTECT` keeps its stack semantics, strings are
  interned so pointer equality works, and ALTREP is a first-class mechanism. Inside the interpreter
  and the JIT, scalars stay unboxed and environments are materialized only when something needs one.
- **Write the runtime in Rust, with a thin C layer at the native boundary** that turns R's
  `longjmp`-based errors into Rust results and back.
- **Execute in two tiers first:** a bytecode interpreter with inline caches and type feedback, then
  an optimizing JIT on Cranelift with speculation and deoptimization. Add a baseline tier only if
  measurements show a gap. ry's static types seed the speculation, so code runs fast before it has
  warmed up.
- **Load packages lazily from ry's own analysis index.** A top-level function definition becomes a
  binding that compiles on first use; other top-level code runs eagerly in collation order. Reloading
  replaces exactly the items that changed and removes deleted ones, which loadfast cannot do.
- **Build the table type on Polars, linked as a Rust crate,** and expose its columns to R and to
  native code as ALTREP vectors. Typed columns come from ry's record types.
- **Share one type model between checker and runtime.** The runtime consumes the checker's types
  through a small, stable projection. Types are hints by default; in strict mode they become
  guarantees wherever a runtime check backs them.
- **Get real packages running first.** The interpreter and the C API come before the JIT, the table
  type and the inline syntax, because nothing else can be measured until the benchmark code runs.

## 2. The C API surface, measured

### What was measured

The scripts are in `tools/research/`. `r_api_usage.py` resolves the dependency closure of the
benchmark's `Imports` (following `Depends`, `Imports` and `LinkingTo`, recursively, but not
`Suggests`) against CRAN, the extra repository `ak-finccam.github.io/r-packages` (which supplies
`azidentity`, `polars` and `Rblpapi`), and the two GitHub remotes. It downloads every source package
and scans the compiled code under `src/` and `inst/include/` for identifiers declared in R 4.6.2's
public headers (`R.h`, `Rinternals.h`, `Rdefines.h`, `Rembedded.h`, `Rinterface.h`,
`Rconnections.h`, `Rmath.h` and every `R_ext/*.h`). `r_api_summary.py` turns the result into the
figures below. The raw data is in `tools/research/data/`: `symbols.csv` has one row per name with
its class, declaring header and the packages that use it, `packages.csv` one row per package,
`r_calls.csv` the R-level foreign calls, and `summary.txt` the output every figure here is quoted
from.

R classifies its own entry points, and the scan keeps that classification:

- **API** and **experimental** are the names *Writing R Extensions* documents with `@apifun`,
  `@apivar`, `@eapifun` and `@eapivar`.
- **Embedding** is the API for programs that embed R.
- **Non-API** is the list `tools:::nonAPI` that `R CMD check` warns about.
- **BLAS/LAPACK** is every routine declared in `R_ext/BLAS.h`, `Lapack.h` and `Linpack.h`. These are
  Fortran libraries to link against, not R API to reimplement, so they are reported separately.
- **Header** is everything else declared in a public header: mostly types (`SEXP`, `R_xlen_t`), type
  codes (`INTSXP`), constants, the Rmath distribution functions and the graphics engine.

The scan is lexical, and its limits matter when you read the numbers:

- A file counts only if it can see R's headers. That means it includes one, or it is connected to
  such a file through the package's own `#include` edges in either direction, which is how Rcpp's
  umbrella header reaches the many files that use the API without including anything. A vendored C
  library that no R-facing file includes stays out.
- Unprefixed names such as `length` count only where R would remap them. They do not count in C++
  files (since R 4.5.0, `R CMD INSTALL` compiles C++ with `-DR_NO_REMAP`), in packages that define
  `R_NO_REMAP`, or for Rmath's short names (`dt`, `df`, `beta`) in files that do not include
  `Rmath.h`.
- Both branches of an `#if` are counted, so a fallback branch for an older R counts as much as the
  modern one. Names that R 4.6 removed from its public headers (`TRUELENGTH`, `SETLENGTH`,
  `IS_GROWABLE`) are not counted at all, so the measured surface is R 4.6's, not what those
  fallbacks need.
- A package-local macro that shadows an API name still counts as that name. Two known cases are
  data.table's own `BODY` macro and simdjson's `OBJECT` enumerator.
- `polars` and `azidentity` fetch their Rust dependencies with Cargo at build time. Only the code in
  the tarball is scanned, which includes each package's own glue but not `savvy-ffi` or `libR-sys`,
  where their calls into the API live.

### The closure

| | |
| --- | --- |
| Packages in the closure, base packages excluded | 232 |
| ... whose compiled code uses the R API | 121 |
| Direct (non-base) dependencies | 67; 36 ship code that uses the API (35 compile, and `progress` ships a header) |
| Compiled source lines: C / C++ / headers | 1.21M / 0.64M / 3.97M |
| Compiled source lines: Fortran / Rust | 24.7K / 11.6K |

The header count is dominated by vendored libraries: `BH` (Boost), `RcppArmadillo`, `stringi`'s ICU
and the SuiteSparse copy inside `Matrix`. That code never sees `SEXP`, but it has to compile and link,
so ry's package build must handle large C++ code bases and Fortran.

Most packages call the C API directly rather than through a binding layer:

| Binding layer | Packages |
| --- | --- |
| The C API directly | 91 |
| Rcpp | 19 (`rugarch`, `rmgarch`, `RcppDE`, `FLSSS`, `Rblpapi`, `RcppSimdJson`, `odbc`, `openxlsx`, `pdftools`, `httpuv`, `later`, ...) |
| cpp11 | 9 (`readr`, `readxl`, `vroom`, `tzdb`, `systemfonts`, ...) |
| Rust | 2 (`polars` through savvy, `azidentity` through extendr) |

### The names

The closure uses **942 distinct names** from R's headers, 131,275 times in total. 150 of them are
BLAS and LAPACK routines, which leaves **792 names of R API proper** (130,074 occurrences). By class,
307 are API, 102 experimental, 5 embedding, 30 non-API and 348 header-only. Of the 570 names R
documents as API or experimental API, the closure uses 409.

The most widely used names are the ones any implementation starts with:

| Name | Class | Packages | Occurrences |
| --- | --- | ---: | ---: |
| `R_registerRoutines`, `R_useDynamicSymbols` | api | 117 | 133 each |
| `SEXP` | header | 107 | 41,844 |
| `Rf_error` | api | 83 | 5,836 |
| `R_NilValue` | api | 82 | 2,429 |
| `PROTECT` / `UNPROTECT` (`Rf_protect` / `Rf_unprotect`) | api | 79 / 78 | 5,092 / 3,144 |
| `Rf_allocVector` | api | 77 | 2,791 |
| `INTEGER` | api | 71 | 3,391 |
| `STRING_ELT` | api | 70 | 1,717 |
| `CHAR` (`R_CHAR`) | api | 68 | 1,449 |
| `Rf_length` | api | 65 | 2,568 |
| `REAL` | api | 61 | 3,777 |
| `TYPEOF` | api | 59 | 1,333 |
| `SET_VECTOR_ELT` / `VECTOR_ELT` | api | 50 / 47 | 1,635 / 1,519 |
| `Rf_setAttrib` / `Rf_getAttrib` | api | 48 / 39 | 664 / 590 |
| `Rf_mkChar` | api | 47 | 651 |
| `Rf_install` | api | 45 | 843 |
| `Rf_eval` | api | 38 | 195 |
| `R_RegisterCCallable` | api | 25 | 276 |
| `R_ExternalPtrAddr` | api | 25 | 469 |

But the distribution has a long tail, and the tail is not optional. 499 names are used by at least
two packages, 249 by at least five, 145 by at least ten, and only 24 by fifty or more. The coverage
curve shows what that means: if ry implemented the N most widely used names, this many of the 121
compiled packages would have every name they use.

| Names implemented | 25 | 50 | 100 | 150 | 200 | 300 | 400 | 500 | all 942 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Packages fully covered | 2 | 5 | 9 | 19 | 24 | 33 | 43 | 62 | 121 |

Covering half of the compiled packages takes the 495 most widely used names. The curve charges a
package with the names of every header-only layer in its `LinkingTo` as well as its own, which
overstates Rcpp's share: Rcpp's headers mention 358 names, most of them Rmath distribution functions
it wraps, and an unused inline wrapper never reaches a package's binary. In their own code, the
packages that use the most names are the ones the benchmark cannot do without: Rcpp (358),
`Matrix` (257), `vctrs` (250), `pak` (237), `rlang` (233) and `data.table` (202). So the target is the
public header set as a whole, and a "most common 200" subset would leave the core of the tidyverse
and data.table unable to load.

Grouped by subsystem (by declaring header, then by name; the rules are in `r_api_summary.py`):

| Subsystem | Names | Packages using any |
| --- | ---: | ---: |
| Registration and cross-package C calls | 8 | 117 |
| Vector data access (`INTEGER`, `REAL`, `VECTOR_ELT`, lengths, resizable vectors) | 50 | 96 |
| Non-local exits, errors and interrupts | 19 | 93 |
| Allocation, coercion, duplication | 24 | 85 |
| Protection and GC | 13 | 82 |
| Strings and encodings | 33 | 82 |
| Math and RNG (Rmath, `R_ext/Random.h`, `Arith.h`, `Applic.h`) | 172 | 77 |
| Attributes, classes and S4 | 30 | 58 |
| Evaluation, calls and pairlists | 45 | 57 |
| Environments, closures, promises | 57 | 39 |
| External pointers, weak references, finalizers | 18 | 28 |
| BLAS, LAPACK and LINPACK | 150 | 10 |
| ALTREP | 31 | 9 |
| Connections | 5 | 5 |
| Graphics engine and devices | 112 | 5 |
| Types, constants and everything else | 175 | 121 |

### Interfaces that shape the runtime

A few interfaces are used by fewer packages but constrain the design far more than their counts
suggest:

- **ALTREP** (9 packages: `cli`, `cpp11`, `data.table`, `lazyeval`, `pak`, `rlang`, `stringi`,
  `vctrs`, `vroom`). Four of them define ALTREP classes of their own (`vroom` builds its lazily
  parsed columns on it, and `cli`, `vctrs` and `pak` define classes too), and the rest test for ALTREP
  objects or read them through the region and element accessors.
- **Cross-package C callables** (27 packages register or fetch them with `R_RegisterCCallable` and
  `R_GetCCallable`, among them `Rcpp`, `rlang`, `vctrs`, `xts`, `zoo`, `Matrix` and `data.table`).
  `xts` calls `zoo`'s C functions this way (`R_GetCCallable("zoo", "zoo_lag")`), so native libraries
  must be able to find each other through the runtime.
- **External pointers and finalizers** (27 packages). Every handle to a database connection,
  HTTP session, git repository or parser lives in one.
- **Non-local exits** (15 packages use `R_UnwindProtect`, `R_ToplevelExec`, `R_tryEval` or their
  relatives). Rcpp and cpp11 call `R_UnwindProtect` directly so that C++ destructors run when an R
  error unwinds through their frames; savvy and extendr do the same for Rust from crates the scan
  does not see.
- **Connections** (`readr`, `vroom` and `xml2` read and write R connections; `curl` and `pak` create
  custom ones), which exposes the `Rconnection` struct layout, versioned by `R_CONNECTIONS_VERSION`.
- **Graphics** (5 packages: `ragg` implements a device, and `systemfonts`, `farver`, `later` and `pak`
  use smaller parts). The device API can come late, but the graphics headers also declare
  `Rf_ucstoutf8` and the interrupt-suspension macros, which `rlang`, `readxl` and `later` use, and
  Rcpp, rlang and vctrs include them, so the headers themselves and those few names are needed early.

At the R level, 104 packages call `.Call` (4,255 call sites), 18 use `.C` (443), 5 use `.Fortran`
(127), and 8 use `.External` or `.External2` (58). Three call `.Internal` directly (`pkgload`,
`processx`, `renv`), which means ry must also expose some of base R's internal entry points by name.

### The non-API surface is shrinking, and ry should follow it

30 non-API names appear in the closure, used by 28 packages. The heaviest users are `rlang`,
`vctrs`, `lazyeval` (which vendors rlang) and `magrittr` (promise and environment internals such as
`PRVALUE`, `SET_PRENV`, `ENCLOS`), `data.table` (`DATAPTR`, `LEVELS`, `IS_UTF8`) and Rcpp
(`STRING_PTR`, `PRCODE`, `CLOENV`, `BODY`).

Many of these uses are fallbacks. R 4.5 and 4.6 added API replacements for the internals packages
used to reach for, such as `R_ParentEnv`, `R_getVar`, `R_ClosureFormals` and the resizable-vector
functions `R_allocResizableVector`, `R_resizeVector` and `R_mapAttrib`, and the packages switched
with version guards. `rlang`'s `env.h` returns `R_ParentEnv(env)` under
`#if R_VERSION >= R_Version(4, 5, 0)` and `ENCLOS(env)` only otherwise, `cli` maps `R_getVar` to
`Rf_findVar` only before R 4.5, and `data.table` 1.18 keeps its `TRUELENGTH`-based growable vectors
only as a backport for older R. The `TRUELENGTH` family is no longer declared in R 4.6's public
headers at all.

So **ry should present itself as R 4.6**, which makes every guarded package take the modern branch.
That means `R_VERSION` and `Rversion.h`, and also `R_SVN_REVISION`: data.table's guard is
`R_VERSION < R_Version(4, 6, 0) || R_SVN_REVISION < 89077` (and `< 89194` for `R_mapAttrib`), so ry
must report a revision of at least 89194 or data.table takes the backport.

In practice the runtime runs the base R code of the GNU R installation its R home links, so ry
presents that installation's version. `R.version`, `getRversion()` and the home's generated
`Rversion.h` all come from its `library/base/DESCRIPTION`, and the revision is raised to at least
89194. Linked to R 4.6, ry presents 4.6 as recommended here. Linked to an older R, such as the 4.3.3
the corpus baseline runs on, it presents that version, because a package whose C code takes the 4.6
branches while its R code runs on a 4.3 base breaks (rlang's `ns_registry_env()` is one example).

A real non-API surface remains after that, unguarded: the promise accessors in `rlang`, `vctrs`,
`lazyeval` and `magrittr` and `PRCODE`/`PRENV` in Rcpp's `Promise.h`, `ATTRIB` and `SET_ATTRIB`,
`IS_UTF8` and `IS_ASCII` in `stringi`, `fansi` and data.table's `forder.c`, `Rf_findVarInFrame` in
`S7`, `BODY` in Rcpp's `Function.h`, `OBJECT` in `reshape2` and `R_Outputfile` in `Rmpfr`. `later`
declares `R_GlobalContext` itself rather than taking it from a header. ry should implement all of
these, because a package that does not load is a failed benchmark, whatever R core's policy says.

### What the API implies for the object model

**`SEXP` is opaque.** Since R 4.2.0, defining `USE_RINTERNALS` no longer exposes R's internal
structures. In R 4.6's `Rinternals.h`, `SEXP` is `typedef struct SEXPREC *SEXP` with no definition
of the struct, and every accessor (`INTEGER`, `LENGTH`, `TYPEOF`, `CAR`) is a function. A compiled
package therefore depends on R's *function* ABI, not its object layout, which is what makes an
ABI-compatible reimplementation possible at all.

What native code does rely on, and ry must keep:

- **Pointer identity.** Code compares `x == R_NilValue`, compares symbols by pointer after
  `Rf_install`, and compares `CHARSXP` strings by pointer, because `Rf_mkChar` returns the one
  interned copy of each string. So handles must be canonical: one object, one pointer.
- **Stable, writable data pointers.** `INTEGER(x)` returns a pointer into the vector's storage, and
  code keeps it across later allocations and writes through it. Objects reachable from native code
  must not move, and a pointer handed out by a writable accessor must point at memory that native
  code may write.
- **Writes that ignore reference counts.** Correct code checks `MAYBE_SHARED` before mutating, but
  some code mutates in place on purpose: data.table's `:=`, `set()` and `setattr()` write into the
  object they are given. The runtime cannot assume a vector's contents survive a native call.
- **Precise rooting through `PROTECT`.** An unprotected object may be collected at the next
  allocation, and correct packages protect everything they keep. `PROTECT_WITH_INDEX`,
  `R_PreserveObject` and the protected slot of an external pointer are the other roots.
- **Pairlists.** Calls (`LANGSXP`), `.External` arguments and attribute lists are walked with `CAR`
  and `CDR`, so these stay linked lists at the boundary.
- **Value semantics by reference count.** Since R 4.0, copy-on-modify uses reference counts, and
  packages check `MAYBE_SHARED` before mutating in place.
- **Struct layouts that are part of the API:** `Rcomplex`, `R_CallMethodDef` and its siblings,
  `Rconnection` (versioned), the graphics `DevDesc`, ALTREP class handles, and the bit patterns of
  `NA_integer_` (`INT_MIN`) and `NA_real_` (a NaN with payload 1954).
- **`longjmp` errors.** `Rf_error` never returns; it jumps to the nearest context. Code between the
  error and the handler is skipped without running destructors, which is why C++ bindings wrap their
  calls in `R_UnwindProtect`.

None of this pins down how ry stores an object internally. The constraints are on the objects native
code can reach, which are exactly the objects passed to or created by it.

### The decision: ABI-compatible `libR`

ry should ship a shared library that exports R's public function and data symbols with GNU R's
calling conventions (`libR.so`, `libR.dylib`, `R.dll`), plus a header set that is source-compatible
with R 4.6's. Packages are built with ry's headers, and binaries built against GNU R's headers load
too. The alternative, source compatibility only, saves almost nothing, because the opaque `SEXP`
already forces ry to provide every accessor as a real function. What ABI compatibility adds is the
data symbols (`R_NilValue`, `R_GlobalEnv`, `R_NaInt`, `R_NamesSymbol`, ...), stable struct layouts for
the few structs above, and the exact type-code numbering. In return it buys three things:

- **Prebuilt binaries.** Posit Package Manager and r-universe ship compiled Linux, macOS and
  Windows binaries. Being able to install them skips compiling `stringi`'s ICU, Boost-heavy and
  Armadillo-heavy packages, which is the slowest part of setting up the benchmark environment.
- **A differential oracle.** The same compiled package binary can run under GNU R and under ry, so
  a behavioral difference is ry's bug, not a build difference.
- **A migration path for users,** who can point an existing library tree at ry.

The cost is that ry's internal object header is free, but the handle it hands out is a raw pointer
to a non-moving object, so the GC design follows (section 4).

## 3. Prior art

Every serious attempt at a faster R either forked GNU R, and inherited its C API for free but also
its limits, or reimplemented R and then struggled with packages. That pattern is the main lesson.

| Project | Approach | Native packages | Outcome |
| --- | --- | --- | --- |
| GNU R + `compiler` | AST interpreter plus a stack bytecode compiler (Tierney), JIT on by default since R 3.4.0 | Native: it *is* the API | The baseline; the bytecode removes interpretive overhead on scalar loops but keeps R's environments, promises and boxed values |
| pqR (Neal) | Fork of R 2.15 with a faster interpreter, deferred evaluation on helper threads, and lower allocation | Native, as a fork | Real speedups, but it never followed mainline R, so it fell behind the packages |
| CXXR / Rho | Incremental C++ rewrite of R's internals, later with an LLVM JIT | Kept R's API by construction | Discontinued |
| Riposte (Talbot et al.) | Tracing JIT and parallel VM for vector code | None | Research prototype; showed large gains from fusing vector operations, beside scalar trace compilation |
| Renjin | JVM interpreter; GCC-Bridge compiled C and Fortran to JVM bytecode | Recompiled, with no raw memory model | No longer maintained; packages relying on pointer-level behavior broke |
| FastR (Oracle) | Truffle AST interpreter with Graal partial evaluation | Emulated C API over Java objects, so a raw data pointer required native memory to be kept in sync with the Java-side vector | Large speedups on R code, but the native boundary was slow and incomplete; no longer actively developed, last release 22.3 |
| TERR (TIBCO, now Spotfire) | Closed-source clean-room engine from the S-PLUS lineage | Own implementation of the C API | Commercial and still shipped; checks CRAN compatibility on every release, which is the cost of not being a fork |
| Ř / rir (Vitek group) | Bytecode tier plus an optimizing tier with an SSA IR (PIR), speculation, deoptimization and contextual dispatch, LLVM backend | Native: built on a patched GNU R | Large speedups on R-level code in the published evaluations; remains a research VM tied to its GNU R base |
| rcp (Kocourek, Křikava, Vitek, 2025) | Copy-and-patch JIT from GNU R bytecode, as a package | Native, inside GNU R | 1.15x to 1.91x over GNU R on 57 programs, at about 980 bytecode instructions per millisecond of compile time |

What carries over to ry:

- **The C API is the gate.** FastR and Renjin had excellent engines and lost on packages, because
  emulating raw pointers (copying data out to native memory, or translating C to the JVM) was slow
  and leaky. ry avoids that by making its native representation the real one: an R vector is a
  contiguous buffer that `INTEGER()` returns directly.
- **Forks do not keep up.** pqR and Ř get the API for free but cannot follow upstream R. ry should not
  fork GNU R's C sources. It reimplements the runtime, and treats GNU R as an oracle, not a codebase.
- **Most of R's cost is semantic, not instruction dispatch.** Ř's papers show that the wins come from
  removing environment allocation, promise creation and argument matching through speculation and
  deoptimization. rcp's 1.15x to 1.91x for a fast template JIT is consistent with it: compiling R's bytecode
  faster does not remove the R semantics it encodes.
- **Riposte's lesson** is that vector code wants fusion (`sum(x * y + z)` without temporaries),
  which a scalar JIT does not give you.
- **TERR shows the reimplementation path is viable** commercially, at the price of a permanent
  compatibility test against CRAN. ry should build that test from day one (section 10).

## 4. The runtime

### Language and structure

The runtime is Rust, in new crates beside the analysis: `heap` for the object model and GC, one for
the interpreter and the base library, one for the JIT, and `libr` for the C API. `libr` builds the
shared `libR` and contains a small amount of C, for the reason in "Errors across the boundary" below.
Because a package built against GNU R records `libR.so` as a needed library, `libR` is the whole
runtime: the interpreter and the JIT link into it, and the `ry` binary loads it.
The `repl` crate drives both: GNU R behind `ry R`, and ry's runtime behind `ry run`.

### Objects

Every R value is a heap object with a header that ry defines: type code, flags (object bit, S4 bit,
named or shared bits), a reference count, the attribute pairlist and the GC bits. Vectors store
their elements inline after the header, so `INTEGER()` and `REAL()` return a pointer into the
object. Long vectors use `R_xlen_t` lengths throughout.

- **Interned strings.** `CHARSXP`s live in a global, weakly held intern table keyed by bytes and
  encoding, so pointer equality means string equality, as native code expects. Encoding flags
  (`IS_UTF8`, `IS_ASCII`) are stored at interning time.
- **Symbols** are interned `SYMSXP`s with a cached global binding cell, which is what makes base
  function lookup and JIT watchpoints cheap.
- **Pairlists** are kept for language objects, attributes and `.External` arguments, because native
  code walks them. Ordinary function calls inside the interpreter never build them.
- **Environments** are a hashed frame of binding cells, with a fast path for small frames. A binding
  cell is a heap object of its own holding the symbol, the value and the active-binding and locked
  flags, so its address is stable for as long as anything references it and the JIT can watch it
  for redefinition. The base environment's cells hang off the symbols.
- **ALTREP is native.** An ALTREP object is a vector whose methods table decides length, element
  access, region access and dataptr materialization. Package-defined classes use the R API; ry's own
  compact sequences, memory-mapped vectors and table columns (section 6) use the same mechanism.

### Memory management

The GC is a **non-moving, generational mark-sweep collector with size-segregated pages**, which is
the design GNU R uses, kept because native code holds raw pointers to object data. Reference counts
drive copy-on-modify exactly as `MAYBE_SHARED` expects. The roots are the `PROTECT` stack, the
precious list behind `R_PreserveObject`, global variables, the interpreter's frames, and JIT frames
through Cranelift's stack maps.

The JIT recovers what a moving nursery would buy in other VMs by not allocating in the first place:
unboxed scalars, scalar-replaced temporaries, and vectors fused across a loop (section 5). If
profiling later shows fragmentation, Immix-style regions with opportunistic, pinning-aware compaction
of objects that native code has never seen are the upgrade path, and they need no API change.

**Native calls get a handle scope.** Every object passed into `.Call` and every object the API hands
to native code stays alive until that call returns, as if it were protected. This makes ry strictly
more forgiving than GNU R (code that forgets to `PROTECT` a temporary keeps working) while `PROTECT`
itself keeps its exact semantics for objects that outlive the call through globals or preserved
references. The scope roots results, not allocations, so the temporaries of an `Rf_eval` that native
code calls stay collectable. Each native call also owns a region of the `PROTECT` stack: unprotecting
below it is an error, and a region left unbalanced on return is a warning and is cut back.

Ready finalizers run when a native call returns and on `R_gc`, not inside the collection that found
them, because a collection happens deep inside an allocation, where no native code may run.

**`R_alloc` is an arena.** R frees `R_alloc` memory when the `.C`, `.Call` or `.External` call that
allocated it returns, and `vmaxget`/`vmaxset` let code reset it earlier, so it is a stack of nested
watermarks. ry implements it as a bump allocator that keeps R's alignment and restores the watermark
on return and on a `longjmp`.

### Errors across the boundary

R signals an error with `longjmp`, and a `longjmp` across Rust frames is undefined behavior. So the
boundary is a thin C layer with two directions:

- **ry calling native code** (`.Call`, `.External`, ALTREP methods, finalizers, C callables) goes
  through a C trampoline that sets up a jump target. A native `Rf_error` or an R error raised in a
  nested `Rf_eval` jumps back to the trampoline, which returns an error value to Rust.
- **Native code calling ry** (every API function) enters Rust through a wrapper that catches the
  Rust-side error (and any panic), drops everything, and only then calls the C function that
  performs the `longjmp` to the innermost native context. The jump skips only the wrapper's frame and
  the exported function's, which hold nothing with a destructor at that point. No Rust frame with
  pending destructors is ever crossed.

`R_UnwindProtect`, `R_ToplevelExec`, `R_ExecWithCleanup` and `R_tryCatch` are C contexts of their
own, because the callbacks native code hands them are native code too, and a C++ exception that a
binding throws from an `R_UnwindProtect` cleanup to run its destructors must unwind only C frames.
`R_tryEval` is plain Rust, because inside Rust an error is already a value. R conditions, restarts
and `on.exit` are implemented natively in Rust, with the C layer only as the bridge. The variadic
functions (`Rf_error`, `Rprintf` and their family) are C too, because stable Rust cannot define a C
variadic function.

### Native packages

Package libraries load with `dlopen`, then `R_init_<pkg>` runs and registers its routines.
`useDynLib` resolves registered symbols first, and C callables live in a global table keyed by
package and name. ry builds source packages itself (`R CMD INSTALL` semantics, `Makevars`, the
compiler flags `R CMD config` reports), and installs prebuilt binaries when the platform matches.

A package library built by GNU R must bind its API calls to ry's `libR`, not GNU R's. On Linux it
names its dependency by soname, `libR.so`, and ry's library carries that soname and is loaded with
`RTLD_GLOBAL` first, so the dynamic loader reuses it. Mach-O binds each symbol to a named library
instead (the two-level namespace), and CRAN's binaries name
`/Library/Frameworks/R.framework/Versions/4.x-arm64/Resources/lib/libR.dylib` while Homebrew's name
its own `libR.dylib`. ry therefore loads a copy of each such library whose `libR` load command is
rewritten in place to `@rpath/libR.dylib`, the install name of ry's `libR.dylib`, and re-signed ad
hoc (`crates/libr/src/macho.rs`, `crates/libr/src/dynload.rs`). dyld matches that load command
against the already loaded image with the same install name, so the package binds to ry. A
dependency the package finds relative to itself must resolve from the copy too, so the cache
mirrors the original's surroundings with links: the copy's directory links to every file beside
the original, and when a load command or rpath climbs (`@loader_path/../lib`), as many ancestor
directories are mirrored as it climbs, each linking to the original's entries. Packages
that `rcmd` builds on macOS link with `-undefined dynamic_lookup` and record no `libR` at all, so
the same build loads under ry and under GNU R. `decisions.md` ("macOS packages bind to ry's libR")
has the reasoning.

Windows needs no rewriting. A package DLL that Rtools built imports from `R.dll` by module name,
and the Windows loader satisfies that from an already loaded module with the same base name, so
naming ry's library `R.dll` and loading it first is enough. Package DLLs load with `LoadLibraryExW`
and `LOAD_WITH_ALTERED_SEARCH_PATH`, so a package's own dependent DLLs in `libs/x64/` resolve.

### Threads

R's API is single-threaded, and so is ry's R-level evaluation. Packages such as `RcppParallel`,
`data.table` (OpenMP) and polars run threads that never call the API, which needs no support beyond
the API not being reentrant. `parallel`, `callr`, `future` and `doParallel` use processes, so ry
must be a working `Rscript` replacement that worker processes can start.

## 5. Execution: interpreter and JIT

### The performance bar

Bun's speed comes from three things, and only one of them is the JIT. JavaScriptCore runs four tiers
(the LLInt interpreter, a baseline JIT, the DFG optimizer and the FTL backend) with inline caches,
value profiling and watchpoints, so code warms up gradually and optimized code is invalidated when an
assumption breaks. Bun adds fast startup (a transpiler cache by default, and opt-in bytecode caching with
`bun build --bytecode`), and it implements its hot
builtins in a systems language instead of in JavaScript. ry's equivalents are:

- **A fast first tier and cached bytecode,** so `ry run script.R` starts in milliseconds and a
  package's compiled code is reused across sessions.
- **Base R's primitives in Rust,** with vectorized kernels (SIMD where it pays) for arithmetic,
  comparison, subsetting, `sum`, `cumsum`, `match`, `order` and `paste`. Much real R code is a
  sequence of calls to these, and no JIT can make a slow primitive fast.
- **An optimizing JIT for the R code in between,** which is what the rest of this section is about.

### Tiers

**Tier 0 is a register-based bytecode interpreter.** R code compiles to bytecode when a closure is
first called (or when a package is installed, cached on disk). Each call site, variable lookup and
arithmetic operation carries an inline cache and a small type profile: observed `SEXPTYPE`, length
one or not, attributes or none, NA seen or not.

**Tier 1 is an optimizing compiler on Cranelift.** Hot functions and hot loops (through on-stack
replacement) compile from bytecode to an SSA IR with R-specific operations, in the style of Ř's PIR.
The IR represents environments, promises and boxed values explicitly, so they can be optimized away:

- **Speculation with deoptimization.** The compiler assumes what the profile and the static types
  say (this `x` is a length-one double without attributes, `+` is base `+`, `f` is the closure seen
  before) and guards each assumption. A failed guard rebuilds the interpreter frame, including any
  environment and promises it elided, and continues in tier 0.
- **Watchpoints instead of guards** for facts that change rarely: a binding cell for `+`, `c` or a
  package function carries a watch set, and redefining it invalidates the dependent compiled code.
- **Environment elision.** A function whose body never reflects on its frame needs no environment
  object. Reflection is visible statically (`environment()`, `sys.function`, `eval`, `assign`,
  `parent.frame` in a callee that the compiler cannot see) and dynamically through the callee check.
- **Promise elision.** An argument that is always forced, and whose expression has no effects,
  is evaluated eagerly, which removes the promise. This is where ry's static analysis helps most,
  because effects and strictness are facts the checker can compute per function.
- **Unboxing and scalar replacement.** Length-one vectors without attributes become machine
  integers and doubles. `NA_integer_` is `INT_MIN`, so integer NA checks are a compare and overflow
  checks are the hardware flag.
- **Vector fusion.** Element-wise chains over vectors of equal length compile into one loop, without
  intermediate allocations, writing into a result allocated once at its final size (construction in
  place, as in goose).
- **Reference-count elision.** Inside compiled code, counts are tracked precisely and increments
  and decrements that cancel are dropped, as Roc does with Perceus. A binding in an elided
  environment still counts as a reference, so `y <- x; x[1] <- 0` copies, and deoptimization
  rebuilds the exact counts. A loop like `for (i in ...) x[i] <- v` copies at most once, on entry if
  `x` is shared, and the per-iteration sharing check is hoisted out. Every elided count is
  materialized before a native call, and native code may write into any vector it is given (section
  2), so the JIT assumes nothing about a vector's contents across one.
- **Borrowed arguments.** An argument the callee only reads is passed without touching its count,
  as Bend's and Roc's borrow inference do. This is a speculation: it applies only when the whole
  callee is compiled and visible, any call to unknown R code or to `.Call` or `.External` counts as an
  escape, and deoptimization restores the counts.
- **Bounds-check elimination** over difference constraints between loop indices and vector lengths,
  as goose does, so `for (i in seq_along(x)) y[i] <- x[i]` runs without per-iteration checks. Only
  positive in-range subscripts are bounds facts; zero, negative and `NA` subscripts, `[[`, and classed
  vectors with their own `[` methods stay behind guards, and a fact about `length(x)` dies when `x` is
  rebound in the loop body.
- **Closure specialization at higher-order call sites.** A call like `vapply(xs, function(x) x * 2,
  double(1))` with a known closure compiles to a loop that calls it directly (ordinary callback
  inlining, inspired by Roc's lambda sets), guarded so that reflection on the closure still works.
  This covers base R's higher-order functions, which ry implements itself; `purrr::map` runs its loop
  in the package's own C code, which the JIT cannot see into.

Cranelift is the right backend: it is a Rust crate, it was designed to compile much faster than
LLVM's optimizing pipeline, and it supports the stack maps the GC needs. LLVM would add a large C++
dependency and compile latency in exchange for low-level optimizations that matter less than the
R-level ones above. Copy-and-patch is the candidate for an optional baseline tier between the two, if
measurements show that tier 0 is too slow for code that is warm but not hot; rcp's results suggest
that the gain over a good interpreter is real but modest.

### What tier 1 does today

`crates/jit` implements the core of this design for loops; the rest of the list above is still
ahead. The JIT is on by default, and `RY_JIT=off` turns it off.

- **When it compiles.** Tier 0 counts entries and backward jumps per body. A body compiles on its
  1000th entry, and a running loop compiles after 10,000 backward jumps and continues in compiled
  code at the loop header (on-stack replacement). The thresholds are high because Cranelift is an
  optimizing compiler: a lower one compiled the methods package's code that a package load runs
  only a few dozen times, which made the load slower.
- **The IR.** The analysis turns the bytecode and its type feedback (one byte per instruction) into
  a typed plan: one action per instruction plus the abstract state before it, which records each
  register's and variable's type and where each variable's value lives. Cranelift's frontend builds
  the SSA form from that plan. There is no separate R-level IR yet, so the optimizations that need
  one (vector fusion, bounds-check elimination, inlining) are not built.
- **Unboxing.** Integers, doubles and logicals of length one without attributes live in machine
  registers, with R's `NA` and overflow rules. Element reads and writes on integer, double and
  logical vectors are inline, and a write copies only when the vector is shared.
- **Variables.** A body that never reflects on its frame keeps its variables in slots instead of
  its environment. The environment object still exists, but compiled code writes a variable back
  only before something that could observe it runs (a generic call, a dispatching operation, the
  first read of a formal whose default is an expression, a return, a deoptimization) and reloads it
  after something that could change it.
- **Calls.** A call to a frame-safe closure whose binding was resolved at compile time is direct:
  effect-free arguments are evaluated eagerly and passed as already forced promises, bound by
  position without the general matcher. Evaluating early is invisible only if nothing can assign
  what an argument reads before the callee forces it, so the callee must be frame-safe: its body
  and defaults call only itself and a fixed list of base builtins, and assign only its own locals.
  The call goes through tier 0 instead when a calling handler is established, because a handler
  could run R code in between. A call of a small closure defined in the same body, which computes a scalar
  from scalars without effects, is inlined on unboxed registers. Function bindings are speculated
  against one global binding epoch, which the heap bumps whenever a watched binding changes or a
  watched name is defined in a function frame.
- **Speculation and deoptimization.** A guard checks each fed-back type, and a function lookup is
  speculated against the binding epoch. A failed guard boxes the live values, writes back the dirty
  variables and the loop states, and continues in tier 0 at the guarded instruction; the code is
  then invalidated and may recompile, up to four times per body. An R error, `break` or `next`
  raised inside a helper leaves compiled code the same way, so tier 0 handles every non-local exit.
  Compiled code returns to tier 0's loop to deoptimize rather than calling into it, so repeated
  deoptimization never deepens the native stack.
- **Warnings.** A warning can run a calling handler, which may read or assign the frame's
  variables. The fast paths that can warn (integer overflow, an out-of-range read, `sqrt` or `log`
  of a negative number) leave for tier 0 at that instruction without invalidating the code, and
  plain arithmetic or a pure builtin writes the frame back and reloads it afterwards whenever a
  calling handler is established.
- **The C API and the collector.** Every boxed value compiled code holds is stored in the register
  window or a slot's root cell, which the collector scans, so every object is a valid `SEXP` at every
  allocation and every helper call.
- **Not yet built:** calls from compiled code straight into the callee's compiled code (a direct
  call still enters through tier 0's call path and allocates a frame and forced promises), the use
  of static types as early profiles, and freeing the machine code of invalidated bodies.

### How the static types feed the JIT

ry's types are not sound in plain R, because R code can redefine anything at any time. So the JIT
treats them in two ways, depending on whether a guarantee exists:

- **Default mode: types are profiles that arrive early.** A function annotated
  `fn(values: double[], scale: double) -> double[]` starts with that type profile before its first
  call, so tier 1 can compile it on the first hot loop instead of after warmup. The guards stay.
- **Strict mode: types are guarantees where a check backs them.** Strict mode does not report every
  imprecision (it never reports `Any`, nor an `Unknown` nested inside a type, and the stubs use `Any`
  on purpose), so a strict package can still receive an imprecise value from typed code. ry
  therefore inserts a check wherever an `Any` or `Unknown` value flows into a precise type, at an
  untyped caller and inside typed code alike, and keeps guards on facts that come only from
  hand-written stubs, because a wrong stub must never mean memory-unsafe compiled code. An atomic
  type costs a type-code, length and attribute check; a record or list type costs a walk of the
  value; a function type cannot be checked at the boundary at all and needs a wrapper that checks
  each call. Between checks, the body is compiled without guards on the checked facts.

The ideas above that come from Roc, Bend and goose, and the ones from those languages and from Vx
that ry rejects, are argued in [prior art from new language implementations](/contributing/design/prior-art/).
The JIT never reads the checker's internal data structures. It consumes the per-function summaries
of section 8, which the analysis computes and which are cached with the bytecode.

## 6. Lazy package loading

### How the existing tools work

`devtools::load_all` (implemented by `pkgload`) builds a fresh namespace environment, processes
`NAMESPACE` imports, sources every file in `R/` in `Collate` order, runs `.onLoad`, registers S3
methods and exports, compiles `src/` if needed, and attaches the result. It does all of it on every
call.

`loadfast` keeps the same steps for the first load and makes later loads incremental: it hashes the
`R/` files with MD5 and re-sources only those that changed, rebuilds S3 method tables, and tracks
which loaded packages import from which, so reloading a dependency upgrades its importers' next load
to a full reload. Its documentation is candid about the limits: the README explains that it does not
track which symbols came from which file, so deleted files and removed functions leave stale objects
until a full reload, and `TECHNICAL_DEBT.md` records that it does not re-read `NAMESPACE`, so new
exports or imports also need one. Compiled code is rebuilt into a new temporary
library and loaded beside the old one.

Both tools are eager: every top-level expression in every file runs before the first function is
called. GNU R's *installed* packages are already lazy, because `R CMD INSTALL` serializes each
object into a lazy-load database and the namespace holds promises that fetch an object on first use.
The cost there is the install step, which is exactly what the development loop wants to avoid.

### ry's design

The loader is built, ahead of the runtime, on the GNU R backend that `ry R` embeds
([loading packages](/guides/loading-packages) is the user page). It splits into three parts, and only the
last one knows which runtime runs the code:

- **The item index** (`semantics::load_index`) is derived from the analysis: per item a content
  hash, whether it is a function definition, the S4, R6 and S3 registrations it makes, and the names
  it reads while it runs; per file the text R reads and each item's position; per package which
  names are lazy. It is incremental like the rest of the analysis
  ([architecture](/contributing/architecture/#the-package-load-index)).
- **The plan** (`crates/loader`) is the diff between the index a session has loaded and the current
  one: what to bind lazily, which statements to run in which order, which bindings to remove, which
  registrations to undo or make, and whether imports, exports, data, compiled code and hooks need
  work. It says nothing about how.
- **A backend** applies the plan. The GNU R backend sends it to R as an RDS file, and R code
  installed in the session (`runtime.R`) applies it. ry's runtime will apply the same plan natively,
  so neither the index nor the diff changes when it arrives.

The rules the implementation settled:

- **A function definition is lazy when its name has exactly one top-level writer.** The namespace
  holds a promise that parses and evaluates only that definition on first access. On GNU R the
  promises are made in C by the `makeLazy` internal that installed packages use, so binding 24,500
  definitions takes about 30 ms. Any second writer, conditional or replacing (`f <- memoise(f)`),
  makes every writer of the name eager, in order.
- **Every other top-level expression runs eagerly in `Collate` order**, then the files `Collate`
  leaves out in C-locale order, as `load_all` does. Making a provably pure constructor call lazy is
  not implemented.
- **Imports are copies, and a reload rebuilds them when `NAMESPACE` imports change.** Updating the
  copies other development packages hold of a changed binding is not implemented; a session loads
  one development package at a time without that cascade.
- **A reload is a diff over items.** A changed or new function is rebound and a deleted one removed.
  Statements in a changed file are matched to the file's old statements by content, in order, so an
  inserted statement runs alone while two swapped writers of a name rerun. A statement reruns when it
  is new, when it read a binding that changed while it ran (it holds the old value), or when it
  writes a name that changed (the last writer must win again). A rerun statement that reads a name
  it or a later statement rewrites first reruns that name's earlier writers, so `f <- memoise(f)`
  sees `f` as at load time. A reorder of files by `Collate` falls back to a full load. The property
  test in `crates/loader/tests/test_reload_fuzz.rs` checks all of this against a fresh load.
- **Compiled code** is built by pkgbuild into `src/` and loaded, and a rebuild unloads the previous
  library first, as `load_all` does. Loading a rebuild under a new name, so objects that point into
  the old library stay valid, is not implemented.
- **Installed packages** keep GNU R's installed layout, so the same directory serves GNU R and ry.
  ry reads their lazy-load databases and `.rds` files, and builds the object index and bytecode on
  first load into a cache of its own, never into the package
  ([packages and R versions](/contributing/design/packaging/)). This part waits for the runtime.

Hooks run where R runs them: `.onLoad` after the eager expressions and S3 registration, `.onAttach`
on attach. A reload runs `.onLoad` again only when the `.onLoad` definition changed, because it
usually sets options or registers things once. `ry load` and the `ry R` console are the entry points, and
`load_all()`, attached in `tools:ry`, serves both ry's console and plain R sessions; ry does not
intercept `devtools::load_all()`.

## 7. The table type

### What has to be beaten

data.table is fast for specific, well-known reasons: a parallel radix sort (`forder`) that also
drives grouping and joins, "GForce", which replaces common aggregations inside `[` (`sum`, `mean`,
`min`, `max`, `.N`, ...) with specialized grouped C loops, keyed and indexed binary-search joins,
in-place update by reference with `:=` (columns are over-allocated so that adding one does not copy
the table), and a multithreaded `fread`. Its weak spots are equally clear: anything outside GForce
evaluates R code once per group, all of it is single-threaded at the R level, and its query is not
optimized as a whole.

### Recommendation: Polars, linked as a Rust crate

Polars is a Rust dataframe engine on Arrow's columnar memory, with multithreaded hash group-by and
join kernels, a lazy query planner (predicate and projection pushdown, common-subexpression
elimination) and a streaming engine. Because ry is Rust, it can link `polars-core` and `polars-lazy`
directly, with no FFI layer between the runtime and the engine. The R package `polars` in the
benchmark closure already does this through savvy, which proves the engine works for R users; ry's
table type removes the binding layer and teaches the type checker about it.

The alternatives are weaker for this purpose. DuckDB is excellent, and it already scans R data
frames and Arrow data without copying (duckdb-r returns ALTREP-backed results), but it is a C++
engine with a SQL-shaped planner, so ry would embed it through a C API rather than link a Rust
library whose plan it can build directly. Arrow's own compute
kernels lack a query planner. A custom engine would spend years re-deriving what Polars has.

### The design

The engine is `crates/table` and its query IR; `crates/interp/src/frame.rs` makes frames
interpreter values and lowers queries into the IR, and `crates/libr/src/frame_altrep.rs` exposes
columns as ALTREP vectors to native code. The user guide is [DataFrames](/guides/dataframes/).

- **The type is `DataFrame`, an external pointer that owns a Polars `DataFrame`.** The runtime
  class is `"DataFrame"` alone (a DataFrame is no `data.frame`; `as.data.frame()` and
  `as.DataFrame()` convert), and the checker types it as
  `DataFrame{id: integer[], price: double[], ticker: character[]}` (the "DataFrames" section of the
  typing reference). `LazyFrame` holds a plan instead of a table, and `GroupBy` a DataFrame or plan
  plus its keys.
- **The primary API is Polars' verbs over expressions** (`DataFrame.select`, `with_columns`,
  `filter`, `group_by` + `GroupBy.agg`, `sort`, `join`, `head`, with `col("x")` expressions), all
  qualified names in `package:ry`. Every verb builds a step of `table::lazy::Plan`, and an eager
  verb collects at once. The planner pushes filters below `with_columns` and projections, and pushes
  filters, needed columns and row limits into a file scan. The bracket syntax below stays as
  data.table compatibility and lowers to the same IR.
- **A column reaches R code as a copied R vector, and native code as an ALTREP vector over the
  Arrow buffer.** The interpreter copies a column when R code reads it (`$`, `as.list`, a data
  mask), which is cheap next to the query. The ALTREP path in `libr` is built but no interpreter
  path hands it out yet. Element and region reads
  go to the buffer, and so does `DATAPTR_OR_NULL` or a read-only data pointer for an integer or
  double column without nulls, because those bytes are identical in both layouts. A writable pointer
  (`INTEGER`, `REAL`, `DATAPTR`), a column with nulls and a logical column (bit-packed in Arrow,
  32-bit in R) materialize R's layout once into the ALTREP object's `data2`. A write through that
  pointer changes the R vector and never the frame, which is R's value semantics. Character columns
  intern their `CHARSXP`s on first access, and factors are copied, because R's one-based `int` codes
  differ from the enum's codes. A column R never materialized comes back into a frame as the same
  Arrow buffer.
- **Residency is per column:** a column lives in the Arrow buffer until R code or native code needs
  R's layout. Vx tracks where data lives in the type and makes every transfer explicit
  ([prior art](/contributing/design/prior-art/#vx)); ry keeps the transfer implicit, and plans a lint
  for typed code that forces a column to materialize inside a loop.
- **NA maps onto Arrow validity, and NaN stays a value.** Integer NA (`INT_MIN`) and double NA (the
  `NaN` with low word 1954) are rewritten into validity at the boundary. A vector coming in from R
  is copied into a new Arrow buffer; only a column R never materialized comes back zero-copy.
- **Queries go through a small IR to a Polars lazy plan.** The evaluator and the bytecode hand
  `tbl[...]` on a DataFrame to the lowering before `i`, `j` and `by` are evaluated, so
  `tbl[price > 0, .(total = sum(price)), by = ticker]` becomes a `Query` (rows, `j`, `by`, `keyby`), which lowers to one lazy frame, so the
  planner sees the whole query and every grouping expression runs as a Polars kernel. The IR covers
  filters, ordering, positions, projections, grouped aggregation over data.table's GForce set (`sum`,
  `mean`, `min`, `max`, `.N`, `first`, `last`, `median`, `sd`, `var`, plus `head` and `cor`), equi,
  keyed, rolling and non-equi joins, `by = .EACHI`, `mult =`, and `:=`. Lowering adds R's
  semantics where Polars differs: integer overflow becomes NA, aggregates propagate NA unless `na.rm`, `%/%` and `%%` follow R's signs, and
  joins follow data.table's row and column layout. A query returns the warnings data.table would
  raise (an overflowing grouped integer sum, a lossy `:=`) as data, and the interpreter raises them as R warnings. Anything
  the IR does not cover, such as an R function inside an aggregation, runs as R code once per group
  in a data mask that binds the columns it mentions; a lint saying so is planned.
- **`:=` is copy-on-write with reference counts,** not over-allocation. A frame whose reference count
  is one is updated in place, so `tbl[, x := y * 2]` costs one new column, and a shared frame first
  copies only its column list, because the Arrow buffers themselves are immutable and shared. In the
  checker, `:=` rebinds the frame to its extended type. data.table's `:=` on a `data.table` object
  keeps data.table's own by-reference semantics, because that package runs unchanged.
- **Interop is zero-copy through the Arrow C data interface,** for a single column and for a whole
  frame as a struct array, which is how the R `polars` and `arrow` packages, duckdb and nanoarrow
  exchange data.

The claim that the frame outperforms data.table rests on a benchmark of its own
(`tools/bench/table/`): the db-benchmark group-by and join questions (the suite H2O started and
DuckDB Labs maintains) at 0.5 GB and 5 GB, `:=` update loops and a keyed rolling join, run on the
same machine against data.table with all threads.

At db-benchmark's 0.5 GB size (1e7 rows, 4 threads), DataFrames from R code under
`ry run` beat data.table under GNU R on every question, and all three runs give the
same checksums. Each cell is the faster of two runs, in seconds; the last column is the same engine
driven from Rust, without R.

| Question | data.table | ry, from R | ry, from Rust |
|---|---|---|---|
| groupby q1: `sum(v1)` by `id1` | 0.151 | 0.022 | 0.034 |
| groupby q2: `sum(v1)` by `id1, id2` | 0.192 | 0.020 | 0.044 |
| groupby q3: `sum(v1)`, `mean(v3)` by `id3` | 0.272 | 0.096 | 0.110 |
| groupby q4: three means by `id4` | 0.357 | 0.158 | 0.175 |
| groupby q5: three sums by `id6` | 0.291 | 0.089 | 0.157 |
| groupby q6: `median(v3)`, `sd(v3)` by `id4, id5` | 0.692 | 0.091 | 0.146 |
| groupby q7: `max(v1) - min(v2)` by `id3` | 0.714 | 0.053 | 0.081 |
| groupby q8: the two largest `v3` by `id6` | 0.759 | 0.177 | 0.235 |
| groupby q9: `cor(v1, v2)^2` by `id2, id4` | 0.769 | 0.041 | 0.099 |
| groupby q10: `sum(v3)`, count by all six ids | 1.733 | 1.033 | 0.896 |
| `:=` on a whole column, 100 times | 5.349 | 0.291 | 2.081 |
| `:=` on 1% of rows, 100 times | 3.803 | 1.112 | 0.891 |
| `:=` adding a column, 100 times | 5.159 | 0.744 | 2.666 |
| join q1: small, on `id1` | 0.356 | 0.110 | 0.149 |
| join q2: medium, on `id2` | 0.522 | 0.131 | 0.229 |
| join q3: medium, left, on `id2` | 0.612 | 0.179 | 0.192 |
| join q4: medium, on factor `id5` | 0.499 | 0.119 | 0.230 |
| join q5: big, on `id3` | 3.797 | 0.664 | 1.028 |
| keyed rolling join, 1e6 trades | 0.130 | 0.094 | 0.100 |
| total | 26.16 | 5.22 | 9.54 |

The R script uses the Polars-style verbs for every question except the `:=` loops, which measure
in-place update and so keep data.table's syntax. Run-to-run times vary by up to 2x on the
allocation-heavy questions, because the allocator returns large blocks to the OS between queries
(`.agents/memory/backlog.md`, "Large allocations page-fault on every query").

`tools/bench/table/ops.R` and the `ops_benchmark` bench in `crates/table` time data.table's helpers on
the db-benchmark group-by file (10 million rows, 4 threads), and both give the same checksums. The
numbers are measured from Rust, in seconds, as the faster of two runs. Calls from R do not reach
these kernels yet.

| Operation | data.table | ry |
|---|---|---|
| `fread` | 2.92 | 1.39 |
| `fwrite` | 0.87 | 0.70 |
| `setkey` | 0.42 | 0.46 |
| `setorder` | 0.97 | 0.69 |
| `unique` | 0.10 | 0.09 |
| `uniqueN` | 0.13 | 0.10 |
| `rbindlist` | 0.88 | 0.29 |
| `melt` | 1.20 | 0.39 |
| `dcast` | 0.26 | 0.26 |
| `shift` | 0.024 | 0.004 |
| `fifelse` | 0.084 | 0.050 |
| `fcoalesce` | 0.059 | 0.047 |
| `nafill` | 0.081 | 0.042 |
| `rleid` | 0.026 | 0.011 |
| `between` | 0.018 | 0.013 |
| `%chin%` | 0.049 | 0.036 |
| `tstrsplit` | 5.17 | 1.86 |

Each cell is the faster of two runs.


## 8. Typing integration

### The inline syntax

Three drafts and decisions bear on this, and the mandate settles what they left open.
`decisions.md` ("An annotation is one concept with pluggable spellings") makes the surface spelling
pluggable at lowering, names a valid-R inline form through R 4.4's `declare()` primitive as the route
that keeps every file ordinary R, and leaves a superset dialect as a product decision.
`contributing/design/inline-type-syntax.md` costed that superset as a dialect compiled to plain R, in
its own `.ry` files, and recommended against building it for inline types alone. The mandate now
requires an inline syntax that is on by default and that ry warns about when it is turned off. A
`declare()` form is valid R, so it would need no switch and no warning, which means the mandate asks
for the superset. A runtime also changes the draft's cost argument: ry executes the syntax directly,
so there is no compile step and no generated R to keep in sync.

The recommendation is therefore the superset, using the four positions the draft chose:

```
scale_by <- function<T: numeric>(values: T, scale: T) -> T {
  values * scale
}
count: integer <- 0L
```

The parameter, return and binder positions are syntax errors in GNU R (checked with R 4.3.3:
`unexpected '<'`, `unexpected ':'` and `unexpected '->'`), so no valid R program changes meaning. The
binding position is the one subtle case. R parses `count: integer <- 0L` as an assignment to
`count:integer`, and running it fails, with `object 'count' not found` when `count` does not exist
yet and `could not find function ":<-"` when it does. So the only R programs it could change are ones
that already fail. The rest follows from the mandate:

- **The syntax is on by default** in ry's parser and runtime, and ry warns when a project turns it off
  in `ry.toml`.
- **`declare(type(...))` stays a recognized spelling** of the same annotation, for code that must
  remain valid R, as the pluggable design intends.
- **A typed file keeps the `.R` extension,** which reverses the draft's `.ry` files: in a runtime
  that executes the syntax, a second extension buys nothing. The cost is real, though. Every other R
  tool (`R CMD check`, roxygen2, lintr, styler) fails on a typed file under `R/`. Code that must also
  run on GNU R, such as a package submitted to CRAN, therefore needs a strip step: a `ry build`
  command would write plain R with the inline types moved into `#:` comments.
- **Comment and inline annotations must agree.** When both annotate the same item, ry compares them
  as types, after alias expansion, and reports a warning on the inline annotation when they differ.
- **Strict mode rejects untyped code.** The settled rule is in the typing reference
  ([untyped parameters](/reference/type-system#untyped-parameters)): a parameter nothing constrains
  is reported, while an undetermined return or binding is reported once, at the origin it traces to.

This settles the question `decisions.md` left open, and the decision is recorded there ("The inline
annotation syntax is a superset of R").

### One type model

The checker's types (`TyKind` in `crates/semantics/src/types.rs`) are interned in the salsa database
and reference inference variables and arena names, which the runtime should not depend on. So the
runtime receives two small, owned, serializable values per function, computed by tracked queries:

- **The runtime type summary** comes from the checker's exported scheme. It keeps what the runtime
  can use (atomic type, scalar or vector shape, names, record fields, function arity, nominal types)
  and collapses what it cannot check cheaply (type parameters, deep unions) to "unknown". It changes
  only when the function's interface changes.
- **The runtime body facts** come from the function's body, not its scheme: whether it reflects on
  its frame, which parameters it always forces, which it only reads, and whether it is free of
  effects. They change on any body edit, so they live in their own query, and only the function's
  own compiled code depends on them. The JIT's callers read a callee's facts through a watchpoint,
  so an edit deoptimizes the callers that relied on the old facts instead of widening the interface,
  which keeps the interface firewall that makes incremental analysis cheap.

Both are cached with the bytecode, keyed by a content hash so the cache stays valid across processes
(salsa's ids are stable only inside one database). This keeps a single source of truth (the
checker's types and the syntax tree) and makes the runtime's copies derived, recomputable
projections. The same summary drives strict mode's boundary checks, the JIT's initial
profiles, and the table type's column checks.

## 9. The benchmark code bases

The three code bases (finance, data science, biostatistics, about 300,000 lines in about 500 files
each, all using the dependency set in `AGENTS.md`) do not exist yet. They should be generated, and
built to exercise what real code exercises rather than to be easy to run:

- **Generator.** A ry tool (`tools/benchmark-gen/`) emits each code base as an R package with a
  `DESCRIPTION` that has exactly the benchmark `Imports`, `R/` files of 400 to 800 lines, and
  `tests/testthat/`. A seeded generator composes modules from domain templates (pricing, risk,
  yield curves and GARCH models for finance; data cleaning, feature pipelines, model fitting and
  reporting for data science; survival, trial simulation and mixed models for biostatistics), so a
  code base can be regenerated at any size.
- **Realism.** Each template uses the dependencies the way real code does: data.table with `:=` and
  `by`, R6 classes, S4 generics, `purrr` maps, `foreach` with `doParallel`, `httr2` and Azure clients
  behind mocks, `testthat` suites, `lgr` logging, and `rlang` tidy evaluation. A fraction of each code
  base carries `#:` annotations, and a fraction carries inline ones, so the analysis has typed and
  untyped code to check. GNU R runs the inline-typed files only after the strip step of section 8.
- **Hermetic runs.** Network-dependent packages (Azure, Bloomberg via `Rblpapi`, ODBC) run against
  local mocks or fakes, so a benchmark run is deterministic.
- **Three measurements:** run the test suite under GNU R (on the stripped sources) and under ry (wall
  time, and outputs must match), check the code base with `ry check` (cold and incremental keystroke latency), and load it
  with `ry load` (cold and incremental reload).

## 10. Build order

Each step ends with something real running, and each one is a large, whole change.

1. **Corpus and oracle first.** Fetch the benchmark closure (the scripts in `tools/research/`
   already resolve it) together with the `Suggests` its packages' tests need, install it into a GNU R
   library, and build a differential harness that runs
   an R file under GNU R and under ry and compares output, errors and warnings. Write the benchmark
   generator and generate small versions of the three code bases.
2. **The object model, GC and C API skeleton (done).** Objects, interned strings and symbols, the GC
   with `PROTECT`, and the `libR` shared library with the top 150 or so symbols, enough for a C
   package to load and run its `.Call` entry points from a Rust test harness. `libR` exports 149 of
   the 150 API names the closure uses most (the gap is `R_ParseVector`, which needs the parser) and
   195 of the top 200. brio from the closure and a purpose-written package run under it, built both
   against ry's headers and by GNU R's `R CMD SHLIB`.
3. **The interpreter and base R.** The tier-0 bytecode interpreter, closures, promises, environments,
   conditions, S3 dispatch, and base R's primitives and internals in Rust. Base R's R code (the
   `base`, `stats`, `utils`, `methods`, `tools` and `parallel` sources) is reused as R source, so only
   the internals behind `.Internal` and `.Primitive` are reimplemented. Run the base package's own
   tests through the oracle.

   The interpreter runs. 111 of the 112 oracle cases match GNU R (`oracle compare --engine ry`); the
   other, `stats__lbfgsb`, agrees only to about seven digits because ry's L-BFGS-B takes a slightly
   different path to the same optimum. Of GNU R 4.3.3's own `tests/*.R`, 47 of 66 end with GNU R's
   exit status and 23 print exactly GNU R's output. The rest stop at a gap such as `env.profile`,
   URL connections, `kmeans`, `qbeta` precision, S4 `initialize` and invalid multibyte strings, and
   `reg-large` writes files of several gigabytes. All six corpus packages that run R code while
   loading (S7, arrow, nanoarrow, reticulate, minqa and yaml) now load under `ry run`.
   Parsed code keeps source references when `keep.source` asks for them. Connections are a single
   implementation shared by the interpreter and `libR`. R's graphics engine runs on ry's runtime
   with the R_GE_version 17 device API, so base graphics, grid, lattice and device packages such as
   ragg share one engine, and the built-in `pdf`, `svg`, `postscript`, `png`, `jpeg`, `bmp` and
   `tiff` devices are engine devices too. On ragg captures, base graphics, grid and lattice match
   GNU R pixel for pixel.
4. **The full C API and packages.** The rest of the 792 measured names and the BLAS and LAPACK
   interfaces, ALTREP, connections,
   unwind protection, C callables, `R CMD INSTALL` semantics and binary installs. Exit criterion: all
   232 packages of the closure install and pass their own tests under ry (with their `Suggests`
   installed), and the three generated
   code bases pass their test suites with output identical to GNU R.

   The C API is done, with evaluation behind a hook the interpreter installs (`libr::eval::Evaluator`). `libR` exports every one of the 444 function and variable
   names the closure uses (the rest of the 792 are types, macros and enum constants in the headers)
   and implements Rmath, Applic, LINPACK, ALTREP, connections and serialization; BLAS and LAPACK
   are declared in headers and come from the system libraries. Against the corpus library (GNU R
   4.3.3 binaries, `tests/test_corpus_binaries.rs`), 192 of the closure's 199 non-base compiled
   packages load and register under ry. Of the other seven, six evaluate R code while loading (S7,
   arrow, nanoarrow, reticulate, minqa, yaml) and tesseract needs a system library that is not
   installed. All 23 recorded `.Call` routines that need no evaluation return what GNU R returns
   (`tests/test_corpus_calls.rs`). The `rcmd` crate compiles packages from source as
   `R CMD INSTALL` does: 111 of the 113 compiled packages in the pinned sources build and load.
   Rblpapi needs Bloomberg's proprietary blpapi3 library and Rmpfr needs the MPFR headers. `rcmd`
   also runs the R-level half of installation (the lazy-load database, `Meta/` and help indices)
   on ry's interpreter, and `oracle install` installs the whole corpus that way: 236 of 238
   packages install from source and load. Rblpapi needs blpapi3 and magick needs ImageMagick's
   Magick++. Run on those installs, ry passes the tests of 90 of the 165 packages whose tests pass
   under GNU R (54.5%). The rest fail on interpreter gaps, and the classes are listed in
   `.agents/memory/backlog.md`.
5. **Lazy loading.** The item-based loader on top of the analysis index, replacing `load_all` and
   `loadfast` for the code bases. Built ahead of steps 1 to 4 on the GNU R backend; once the runtime
   exists it applies the same plans.
6. **Inline syntax, agreement warnings and strict mode (done),** in the parser, the checker and the
   runtime, with the runtime type summary and body facts.
7. **The JIT (loops done, calls next).** The SSA IR, speculation, deoptimization and Cranelift backend, then environment and
   promise elision, then vector fusion, driven by the benchmark code bases and a microbenchmark set
   (the Ř benchmark suite is a good public start).
8. **The table type,** on Polars, with its own benchmark against data.table. The engine, its query
   IR, ALTREP exposure and the checker's `DataFrame` type exist (section 7); the interpreter now
   runs DataFrames and their Polars-style verbs, and data.table's queries run on the engine. Routing data.table's readers, writers and
   helpers to it is next.

9. **Portability.** macOS runs the runtime: `libR.dylib` builds with the install name
   `@rpath/libR.dylib` and the full export list, GNU R's package binaries are rebound to it (section
   4), and `rcmd` reads its toolchain from GNU R's `Makeconf`. All of this is verified by
   cross-compiling from Linux for `aarch64-apple-darwin` and `x86_64-apple-darwin` and by running
   the Mach-O rewriter on a linker-built package; the release smoke test is the first run on a Mac.
   Windows builds `R.dll` and `ry.exe` for `x86_64-pc-windows-gnu`, and `ry run` runs base R
   under Wine; what is verified and what is open is in section 11.

The JIT comes late on purpose. Until real packages run, there is nothing representative to optimize,
and an object model or calling convention changed for the JIT's sake after the C API exists would
break every package. The C API and the object model are designed first, as section 2 argues, and the
JIT fits around them.

## 11. Open questions

- **Windows runtime.** Built for `x86_64-pc-windows-gnu`, and run under Wine, not yet on
  Windows. What exists:
  - `libr` builds as `R.dll`, the mingw-w64 ABI that Rtools builds packages with. `native.c`
    marks its exports `__declspec(dllexport)` and rustc exports the Rust side, so no `.def` file
    is needed, and the linker writes the import library `libR.dll.a` beside the DLL.
  - The headers' `LibExtern` is `__declspec(dllimport) extern` under `_WIN32`, as in GNU R, so a
    package that `rcmd` builds reaches data exports such as `R_NilValue` through the import table.
  - `native.c` uses `__builtin_setjmp` and `__builtin_longjmp` on Windows, because mingw's
    `longjmp` unwinds through SEH. A jump never crosses a Rust frame either way.
  - The interpreter's operating-system calls go through `crates/interp/src/platform.rs`: CPU
    times, file modes, the umask, file times, links, globbing, processes, dynamic libraries,
    `errno`, the native codeset and `iconv`. Windows has no `iconv(3)`, so ry converts there
    itself (UTF-8, Latin-1, ASCII, UTF-16, UTF-32, and the other encodings `encoding_rs` knows),
    behind the same interface `Riconv` exposes to packages.
  - The session reports itself as Windows: `.Platform$OS.type` is `"windows"`, the path
    separator `;`, the platform `x86_64-w64-mingw32`, and `normalizePath` returns backslashes
    without the `\\?\` prefix Rust's canonical paths carry. The default packages' DLLs are
    found under `library/<pkg>/libs/x64/`, and the Windows-only routines that `utils` and
    `grDevices` call at load time (`winver`, `getIdentification`, `readRegistry` and so on) are
    registered (`WINDOWS_ROUTINES` in `crates/interp/src/builtin_libraries.rs`).
  - `ry.exe` reserves a 64 MiB main-thread stack (`crates/ry/build.rs`), because the session
    evaluates on the main thread and Windows' default of 1 MiB overflows while base R loads.
  - `ry run` loads `R.dll` from beside `ry.exe`.

  The DLL must be linked with LLVM's `lld`. GNU ld as packaged by Ubuntu's mingw-w64 (2.41)
  emits a base relocation for each `SECREL32` thread-local offset, so when Windows loads the DLL
  at another address than its preferred one (ASLR does this routinely), every `thread_local`
  access lands on a garbage address and crashes. lld writes no such relocation. mingw's `gcc`
  ignores `-fuse-ld=lld`, so the build uses `clang` as the linker driver:

  ```sh
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=clang \
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-C link-arg=--target=x86_64-w64-mingw32 -C link-arg=-fuse-ld=lld" \
    cargo build --target x86_64-pc-windows-gnu -p libr -p ry-lang
  ```

  Verified under Wine 9 with a Windows R 4.5 home whose DLLs were not executed: scripts that
  print, sort, summarize `mtcars`, format dates, run regular expressions, read files, load
  `stats`, `utils` and `tools`, and call `normalizePath()` print what GNU R prints. That
  comparison is against GNU R's documented and Unix output, since GNU R itself did not run.
  `win.version()` and `Sys.info()` report a fixed `Windows 10 x64` with no build number.

  Open:
  - BLAS and LAPACK come from the R installation's `bin/x64/Rblas.dll` and `Rlapack.dll`, which
    no check has loaded yet, so `lm` and the rest of linear algebra are unverified.
  - The C runtime differs. Rust's `windows-gnu` target links `msvcrt.dll`, while Rtools 4.2 and
    newer link the UCRT. The R API itself does not cross the C runtime, but `errno`, `FILE *`
    and `malloc`/`free` pairs do: a package that frees memory R allocated with `malloc`, or reads
    `errno` after `Riconv`, sees a different runtime's state. Building `R.dll` with the
    `x86_64-pc-windows-gnullvm` target (UCRT) closes this, and needs an llvm-mingw toolchain.
  - No package DLL has been loaded yet, because the only ones at hand were GNU R's own, which
    the checks do not execute.
  - The release workflow builds `ry.exe` with MSVC and does not build `R.dll`, so the Windows
    archive carries no runtime. Shipping it means installing the `x86_64-pc-windows-gnu` target,
    a mingw-w64 sysroot and LLVM on the Windows runner, linking with lld as above, and either
    building `ry.exe` for the same target or confirming that the MSVC `ry.exe` loads it (the
    runtime identity is a hash of the sources, so the two targets agree on it).
  - `rcmd` assembles its R home from symbolic links, which Windows creates only in Developer Mode
    or as administrator.
  - Rust's `windows-gnu` target embeds no application manifest that selects UTF-8 as the active
    code page, so the console and the narrow Win32 APIs use the system code page.
- **Bytecode compatibility.** Installed binary packages contain GNU R bytecode in their lazy-load
  databases. ry can ignore it and recompile from the deparsed closures, which is the current plan,
  but it costs install time.
- **How much of base R to reimplement natively versus reuse as R source.** The plan above reuses the
  R sources, which is less work but ties ry to GNU R's base implementation details.

Settled: **licensing.** Base R's R sources are GPL-2-or-later, so ry loads them at runtime from the
user's own GNU R installation instead of vendoring them, and needs GNU R installed. The owner
confirmed this default; the reasoning is in `.agents/memory/decisions.md` ("runtime licensing").

## Sources

- R 4.6.2 sources: `src/include/` (the public headers), `doc/manual/R-exts.texi` (API classes),
  `src/library/tools/R/sotools.R` (the non-API list).
- The measured data: `tools/research/data/` and the scripts beside it.
- loadfast: [github.com/finccam/loadfast](https://github.com/finccam/loadfast), README and
  `TECHNICAL_DEBT.md`.
- Ř: Flückiger et al., "R Melts Brains: An IR for First-Class Environments and Lazy Effectful
  Arguments" (DLS 2019) and "Contextual Dispatch for Function Specialization" (OOPSLA 2020).
- rcp: Kocourek, Křikava and Vitek, "Copy-and-Patch Just-in-Time Compiler for R" (VMIL 2025),
  [d3s.mff.cuni.cz](https://d3s.mff.cuni.cz/publications/kocourek_copyandpatch_2025/).
- FastR status: the [GraalVM R runtime reference manual](https://www.graalvm.org/dev/reference-manual/r).
- Renjin status: [renjin.org/support](https://www.renjin.org/support.html).
- TERR package compatibility: the [Spotfire Enterprise Runtime for R release notes](https://docs.tibco.com/pub/enterprise-runtime-for-R/6.0.3/doc/html/TIB_terr_Documentation/relnotes/topics/package_compatibility.html).
- Riposte: Talbot, DeVito and Hanrahan, "Riposte: A Trace-Driven Compiler and Parallel VM for Vector
  Code in R" (PACT 2012).
- Copy-and-patch: Xu and Kjolstad, "Copy-and-Patch Compilation" (OOPSLA 2021).
