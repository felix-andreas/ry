---
title: The R runtime
description: "Proposal, not implemented: how ry runs R, from the C API up to the JIT, the table type and lazy package loading"
---

This is a design draft. Nothing on this page is implemented: ry today is a type checker, formatter,
linter and language server, and its console (`ry repl`) still drives a GNU R that it loads at runtime.
The page sets out how ry becomes an R implementation of its own, in what order to build it, and why.
The C API measurements in section 2 are real and reproducible; everything else is a recommendation.

## 1. The recommendations

The rest of the page argues for each of these, and section 10 orders them into a build plan.

- **Implement R's C API as an ABI-compatible `libR`.** A package's compiled code must load and run
  unchanged, including binaries built against GNU R's headers. This is cheap to promise, because
  since R 4.x the public headers treat `SEXP` as an opaque pointer, so ry's object layout stays its
  own. The surface is the whole public header set of R 4.6, not a subset: the benchmark closure uses
  814 distinct API names, and no smaller set covers even half its packages.
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
  through a small, stable projection. Types are hints by default and become guarantees, with checks
  at the boundary, in strict mode.
- **Get real packages running first.** The interpreter and the C API come before the JIT, the table
  type and the inline syntax, because nothing else can be measured until the benchmark code runs.

## 2. The C API surface, measured

### What was measured

The scripts are in `tools/research/`. `r_api_usage.py` resolves the dependency closure of the
benchmark's `Imports` (following `Depends`, `Imports` and `LinkingTo`, recursively) against CRAN,
the extra repository `ak-finccam.github.io/r-packages` (which supplies `azidentity`, `polars` and
`Rblpapi`), and the two GitHub remotes. It downloads every source package and scans the compiled
code under `src/` and `inst/include/` for identifiers declared in R 4.6.2's public headers (`R.h`,
`Rinternals.h`, `Rdefines.h`, `Rembedded.h`, `Rinterface.h`, `Rconnections.h`, `Rmath.h` and every
`R_ext/*.h`). `r_api_summary.py` turns the result into the figures below. The raw data is in
`tools/research/data/`: `symbols.csv` has one row per symbol with the packages that use it,
`packages.csv` one row per package, `r_calls.csv` the R-level foreign calls, and `summary.txt` the
output these tables quote.

R classifies its own entry points, and the scan keeps that classification. "API" and "experimental"
are the functions *Writing R Extensions* documents with `@apifun` and `@eapifun`, "embedding" is the
API for programs that embed R, and "non-API" is the list `tools:::nonAPI` that `R CMD check` warns
about. Everything else declared in a public header is "header": mostly types (`SEXP`, `R_xlen_t`),
type codes (`INTSXP`), constants (`R_NaInt`), the Rmath distribution functions and the graphics
engine.

The scan is lexical, and three of its limits matter when you read the numbers:

- A file counts only if it can see R's headers, directly or through a local include, which keeps a
  vendored C library such as ICU out of the counts. Inside a file that defines `R_NO_REMAP`, or uses
  Rcpp or cpp11 (both define it), an unprefixed name like `length` is the package's own and is not
  counted.
- Both branches of an `#if` are counted. Many non-API uses sit in fallback branches for older R, so
  the non-API figures are an upper bound (more on this below).
- `polars` and `azidentity` fetch their Rust dependencies with Cargo at build time. Only the code in
  the tarball is scanned, which includes each package's own FFI layer but not `savvy-ffi` or
  `libR-sys`.

### The closure

| | |
| --- | --- |
| Packages in the closure, base packages excluded | 232 |
| ... with compiled code that uses the R API | 121 |
| Direct (non-base) dependencies | 67, of which 36 are compiled |
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

Rcpp and cpp11 are header-only, so their own use of the API compiles into every package that links
to them. The coverage curve below charges each package with its own symbols plus those of every
package in its `LinkingTo`.

### The symbols

The closure uses **814 distinct names** from R's headers, 116,673 times in total. By class, 292 are
API, 91 experimental, 5 embedding, 29 non-API and 397 header-only. Of the 582 functions R documents
as API or experimental API, the closure uses 383.

The most widely used names are the ones any implementation would start with:

| Symbol | Class | Packages | Occurrences |
| --- | --- | ---: | ---: |
| `R_registerRoutines`, `R_useDynamicSymbols`, `DllInfo` | api / header | 117 | 133 |
| `SEXP` | header | 107 | 35,873 |
| `R_NilValue` | api | 81 | 2,302 |
| `Rf_error` | api | 80 | 4,220 |
| `Rf_allocVector` | api | 77 | 2,723 |
| `INTEGER` | api | 71 | 3,327 |
| `PROTECT` / `UNPROTECT` (`Rf_protect` / `Rf_unprotect`) | api | 70 | 4,777 / 2,922 |
| `STRING_ELT` | api | 70 | 1,705 |
| `CHAR` (`R_CHAR`) | api | 68 | 1,404 |
| `Rf_length` | api | 64 | 1,872 |
| `REAL` | api | 60 | 2,410 |
| `TYPEOF` | api | 58 | 1,269 |
| `SET_VECTOR_ELT` / `VECTOR_ELT` | api | 50 / 46 | 1,608 / 1,494 |
| `Rf_setAttrib` / `Rf_getAttrib` | api | 48 / 39 | 633 / 571 |
| `Rf_mkChar` | api | 47 | 627 |
| `Rf_install` | api | 44 | 810 |
| `Rf_eval` | api | 36 | 187 |
| `R_RegisterCCallable` | api | 25 | 275 |
| `R_ExternalPtrAddr` | api | 24 | 457 |

But the distribution has a long tail, and the tail is not optional. 460 names are used by at least
two packages, 243 by at least five, 146 by at least ten, and only 25 by fifty or more. The coverage
curve shows what that means: if ry implemented the N most widely used names, this many of the 121
compiled packages would have everything they need.

| Names implemented | 25 | 50 | 100 | 150 | 200 | 300 | 400 | 500 | all 814 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Packages fully covered | 1 | 5 | 10 | 19 | 24 | 37 | 43 | 64 | 121 |

The packages that need the most are the ones the benchmark cannot do without: `Matrix` needs 259
names, `vctrs` 245, `pak` 224, `data.table` 206 and `rlang` 203. So the target is the public header
set as a whole, and a "most common 200" subset would leave the core of the tidyverse and data.table
unable to load.

Grouped by subsystem (the grouping is by name prefix, in `r_api_summary.py`):

| Subsystem | Names | Packages using any |
| --- | ---: | ---: |
| Registration and cross-package C calls | 8 | 117 |
| Vector data access (`INTEGER`, `REAL`, `VECTOR_ELT`, lengths, resizable vectors) | 51 | 101 |
| Non-local exits and errors (`Rf_error`, `R_UnwindProtect`, `R_ToplevelExec`) | 15 | 88 |
| Allocation, coercion, duplication | 24 | 86 |
| Math, RNG, BLAS and LAPACK | 84 | 82 |
| Strings and encodings | 28 | 81 |
| Protection and GC | 19 | 79 |
| Evaluation, calls and pairlists | 39 | 57 |
| Attributes, classes and S4 | 31 | 57 |
| Environments, closures, promises | 41 | 38 |
| External pointers, weak references, finalizers | 15 | 27 |
| ALTREP | 14 | 9 |
| Graphics engine and devices | 112 | 7 |
| Connections | 6 | 5 |
| Types, constants and everything else | 327 | 121 |

### Interfaces that shape the runtime

A few interfaces are used by fewer packages but constrain the design far more than their counts
suggest:

- **ALTREP** (9 packages: `cli`, `cpp11`, `data.table`, `lazyeval`, `pak`, `rlang`, `stringi`,
  `vctrs`, `vroom`). Four of them define ALTREP classes of their own (`vroom` builds its lazily
  parsed columns on it, and `cli`, `vctrs` and `pak` define classes too), and the rest test for ALTREP
  objects or read them through the region and element accessors.
- **Cross-package C callables** (27 packages register or fetch them with `R_RegisterCCallable` and
  `R_GetCCallable`, among them `Rcpp`, `rlang`, `vctrs`, `xts`, `zoo`, `Matrix` and `data.table`).
  `xts` calls `zoo`'s C functions this way, so native libraries must be able to find each other
  through the runtime.
- **External pointers and finalizers** (27 packages). Every handle to a database connection,
  HTTP session, git repository or parser lives in one.
- **Unwind protection** (14 packages, including Rcpp, cpp11, polars and rlang). C++ and Rust
  bindings use `R_UnwindProtect` to run destructors when an R error unwinds through their frames.
- **Connections** (`readr`, `vroom`, `xml2` read and write R connections; `curl` and `pak` create
  custom ones), which exposes the `Rconnection` struct layout, versioned by `R_CONNECTIONS_VERSION`.
- **Graphics devices** (`ragg`, `textshaping`, `systemfonts`, ...), which expose the `DevDesc` and
  graphics-engine structs. Only 7 packages need them, so they can come late.

At the R level, 104 packages call `.Call` (4,255 call sites), 18 use `.C` (443), 5 use `.Fortran`
(127), and 8 use `.External` or `.External2` (58). Three call `.Internal` directly (`pkgload`,
`processx`, `renv`), which means ry must also expose some of base R's internal entry points by name.

### The non-API surface is shrinking, and ry should follow it

29 non-API names appear in the closure, used by 28 packages. The heaviest users are `rlang`,
`vctrs` and `lazyeval` (promise and environment internals such as `PRVALUE`, `SET_PRENV`, `ENCLOS`),
`data.table` (`DATAPTR`, `LEVELS`, `IS_UTF8`) and Rcpp (`STRING_PTR`, `CLOENV`, `BODY`).

Most of these uses are fallbacks. R 4.5 and 4.6 added API replacements for the internals packages
used to reach for, such as `R_ParentEnv`, `R_getVar`, `R_ClosureFormals` and the resizable-vector
functions `R_allocResizableVector`, `R_resizeVector` and `R_mapAttrib`, and the packages switched
with version guards. `rlang`'s `env.h` returns `R_ParentEnv(env)` under
`#if R_VERSION >= R_Version(4, 5, 0)` and `ENCLOS(env)` only otherwise, `cli` maps `R_getVar` to
`Rf_findVar` only before R 4.5, and `data.table` 1.18 keeps its `TRUELENGTH`-based growable vectors
only as a backport for R older than 4.6. The `TRUELENGTH` family is no longer even declared in
R 4.6's public headers.

So **ry should present itself as R 4.6** (`R_VERSION` and `Rversion.h`), which makes every guarded
package take the modern branch. What remains genuinely non-API after that is a short list (the
promise accessors in `rlang`, `vctrs` and `magrittr`, `ATTRIB` and `SET_ATTRIB`, `R_GlobalContext` in
`later`), and ry should implement those too, because a package that does not load is a failed
benchmark, whatever R core's policy says.

### What the API implies for the object model

**`SEXP` is opaque.** In R 4.6's `Rinternals.h`, `SEXP` is `typedef struct SEXPREC *SEXP` with no
definition of the struct, and every accessor (`INTEGER`, `LENGTH`, `TYPEOF`, `CAR`) is a function.
The old `USE_RINTERNALS` escape hatch that let packages read fields directly no longer exposes the
layout. A compiled package therefore depends on R's *function* ABI, not its object layout, which is
what makes an ABI-compatible reimplementation possible at all.

What native code does rely on, and ry must keep:

- **Pointer identity.** Code compares `x == R_NilValue`, compares symbols by pointer after
  `Rf_install`, and compares `CHARSXP` strings by pointer, because `Rf_mkChar` returns the one
  interned copy of each string. So handles must be canonical: one object, one pointer.
- **Stable data pointers.** `INTEGER(x)` returns a pointer into the vector's storage, and code keeps
  it across later allocations. Objects reachable from native code must not move.
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
  error and the handler is skipped without running destructors, which is why C++ and Rust bindings
  wrap their calls in `R_UnwindProtect`.

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
| Riposte (Talbot et al.) | Tracing JIT and parallel VM for vector code | None | Research prototype; showed that fusing vector operations matters more than scalar speed |
| Renjin | JVM interpreter; GCC-Bridge compiled C and Fortran to JVM bytecode | Recompiled, with no raw memory model | No longer maintained; packages relying on pointer-level behavior broke |
| FastR (Oracle) | Truffle AST interpreter with Graal partial evaluation | Emulated C API over Java objects, so a raw data pointer required native memory to be kept in sync with the Java-side vector | Large speedups on R code, but the native boundary was slow and incomplete; no longer actively developed, last release 22.3 |
| TERR (TIBCO, now Spotfire) | Closed-source clean-room engine from the S-PLUS lineage | Own implementation of the C API | Commercial and still shipped; checks CRAN compatibility on every release, which is the cost of not being a fork |
| Ř / rir (Vitek group) | Bytecode tier plus an optimizing tier with an SSA IR (PIR), speculation, deoptimization and contextual dispatch, LLVM backend | Native: built on a patched GNU R | Large speedups on R-level code in the published evaluations; remains a research VM tied to its GNU R base |
| rcp (Kocourek, Křikava, Vitek, 2025) | Copy-and-patch JIT from GNU R bytecode, as a package | Native, inside GNU R | 1.26x mean over GNU R on 57 benchmarks, at about 980 bytecode instructions per millisecond of compile time |

What carries over to ry:

- **The C API is the gate.** FastR and Renjin had excellent engines and lost on packages, because
  emulating raw pointers (copying data out to native memory, or translating C to the JVM) was slow
  and leaky. ry avoids that by making its native representation the real one: an R vector is a
  contiguous buffer that `INTEGER()` returns directly.
- **Forks do not keep up.** pqR and Ř get the API for free but cannot follow upstream R. ry should not
  fork GNU R's C sources. It reimplements the runtime, and treats GNU R as an oracle, not a codebase.
- **Most of R's cost is semantic, not instruction dispatch.** Ř's papers show that the wins come from
  removing environment allocation, promise creation and argument matching through speculation and
  deoptimization. rcp's modest 1.26x for a fast template JIT confirms it: compiling R's bytecode
  faster does not remove the R semantics it encodes.
- **Riposte's lesson** is that vector code wants fusion (`sum(x * y + z)` without temporaries),
  which a scalar JIT does not give you.
- **TERR shows the reimplementation path is viable** commercially, at the price of a permanent
  compatibility test against CRAN. ry should build that test from day one (section 10).

## 4. The runtime

### Language and structure

The runtime is Rust, in new crates beside the analysis: one for the object model and GC, one for the
interpreter and the base library, one for the JIT, and one for the C API. The C API crate builds the
shared `libR` and contains a small amount of C, for the reason in "Errors across the boundary" below.
The `repl` crate keeps driving GNU R until ry's runtime can run the console's own code, and then
gains a backend switch, so `ry repl` and `ry run` keep working throughout.

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
  cell holds the value plus active-binding and locked flags, and the JIT can watch a cell for
  redefinition.
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

**Native calls get a handle scope.** Every object passed into `.Call` and every object a native
function allocates stays alive until that call returns, as if it were protected. This makes ry
strictly more forgiving than GNU R (code that forgets to `PROTECT` a temporary keeps working) while
`PROTECT` itself keeps its exact semantics for objects that outlive the call through globals or
preserved references.

### Errors across the boundary

R signals an error with `longjmp`, and a `longjmp` across Rust frames is undefined behavior. So the
boundary is a thin C layer with two directions:

- **ry calling native code** (`.Call`, `.External`, ALTREP methods, finalizers, C callables) goes
  through a C trampoline that sets up a jump target. A native `Rf_error` or an R error raised in a
  nested `Rf_eval` jumps back to the trampoline, which returns an error value to Rust.
- **Native code calling ry** (every API function) enters Rust through a wrapper that catches the
  Rust-side error and, only once it is back in C, performs the `longjmp` to the innermost native
  context. No Rust frame with pending destructors is ever crossed.

`R_UnwindProtect`, `R_ToplevelExec`, `R_tryEval` and `R_tryCatch` map onto the same contexts. R
conditions, restarts and `on.exit` are implemented natively in Rust, with the C layer only as the
bridge.

### Native packages

Package libraries load with `dlopen`, then `R_init_<pkg>` runs and registers its routines.
`useDynLib` resolves registered symbols first, and C callables live in a global table keyed by
package and name. ry builds source packages itself (`R CMD INSTALL` semantics, `Makevars`, the
compiler flags `R CMD config` reports), and installs prebuilt binaries when the platform matches.

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
assumption breaks. Bun adds fast startup by caching compiled bytecode, and it implements its hot
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
- **Reference-count elision.** Inside compiled code, counts are tracked precisely, increments and
  decrements that cancel are dropped, an update of a uniquely owned vector becomes a store (Roc's
  Perceus scheme), and an argument the callee only reads is passed without touching its count
  (Bend's borrow inference).
- **Bounds-check elimination** over difference constraints between loop indices and vector lengths,
  as goose does, so `for (i in seq_along(x)) y[i] <- x[i]` runs without per-iteration checks.
- **Closure specialization at higher-order call sites.** A call like `vapply(xs, function(x) x * 2,
  double(1))` with a known closure compiles to a loop that calls it directly, in the spirit of Roc's
  lambda sets, guarded so that reflection on the closure still works.

Cranelift is the right backend: it is a Rust crate, it was designed to compile much faster than
LLVM's optimizing pipeline, and it supports the stack maps the GC needs. LLVM would add a large C++
dependency and compile latency in exchange for low-level optimizations that matter less than the
R-level ones above. Copy-and-patch is the candidate for an optional baseline tier between the two, if
measurements show that tier 0 is too slow for code that is warm but not hot; rcp's results suggest
that the gain over a good interpreter is real but modest.

### How the static types feed the JIT

ry's types are not sound in plain R, because R code can redefine anything at any time. So the JIT
treats them in two ways, depending on whether a guarantee exists:

- **Default mode: types are profiles that arrive early.** A function annotated
  `fn(values: double[], scale: double) -> double[]` starts with that type profile before its first
  call, so tier 1 can compile it on the first hot loop instead of after warmup. The guards stay.
- **Strict mode: types are guarantees with boundary checks.** In a strictly typed package, ry checks
  argument types where untyped code calls a typed function (a type-code, length and attribute check,
  which costs a few instructions per argument), and the body is compiled without guards on those
  facts. This is the gradual-typing contract: a value that crosses into typed code is checked once,
  at the crossing.

The ideas above that come from Roc, Bend and goose, and the ones from those languages that ry
rejects, are argued in [prior art from new language implementations](/contributing/design/prior-art/).
The JIT never reads the checker's internal data structures. It consumes a per-function summary
(section 8) that the analysis computes and that is cached with the bytecode.

## 6. Lazy package loading

### How the existing tools work

`devtools::load_all` (implemented by `pkgload`) builds a fresh namespace environment, processes
`NAMESPACE` imports, sources every file in `R/` in `Collate` order, runs `.onLoad`, registers S3
methods and exports, compiles `src/` if needed, and attaches the result. It does all of it on every
call.

`loadfast` keeps the same steps for the first load and makes later loads incremental: it hashes the
`R/` files with MD5 and re-sources only those that changed, rebuilds S3 method tables, and tracks
which loaded packages import from which, so reloading a dependency forces its importers to reload.
Its README is candid about the limits: it does not track which symbols came from which file, so
deleted files and removed functions leave stale objects until a full reload, and it does not re-read
`NAMESPACE`, so new exports or imports also need one. Compiled code is rebuilt into a new temporary
library and loaded beside the old one.

Both tools are eager: every top-level expression in every file runs before the first function is
called. GNU R's *installed* packages are already lazy, because `R CMD INSTALL` serializes each
object into a lazy-load database and the namespace holds promises that fetch an object on first use.
The cost there is the install step, which is exactly what the development loop wants to avoid.

### ry's design

ry already has the index that makes this cheap: the analysis core parses every file, knows every
top-level item and its name (`item_spans`), and recomputes only what an edit touched. The loader
reads that index:

- **A top-level function definition** (`f <- function(...) ...`) becomes a lazy binding: the
  namespace holds a cell that compiles the function from its syntax node on first access. Nothing
  is evaluated at load time, which matters because function definitions are most of a package.
- **Every other top-level expression** (`setClass`, `R6Class`, constants built by calls, `S7`
  classes, registration calls) runs eagerly in `Collate` order, as R would, because it may have
  effects. A constructor call that the analysis proves pure (for example a constant `list()` or a
  call to a known pure base function) may also become lazy.
- **Imports are forwarding cells,** not copies. An importer's import environment points at the
  exporter's binding cell, so reloading a dependency is seen by every importer immediately and no
  reload cascade is needed. This removes the snapshot problem that loadfast handles with flags.
- **Reload is a diff over items.** The analysis knows which items changed, appeared or disappeared,
  so reloading rebinds the changed ones, removes the deleted ones, and reruns only the eager
  expressions that changed or that read a changed binding. Stale symbols, loadfast's documented
  limitation, cannot arise. `NAMESPACE` and `DESCRIPTION` are inputs to the same analysis, so a new
  export or import is just another change.
- **Compiled code** is rebuilt incrementally and loaded under a new name. Registered routines are
  re-resolved, so calls through `useDynLib(.registration = TRUE)` reach the new code, and objects
  that still point into the old library keep it alive.
- **Installed packages** use the same mechanism, with the bytecode and the item index cached at
  install time instead of a serialized lazy-load database. ry must still read GNU R's lazy-load
  databases and `.rds` files, because installed binary packages ship them.

Hooks run where R runs them: `.onLoad` after the eager expressions, `.onAttach` on attach, and
`.onUnload` before a full teardown. `ry load` (or `load_all()` called from inside ry, which ry
intercepts) is the user-facing entry point.

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

The alternatives are weaker for this purpose. DuckDB is excellent but C++, SQL-shaped, and its
in-process API is not designed to share memory with a host language's vectors. Arrow's own compute
kernels lack a query planner. A custom engine would spend years re-deriving what Polars has.

### The design

- **A table is a Polars `DataFrame` held by a ry object,** and each column is exposed to R and to
  native code as an ALTREP vector over the Arrow buffer. Reading a column costs nothing; native code
  that asks for `INTEGER()` gets the buffer directly when the layout matches (integers and doubles
  without nulls) and a materialized copy otherwise. This is what lets existing code that expects a
  `data.frame` keep working when it receives a table.
- **Residency is tracked per column.** A column's data lives in Polars' Arrow buffer until R code or
  native code asks for an R vector, and only then is it materialized, much as Vx makes a transfer
  between memories explicit ([prior art](/contributing/design/prior-art/#vx)).
- **NA handling** maps R's NA onto Arrow validity bitmaps. Integer NA (`INT_MIN`) and double NA need
  conversion at the boundary when nulls are present, which is a vectorized pass, and it only happens
  when R code materializes a column.
- **Typed columns come from ry's record types.** A table type is a row record,
  `table{id: integer, price: double, ticker: character}`, so column references in a query are
  checked, and the data-masking design (`contributing/design/data-masking.md`) gets the column
  vocabulary it is waiting for.
- **Queries build a lazy plan.** `tbl[price > 0, .(total = sum(price)), by = ticker]` and its verb
  equivalents lower to a Polars lazy frame, so the planner sees the whole query, and every grouping
  expression runs as a Polars kernel instead of as R code per group. An R function inside an
  aggregation falls back to per-group evaluation, with a lint saying so.
- **`:=` is copy-on-write with reference counts,** not over-allocation. A table whose reference count
  is one is updated in place, so `tbl[, x := y * 2]` costs one column, and a shared table copies only
  the column metadata, because Arrow buffers are immutable and shared.
- **Interop is zero-copy where possible:** conversion to and from `data.frame`, data.table and the R
  `polars` package goes through the Arrow C data interface.

To claim that it outperforms data.table, the table type needs a benchmark of its own: the db-benchmark
group-by and join questions (the suite H2O started and DuckDB Labs maintains) at 0.5 GB and 5 GB, plus
`:=` update loops and keyed rolling joins (common in the finance code base), run on the same machine
against data.table with all threads. Only that measurement can settle the claim.

## 8. Typing integration

### The inline syntax

`contributing/design/inline-type-syntax.md` designed inline types as a dialect compiled to plain R,
and recommended against building it for inline types alone, mainly because a typed file would be
invisible to every other R tool. A runtime changes that calculation: ry executes the syntax
directly, so there is no compile step and no generated R to keep in sync. The four positions that
draft chose stay as they are:

```
scale_by <- function<T: numeric>(values: T, scale: T) -> T {
  values * scale
}
count: integer <- 0L
```

The parameter, return and binder positions are syntax errors in GNU R (checked with R 4.3.3: `unexpected '<'`,
`unexpected ':'` and `unexpected '->'`), so no valid R program changes meaning. The binding position is
the one subtle case: R parses `count: integer <- 0L` as an assignment to `count:integer`, and running
it fails with `could not find function ":<-"`, so the only R programs it could change are ones that
already fail. The mandate's switches follow:

- **The syntax is on by default** in ry's parser and runtime, and ry warns when a project turns it off
  in `ry.toml`, as the mandate requires.
- **A typed file stays a `.R` file.** The cost the earlier draft identified remains for code that
  must also run on GNU R, such as a package submitted to CRAN; for those, a `ry build` command would write
  plain R with the inline types moved into `#:` comments.
- **Comment and inline annotations must agree.** When both annotate the same item, ry compares them
  as types, after alias expansion, and reports a warning on the inline annotation when they differ.
- **Strict mode rejects untyped code.** Today's strict mode reports the origins of `Unknown`; the
  mandate extends it to report every function parameter, return and top-level binding without a
  declared or fully inferred type.

### One type model

The checker's types (`TyKind` in `crates/semantics/src/types.rs`) are interned in the salsa database
and reference inference variables and arena names, which the runtime should not depend on. So the
runtime receives a **runtime type summary** per function: a small, owned, serializable value computed
by a tracked query from the checker's exported scheme.

- It keeps what the runtime can use: atomic type, scalar or vector shape, names, record fields,
  function arity, and nominal types.
- It collapses what the runtime cannot check cheaply (type parameters, deep unions) to "unknown".
- It adds the effect facts the JIT needs: whether the function reflects on its frame, which
  parameters it always forces, which parameters it only reads (so the JIT can borrow them, as Bend
  does), and whether it is free of effects.
- It is cached with the bytecode, and invalidated by the same salsa revision that invalidates the
  function's interface.

This keeps a single source of truth (the checker's types) and makes the runtime's copy a derived,
recomputable projection. The same summary drives strict mode's boundary checks, the JIT's initial
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
  untyped code to check.
- **Hermetic runs.** Network-dependent packages (Azure, Bloomberg via `Rblpapi`, ODBC) run against
  local mocks or fakes, so a benchmark run is deterministic.
- **Three measurements:** run the test suite under GNU R and under ry (wall time and outputs must
  match), check the code base with `ry check` (cold and incremental keystroke latency), and load it
  with `ry load` (cold and incremental reload).

## 10. Build order

Each step ends with something real running, and each one is a large, whole change.

1. **Corpus and oracle first.** Fetch the benchmark closure (the scripts in `tools/research/`
   already resolve it), install it into a GNU R library, and build a differential harness that runs
   an R file under GNU R and under ry and compares output, errors and warnings. Write the benchmark
   generator and generate small versions of the three code bases.
2. **The object model, GC and C API skeleton.** Objects, interned strings and symbols, the GC with
   `PROTECT`, and the `libR` shared library with the top 150 or so symbols, enough for a C package to
   load and run its `.Call` entry points from a Rust test harness.
3. **The interpreter and base R.** The tier-0 bytecode interpreter, closures, promises, environments,
   conditions, S3 dispatch, and base R's primitives and internals in Rust. Base R's R code (the
   `base`, `stats`, `utils`, `methods`, `tools` and `parallel` sources) is reused as R source, so only
   the internals behind `.Internal` and `.Primitive` are reimplemented. Run the base package's own
   tests through the oracle.
4. **The full C API and packages.** The rest of the 814 measured names, ALTREP, connections,
   unwind protection, C callables, `R CMD INSTALL` semantics and binary installs. Exit criterion: all
   232 packages of the closure install and pass their own tests under ry, and the three generated
   code bases pass their test suites with output identical to GNU R.
5. **Lazy loading.** The item-based loader on top of the analysis index, replacing `load_all` and
   `loadfast` for the code bases.
6. **Inline syntax, agreement warnings and strict mode,** in the parser, the checker and the
   runtime, with the runtime type summary.
7. **The JIT.** The SSA IR, speculation, deoptimization and Cranelift backend, then environment and
   promise elision, then vector fusion, driven by the benchmark code bases and a microbenchmark set
   (the Ř benchmark suite is a good public start).
8. **The table type,** on Polars, with its own benchmark against data.table.

The JIT comes late on purpose. Until real packages run, there is nothing representative to optimize,
and an object model or calling convention changed for the JIT's sake after the C API exists would
break every package. The C API and the object model are designed first, as section 2 argues, and the
JIT fits around them.

## 11. Open questions

- **Graphics.** Seven packages need the graphics engine. Implementing R's device API is large; the
  alternative is to defer graphics to GNU R in a subprocess. Undecided, and not needed for the
  benchmark's test suites.
- **Windows.** The C layer and the `dlopen` path need a Windows equivalent; the REPL's Windows work
  (`contributing/design/repl.md`) is the starting point.
- **Bytecode compatibility.** Installed binary packages contain GNU R bytecode in their lazy-load
  databases. ry can ignore it and recompile from the deparsed closures, which is the current plan,
  but it costs install time.
- **How much of base R to reimplement natively versus reuse as R source.** The plan above reuses the
  R sources, which is less work but ties ry to GNU R's base implementation details.
- **Licensing.** Reusing base R's R sources brings their GPL-2-or-later license into the runtime
  crates, which must be decided before step 3.

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
