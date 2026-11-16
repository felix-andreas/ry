# Decisions: C API, R home and packages

The `libR` C API, `R CMD`, package installation, the bundled base R, and the loader behind `ry load`, `ry test` and `ry document`.

## Packages and R versions in the runtime

**Status:** built, except the `[r]` keys of `ry.toml` and the renv pin of the corpus, which do not exist, and three superseded parts: the runtime choice ("ry run and ry R: the command chooses the runtime"), the per-package cache ("Bytecode is build output, not a cache") and the GNU R links in ry's `R_HOME` ("ry ships its own base R, and GNU R through `R_HOME` is the opt-in").

`development/design/packaging.md` holds the design and the measurements. Nothing existed before, so
there was no previous source of truth; the choices that settle architecture are these.

- **One installed package directory serves GNU R 4.6 and ry's runtime,** because ry's `libR` is
  ABI-compatible with R 4.6's headers and reports the version of the GNU R it loads base R from. ry treats the `Built:` field as GNU R does (refusing only
  pre-3.0.0 builds), so existing libraries load unchanged. A package built under ry carries an extra
  `RyBuilt:` field, and `RyAbi:` ("A package ry compiled records its headers' binary interface"),
  and the corpus is built only by GNU R.
- **ry's per-package data (object index, bytecode) lives in ry's own cache, never inside the
  package,** so the differential oracle loads byte-identical packages under both runtimes and shared
  libraries (site libraries, renv's cache) are never written to. It is filled on first load, keyed by
  a hash of the installed `DESCRIPTION` and `.rdx` plus the runtime version, because hashing the whole
  directory on every load would cost what the cache saves.
- **Package management runs as R code under the runtime** (`install.packages`, renv, pak). A package
  manager of ry's own (`ry add`, `ry sync`, a lockfile) is deliberately left open, because the runtime
  does not need it; if it comes, it must read and write `renv.lock`.
- **The runtime is chosen by configuration, never by failure** (`[r] runtime`, `--runtime`,
  `RY_RUNTIME`), because a switch on failure hides what the transition must surface. GNU R is the
  default until build step 4's exit criterion, then ry. An explicit version other than 4.6 selects GNU
  R, and combined with `runtime = "ry"` it is an error. The `renv.lock` R version and `Depends: R` are
  advisory only, or most renv projects would stay on GNU R after the flip. ry's own `R_HOME` has
  `bin/` scripts that start ry (so `parallel` and `callr` workers inherit the runtime), ry's `etc/`
  and `include/`, and `library/` and `share/` linked to a GNU R 4.6 installation.
- **The analysis reads the installed library, not a running R,** found by a fixed rule (`[r]
  libraries`, else renv's project library, else `R_LIBS`/`R_LIBS_USER` and the selected R's
  libraries; `.Rprofile` is never evaluated). Per package name it reads the installed `DESCRIPTION`,
  `NAMESPACE` and `.rdx` object names through a tracked by-name lookup, with stubs chosen by version
  range. Generated export manifests remain for base R and as the no-library fallback.
- **The corpus is pinned with renv** on a dated P3M snapshot in build step 1, because renv is in the
  benchmark set and R's own installer cannot backtrack to older compatible versions.

## The full C API surface and package installation

**Status:** built, except base R from GNU R's home, the header-only BLAS and LAPACK link line, the skipped byte-compile and the routines rewritten from published algorithms, which "ry ships its own base R, and GNU R through `R_HOME` is the opt-in", "BLAS and LAPACK come from the system, then from GNU R's Makeconf", "Bytecode is build output, not a cache" and "Numeric routines are exact ports of GNU R 4.6.2, and LINPACK lives in the interpreter crate" supersede.

This settles the part of step 4 of `development/design/r-runtime.md` that does not need the
interpreter. Before it, `libr` exported the top 150 measured names, `Rf_eval` evaluated nine
primitives, and nothing built or installed packages.

- **Evaluation is a hook the interpreter installs, not a dependency.** `libr::eval::Evaluator`
  (eval, apply, parse, force_and_call, print_value, current_env, current_srcref, dot_last) is set
  once with `install_evaluator`; every evaluation entry point of the C API (`Rf_eval`,
  `R_tryEval`, `R_forceAndCall`, `R_ParseVector`, `Rf_PrintValue`, R-level finalizers,
  `R_GetCurrentEnv`, the S4 class functions through `methods`, namespace loading in
  `R_GetCCallable` and the unserializer) goes through it. The interpreter calls libR back as
  ordinary Rust (`native_call`, `connections::*`, `serialize::*`, `altrep::*`). Without an
  evaluator the built-in evaluator of native calls runs, so libR stays testable alone.
- **BLAS and LAPACK are header-only.** `R_ext/BLAS.h` and `Lapack.h` declare the system
  libraries, which packages link themselves (`R CMD config BLAS_LIBS` is `-lblas`), as Debian's
  GNU R and every corpus binary do (they record `libblas.so.3`/`liblapack.so.3`). A Rust LAPACK
  does not exist at reference quality, and re-exporting the routines from libR would duplicate
  the system's. Where R 4.6's declarations contradict the reference routine (missing or stale
  arguments, pointer-to-pointer typos), ry's headers follow the routine; hidden string-length
  arguments always follow R.
- **libR depends on zlib, bzip2 and xz,** as GNU R's does, because binaries (gert, httpuv) call
  zlib without linking it and find it through libR.
- **Names R 4.6 dropped stay exported** (`legacy.rs`), because binaries built for older R import
  them and a missing symbol stops a library from loading at all. `TRUELENGTH` is the capacity of a
  shrunk vector; any other value lives in `Runtime::truelengths`, and its object is preserved
  while it has one, so no address is reused under a stale value. `SETLENGTH` makes the vector
  resizable. `FRAME` returns a snapshot pairlist (frames are hashed). `R_PPStack` is a real array
  that a root source scans up to `R_PPStackTop`, for code compiled with R's internal macros.
- **The graphics engine C API is real** (`crates/libr/src/graphics.rs` over
  `crates/graphics/src/engine.rs`); see "One graphics engine for base graphics, grid and
  device packages" below.
- **`.C` and `.Fortran` copy their arguments** and pass data pointers through the same
  dispatcher as `.Call` (all arguments are pointers), returning the copies as a named list.
  `.Call` accepts `NativeSymbolInfo` objects, which packages that call `R_forceSymbols` require.
- **`R_withCallingErrorHandler` runs its handler on the unwinding error,** after the native frames
  between the error and the handler are gone. GNU R runs it before unwinding; the difference shows
  only to a handler that inspects those frames.
- **Hash tables (`R_mkhashtab`) live entirely in heap objects** (an external pointer whose tag
  is `c(type, count)` and whose protected value is the bucket list), so the collector traces
  them and nothing is freed by hand.
- **ALTREP methods have three states:** default, native (through a C trampoline) and built in
  (Rust returning `ApiResult`, no `setjmp`), so ry's own compact sequences cost nothing at the
  boundary. A class handle has GNU R's layout (a raw vector whose attribute is the class,
  package and type), registered by (class, package) symbols. Its bytes start with GNU R's
  method table (`altinteger_methods_t` and relatives) because vctrs calls `Extract_subset`
  straight through `STDVEC_DATAPTR(ALTREP_CLASS(x))`; every slot is a trampoline into ry's
  dispatch, so the table depends only on the type and ry's own three-state table stays the
  single source of truth. `Rf_coerceVector` of a plain
  integer or double vector to character returns a deferred string, as GNU R does.
- **Serialization writes ALTREP objects as `ALTREP_SXP` in version 3** and reads them through
  the class registry (loading the package's namespace for an unknown class when an interpreter is
  installed), so compact sequences round-trip byte for byte with GNU R. GNU R bytecode reads as
  the expression in its constant pool, because ry does not run GNU R bytecode. Environments are
  written unhashed. Namespace and package references go through a resolver the interpreter
  replaces; the default creates registered placeholders.
- **A connection is identified by its `conn_id` external pointer object,** validated on use, so
  a stale connection object errors with "invalid connection" instead of reaching a reused slot.
  The pointer's address is not the identity, because packages overwrite it (curl stores its
  multi handle there with `R_SetExternalPtrAddr`).
  Buffering lives in each connection kind, not in `Rconn`'s `buff` fields, so
  `R_ReadConnection` after `readLines` sees exactly the unread bytes.
- **Header export coverage is checked by the compiler** (`-aux-info` and `-E` in
  `tests/test_headers.rs`), not by scanning header text, because rename macros and struct fields
  defeat a text scan.
- **Package installation is the `rcmd` crate, and it runs on ry.** ry's R home has ry's
  headers and `libR.so`, an `etc/Makeconf` written for ry and `share/make/shlib.mk`. It links
  everything else from GNU R's home: the `library/` packages (base R's R code), `share/`,
  `etc/` and the `R CMD` scripts in `bin/` (build, check, Rdconv, ...). Its own `bin/R`,
  `bin/Rscript` and `bin/Rcmd` exec `ry-rcmd`, which answers `R CMD INSTALL|SHLIB|config` and
  `R RHOME` in Rust and runs every other command line (`-e`, `-f`, a script, stdin) on ry's
  interpreter by dlopening the home's `libR.so` and calling `ry_run`. GNU R's `R CMD` scripts
  call back into `bin/R`, so they run on ry too; no GNU R process runs at install time.
  - Install: Rust configures and compiles `src/`, then an embedded R driver
    (`crates/rcmd/src/install.R`) runs the R-level half on ry with GNU R's own `tools`
    functions, in `tools:::.install_packages`' order (DESCRIPTION and `Meta/`, collated code,
    `sysdata`, lazy data, demos, `inst/`, the lazy-load database, Rd objects and help indices,
    vignette index, `nsInfo`), then test-loads in a fresh ry process. Reusing `tools` keeps the
    installed layout GNU R's byte for byte in structure, so GNU R can read what ry installs.
  - **Nothing is byte-compiled.** ry compiles closures itself when they run, and GNU R
    bytecode in a lazy-load database is read back as its source expression anyway, so a GNU R
    compile step would only cost install time. `ByteCompile: yes` is ignored.
  - The previous shape delegated `bin/R` and `bin/Rscript` to GNU R and kept `R/` as sources;
    both are gone.
- **Rmath, Applic, LINPACK and the Fortran helpers are Rust** (`rmath.rs`, `applic.rs`,
  `crates/numeric/src/linpack.rs`, `utils.rs`). Much of Rmath translates GNU R's nmath sources, which each file's
  header names (see "licensing"); LINPACK is written from the published algorithm. The oracle is GNU R itself: `tests/data/rmath_cases.R` records answers and random
  streams through the `rmathtest` package, and `test_rmath` replays them in ry's libR with a
  per-function tolerance that names its reason. The random variates draw uniforms in GNU R's
  order, so seeded streams match. `R_binom_kind` reports `BUGGY_BTPE` and `R_sample_kind`
  `REJECTION`, the defaults of the R version the corpus was recorded with.
- **An error raised while `dlopen` runs a library's static initializers surfaces as a
  `dyn.load` error,** as in GNU R: `ry_call_dlopen` sets a jump target around `dlopen`
  (Rcpp's `Function c("c")` globals call `Rf_findFun` there). Longjmping out of an initializer
  leaves glibc's recursive loader lock with that thread, as it does in GNU R, so a process that
  exits from another thread deadlocks in `_dl_fini`; R runs on the main thread, and the tests
  that provoke it (`test_load_errors`) do too.
- **`getNamespace` is a built-in of the native-call evaluator,** answered by the serializer's
  namespace resolver, because Rcpp's initializers look it up before any interpreter exists.
- **The headers keep declaring the names R 4.6 removed** (`SETLENGTH`, `SET_TRUELENGTH`,
  `FRAME`, ...) and ship an empty `R_ext/PrtUtil.h`, because pinned package sources (readr,
  isoband, yaml) still use them and `legacy.rs` exports them anyway.
- **Some numerical routines were rewritten from the published algorithms.** The LINPACK QR
  family (`dqrdc2`/`dqrls` and friends), the optimizers `vmmin`, `nmmin`, `cgmin`, `samin`,
  `R_max_col` and `R_pretty` were rewritten from LINPACK's users' guide, Nash's *Compact
  Numerical Methods*, Nelder and Mead, and the Wilcoxon and signed-rank recurrences; a licence
  audit found the LINPACK rewrite genuinely restructured. The rest of Rmath was not rewritten
  this way: TOMS 708, the normal, t and studentized-range functions and the random variate
  generators translate GNU R's C (see "licensing"). Results match GNU R exactly except where the
  system BLAS (OpenBLAS) reorders sums; those cases carry a named tolerance. The rewritten incomplete
  gamma, noncentral chi-squared and beta, and discrete quantiles are more accurate than GNU R in
  far tails (checked with mpmath), so `test_rmath` accepts a quantile when ry's own distribution
  function maps GNU R's answer back to p within GNU R's error, and a noncentral function whose
  Poisson index exceeds i32 returns NaN with a precision warning. SANN does not accept a
  custom candidate generator and always takes the Gaussian step.
  - Some Rmath helpers and quantiles were rewritten from the published methods: `stirlerr`
    from Loader (2000), with the half-integer table recomputed in mpmath; one shared
    Horner-evaluated `progression_series` in place of the continued fraction behind `log1pmx`
    and `lgamma1p`, whose surrounding code still follows GNU R's `pgamma.c`; `qgamma` as Halley's method on
    `ln P(ln y)` from Wilson-Hilferty and the leading series and asymptotic terms (Abramowitz
    and Stegun 26.4.17, 6.5.29, 6.5.32), polished to the closest double; `dnchisq` as the
    Poisson mixture summed outward from its peak (Johnson, Kotz and Balakrishnan), with
    Patnaik's two-moment approximation past a peak of 1e10; `d1mach`/`i1mach` from the PORT
    paper (Fox, Hall and Schryer 1978). `qgeom` keeps an absolute 1e-12 fuzz because that is
    what GNU R's answers show. GNU R's `dnchisq` truncates its sum at an absolute 5e-15, so in
    the tails ry differs from it but matches mpmath; `test_rmath` allows that absolute error.
- **One L-BFGS-B, written from the BSD reference v3.0** (Zhu, Byrd, Lu and Nocedal's Fortran,
  `lbfgsb.f`). `crates/stats/src/lbfgsb.rs` is the only implementation: `optim` calls it
  through `crates/stats/src/optimize.rs`, and libR's `lbfgsb` entry point only maps `nbd` and wraps the C
  callbacks. GNU R behaves as a hybrid of the versions, found by comparing every iterate bitwise
  (`%a`) against Rscript: the subspace step backtracks to the box without v3.0's Morales-Nocedal
  projection, and the Cauchy search keeps the `f2` floor (`eps * f2_org`) and is skipped for an
  unconstrained problem once memory exists. The order of additions matters to the last bit and
  is pinned with comments where it is not the reference's (grouped terms in `cmprlb`, `cauchy`,
  the subspace product and `formk`'s (2,1) block, ungrouped in its diagonal blocks). Traces,
  messages and the abnormal line-search exit were matched the same way, and
  `stats__lbfgsb_paths` and `test_numerics` hold the result exactly. The trace text for a line
  search that exhausts 20 evaluations is unverified, since no case reached it in GNU R.
- **`nlminb` runs PORT's own drivers, written from netlib's reference** (<https://netlib.org/port/>,
  the code R's `COPYRIGHTS.portsrc` relies on; GNU R's `portsrc.f`/`port.c` were never read).
  `crates/stats/src/port_minimizer.rs` holds `DRMNG`/`DRMNGB` (gradient: BFGS factor,
  double dogleg), `DRMNH`/`DRMNHB` (gradient and Hessian: More-Hebden steps) and
  `DRMNF`/`DRMNFB` (finite differences around the secant drivers), behind one reverse-
  communication `Minimizer`. GNU R's driver, found by black-box tests: a Hessian without a
  gradient is ignored; it loops "call PORT, then evaluate the gradient (and Hessian) if PORT
  asked for it and one exists, else the objective" until the code is 3 or more, so the objective
  runs once more, uncounted, at the final point; a NaN objective warns "NA/NaN function
  evaluation" and becomes Inf; the Hessian's lower triangle is read by rows; `trace` prints at
  PORT's `DITSUM` points when `niter % trace == 0`; dparck's control checks report the last bad
  `v` slot and a nonpositive `scale` gives code 18. Quirks kept to match: `DRMNGB`'s `G0` shares
  storage with `DD7DGB`'s and `DQ7RSH`'s scratch, `DD7DGB` leaves `NRED` unset with no free
  variables (a start on an active bound where the objective is 0 returns code 3 after one iteration), and `DS7BQN` keeps
  the last violating index. `DV2NRM` is BLAS `dnrm2` (LAPACK 3.12's Blue algorithm), not PORT's
  scaled norm, because GNU R links PORT's norm to the BLAS: with PORT's own norm the iterates
  differ in the last bit from the second step. `stats__nlminb_port_paths` pins the matching
  cases bitwise; the open divergences are in `backlog.md`. `nls(algorithm = "port")` still runs
  ry's own trust-region method in `crates/stats/src/port.rs`.
- **KalmanLike and `prod` accumulate as GNU R observably does.** The Kalman filter subtracts and
  adds term by term (`y -= z_i a_i`, `V[i,j] += m T`), and `prod` multiplies each argument in long
  double, combines arguments in double, and lets the first NaN win, so `prod(NaN, NA)` is NaN.
- **`R CMD SHLIB` passes `-l`, `-L` and `.a` arguments through to the link,** because configure
  scripts probe for system libraries with `R CMD SHLIB conftest.c -lfoo` (Rglpk). Other options
  are ignored, as before.
- **`Rf_rnbeta` is exported** although GNU R's library does not define it, because R's `Rmath.h`
  declares `rnbeta` and Rcpp's sugar headers refer to `::Rf_rnbeta`, so a declaration with no
  symbol fails the header coverage test and a missing declaration breaks Rcpp builds.

## The package-development loader

**Status:** built, except the ignored `if` in `NAMESPACE` and the console's own stat fingerprint, which "`if` in NAMESPACE is evaluated by the host, then resolved in text" and "Source-change detection lives in the loader, so both runtimes reload the same way" supersede.

Nothing loaded packages before; `development/design/r-runtime.md` section 6 and the user page
`reference/package-loading.md` describe the result. These choices settle architecture.

- **The index lives in `semantics`, the diff in `crates/loader`, and only a backend knows the
  runtime.** `semantics::load_index` derives per-file facts (`file_items`, `file_binds`), the
  binding join (`package_bindings`) and the package join (`package_index`) from the analysis, with
  its own input `PackageSources` so a loader never perturbs a project's `ProjectFiles`. The diff
  produces a backend-neutral `loader::Plan`; the GNU R backend (`r.rs` plus `runtime.R`) applies
  it, and ry's runtime will apply the same plan, so the runtime swap touches neither the index nor
  the reload logic.
- **A plan travels to R as an uncompressed RDS file** written in Rust (`r::rds`), because
  `readRDS` builds the vectors in C without parsing R code, and one format serves both transports:
  the `.Call` routines registered on the embedding `DllInfo` inside `ry repl`/`ry run`/`ry load`, and
  the `ry load --emit-plan` process a plain R session runs. Out of process, the loaded state is a JSON
  `LoadState` in the session's `tempdir()`, committed by renaming `<state>.pending` once R applied the
  plan, so a failed apply never records a load that did not happen; in process, `Loader::commit`
  does the same.
- **Laziness is decided per name: lazy only when the one writer defines a function.** Two writers
  make every writer eager in order, which keeps `f <- memoise(f)` correct without special cases.
- **Reload rules (each one found by the reload property test):** statements match by content in
  order (longest common subsequence) within a file; a statement reruns when new, when it read a
  changed binding, or when it writes a changed name; a rerun statement reading a name rewritten at
  or after it reruns that name's earlier writers first; the rerun set is a fixpoint that repeats
  while the changed-name set grows; a `Collate` reorder falls back to a full load.
- **Durability, not a host-side cache, keeps a reload proportional to the edit.** Files enter at
  `Durability::HIGH` and drop to `LOW` on their first edit, so after the first edit of a file
  salsa verifies untouched files in O(1). The alternative, the host reusing per-file results it
  knows are unchanged, would duplicate salsa's own bookkeeping.
- **The R runtime is byte-compiled once per runtime hash and R version and cached** under
  `dirs::cache_dir()/ry`, because R's JIT otherwise compiles each runtime function on first call and
  that alone cost more than a reload (about 200 ms per session). The cache stores the whole
  environment, because `environment<-` strips bytecode from a closure.
- **A statement's dependencies include calls.** Function items record the identifiers their body
  mentions (a syntactic superset, cheap without name resolution), and the diff closes the changed
  set over that call graph inside the rerun fixpoint, and applies the earlier-writer rule to
  everything a statement's calls reach.
- **The REPL reloads by feeding `.ry_autoreload()` as its own top-level command** before the user's
  command, when a stat fingerprint of the loaded packages changed and R is at its top-level context
  (`R_GlobalContext` has no parent; comparing prompt text broke on `options(prompt =)` and fed
  `readline()`). A failed reload is retried only after the next change on disk. This is the one place the console feeds input after startup; it is a
  user-visible command like typing `load_all()`, and it keeps `.Last.value`.
- **testthat follows pkgload 1.3's `load_all()`, not convenience.** A package with
  `tests/testthat` or `inst/tests` and testthat installed gets `library(testthat)` after its code
  and before `.onLoad` (so `package:testthat` sits behind `package:<name>`), and its `helper*.R`
  files are sourced with `NOT_CRAN=true` into the attached package environment, not the namespace,
  only when the load attaches and exports everything. Helpers used to go into the namespace, which
  let package code see them and leaked them into `pkg:::`. Their names are kept in
  `state$helper_names` so a package-environment sync does not drop them. Exports sync after
  `.onLoad`, as pkgload's `setup_ns_exports` does, so a name `.onLoad` binds is attached too.
- **Deferred:** updating other development packages' import copies of a changed binding, loading a
  rebuilt shared library under a new name, a `system.file()` shim for `inst/`, and
  evaluating `if` in `NAMESPACE` (ignored with a warning). `backlog.md` lists them.

## The loader on ry's runtime

**Status:** built.

`ry load` applies the loader's plan on ry's interpreter by default; `--runtime gnu-r` (or
`RY_RUNTIME=gnu-r`) keeps the GNU R path. `ry repl` and plain-R `load_all()` stay on GNU R.

- **Previous shape:** the plan crossed to R as an RDS file whose code entries carried source text,
  which GNU R parsed again on force, so every forced function was parsed twice (once by the
  analysis, once by R).
- **Target shape:** one R runtime (`runtime.R`) with backend hooks (`.ry_request`,
  `.ry_sources`, `.ry_bind_lazily`, `.ry_run`). GNU R's hooks (`gnur.R`) keep the RDS plan and
  `makeLazy`; ry's hooks (`ry.R`) call `.Internal(ryLoaderPlan/Commit/Bind/Run/Force)` in
  `libr::loader_backend`, which keeps the plan in process. A lazy binding is a promise over a call to
  `ryLoaderForce(tree, start, end)`, where `tree` is an external pointer to the file's `Parse`
  from the analysis, and forcing lowers the node spanning `start..end` with
  `interp::lower::lower_node`. The plan carries byte offsets (`SourceSpan::source_start/end`) so the
  node is found without text.
- **Why a tree, not text:** the analysis already parsed every file, the tree is an `Arc` and cheap
  to clone, and lowering a node skips the lexer and parser entirely. Memory is the trees
  the analysis already holds. The finalizer drops a tree after no promise of its file remains, at the
  collector's next finalizer point (a native call's return, `gc()`, or exit), not immediately.
- **Why `.Internal`s and not `.Call`:** the interpreter registers Rust builtins directly, so the
  backend needs no `DllInfo`, no RDS round trip and no source strings in R memory.
- **Planning runs on its own thread** with `ANALYSIS_STACK_SIZE`, as every analysis entry point
  does, because the analysis recurses as deeply as the code nests.
- **The first plan of a process is made cheap, not cached.** A process's first plan is a full load,
  even when no file changed since the last process, so it pays `diff` over the whole package.
  - **What was slow:** `deferred_s4` and `lazy_generics` scanned a `Vec` of generics or classes
    once per S4 definition, and `lazy_generics` walked every definition once per generic. That
    cost grows with generics × definitions, so an S4-heavy package paid most. The full-load path
    also built reload-only state it never read: `flipped`, the eager LCS, `changed` and the
    rerun fixpoint.
  - **Target shape:** hash sets of the package's own classes and generics; one pass that groups
    each generic's statements; per-file start offsets instead of a map of item positions; and a
    full load skips the reload bookkeeping, since it reruns every statement anyway.
  - **Why not a stamp-keyed plan:** persisting the full plan would add a second on-disk copy
    derived from the index, which the index cache already keys by content. Once the diff is
    linear, the plan is a small fraction of a load.
  - **Impact:** on a 591-file, 65,000-line S4 package (`tools/loader-bench/s4_package.py`),
    `ryLoaderPlan` drops from 38 ms to 14 ms, and the plan thread from 116M to 35M
    instructions, with a byte-identical plan.
  - **What remains, all linear in package size:** `diff` itself (about 7 ms there), reading
    the sources' stamps (3 to 6 ms), and converting the plan to an R value (about a third of the plan thread). The
    `prepare` thread also deserializes the cached index (18 ms for 1.7 MB of bincode) while R
    starts. That runs in parallel unless the plan arrives first.
- **Runtime fixes the load path needed, each pinned by an oracle case:** `readDCF`, `load()`/`save()`
  of XDR workspaces (`R/sysdata.rda`, gzip, bzip2 or xz through `gzfile()`), `readChar` on a connection, comparisons and `&`/`|` keeping
  only names, dim and dimnames, and two hot paths (`match()` on interned ASCII strings by pointer,
  `names(env)` and `ls()` from print names without a string round trip).
- **Impact:** on the 326,000-line bench package a cold load is faster than the GNU R backend on the
  same machine and a one-function reload slightly slower (932 ms and 58 ms against 1,036 ms and
  53 ms); on the 14,000-line package the cold load is slower (104 ms against 82 ms), because the
  interpreter evaluates the eager statements more slowly than GNU R. Correctness
  and the reload logic are shared, because both backends apply the same plan; `test_load.rs` runs its
  namespace, S3/S4, collation and reload tests on both runtimes.

## libr reads options from base's `.Options`

**Status:** built.

- **Previous shape.** libr's `Runtime` held its own options environment that nothing wrote, so `Rf_GetOption1` always returned `NULL` and packages (data.table's `datatable.*` options) saw defaults only.
- **Target shape.** `Rf_GetOption1` walks `interp::envir::options()`, the interpreter's `.Options` pairlist, which is the single store `options()` writes.

## One serializer writer, in libr

**Status:** built.

- **Previous shape.** The interpreter (`crates/interp/src/serialize.rs`) had its own writer behind `.Internal(serialize)`, `serializeToConn` and `saveToConn`, and libr (`crates/libr/src/serialize.rs`) had the full `R_Serialize` writer that `saveRDS` and native code used. The interpreter's copy wrote only XDR version 3, so `serialize(x, NULL, version = 2)` gave version-3 bytes and digest's hashes of version-2 serializations were wrong.
- **What was duplicated.** Two writers for one format, which drift: the interpreter's lacked versions, ASCII, native byte order and refhooks.
- **Target shape.** libr's writer is the only one. libr registers the four internals (`serialize`, `serializeb`, `serializeToConn`, `saveToConn`) through `interp::register_builtins`, the same way the loader's builtins arrive, because the interpreter cannot depend on libr. The header is written one field per output call, as GNU R does, since digest's `spooky` algorithm counts the calls through a custom output function.
- **Impact.** One source of truth for the format, version-2 output and refhooks for free, and digest's suite passes. The readers are still duplicated (`interp::serialize`'s `unserialize` and libr's `R_Unserialize`); `backlog.md` tracks merging them the same way.

## A package ry compiled records its headers' binary interface

**Status:** built.

- **Previous state.** An installation by ry recorded only `RyBuilt:` (ry's version). Nothing tied a package's compiled code to the headers it was compiled against, so when the inline `DATAPTR_RO` and `STRING_PTR_RO` stopped bypassing the write barrier ("Read-only element pointers escape too"), every package ry 0.4.5 or earlier had compiled kept the unsafe inline code and nothing said so.
- **Shape.** `rcmd::NATIVE_ABI` is the version of the binary interface the headers' inline accessors give compiled code; `R CMD INSTALL` under ry writes it as `RyAbi:` beside `RyBuilt:`. `dyn.load` (`crates/libr/src/eval.rs`) refuses the shared library under a package's `libs/` when its `DESCRIPTION` has `RyBuilt:` and a missing or different `RyAbi:`, with an error that names the package and says how to reinstall it (`rcmd::stale_native_build`). The comment over the inline accessors in `Rinternals.h` says to bump the constant. Pinned by `crates/ry/tests/scenarios/install-stale-binary.scenario`.
- **Refuse, not reinstall.** Reinstalling at load would need the source, the repositories and the compilers inside a `library()` call, and would rewrite a library (a site library, renv's cache) that may not be the session's to write. A clear error is predictable and names the one command that fixes it.
- **Not a version comparison.** `RyBuilt:` moves with every release while the headers rarely change, so keying on it would force a reinstall per upgrade. Equality on `RyAbi:` refuses exactly the binaries whose inline code differs, newer ones included.
- **In-place builds are stamped too.** `ry load` builds `src/` in place, and an install copies a source tree with the objects such a build left, but make rebuilds only what the sources outdate, so objects from old headers would be reused (and an install would stamp them current). `rcmd::compile_in` keeps `src/.ry-abi` and deletes the objects and shared libraries under `src/` when it is missing or differs; static libraries stay, since a package may ship one prebuilt. Pinned by `load-stale-objects.scenario`.
- **GNU R's builds load unchanged.** GNU R's package headers reach element pointers through libR's functions, so they skip nothing ry does on their behalf; they carry no `RyBuilt:` and are not checked.

## ALTREP elements go through the class methods everywhere

**Status:** built.

- **Previous shape.** libr's C entry points dispatched ALTREP `Elt` methods, but the interpreter read and wrote vector elements through `Sexp::element` and `Heap::set_element`, which read raw storage. About 370 interpreter call sites then saw an ALTREP instance's private triple instead of its elements (nanoarrow's character columns crashed DBI's arrow round trip).
- **Target shape.** `heap::object::install_altrep_elements` takes a read and a write function, which libr installs at startup, and `Sexp::element` and `Heap::set_element` dispatch to them for an ALTREP object, so there is one access path and no call site can forget it. The accessor cannot fail, so an unwind from the method (an error, a condition handler's jump, an interrupt) is parked in the interpreter (`interp::unwind::park`) and raised as soon as the builtin that made the read returns (`call_builtin`), before its value reaches any R code, so `tryCatch(x[[2]], ...)` catches it; a read outside a builtin raises it at the evaluator's next poll instead. Meanwhile the read returns `NA` (strings) or `NULL`. `length()` asks the class's `Length` method, and `[[` on a string or list instance reads only the element it selects, so an R caller runs `Elt` exactly where GNU R does (`r_code_reads_only_the_string_elements_it_asks_for` in `crates/libr/tests/test_altrep.rs`). Numeric instances are still materialized whole for slice access. Making `element` fallible would touch every call site for a case only ALTREP classes reach. An ALTREP instance is never marked mutable, so the interpreter copies it before modifying it in place, as GNU R's `DATAPTR` contract implies.

## The presented R version comes from the base package the runtime runs

**Status:** built, except the source of the revision and date, which "The runtime emulates the GNU R version it loads base R from" supersedes.

- **Previous shape.** Three sources: `Rversion.h` and `libr`'s `R_VERSION_STRING` said 4.6.2, `rcmd::R_VERSION` said 4.6.2, and the interpreter's `R.version` said 4.3.3, while `packageVersion("base")` came from the linked GNU R library. A package compiled from source took its R 4.6 C branches while its R code took the 4.3 branches: rlang's C made `ns_registry_env()` defunct and its R code still called it, which broke pkgload, generics and downlit; presenting 4.6.2 in R as well made `is_installed("base", version = getRversion())` false and sent R code to 4.4+ base functions that a 4.3 base lacks.
- **Target shape.** One source of truth: `library/base/DESCRIPTION` of the R home (`Version`, `Revision`, `Last Changed Date`). The interpreter reads it for `R.version` and `getRversion()` (`interp::system::r_version`), `serialize` writes it as the writer version, and `rcmd::assemble` generates the home's `include/Rversion.h` from the same file. The revision is raised to at least 89194, because ry implements the API data.table guards on it. The checked-in `crates/libr/include/Rversion.h` (4.6.2) serves only code built directly against `crates/libr/include`.
- **Why not always 4.6.** The runtime runs the base R code of the linked installation, so a version claim above it is false at the R level. With an R 4.6 library linked, as the packaging design intends, ry presents 4.6 and packages take their modern-API branches; with 4.3.3 linked, they take the 4.3 branches, which match the GNU R baseline the corpus is measured against.
- **Impact.** Packages installed from source must be reinstalled after a home's base version changes. `crates/libr/tests/test_install.rs` pins that `getRversion()`, `packageVersion("base")`, `Rversion.h` and `R --version` agree.

## ry run and ry R: the command chooses the runtime

**Status:** built.

- **Previous shape.** Two commands each carried a `--runtime` flag with its own default: `ry run script.R` and the `ry repl` console defaulted to GNU R, `ry load` to ry's runtime, and `RY_RUNTIME` overrode all three. Which runtime ran a file depended on the command, a flag and an environment variable, and the docs showed `ry run --runtime ry` everywhere. `ry run` also took FILE as its own positional, so clap still parsed `--help` or `--no-load` written after it.
- **Target shape.** The command names the runtime (`crates/ry/src/command_line.rs`). `ry run [FILE] [ARGS]` runs on ry's runtime, `ry R [FILE] [ARGS]` on GNU R, and without FILE each opens the console. `-i FILE` runs FILE and then opens the console, which is the home of `ry repl -f`. Both consoles load the package the working directory belongs to, and `--no-load` turns that off. `Command::session` resolves every spelling to one `Session { runtime, mode }`, so `main` dispatches on what to run.
- **Compatibility.** `ry repl` stays as a hidden alias with its GNU R default and its `-f`; `--runtime` on `run`, `repl` and `load` and `RY_RUNTIME` stay honoured but undocumented. The flag wins over the variable; `ry R` ignores the variable, because a command named after GNU R must run GNU R. Like every option, `--runtime` must come before FILE now: `ry run script.R --runtime gnu-r` hands `--runtime gnu-r` to the script on ry's runtime. That follows `Rscript` and was accepted deliberately.
- **One trailing positional.** FILE and its arguments are one `trailing_var_arg` vector: clap stops reading options only once a trailing positional has its first value, so everything after FILE reaches the script verbatim, as with `Rscript`. On GNU R the arguments follow `--args` in the embedded argv, so `commandArgs(trailingOnly = TRUE)` reads them on both runtimes. `crates/ry/tests/test_command_line.rs` fuzzes the parser against this property. `--keybindings` and `--no-load` conflict with a script, because they would be silently ignored there.
- **Exit codes.** 0 for success, 1 for a top-level error or a strict-mode refusal, 2 for a usage error, an unreadable script (read up front, before any R starts) or a runtime that cannot start; `q(status = n)` exits n. A batch run on GNU R starts R non-interactive with `--no-echo`, as `Rscript` does: a script sees `interactive()` as FALSE, a top-level error prints "Execution halted" and exits 1 through R's own path, and R prints no trailing newline when the session ends.
- **`-i FILE`** is checked under its own name with the same `Preflight` the console then continues (`Preflight::rename`), so a refusal exits 1 before the console opens, later commands see the file's definitions, and the file is fed as already-checked console input.
- **Impact.** One rule for users (the command is the runtime) and one resolution function, `resolve_runtime`, which `Command::session` and `ry load` both call; the property tests pin it. No analysis state changes.

## Source-change detection lives in the loader, so both runtimes reload the same way

**Status:** built.

- **Previous shape.** The ry binary kept a per-root fingerprint map beside its `Loader` (`crates/ry/src/load.rs`), updated by the GNU R `.Call` routines, and its `ReloadWatcher` compared it before each command. `libR.so` has its own `Loader` (`crates/libr/src/loader_backend.rs`) that the binary cannot see, so ry's console had no watcher, no `load_all()` and no package autoload. The fingerprint was duplicated state outside the loader that owns what is loaded.
- **Target shape.** `Loader` records the fingerprint of the sources a plan read (taken before reading, so an edit made mid-read counts later) and promotes it on `commit`. `Loader::changed_roots` reports each committed package whose fingerprint moved, once per change. Both backends ask it: the binary's `ReloadWatcher` for GNU R, and for ry's runtime `libR.so` exports `ry_loader_changed`, which `repl`'s `RuntimeReloadWatcher` calls before each command. `ry_repl` attaches `tools:ry` and runs a startup load, so `ry run` in a package loads it as `ry R` does.
- **Impact.** One source of truth per loaded package, one console loop on both runtimes, and the reload is the same `.ry_autoreload()`. The check stats `DESCRIPTION`, `NAMESPACE` and the code files in `R/` (the same extension test `collated_r_files` uses, so editor swap files do not reload) without reading them. It overlaps the per-file `stamps` that `Loader::read` keeps, on purpose: `stamps` decides which texts a plan rereads, the fingerprint decides whether to plan at all, and merging them would make the per-command check walk `Collate` too. `crates/loader/tests/test_reload_fuzz.rs` and `test_repl_e2e.rs` pin it.

## A reload re-derives one file and rewrites only the bindings it changed

**Status:** built.

- **Previous shape.** `package_index` built the package's name table (`names`) in its own body from every file's items, so any edit re-ran a loop over all 25,000 items of a large package; `loader::diff` then rescanned every name for a laziness flip, rebuilt the eager and constant lists, and compared the S3 directive sets in full. Each item's load facts were a tracked query (`item_load`, with `item_syntax` and `item_node` behind it), and on the R side every reload resynced the namespace's exports and the attached package environment by rescanning all 3,100 exports. A body edit on the 326,000-line package took 35 to 60 ms to reach output, 18 to 40 ms of it planning.
- **Target shape.** The binding facts are a firewall of their own: `file_binds` per file and `package_bindings` joining them into `PackageBindings` (laziness per name, each lazy name's item position, the eager and constant positions in collation order), returned behind an `Arc`. A body edit leaves a file's binding facts equal, so `package_bindings` keeps its value and `diff` sees the same `Arc` (`Arc::ptr_eq`) and skips the laziness flip scan. An item's load facts are computed inside `file_items` from its node rather than memoized per item. The plan carries `keeps_names` (every binding it writes was bound before and none goes away), which lets `runtime.R` skip the exports resync and copy only the touched names into the package environment.
- **Why per file, not per item.** Salsa re-verifies every memo whose durability an edit's input had: the package's files start at high durability, so the first edit of each file re-verifies every memo the plan reaches, and only later edits of the same file (now low durability) take the durability shortcut. One memo per file is far cheaper to re-verify than several per item; the first edit of a new file still re-verifies about 8 memos per file and 5 per eager item. Starting files at low durability would make every edit pay that cost, so it was rejected.
- **Impact.** On the 326,000-line package a repeated body edit reaches output in 9 to 12 ms (plan 6 to 8 ms), and the first edit of each file in about 30 ms. `test_incremental.rs` pins that a body edit re-runs one file's layer and stops at `package_bindings`. The cold load also warms `file_binds` across cores.

## A cold load is analyzed while base R starts

**Status:** built.

- **Previous shape.** The first `load_all()` of a console or `ry load` session planned from nothing after base R had loaded, so parsing and indexing the package (about 400 ms on the 326,000-line package) ran strictly after R's own startup (about 140 ms). The plan also deep-cloned every indexed item into the diff's working lists, re-erased each item's text a second time only to hash it, and matched erasure marks through a hash lookup at every token boundary.
- **Target shape.** `Loader::prepare` reads the sources and derives the whole index, `package_index` included, without making a plan; the host calls it on a thread before starting R (`ry_loader_prepare` exported from libR for ry's runtime, `load::prepare` for GNU R). The plan that follows takes the loader's lock, finds everything memoized, and only diffs. Strict mode moved to an atomic beside ry's runtime's session, so applying the session's flags never waits for the prepare to release the lock. The diff borrows items instead of cloning them, an item's hash comes from its slice of the file's erased text, and the eraser walks marks sorted in a single pass.
- **Impact.** Cold `ry run` on the 326,000-line package went from about 0.95 s to about 0.74 s (median of seven), and the analysis has no second source of truth: prepare and plan share one salsa database, so a file edited between them is read again and re-derived as usual.

## ry refuses a `libR.so` built from other sources

**Status:** built.

- Cargo builds `libR.so` separately from `ry`, so after an edit to a shared crate (the parser, the
  interpreter) a leftover library silently ran old code. `libr` and `repl` both hash the sources
  (`src/`, `include/`, `build.rs`) of `libr` and every workspace crate it links, found by following
  `path` dependencies (`crates/libr/build_identity.rs`, included by both build scripts).
  `libR.so` exports the hash as `ry_runtime_id`, and `ry` compares it right after `dlopen`, then
  exits 2 naming `just build` and `cargo build -p libr`.
- Contents, not timestamps or a git commit: a rebuild or `touch` that changes nothing keeps the
  identity, git is not needed, and the two binaries of one `cargo install` or release build always
  agree. Edits to crates `libr` does not link (the formatter, the IDE layer) leave it alone.
- Not covered on purpose: `Cargo.toml` files (adding a test target must not look like a runtime
  change), `Cargo.lock` and the compiler version. A dependency bump alone does not trip the check.

## The C headers declare the API of the presented R version

**Status:** built, except the default version 4.3.3, which "ry ships its own base R, and GNU R through `R_HOME` is the opt-in" supersedes.

- ry presents the R version of its base `DESCRIPTION` (4.3.3), and packages branch on `R_VERSION`
  to backport newer API. If the headers declared every name ry implements, those backports would
  collide (rlang's `ANY_ATTRIB`, `CLEAR_ATTRIB` and `R_mapAttrib`, yaml's `R_mkClosure`), and names
  newer R removed would be missing (Rcpp's `VECTOR_PTR` below 4.5).
- So every declaration GNU R added in 4.4, 4.5 or 4.6 sits behind `#if R_VERSION >= R_Version(...)`,
  with the version taken from GNU R's own headers, and `VECTOR_PTR` is declared below 4.5. The
  headers include `Rversion.h`, which `ry-rcmd assemble` generates for the home.
- The crate's fallback `Rversion.h` says 4.6, so ry's own tests and code built straight against
  `crates/libr/include` still see every name. `libR.so` exports all of them regardless.

## macOS packages bind to ry's libR

**Status:** built.

- **Before.** `crates/libr/build.rs` set the export list and soname for Linux only, so a macOS
  `libR.dylib` exported nothing from the C layer and had no stable install name, and nothing made
  a GNU R package binary call into it. On Linux the problem never arises: a package records
  `libR.so` by soname, and ry's library, loaded first with `RTLD_GLOBAL`, satisfies it.
- **The macOS problem.** Mach-O uses a two-level namespace: every undefined symbol in a package
  binary is bound to one named library. CRAN's binaries link `-framework R` and name
  `/Library/Frameworks/R.framework/Versions/4.x-<arch>/Resources/lib/libR.dylib`; Homebrew's
  link `-lR` and name Homebrew's `libR.dylib`. dyld would load GNU R's libR beside ry's, and the
  package would run against GNU R's heap.
- **Options weighed.** Forcing a flat namespace (`DYLD_FORCE_FLAT_NAMESPACE`) needs an environment
  variable set before ry starts, is stripped by SIP for protected binaries, and changes binding for
  every library in the process. Interposing (`DYLD_INSERT_LIBRARIES`) has the same launch problem.
  Rebuilding every package from source gives up CRAN's binaries.
- **Shape.** ry's `libR.dylib` has the install name `@rpath/libR.dylib`, and its exports are
  rustc's list plus a second `-exported_symbols_list` naming the `RY_EXPORT` C symbols (ld64 and
  ld64.lld merge the two lists). When `dyn.load` opens a Mach-O library whose load commands name
  GNU R's libR (`crates/libr/src/macho.rs`), it opens a copy instead, with that load command
  rewritten in place to `@rpath/libR.dylib` and re-signed ad hoc with `codesign --force --sign -`,
  because arm64 refuses to map a modified, unsigned image. The rewrite also zeroes the compatibility version the
  load command records (GNU R links libR with its own version, and ry's libR has none, so dyld
  would refuse it). dyld satisfies a load command from an
  already loaded image whose install name matches, so the copy binds to ry's library. The copy is
  cached under the user cache directory keyed by the rewrite's version (`REBIND_VERSION` in
  `dynload.rs`, bumped whenever the rewrite changes), path, size and mtime, and a library that names no
  libR is recorded with a marker so it is read once. The DLL record keeps the original path.
- **ry-built packages.** `rcmd` links with `-undefined dynamic_lookup` and an empty `LIBR`, so the
  binary records no libR and resolves R's API in the flat lookup that falls back to every loaded
  image. The same build then loads under ry and under GNU R, and needs no rewriting.
- **Relative dependencies.** The copy lives in the cache, outside the package's `libs/`, so the
  cache mirrors the original's surroundings with links (`mirror_surroundings` in `dynload.rs`):
  the copy's directory links every file beside the original, and `macho::Rebound::climb`, the
  most `..` components any `@loader_path` load command or rpath starts with, decides how many
  ancestor directories are mirrored too. Rewriting `@loader_path` to the original's absolute
  directory was rejected: the longer string does not fit in the load command in place, and
  growing load commands needs header padding and moves offsets. `.target` in the cache entry
  records where the copy sits in the mirror.
- **Known gaps.** CRAN's own `libRblas.dylib` could pull in GNU R's libR if a package links it
  directly; unobserved. An `@rpath/../x` name combined with an `@loader_path` rpath is not counted
  in the climb. The install-name match is reasoned from dyld's source and verified only once the
  release smoke test runs on a Mac.

## Windows runtime: `R.dll` and `ry.exe` for windows-gnullvm, linked with lld

**Status:** built, except shipping `R.dll` beside `ry.exe`, which "A release is `ry` alone, with the runtime embedded and the R front end folded in" supersedes.

- **Before.** The runtime called libc directly (`dlopen`, `umask`, `utimensat`, `nl_langinfo`,
  `iconv`, `kill` and signal numbers, process priorities, `getpwnam`), so it built only on Unix, and the Windows archive
  carried `ry.exe` alone.
- **Target.** `x86_64-pc-windows-gnullvm` for both binaries: the mingw-w64 ABI Rtools builds
  packages with, on the UCRT that Rtools 4.2 and newer link. `windows-gnu` links `msvcrt.dll`, so
  `errno`, `FILE *` and `malloc`/`free` pairs crossing the API would hit two C runtimes, and an MSVC
  `ry.exe` loading a mingw `R.dll` mixes runtimes the same way, so neither is shipped. Package DLLs
  import from `R.dll` by module name, and the loader reuses an already loaded module with the same
  base name, so naming ry's library `R.dll` binds them with no rewriting. Exports come from
  `__declspec(dllexport)` in `native.c` plus rustc's own exports, so there is no `.def` file to
  drift from the code; the linker writes `libR.dll.a` for `rcmd`. `LibExtern` is
  `__declspec(dllimport) extern` under `_WIN32`.
- **One platform seam.** Every operating-system call the interpreter makes goes through
  `crates/interp/src/platform.rs`, which has a Unix and a Windows `imp` with the same functions.
  Operating-system *calls* never happen outside it, so a new libc use shows up as a Windows build
  error in one file; platform *conventions* (`.Platform`, the platform string, `libs/x64`, the
  routine table) still branch on `cfg!` where they are used. The heap's address-space
  reservation has its own small seam (`virtual_memory` in `crates/heap/src/space.rs`: `mmap` or
  `VirtualAlloc`). Windows has no `iconv(3)`, so `PortableIconv` converts with `encoding_rs` behind the
  same interface `Riconv` exports.
- **lld is required.** GNU ld 2.41 (Ubuntu's mingw-w64) emits a `HIGHLOW` base relocation for
  each `SECREL32` thread-local offset, so a relocated `R.dll` (ASLR, or a base-address collision)
  computes a garbage address on every `thread_local` access. lld emits none. mingw's `gcc` is
  configured `--with-ld` and ignores `-fuse-ld=lld`, so `clang --target=x86_64-w64-mingw32
  -fuse-ld=lld` is the linker driver (`development/design/r-runtime.md`, section 11, has the
  command).
- **A 64 MiB main-thread stack** for `ry.exe` (`crates/ry/build.rs`): the session evaluates on the
  main thread, and base R's startup overflows Windows' 1 MiB default. The UTF-8 code-page
  manifest the MSVC build embedded through linker flags goes in as a `windres` resource object,
  so the gnullvm build needs `windres` (`WINDRES`, else `llvm-windres` or `windres` on `PATH`).
- **Jumps use `_setjmp(buffer, NULL)` and `longjmp`.** mingw's `setjmp` macro records the frame
  and `longjmp` then unwinds through SEH. clang's `__builtin_setjmp` stores the canonical frame
  address, but its landing code uses it as the frame pointer, so `R_UnwindProtect`, whose frame
  pointer sits above its stack pointer, resumed with garbage locals when an R error landed in it.
  A null frame makes the CRT's `longjmp` a plain register restore, as on Unix.
- **`xerbla_` never jumps over Rust.** Windows' R.dll exports it for R's `Rblas.dll`/`Rlapack.dll`.
  It raises an R error for a package's call, but returns while the interpreter's `DirectFortran`
  guard (`linalg.rs`) is alive, and the routine's negative `info` reaches the Rust caller.
- **Two modules named `R.dll`.** ry's runtime beside `ry.exe` shares GNU R's DLL name, so `ry R`
  loads GNU R's `R.dll` first by full path; later by-name imports bind to the loaded module.
- **Toolchain.** The release runner uses MSYS2's clang64 environment (`msys2/setup-msys2`).
  Cross-building from Linux works with the host clang and lld over MSYS2's clang64 sysroot
  packages; packages for tests are built with the Rtools ABI from MSYS2's ucrt64 GCC 14 headers
  and static `libstdc++` (GCC 16's needs a newer clang than 18).
- **Windows-only routines** that `utils` and `grDevices` reference at load time are registered in
  `WINDOWS_ROUTINES` (`builtin_libraries.rs`), chained in only on Windows so Unix sessions keep
  GNU R's Unix routine table.
- **Impact.** `ry run` runs base R and Rtools-ABI package DLLs (jsonlite, data.table, digest, cli,
  uuid, Rcpp with exceptions crossing R errors) under Wine with no GNU R binary executed, and the
  release workflow ships `R.dll` beside `ry.exe`. Open items are in `r-runtime.md`, section 11.

- The runtime can be exercised under Wine on Linux. Install `wine64` (the binaries land in `/usr/lib/wine`), set `R_HOME` to `Z:<home>`, and unpack a GNU R home with `innoextract`. A package directory without its native library needs a zero-byte `library/<pkg>/libs/x64/<pkg>.dll` placeholder. `WINEDEBUG=+seh,+loaddll` shows exceptions and library loads, `llvm-objdump --disassemble-symbols` reads a faulting function, and `pip install zstandard` lets Python unpack an MSYS2 `.pkg.tar.zst`.
- `getwd()` answers with `/` separators on Windows, and `basename` and `dirname` split at both separators.
- `do_system` chooses its contract by argument count: seven arguments are the Windows form, which runs the command without a shell, and three are the Unix form, which runs `sh -c`. `crates/interp/tests/test_system.rs` exercises both on Linux.
- A cross-link for `x86_64-pc-windows-gnu` needs an import library for `synchronization`, which mingw does not ship. The flake's `preBuild` synthesizes it with `zig dlltool`. A crane build of the dependencies alone links nothing, so it cannot catch a break in that step.

## ry ships its own base R, and GNU R through `R_HOME` is the opt-in

**Status:** built.

- **Before.** `ry run` loaded the base packages' R code, lazy-load databases, help and `share/`
  files from an installed GNU R (found through `R_HOME`, the usual install paths, then `R RHOME`),
  which was the only reason a user needed GNU R for ry's runtime.
- **Now.** ry pins one GNU R release, R 4.6.2 (`tools/base-r/VERSION`, SHA-256 in
  `tools/base-r/SHA256SUMS`, patches in `tools/base-r/patches/`). `just base-r` downloads the
  tarball, checks it and runs `ry-rcmd build-base` (`crates/rcmd/src/base.rs`), which replays GNU
  R's `src/library/Makefile` on ry's own interpreter: each step is a separate `ry-rcmd R --vanilla`
  process against the staging tree, as GNU R's make runs `R_EXE`. The output (`target/base-r`, about
  23 MB) is GNU R's layout, so `system.file`, `R.home()`, help and `tools:::` code work unchanged.
  `just package` embeds it zstd-compressed (`RY_EMBED_BASE`, `crates/repl/build.rs`) and `ry`
  extracts it to `cache/ry/base/<key>` on first use, like libR.
- **Lookup order** (`repl::base_r`, `interp::system::find_r_home`): a non-empty `R_HOME` is
  authoritative and must hold base R (no fallback, so a typo is an error, not silent R 4.6.2);
  then `RY_BASE_R`; then a complete tree (`ry-base.txt` present) at `base-r/`, `../base-r`,
  `../../base-r` or `../lib/ry/base-r` beside the executable; then the embedded copy. The GNU R
  search paths and `R RHOME` are only for `ry R` (`crates/repl/src/gnu_r_home.rs`).
- **No GNU R compiled code.** The default packages' routines are ry's (`builtin_libraries.rs`), so
  those packages get a placeholder `libs/<pkg>.so` for `library.dynam` to find. grid and splines
  are compiled from the tarball's C sources against ry's headers (`compile_native`), as before.
- **No precompiled bytecode** in the lazy-load databases (coordinated with the bytecode work):
  ry's interpreter compiles on its own terms.
- **The makebasedb check is rewritten,** because every ry session has `tools:ry`, `package:std`
  and `package:ry` attached: the step ignores them instead of demanding a bare search path.
- **BLAS/LAPACK** come from the system (Linux `libblas.so.3`/`liblapack.so.3`, macOS
  Accelerate). Windows has no system LAPACK and no bundled reference build yet, so linear algebra
  there needs `R_HOME` naming GNU R (`backlog.md`).
- **Embedded archive format:** sorted entries of a kind byte (0 directory, 1 file, 2 executable),
  a u32 LE path length and path, and for files a u64 LE length and the bytes, zstd-compressed as a
  whole and keyed by the FNV hash of the compressed bytes. Paths with empty or `..` parts are
  rejected on unpack.
- **Version emulation is kept for the opt-in:** `R_HOME` naming GNU R 4.3 or later makes ry
  behave like that release ("The runtime emulates the GNU R version it loads base R from" below).
  The oracle's expectations are GNU R 4.6.2 output, so CI sets `R_HOME` to 4.6.2.
- **Impact.** One source of truth for the R version a user gets, with the same tree shape as
  GNU R's. Start-up from that tree is cut by the start-up image ("The start-up image is written
  in process and keyed by what start-up reads" below).

## The runtime emulates the GNU R version it loads base R from

**Status:** built.

- **Scope.** The default is ry's own base R 4.6.2 ("ry ships its own base R" above); this
  applies when `R_HOME` names a GNU R installation.
- **Before.** ry's runtime always presented as R 4.6 at the R level, while its internals followed
  R 4.3's behavior, so base R 4.6's own R code failed its `Built:` check or called internals that
  had changed or did not exist yet.
- **Now.** The version comes from the base package in `R_HOME` (`interp::system::r_version`:
  `library/base/DESCRIPTION` for the version, `SVN-REVISION` of the GNU R home that base links to
  for the revision and date, clamped to at least 89194). `R.version`, `getRversion()`, the `Built:`
  check, the serialization writer version and the `Rversion.h` that `rcmd` generates all report it. Behavior that GNU R changed between versions is gated with
  `interp::system::r_at_least(major, minor)` at the one place it differs (strptime in a DST gap,
  `psignrank` rounding, `%*%` argument counts, `rbind`/`cbind` passing `deparse.level` to S3
  methods, `is.atomic(NULL)`, complex formatting, short hex colours, the graphics engine version).
  Additions such as `Tailcall`, `Exec`, `declare` and zstd compression exist under every version,
  because no older base R calls them.
- **Range.** GNU R 4.3 and later; an older `R_HOME` stops `ry run` with an error naming the version.
  The oracle's expectations are recorded with GNU R 4.6.2; ry with `R_HOME` at GNU R 4.3.3 is
  checked against a 4.3.3 recording of the same cases (`ORACLE_R` and `R_HOME` select the
  installation). Gates for 4.4 and 4.5 were placed from GNU R's sources and NEWS, not from
  running those versions.
- **The default packages' routine table is the union over versions** (`builtin_libraries.rs`),
  but a routine a release does not register is hidden from lookup and from `getDLLRegisteredRoutines`
  (`registered_in_release`: stats' `pKolmogorov2x` and `pKS2` before 4.4, `pkolmogorov_two_exact`
  and `pkolmogorov_two_limit` from 4.4). The table's positions stay fixed, because routine
  pointers and image tokens index it.
- **The C API surface stays R 4.6's whatever the version,** because a newer API is a superset; only
  the version macros in the generated `Rversion.h` follow base. A package compiled from source must
  be reinstalled when the linked base version changes.
- **`Tailcall` re-evaluates the call in the caller's frame without reusing it,** so it gives the
  right result but not constant stack depth; true tail calls are in `backlog.md`.

## The C API draws from the interpreter's generator

**Status:** built.

- **Before.** `libr` kept its own Mersenne Twister for `unif_rand`, `norm_rand`, `exp_rand` and
  `R_unif_index`, beside the interpreter's `interp::random`, which implements every RNG kind. The
  two shared only `.Random.seed`, so a package calling `unif_rand` after `RNGkind("Marsaglia-Multicarry")`
  read a seed its copy could not parse (doParallel's stress test failed with "'.Random.seed' is not
  a valid integer vector"), and `R_sample_kind` always answered `Rejection`.
- **Now.** `unif_rand`, `norm_rand`, `exp_rand` and `R_unif_index` call `interp::random`, the
  `Rf_r*` deviates are `interp::distributions`' functions drawn through `SessionDeviates`, so no
  deviate algorithm exists twice, and `GetRNGstate`/`PutRNGstate` are the interpreter's
  `get_rng_state`/`put_rng_state`.
  `R_sample_kind` reports the method `RNGkind(sample.kind =)` chose. `R_binom_kind` still
  hard-codes the pre-fix BTPE, which is the only `rbinom` stream ry implements.
  `RNGkind("user-supplied")` finds `user_unif_rand` and its seed routines through the
  `find_native_symbol` interpreter hook, which `libr` fills with `dynload::find_symbol`
  (randtoolbox's WELL tests need it).
- **Impact.** One generator means one state and one stream, so a C draw and an R draw interleave
  exactly as under GNU R, and every kind the interpreter supports works from C at no extra cost.

## Native code reads an environment as a cell

**Status:** built.

- **Before.** `CAR`, `CDR` and `TAG` refused an environment ("bad CAR: a 'environment' has no
  cells"), and `R_NewEnv` refused a `NULL` enclosure.
- **Now.** GNU R lays an environment out as a cell (frame in `CAR`, enclosure in `CDR`, hash table
  in `TAG`), and nanonext looks bindings up by walking `CAR(env)` because `findVarInFrame` is not
  API. `CAR(env)` returns the same read-only frame snapshot as `FRAME`, `CDR(env)` the enclosure
  and `TAG(env)` `NULL`, as `HASHTAB` does. `R_NewEnv(R_NilValue, ...)` gets the empty
  environment as its enclosure, because GNU R stores the enclosure unchecked.
- **Impact.** mirai, and purrr's `in_parallel` on top of it, run. A snapshot costs one cell per
  binding on each `CAR(env)`, which is fine for nanonext's small aio environments.

## The strict load check overlaps the runtime start

**Status:** built, except the whole-root walk, which "The strict load check analyses only the package" supersedes.

- Before, `ry load` on a strict package ran the whole-project strict check, test files included, and only then started the runtime, so a cold typed load cost the check plus the load (4.8 s against 2.4 s untyped on finbench).
- On ry's runtime the check now runs on a thread (`run_load` in `crates/ry/src/main.rs`) while the runtime starts. The loader receives a verdict callback (`ry_loader_set_verdict`) and calls it before any statement that is not effect-free, before compiled code and `.onLoad` (`.ry_await_verdict()` in `runtime.R`), after the load returns, and before an S4 statement held back for a later lookup (`do_run_deferred`) that is not effect-free; a refusal exits with the verdict's status. GNU R still checks before it starts, because its loader cannot call back into ry.
- An eager item is effect-free (`EagerItem.effect_free`, `load_index.rs`) when it binds a function literal, or when `item_effect_free` (`top_level.rs`) holds: a strict constant, or a definition whose declaring calls' arguments have no effect. Strict mode's `Definition` alone is not enough, because it leaves a declaring call's arguments to R, so `setRefClass(..., methods = list(m = { cat(1); function() 1 }))` passes strict mode yet prints. Loading an imported package (NAMESPACE imports, `library()` with effect-free arguments) is allowed before the verdict; this package's own code with an effect never is.
- `.ry_await_verdict()` sits before `sysdata.rda` and `data/*.R`, because a data file is ordinary code.
- A constant may call a user `@pure` function before the verdict. That purity is trusted, not checked, which is the documented contract of `@pure`.
- The check computes diagnostics only for the `R/` files (`analyze_project`'s requested paths): a name that only a test file defines is unbound in the loaded package anyway, and the tests' diagnostics were never reported. It walks the whole root so it can share the check record ("The strict load check shares the check record").
- `package_verdict` is `extern "C"`, so a panic in the check exits 101 instead of unwinding into libR, which would abort.
- The check thread lowers its own priority (`lower_priority`), and the analysis workers inherit it, so on a loaded machine the runtime start keeps the cores it needs; the check has the whole load to finish.

## Every load of a strict package is checked

**Status:** built, except the full analysis on every reload, which "A strict reload re-checks through a persistent salsa database" supersedes.

- Before, only `ry load`'s first load ran the strict check; `load_all()`, a console reload and the load a console makes at start ran a package strict mode would refuse.
- The load index records whether strict mode covers any file (`PackageIndex::strict`, the same test as `prepare_purity`), and every plan carries it as `Plan::strict`, so a host decides without reading ry's configuration twice.
- A host checks a plan that changes something (`!is_noop()`): GNU R's `.Call` routine and `ry load --emit-plan` through `load::strict_value`, ry's runtime through a host callback (`loader_backend::set_check` in `libr`, registered by every command that runs the runtime via `repl::set_package_check_on_ry_runtime`). The loader's crate cannot depend on the CLI's analysis, which is why it is a callback. A refusal turns the plan into an error before R sees it, so nothing runs and the old bindings stay.
- Whether a package is strict is decided per package root on both runtimes, from that package's own `ry.toml` (`load::strict_config`): GNU R's routine reads it directly, and ry's runtime asks a second host callback, the strict query, before each plan. Taking it from the command's configuration instead would let a lax script `load_all()` a strict package unchecked.
- The first plan of a session that has a verdict (`ry load` on ry's runtime) skips the callback: the concurrent check already covers that load, and checking again would serialize what the verdict overlaps.
- The check is the whole-package `preflight::check_package` with its fingerprint-cached clean verdict, so a reload of a large strict package pays a full analysis; checking only the changed files is in `backlog.md`.

## Lazy definitions keep source references

**Status:** built.

- Before, a lazy definition on ry's runtime lowered its node without a srcfile, so `print(f)` deparsed and tracebacks named no file, except under the debugger, which built a whole-file srcfile per force.
- A plan's files become per-file handles (`FileSource`) holding the source and the `keep.source` option read when the handle is made. The first force from a file that keeps source builds its `srcfile` (`srcfilecopy` semantics) and stores it in the handle, so a file costs nothing unless one of its definitions is used, and the srcfile is shared by all of them.
- The definition is reparsed alone after as many spaces as its line has bytes before it, and its srcrefs are computed against the line's real text (bytes and character columns both match GNU R's parser), lowered with a line offset (`interp::lower::lower_node_at_line`), which gives exact line and column srcrefs without parsing or lowering the whole file. The tree in the handle is used directly when no srcref is wanted.
- The first force of a lazy binding builds the file's `srcfile` and reparses only the definition, padded to its line and column, so the source reference carries the position the definition has in the file.

## GNU R's public C API is a checked list, not a measured subset

**Status:** built.

- **Previous shape.** `test_headers.rs` checked that what ry's headers declare is exported, so a
  public name missing from both passed. Diffing GNU R 4.5's `R.dll` export table found such names
  (`R_findVar`, `R_findVarInFrame`, the `R_GetVarLoc*` family, `Rf_formatReal` and its siblings,
  `R_AllocStringBuffer`, `utf8clen`, `Rf_mbrtowc`, `R_MissingArgError`).
- **Target shape.** `crates/libr/tests/headers/r_api_names.txt` lists every name GNU R's public
  headers declare *and* its `R.dll` exports (declared-only names vary by platform and are often
  macros), plus an internal section of non-header names packages link. The test fails when ry's
  headers miss one or `libR` does not export it. Regenerate against a new GNU R release with
  `RY_GNU_R_INCLUDE`, `RY_GNU_R_EXPORTS` and `RY_UPDATE_API_NAMES=1` (`development/testing.md`).
  GNU R's front-end headers (`ga.h`, `graphapp.h`, `iconv.h`, `libintl.h`) are excluded: packages
  get gettext's header from Rtools, and the rest is RGui.
- `R_varloc_t` is a struct holding one pointer, as in GNU R, so `R_findVarLoc` returns the binding
  cell by value through the same ABI.
- `VECTOR_PTR` is declared without `NORET`: GNU R declares it as an error stub, ry returns the data
  pointer, and a `noreturn` declaration would let a compiler drop the code after the call.
- `NORET` is `[[noreturn]]` only for C23. C++ gets `__attribute__((noreturn))`, because Rcpp writes
  `inline void NORET stop(...)`, where clang rejects the standard attribute; `Rf_error` and the
  other no-return entry points carry `[[noreturn]]` directly in C++. A package's own `NORET` wins.
- **Impact.** A header name GNU R exports can no longer go missing silently on any platform. The
  265 `R.dll` exports ry still lacks are GNU R internals and RGui entry points that no package in
  the measured closure uses (`tools/research/data/symbols.csv`).

- A libr test package is compiled twice, against ry's headers and against GNU R's, so its C must be valid for both. GNU R 4.6 does not declare `ATTRIB`, `DATAPTR`, `FRAME`, `fdhess` or the contents of `R_ext/PrtUtil.h`, and it hides `ALTINTEGER_SUM`, `LOGICAL_IS_SORTED` and `rexitc`.
- Test C that needs an undeclared entry declares it itself with a parenthesized name, such as `SEXP (ATTRIB)(SEXP x);`, which is valid whether or not the header defines a macro of that name. Test C that needs a hidden entry reaches it through a weak reference or through an exported path. ry's libR keeps exporting the hidden entries, because installed packages built against older R versions import them.
- gcc 14 turns an implicit function declaration into an error where gcc 13 warns. Compile with `-Werror=implicit-function-declaration -Werror=int-conversion -Werror=incompatible-pointer-types` on an older compiler to get the same verdict.

## Windows `R CMD INSTALL`: the home's `Makeconf` decides the platform

**Status:** built, except the front ends as plain copies of `ry-rcmd.exe`, which "A release is `ry` alone, with the runtime embedded and the R front end folded in" supersedes.

- **Shape.** `rcmd::Platform::of_home` reads `SHLIB_EXT` (and `DYLIB_EXT`) from the R home's
  `etc/Makeconf`, so the home stays the single source of truth for how its packages build, and a
  Linux-hosted test can drive the Windows conventions with a Windows `Makeconf`. From it follow
  the shared-object suffix, `libs/x64/`, `R_ARCH`, and build-file precedence (`.ucrt`, then
  `.win`, then the portable `Makevars`; `configure` and `Makefile` have no Unix fallback, as in GNU
  R). `makeconf(root, toolchain, platform)` writes the Rtools ABI for Windows: `-shared
  -static-libgcc`, no PIC flags, forward-slash paths, `-L"$(R_HOME)/bin/x64" -lR`.
- **The SHLIB is `<package><ext>`**, the name unchanged, as GNU R links it; data.table's Makevars
  renames `data.table.dll` to `data_table.dll` itself. Every shared object `src/` holds is
  installed, so a renamed one is found.
- **Windows home layout.** `R.dll` and its import library `libR.dll.a` in `bin/x64/`; `bin/R.exe`,
  `bin/Rscript.exe` and `bin/x64/{R,Rscript,Rcmd}.exe` are copies of `ry-rcmd.exe`, which picks the
  front end from its file stem and the home from its location (Windows runs no `#!` scripts).
  Directories are symlinked where Windows allows it and copied otherwise; Wine reports a
  successful symlink without creating it, so the link is checked for existence.
- `ry-rcmd` uses `crates/ry/build.rs` as its build script, so both executables get the 64 MiB
  stack and the UTF-8 manifest from one place; the manifest is `include_str!`-ed so the script works
  from either package.
- **Verification limit.** Running MSYS2's `gcc`, `make` or `sh` under Wine is not permitted in the
  agent environment, so the six packages were installed cross-hosted (`r-runtime.md` section 11)
  and only loaded under Wine. The release smoke test is the first native Rtools run.

## The Windows session's native encoding is UTF-8

**Status:** built.

- `init_locale` sets every category from the environment and then, on Windows, swaps a non-UTF-8
  codeset for UTF-8 (`use_utf8_codeset` in `crates/interp/src/system.rs`), as GNU R from 4.2 does
  under its manifest. The manifest alone does not suffice where it is ignored (Wine).
- Locale names report the UTF-8 codeset as `.utf8`, GNU R's spelling, including in the composite
  `LC_ALL` string; Wine's CRT spells it `.65001`. An explicit `Sys.setlocale` to another code page
  is honored.
- `.Internal(Sys.which)` exists because GNU R's Windows base code calls it (Unix base R uses
  `command -v`); it searches `PATH` with `PATHEXT` and returns forward-slash paths.

## A load holds back S4 definitions and caches its index across processes

**Status:** built.

- **Deferred S4** (`loader::deferred_s4`, `.ry_force_s4` in `runtime.R`). Previously every top-level
  `setClass`, `setGeneric`, `setMethod` and `setValidity` ran eagerly through the `methods` package,
  about 300 ms on the 326,000-line bench package. On ry's runtime a package's S4 definitions are now
  held back as one unit: each name they bind becomes a placeholder promise, and the interpreter's
  class-cache miss and `R_getGeneric` hooks (`interp::methods`) force them. A force during the load
  runs only the definitions before the current statement, so order is GNU R's. The unit is
  all-or-nothing because S4 registration is global state (class tables, generic method tables) that
  no per-name promise could reproduce; the rules that keep it eager (a method on a generic the package
  does not define, a reference class, a definition reading a name bound later, a generator name that
  is also written elsewhere) are listed in `reference/package-loading.md`, "S4 definitions", and
  pinned in `crates/loader/tests/reload/deferred_s4.test`. GNU R's backend never defers, since it has
  no hook to force on; `options(ry.load.lazy_s4 = FALSE)` turns it off.
- **Held-back methods for other packages' generics** (`DeferredS4::foreign_methods`). Previously
  any `setMethod` on a generic the package did not define kept the whole unit eager, and nearly
  every S4 package defines a `show` method, so on the synthetic finance code base no S4 was held
  back. Such a method now qualifies when its signature names one of the package's own classes,
  which means it can only be selected for objects of that class. Each kind of generic needs its
  own door, because forcing on every dispatch of `initialize` or `length` would fire during the
  load itself (`.ry_foreign_generic_kind`, `.ry_defer_s4`):
  - A generic function (`show`, `initialize`, any `standardGeneric`): the placeholders are per
    dispatch label, one promise per method in the generic's `.MTable`/`.AllMTable`, labelled as
    `methods` labels them (`.ry_s4_label`: `matchSignature` padded with `ANY` to `.SigLength`).
    A dispatch for another class never reads them, so it never forces; `getMethod`,
    `existsMethod` and a dispatch for a held-back class read the label and force. A signature
    longer than `.SigLength` keeps the unit eager. Another package's `setMethod()` on the same generic
    reads every label (`methods:::.resetInheritedMethods`), so it forces the unit; a force during
    this load from a method after the cursor reads the label with `get0`. Forcing restores each table's previous value
    before the definitions run (`.ry_restore_s4_labels`).
  - A primitive (`length`, `[`): primitives dispatch from C without `standardGeneric`, so
    `s4_dispatch` forces only when a non-promise S4 argument's class is one of the unit's
    classes (`interp::methods::Lookup::Primitive`). A held-back method can apply to nothing
    else, so this is exact.
  - A function that is not yet generic from another package (`as.list`, `toString`; one of the package's own
    functions keeps the unit eager, since a placeholder would replace it): `setMethod` would make it generic
    in the package namespace, so that name and `.__T__name:pkg` become placeholders and the name
    joins the forcing generics.
  - Group generics keep the unit eager: a group member dispatches through the group's table,
    which no per-name placeholder covers.
- **Class unions** (`S4Definition::Union`). A union of only the package's held-back classes is
  held back with them. A union that names other packages' classes edits the global class-cache
  entry of each such member (GNU R changes `.getClassesFromCache("numeric")@contains`, not
  `methods::.__C__numeric`), which no placeholder can stand in for, so a mixed union
  (`c("Cls", "NULL")`) used to keep the unit eager and a union of only other packages' classes
  (`c("numeric", "NULL")`) ran in place at about 10 ms each, which on a real S4 package meant no
  deferral at all or 150 ms of load for 15 such unions. Both are held back, because every
  membership question names the union (`is(NULL, "U")`, a method for `U`) and so forces the unit,
  and `possibleExtends` falls back on the union definition's `@subclasses`. What stays wrong until
  the force is the reverse listing (`extends("NULL")`, `is(NULL)`), documented under Limits.
  Forcing on any lookup of a member class (a hook in `R_getClassFromCache`) would make the
  listing exact but was tried and rejected: the methods package itself calls `is(NULL, ...)`
  constantly (`setOldClass()` does at load), so the unit ran at once and nothing stayed deferred.
  A method on another package's generic
  for a mixed union keeps the unit eager, because dispatch on `NULL` can hit the generic's table
  without any class lookup. A restore must add the unions to those members in definition order,
  because that is the order `extends("NULL")` lists them in after an eager load
  (`cacheMetaData()` only replaces the entries in place), not `getClasses()` order.
- **Coercions** (`S4Definition::Coerce`). `setAs(from, to, def)` is a method on `coerce` (and on
  `coerce<-` with `replace`), so it is held back as a foreign method with signature `(from, to)`
  when either side is a held-back class; `as()` reaches it through `.quickCoerceSelect`, which
  reads the label placeholder. A coercion between other packages' classes runs in place. Before,
  any `setAs` made the unit eager because `setAs` was not in `PURE_S4_READS`.
- **S4 calls are matched as R matches arguments** (`match_arguments` and `s4_formals` in
  `crates/semantics/src/load_index.rs`): exact names, then unique partial names, then positions,
  against each definition function's formals. Reading arguments by name alone missed
  `setAs(from =, to =, function(from) ...)` and `setClassUnion("U", mem = ...)`, which kept the
  whole package eager. A load that keeps S4 in place reports the first definition that forces it
  (`deferred_s4` returns `Err(reason)`, printed by `runtime.R` under `ry.load.verbose`).
- **No `cacheMetaData` while held back** (`.ry_apply`). The sync after a load called
  `cacheMetaData`, which reads every `.__C__`/`.__T__` binding and so forced the unit. Each
  definition already caches what it defines as it runs, and `.ry_force_s4` ends with the same
  call, so the sync skips it while a unit is held back.
- **Cross-check against eager.** Laziness changes when definitions run, not what they compute,
  so the check is a whole-process comparison rather than an in-process invariant:
  `crates/ry/tests/test_load.rs`
  (`a_held_back_method_for_another_packages_generic_is_there_when_that_generic_needs_it`) runs
  seven first-use probes (`show`, primitives, `as.list`, `print`, union membership,
  `initialize` with `callNextMethod`, `readRDS` objects) lazily and eagerly and asserts identical
  output, and `tools/loader-bench/s4_check.R` compares lazy, eager, cache-hit and GNU R results on
  the generated package. The deferred-S4 rules are pinned in
  `crates/loader/tests/reload/deferred_s4.test`.
- **Measured** on a seeded S4-heavy package (`just bench-s4-load`, `tools/loader-bench/s4_package.py`:
  2,863 S4 definitions). Eager, the first load took 5.7 to 5.9 s under `ry run`, 92% of it S4
  (setMethod 2.8 s, setClass 2.3 s, setClassUnion 0.4 s, setGeneric 0.2 s), at about 1.3x GNU R per
  call (`pkgload::load_all` 6.3 to 7.1 s, `library()` of an installed copy 0.6 s). Held back, the
  load takes about 0.27 s; first S4 use costs the full eager run on a miss (see the metadata cache
  below for a hit).
- **Each package generic is its own unit; there is no S4 metadata cache** (`DeferredS4` in
  `loader.rs`; `.ry_force_s4`, `.ry_s4_placeholder` in `runtime.R`; `force_deferred_unit`,
  `run_unit` in `interp/src/methods.rs`). Previously a first S4 use ran or restored every held-back
  definition at once, and a lazy-load database of the resulting bindings (keyed on the
  definitions' text, the build id and the imports' versions) made the restore fast. That cache was
  derived state persisted across processes, it needed every definition to run together, and a miss
  still paid the whole run. Now unit 0 holds the classes, unions, coercions and methods on other
  packages' generics, and each package generic whose methods name only classes defined earlier
  becomes its own unit, run on the first lookup of that generic (its name or its `.__T__` table) or
  on dispatch to it. A generic that a class definition reads, or whose methods name a later class,
  stays in unit 0. On `target/s4heavy`, unit 0 is about 416M instructions, and the lazy generics cost
  nothing until called. Forcing a lazy generic runs unit 0 first when it has not
  run, and it drops that generic's placeholders before unit 0's `cacheMetaData` visits the
  namespace's methods tables, since the generic's own placeholder may be the promise whose
  evaluation started the force. The native `cacheMetaData` skips generics still held back, and a
  lazy unit ends with `ryS4CacheGeneric` (or `.ry_cache_generic` in R) for its one generic.
- **The load-index cache** (`index_cache`, `read_index_cache`, `write_index_cache` in `loader.rs`).
  Previously every cold start parsed all files and derived their facts (about 280 ms on 4 cores).
  The whole `PackageIndex` is now cached per package root, keyed by an xxh3-128 over the build id
  and every input (strictness, DESCRIPTION, NAMESPACE, each path and text). The cache is package-level,
  not per file, because the per-file salsa queries all chain off the parse and cannot be fed a stored
  result without re-parsing; a hit therefore skips salsa analysis entirely and the runtime parses
  each item's slice of text when it is first forced (`Source::Text`). The build id is a hash of the
  analysis crates' sources from `crates/loader/build.rs`, so any change to the facts' derivation
  invalidates old entries without a hand-maintained version number. A hit warms the salsa parse on a
  detached, cancellable db clone so the first reload does not pay for it.

## The strict load check shares the check record

**Status:** built, except the whole-root walk, which "The strict load check analyses only the package" supersedes.

- **Previous shape.** `preflight::check_package` stored a clean verdict in `<cache dir>/ry/strict/<hash of the root>` under one fingerprint of every input, beside `ry check`'s own record in `.ry_cache/check.json`. Two mechanisms recorded the same fact, any edit invalidated the whole verdict, and a package that `ry check` or the editor had just checked still paid a full check on load.
- **Target shape.** The load check is `ry check`'s analysis (`cli::analyze_project`, walking the whole root) asked about `R/` alone, reading and writing the same record. A recorded file's `diagnostics` is `None` when the run that wrote it did not check that file, and `check_cache::plan` treats such a file as stale, so a later `ry check` still checks the tests a load skipped. Kind isolation makes the sharing sound: a name only a test file defines stays unresolved in `R/`, and a test's `library()` does not attach in `R/`, so the `R/` verdict is the same as from an `R/`-only walk.
- **A verdict-only pass was measured and rejected.** A lint may be configured as an error, and `is_strict_error` counts it, so lints cannot be skipped. What a verdict could skip (rendering, annotation agreement) is about 0.3 of 5.3 CPU-s on typed finbench.
- **Impact.** On typed finbench (4 cores, release) a load after `ry check` takes 0.55 to 0.64 s, the same as an untyped load, and an edit inside one body rechecks one file. A verdict-cold load costs 2.5 to 2.7 s, about 0.15 s more than the old `R/`-only walk because the record needs the whole root walked; the rest is analysis CPU (parse about 1 s, item HIR 0.9 s, folds 0.5 s, type check 2.2 s), which belongs to the type checker's own speed.

## `ry test` is `ry load` plus testthat, with affected tests first

**Status:** built.

- **Previous shape.** `load_all()` loaded a package, but running its tests meant `devtools::test()` under GNU R, which reloads the whole package (about a minute on finbench) before every run.
- **Shape.** `ry test` is `ry load` running an R driver (`crates/ry/src/test.R`) that calls `testthat::test_dir(load_package = "none", package =)` against the namespace `load_all()` built, so testthat's own reporters print the output and helpers, setup files and the package environment behave as under `devtools::test()`. Nothing in the reporter path is reimplemented, which keeps the output identical for CI.
- **Watch.** `--watch` waits on operating-system notifications for `R/`, `data/`, `src/`, `inst/`, `tests/`, `DESCRIPTION` and `NAMESPACE` (`loader::watch`, `Loader::wait_edits`; edits under `_snaps/` are dropped because testthat writes there during a run), reloads with `load_all()`, and reads the reload's touched names from `.ry_packages[[root]]$changed` (`NULL` after a full load or one that ran code). `Loader::callers` closes those names over the index's reverse mentions and reads, and a test file is affected when it changed or its `all.names()` meet that closure. Affected files run first, then the rest; a changed helper or setup file, or a `NULL` change set, reruns everything.
- **Why not a test index in the loader.** Test files are parsed with `all.names()` and cached by stamp; that is a few milliseconds per file and keeps the loader ignorant of testthat.

## `ry test --jobs` runs test files in testthat's workers, loaded by ry's loader

**Status:** built.

- **Previous shape.** `ry test` ran every file in one process unless the package set `Config/testthat/parallel: true`, and then testthat's worker hook called `pkgload::load_all()`. That hook loads the whole package eagerly in every worker, needs pkgload and every import installed, and ignores `ry.toml`, so on finbench the workers failed at startup and a declared-parallel package ran under different checks than the main process.
- **Weak point.** Two loaders for one run (ry's in the main process, pkgload in the workers), and no way to ask for workers from the command line.
- **Shape.** testthat's parallel runner stays the scheduler, so reporters, snapshots and the file order of the output are testthat's own. `--jobs N` sets `TESTTHAT_PARALLEL` and `TESTTHAT_CPUS`/`Ncpus` (`N > 1` forces workers, `1` forces one process); without the flag testthat's own switches decide, and the worker count defaults to every core (`thread::available_parallelism`) unless `Ncpus` or `TESTTHAT_CPUS` is set. The count is set only for a run that starts workers, so tests in one process see the settings they see under devtools. On ry's runtime the driver (`crates/ry/src/test.R`, `load_workers_with_ry`) rewrites the body of `testthat:::queue_setup` so the worker hook runs `.Internal(ryLoaderAttach()); load_all(dir, quiet = TRUE, helpers = FALSE)`; testthat sources the helpers in each worker itself, which is why `load_all()` has pkgload's `helpers` argument. The main process passes its runtime switches in `RY_RUN_FLAGS` (`rcmd::front_end::PROJECT_FLAGS_VARIABLE`), which ry's `bin/R` reads and the hook unsets, so tests never see it and grandchild processes run with R's defaults.
- **Why workers stay opt-in for an undeclared package.** Test files of a package that never ran in parallel may share state (a temp file name, a port, an option set in one file and read in the next). `--jobs N` is the explicit request; the DESCRIPTION field is the package's own promise.
- **Why rewrite `queue_setup`.** testthat offers no hook for the worker's load call. The rewrite replaces exactly the `pkgload::load_all` call and, when a future testthat has none, says so once and leaves pkgload in place. Under `--runtime gnu-r` workers are GNU R processes without ry's loader, so they keep pkgload.
- **`--profile-summary` forces one process**, because the summary measures the process that writes it; with `--jobs N > 1` it is an error.
- **Impact.** finbench (501 test files, 43,011 expectations, 4 vCPUs): 251.5 s in one process, 122.5 s with 2 workers, 84.3 s with 4 (2.98x). CPU time rises from 249 s to 300 s, which is each worker binding its own lazy definitions.

## A C API argument that may be a C `NULL` is typed `Option<Sexp>`

**Status:** built.

- **Previous shape.** Every `SEXP` parameter of a `libr` entry point was a `Sexp`, which wraps `NonNull`. `R_getVarEx`'s default was wrapped in `Some(...)` to mean "has a default", and since `Some(NULL)` and `None` share one bit pattern, a C `NULL` default silently became "no default" and the lookup raised "object not found".
- **Why it matters.** Rcpp's `Environment::exists()` and `find()` call `R_getVarEx(sym, env, inherits, NULL)` on R 4.5 and later, so every Rcpp assignment into an environment failed for a name not yet bound. httpuv builds its request environment that way, which broke every HTTP request and with it plumber.
- **Shape.** A parameter or result that C code may legitimately pass or receive as `NULL` is declared `Option<Sexp>` in the `extern "C"` signature (same ABI), and `Handout for Option<Sexp>` (`boundary.rs`) hands out the object when there is one. `get_var` returns `Option<Sexp>` for "unbound", `R_getVar` turns that into the error and `R_getVarEx` into the caller's default. `rytest_get_var` pins it (`crates/libr/tests/test_native.rs`).

## Source edits arrive by OS notification, ordered by a barrier file

**Status:** built.

- **Previous shape.** The console compared a fingerprint of names, sizes and mtimes of `R/`, `data/` and the metadata before every command, and `ry test --watch` polled stamps every 0.2 s. Neither saw `src/` or `inst/`.
- **Shape.** One `notify` watcher per loaded package (`crates/loader/src/watch.rs`) files relevant paths into an inbox. `SourceWatcher::take` writes a barrier file into a private watched directory and waits for its event; since one watcher delivers events in order, every edit made before the call is then in the inbox, so a reload depends on the order of writes and calls, not on thread timing, and the reload fuzz test stays deterministic. The fingerprint stays as the fallback when notifications are unavailable or lost.
- **Compiled code.** A `src/` edit makes the console and `--watch` reload, and `load_all()` rebuilds. On ry's runtime `dyn.unload` retires the library record into `unloaded_dlls` (kept mapped, so a stale pointer cannot crash), and the next `dyn.load` of the same path maps a fresh copy from a temp directory, because `dlopen` returns the already open handle for a path it has seen.

## `ry document` is roxygen2 with block and topic caches and linear topic ordering

**Status:** built.

- **Previous shape.** Documentation meant `devtools::document()`, which reloads the package and reparses every block; on a 22,000-topic package roxygen2's `RoxyTopics$topo_order` (run 15 times by `@inherit` processing) and `RoxyTopics$add` are quadratic and dominate.
- **Shape.** `crates/ry/src/document.R` calls `roxygen2::roxygenise(load_code = )` with ry's `load_all()`, so output is roxygen2's own, byte for byte. For the run it swaps `roxygen2:::parse_package` for a version that caches each file's tokenized blocks by md5 (keyed by roxygen2 version, R version and roxygen options; `package_cache_path(root, "roxygen")`), and replaces roxygen2's quadratic steps with linear equivalents (`roclet_process.roclet_rd`, whose topic list grows by copying, and `RoxyTopics$topo_apply`, `topo_order` and `drop_invalid`, which find topics by scanning names), each only when the installed function's deparsed body equals the embedded original, so another roxygen2 version runs unpatched. Everything is restored on exit. The session runs with `keep.source = TRUE` because roxygen2 reads R6 methods' srcrefs.
- **Topic cache.** A one-file edit on finbench (22,000 topics) still took about 2 minutes in roxygen2's per-topic work, so `<cache>-topics` keeps each topic's finished Rd text, aliases, family and `@inherit*` sources, plus each block's namespace directives. A block is *clean* when its file's md5 is unchanged and it is trackable: a `name <- function` block whose loaded object's srcref names the same file and line (`defined_here`), with no dot in the name (an S3 method's dispatch is not tracked) and none of the `uncacheable_tags` (`@eval`, `@template`, `@example`, `@describeIn`, ...). A clean block skips `block_find_object`. The rebuilt set is every topic touching a dirty or untrackable block, plus the package topic, closed under reverse `@inherit*` edges and family co-membership; the blocks needed to rebuild it add the forward closure. Every other topic is emitted from the cache through a stub that `roclet_output`'s `write_if_different` skips when the file's size and mtime match the record. `namespace_imports` is cached per file md5 (`<cache>-imports`). On finbench: 385 s cold, about 11 s for a no-op rerun, 20 s for a one-file edit (50 topics rebuilt), byte-identical to `devtools::document()`. Most of the warm time is `readRDS` of the 29 MB compressed block cache (about 3 s); uncompressed it is 214 MB, so it stays compressed.
- **Why the dependency tracking is in R and conservative.** roxygen2's `@inherit*`, `@family`, `@describeIn` and `@rdname` merging make one block's Rd depend on others. Rather than reimplement that, the cache only decides *which* topics roxygen2 rebuilds, over-approximating with closures; anything it cannot attribute to a file (generated blocks, S4, R6, data) is always rebuilt.
- **Known staleness.** A cached block keeps markdown links resolved against installed packages at parse time, and a cached topic keeps documentation inherited from another package.
- On finbench generated with `generate.py --roxygen` (22,000 Rd files), `ry document` takes 385 s cold, about 11 s for a rerun that changes nothing and 20 s after a one-file edit, against 1,447 s and 1,280 s for `devtools::document()`. The output is byte-identical to `devtools::document()` on both runtimes, R6 classes included, because the load runs with `keep.source = TRUE` as pkgload's does.

## A strict reload re-checks through a persistent salsa database

**Status:** built.

- **Previous shape.** Every strict check of a changed package ran `analyze_project` from scratch over all of `R/`, about 2 s on the 326,000-line bench package, and `package_index` re-joined every file's names on each reload (about 28 ms) before a 10 ms diff.
- **Shape.** `preflight::PACKAGE_CHECKS` keeps one `cli::PackageCheck` (a salsa database plus its file inputs) per canonical package root for the life of the process, and `check_package_code` updates only the inputs whose text changed, so salsa re-derives what the edit affects. The database lives in the ry binary, not the loader, because the loader runs inside libR on ry's runtime and cannot link the analysis.
- **Loader side.** `namespace_spec` is a tracked query in `load_index.rs`, the spec is shared as `Arc<NamespaceSpec>`, and the reload fingerprint is taken from the read stamps the plan already holds, so a one-file reload plans in about 3 ms and prepares in 4 to 5 ms.
- **With the shared check record.** The first check of a package in a process still goes through `analyze_project` with `CacheUse::ReadWrite` ("The strict load check shares the check record"), so a cold load reuses `ry check`'s findings. That database replays findings it never derived, so it is not kept (`PACKAGE_CHECKS` holds `None`). The first reload builds the kept database, and every reload after it is incremental.
- **Impact.** Measured on the typed finance bench package in a debug build, a one-file reload with only the record takes about 22 s. With the kept database, the first reload takes about 20 s and later ones under 1 s, load included. The remaining costs are listed in `backlog.md`, "Open: the package loader".

## Compiled code under `ry load` builds with ry's own R home

**Status:** built, except that `ry-rcmd` sits beside libR, which "A release is `ry` alone, with the runtime embedded and the R front end folded in" supersedes.

- **Previous shape.** `src/` was compiled by pkgbuild through `callr`, so `R CMD SHLIB` ran GNU R's `bin/R` and linked GNU R's `libR`; the result loaded only because ry's `libR.so` shares the soname, and a package without pkgbuild installed could not load at all.
- **Shape.** On ry's runtime, `.ry_compile` calls `.Internal(ryLoaderCompile(root, .libPaths()))`, which runs `rcmd::compile` in process against an R home from `rcmd::cached_home`. That home lives under the user cache (`ry/home/<hash>`), keyed by ry's version, libR's and `ry-rcmd`'s stamps and GNU R's home, and is built from headers embedded in `rcmd` at build time (`crates/ry/build.rs`, `embed_headers`), so an installed ry needs no source tree. libR is hard-linked into the home (copied only across file systems), so a home costs about 300 KB beyond the libR inode it shares. Every process that uses a home holds a shared `flock` on its `.lock` file. Building a new home prunes every home whose recorded inputs (`.libr`: libR, `ry-rcmd`, base) no longer hash to its name, unless some process holds it; removal renames the home to `.removing-*` first so a half-deleted home is never found by name. Only stale homes are pruned, not every unheld one, because two builds sharing a cache would otherwise delete each other's homes on alternate runs. Stale homes are what grew the cache (each pins an old libR inode, about 190 MB). `ry-rcmd` must sit beside libR, which is why the release archives and the Nix package ship it there.
- **Unload.** `dyn.unload` is real on ry's runtime (`libr::dynload::unload_library`): it calls `R_unload_<pkg>` and moves the library to `unloaded_dlls`, which keeps it mapped, because R objects may still hold pointers into it. Since `dlopen` returns the existing mapping for a path already open, a reload copies the rebuilt library into a fresh temp directory under its own file name (which names `R_init_<pkg>`) before loading it.
- **Symbols.** A registered routine whose R name is already bound in the namespace is skipped with GNU R's `loadNamespace` warning, and on reload only the symbols the previous load bound (and that are still `identical`) are removed, so a user function with a routine's name survives.
- **GNU R runtime** keeps pkgbuild, matching `load_all()`.

## The strict load check analyses only the package

**Status:** built.

- **Previous shape.** `ry load`'s strict check asked about `R/` but built the whole project: every test and script file was parsed and folded, about a quarter of the derive work on the benches, and the verdict thread then freed the analysis (85 ms on typed finbench at 60K lines) before the loader could proceed. The check record's key hashed the ry executable on the same thread before any analysis started.
- **Shape.** A run whose requested files are all package files (`ry check R`, the load's check) analyses only package files, because tests and scripts read the namespace but add nothing to it. The record keeps the unanalysed files: an unchanged one keeps its recorded content, surface, findings and imports used, and when one changed, the record stores no surface for it (`RecordedFile.surface` is an `Option`) and marks every unanalysed file unchecked, so the next full run checks them. `check_cache::plan` compares surfaces only for analysed files. The `NAMESPACE` findings are recomputed on every run that builds an analysis (a few milliseconds) from the analysed files' imports used, and a replay recomputes them when the package files' imports differ from all files' imports, since the record does not say which kind of run wrote it. `preflight::check_package` frees the analysis on its own thread, and the build identity is hashed on a thread that overlaps the project walk.
- **Impact.** The verdict-cold typed load on finbench at 60K lines fell from 0.52-0.55 s to 0.43-0.45 s; the rest of the gap is checker CPU. Package findings are unchanged by construction, and the seeded simulation in `cli.rs` now mixes package-only runs into its edits, comparing each with a fresh run (`tests/scenarios/check-cache-package-only.scenario` pins the two traps it found: a test helper edited between a load and a full check, and the `NAMESPACE` verdict across the two kinds of run).

## A reload diffs S3 registrations per table entry

**Status:** built.

- **Previous shape.** The reload diff compared `S3method` directives as a set, and registered a directive whenever its function was rebound.
- **Why that was wrong.** R registers directives in order into one entry per generic and class, so a later directive for the same pair wins. Rebinding the function of a losing directive re-registered it and made it win, and dropping one of two directives for a pair unregistered an entry that still exists (found by `test_reload_fuzz`, pinned in `crates/loader/tests/reload/edits.test`).
- **Shape.** `loader::diff` maps each pair to its winning directive and registers or unregisters only winners. The same diff's `keeps_names` claim (no binding appears or disappears) now also requires every name a rerun statement may write to be bound already, and a statement whose writes are unknown rules the claim out.

## `R CMD` make variables expand as GNU make expands them

**Status:** built.

- **Shape.** `make_assignments` and `expand_make` (`crates/rcmd/src/makeconf.rs`) follow GNU make 4.3: recursive (`=`) values expand at use, simple (`:=`) values expand at assignment, `?=` assigns once, `+=` keeps the flavor of the original and adds no space to an empty value, a backslash-newline run collapses to one space, `$$` is a literal dollar, a lone trailing `$` stays, and a variable that references itself expands to nothing at the inner reference instead of recursing (GNU make stops there with an error). There is no depth bound, so a non-cyclic doubling chain expands exponentially, exactly as it does under GNU make. `R_HOME` is ry's home whatever the makefile assigns, because R passes it on make's command line.
- **Why.** A package's `Makevars` is written against GNU make, and any difference in expansion changes compiler flags silently. `make_expansion_agrees_with_gnu_make` checks generated makefiles against the real `make`, so the rule is GNU make's behavior, not a reading of its manual.
- **Includes.** An `include`, `-include` or `sinclude` line expands its file list and reads each file through a caller-supplied `read`, applying its assignments at that point, as make does; a file `read` cannot supply is skipped, and nesting stops at 32 levels so a self-include ends. GNU R's `Makeconf` includes `vars.mk`, and the differential generates includes too, so the rule is checked against make.

## BLAS and LAPACK come from the system, then from GNU R's Makeconf

**Status:** built.

- **Previous shape.** The interpreter opened BLAS and LAPACK by bare soname (`libblas.so.3`, `liblapack.so.3`, then Accelerate on macOS), and `rcmd` linked Linux packages with a fixed `-lblas -llapack`. On NixOS there is no global library path, so linear algebra failed even though GNU R's `Makeconf` recorded exactly where its libraries are.
- **Shape.** `library_candidates` (`crates/interp/src/linalg.rs`) tries, in order: the sonames, so `LD_LIBRARY_PATH` stays the user's override and a distribution's R gets the library it links; Accelerate on macOS; each library `BLAS_LIBS` (and for LAPACK, `LAPACK_LIBS` then `BLAS_LIBS`) names, in its `-L` directories and then by soname; and `R_HOME/lib/libRblas`/`libRlapack` last, because R's own libraries leave `xerbla_` to GNU R's `libR`. A candidate must define every routine of the table, so a BLAS tried for LAPACK is passed over. Opening LAPACK loads BLAS first, so a LAPACK whose BLAS sits in a directory of its own resolves the dependency by soname. A failure lists every place searched and the fix.
- **One Makeconf reader.** `make_assignments` and `expand_make` live in `crates/rcmd/src/makeconf.rs`, which `interp` includes by path, as `repl` includes `gnu_r_home.rs`. `Toolchain::for_base` on Linux keeps the default compilers but takes `BLAS_LIBS` and `LAPACK_LIBS` from GNU R's `Makeconf` (`Toolchain::with_makeconf_libraries`), so compiled packages link the same libraries the interpreter loads.
- **Impact.** No change where the sonames load. `La_library()` already reports the file the routines came from; `extSoftVersion()["BLAS"]` now does too. The candidate order is pinned by unit tests in `linalg.rs` and `rcmd.rs`.

- **ry carries the last resort on Linux.** A release archive held only `libR`, so a system with neither library (NixOS without GNU R) had nothing left to load. `tools/base-r/lapack.sh` builds GNU R's reference `libRblas.so` and `libRlapack.so` beside the runtime; `repl`'s build script embeds them with it, and `rcmd::assemble` links them into the R home's `lib/`, where the existing last candidates find them. They define `xerbla_` in libRblas rather than in libR, so libR still does not interpose on a system BLAS's handler, and they link no Fortran runtime (`tools/base-r/fortran.c` supplies the one routine they need), because libgfortran's static archive is not position independent on Ubuntu and a system without LAPACK rarely has the shared one. The smoke test hides the system's libraries in a user and mount namespace and runs `solve()`.
- `open_reference_blas` (`crates/libr/src/dynload.rs`) opens ry's reference `libRblas` and `libRlapack` with `RTLD_LOCAL` before the first package `dlopen`. A package linked against GNU R's copies (quadprog, PerformanceAnalytics) carries `NEEDED libRblas.so`, and the dynamic loader satisfies that by soname from the libraries already open. `RTLD_LOCAL` keeps the reference symbols from interposing on a system BLAS that other code uses.
- `fortran.c` supplies `_gfortran_concat_string` and `xerbla_`, so the reference libraries need no Fortran runtime. libR links with `-z defs`, which turns a new undefined runtime dependency into a link error.
- An OpenBLAS result can depend on its buffer's address modulo 16, which is why `linalg.rs` places every buffer through `Placed`. Haswell and newer kernels hide the dependence. `OPENBLAS_CORETYPE=Zen` reproduces it, and `RY_TEST_REFERENCE_LIBRARIES=1` runs the tests against the reference libraries.

## `ry test --watch` selects callers through S3 dispatch

**Status:** built.

- **Shape.** `Loader::callers` (`crates/loader/src/loader.rs`) walks the reverse mention graph from the changed names, and every name it reaches also reaches the generics its `S3method()` registrations name and every dotted prefix of the name (`format.shape` reaches `format`). The plan's `changed` list (`Plan::changed_names`) is the touched names plus the function and generic of each registration the reload made or removed.
- **Why.** A test calls the generic, never the method, so following mentions alone ran the test that dispatches to an edited method last. The dotted prefix covers a method registered by naming convention or by `registerS3method`, and the registration edge covers `S3method(gen, cls, fun)` with any function name. `check_reload` (`crates/loader/src/testing.rs`) models both, and the `test-watch` scenario edits a helper that only a three-argument registration ties to `format`.

## A release is `ry` alone, with the runtime embedded and the R front end folded in

**Status:** built.

- **Previous shape.** A release archive held `ry`, the runtime (`libR.so`, `libR.dylib` or `R.dll`) and `ry-rcmd` side by side, and `rcmd::runtime_home` required `ry-rcmd` beside the runtime.
- **Embedding.** `crates/repl/build.rs` embeds the file `RY_EMBED_RUNTIME` names, zstd-compressed (level 12 in release, 1 otherwise, since a debug runtime is about a gigabyte), plus `libR.dll.a` beside it on Windows, keyed by a hash of the compressed bytes. `just package` builds `libr` first and then `ry-lang` with the variable set. A build-time variable, not a cargo feature or artifact dependency: a plain `cargo build` embeds nothing and stays fast, and a variable needs no unstable `bindeps`. A section written after linking (Bun's approach) would need per-platform object rewriting; `include_bytes!` keeps the signature and the loader untouched on macOS.
- **Lookup order.** `RY_LIBR`, then beside the executable, `deps/`, `../lib/ry/`, and only then the embedded copy (`repl::ry_runtime_library`), so development builds and distribution packages behave as before.
- **Extraction.** Into `ry/runtime/<key>/` of `XDG_CACHE_HOME`, `%LOCALAPPDATA%` (Windows) or `~/.cache`. Each file is written under a per-process name and renamed into place, the library last, so a complete library means a complete directory and concurrent first runs both succeed; on Windows a rename over a loaded library fails, which counts as success when the file is already complete. A failure to write names `XDG_CACHE_HOME` and `RY_LIBR`; a failure to load an extracted library adds the `noexec` hint.
- **Multi-call.** `rcmd::front_end` holds what was `ry-rcmd`'s `main`. `ry` runs it when started as `ry-rcmd` (argv[0]) or, on Windows, as an R home's `R.exe`, `Rscript.exe`, `Rcmd.exe` or `Rterm.exe`. The R home's scripts exec `bin/exec/ry-rcmd`, a symlink to the front end, and the Windows front ends are hard links where possible, since `ry.exe` carries the runtime. `runtime_home` uses the `ry-rcmd` beside the runtime when there is one and the running executable otherwise. The `ry-rcmd` binary stays for development builds and tests. A home records its runtime and front end in `.libr`, and pruning removes only homes with both equal, so two copies of `ry` sharing one extracted runtime never remove each other's home. The runtime cache and the R homes share `rcmd::cache_directory`, and staging names carry `rcmd::staging_suffix` (process id plus clock nanoseconds), since containers sharing a cache volume often share a process id.
- **Tests.** `crates/ry/tests/scenarios/single-binary.scenario` runs `single-ry`, which the harness builds with `RY_EMBED_RUNTIME` and copies into a directory of its own, then rebuilds plain `ry`. `tools/smoke-test.sh` checks that an archive holds `ry` alone, that the runtime was extracted, and (with `RY_SMOKE_PACKAGES`) that `ry install` compiles C code.

## S-era package sources are R sources

**Status:** built.

- `R CMD INSTALL` collates `R/*.R`, `R/*.r`, `R/*.S`, `R/*.s` and `R/*.q`. ry read only `.R`, so
  every NAMESPACE export defined in an S-era file (Hmisc, VGAM, boot, cluster) was reported as
  undefined: 627 errors on a fresh 99-package sample. `cli::is_package_source` is the one predicate
  the CLI and the server use; a `.s` file outside `R/` (such as `src/fast.s`, assembler) is not R.

## Start-up and imports run GNU R's code, with its hot loops native

**Status:** built. The heap image weighed here for base alone exists ("The start-up image is build output of the base tree, and every start re-reads the environment").

- **Problem.** A warm `ry load` of a large S4 package spends most of its time before the package's
  own code runs: process start (base, `methods` and the default packages, about 130 to 170 ms) and
  the imports' `loadNamespace` (about 230 to 300 ms for lubridate, data.table and xts). Both run
  GNU R's own R code from the installed R home, so the loader does not touch them.
- **Options weighed.**
  - *A preloaded heap image* of the state after start-up (and after the imports). The heap sits in
    one fixed reservation, but the interpreter's own state (symbol and string tables, `Globals`,
    builtin and native-routine tables, connections, external pointers into loaded DLLs) lives in
    Rust collections and C libraries outside it, so an image needs a serializer for every one of
    them, and an import with compiled code cannot be imaged at all. Kept as the end shape for base
    alone, under "bundle base R into the `ry` binary" in `backlog.md`.
  - *A fork server* (a resident process that has started and loaded the imports, forking a child
    per `ry load`). It would cut start plus imports to a few milliseconds, but it is not exact: the
    child inherits the server's environment variables, user profile, `tempdir()` and library
    paths unless each is replayed, packages that start thread pools at load (data.table's OpenMP)
    switch to one thread after a fork, and a package updated on disk leaves the server stale.
    Rejected while exactness is the bar.
  - *Native hot loops behind GNU R's code* (chosen). The runtime replaces one base R function at a
    time with a copy whose per-item loops run natively, guarded so that it applies only to the
    exact GNU R code it was copied from.
- **`registerS3methods`** (`crates/interp/src/register_s3_methods.R`, `do_register_s3_methods` and
  `do_local_s4_generics` in `envir.rs`, `install_register_s3_methods` in `session.rs`). Its loops
  called `tryCatch`, `methods::is` and `get0` per method, and cost 41 ms of the default packages
  and 34 ms of the imports above (37 ms each on GNU R). The native loop
  performs the same steps in the same order (generic lookup, `genfun@default` for an S4 generic,
  table creation, the overwrite check with `identical()`, and the `delayedAssign` promise with the
  same environment `assignWrapped` makes) and calls back into R for each step that can signal or
  dispatch: `asNamespace` of an unloaded package, the not-found error, `methods::is` on an object,
  `@default`, creating a table, reading an unforced binding, `environmentName`. Now 8 ms in each.
  `methods::is(f, "genericFunction")` is skipped for a value with no class attribute and no S4
  bit, where it is always `FALSE`.
- **Why a hash guard.** Base R's R code is read from the installed GNU R so that ry behaves like
  whichever release it finds (licensing record above). The copy follows R 4.6.2, so the runtime
  replaces the installed function only when an FNV-1a hash of its formals and body (symbols and
  strings by their text, attributes ignored) equals the 4.6.2 one. Another release keeps its own
  function at its own speed, and a mismatch on the bundled 4.6.2 is an error, because it means the
  copy and the bundled tree drifted apart. `RY_PRINT_FAST_PATH_HASHES` prints the installed function's hash for
  updating the constant with the copy. The hash skips a nested `function` call's fourth element
  (its srcref), because a parse from source fills it and GNU R's saved namespace leaves it out:
  hashing it made the pristine function hash one way in `build-base` and another in a built home,
  and pinning the second broke every clean build. ry's own built base namespace saves the
  replacement, which the guard recognises by the replacement's own hash and leaves in place.
  `stable_hash_ignores_nested_function_srcrefs` in `session.rs` pins the srcref rule, and CI's
  `just base-r dev` is the end-to-end check because it builds base R from the source tarball.
- **Pinned by** `namespaces_register_s3_methods_as_gnu_r_does` in `crates/libr/tests/test_install.rs`,
  which installs two packages under ry and under GNU R and compares one script's output (every
  branch: known generics, another package's generic, an overwrite and its message, local plain and
  S4 generics, delayed registration), and asserts the replacement is in place under the bundled release.
- **Next by cost.** `methods::cacheMetaData` of the imports (about 150 ms, lubridate most of it) is
  dominated by `.checkGroupSigLength`, which `.updateMethodsInTable` runs for every member of a
  group generic and which rescans the whole group tree each time (1.2 ms per call on ry, 0.8 ms on
  GNU R). It runs natively ("Installed packages load through native fast paths, not an image").

## Installed packages load through native fast paths, not an image

**Status:** built.

- **Question.** Whether `ry install` should write each installed package's loaded namespace as an
  image inside the package, as `build-base` does for the base tree, so `library()` skips namespace
  setup.
- **Measured.** `library()` of 17 benchmark dependencies (31 namespaces: R6, cachem, cli,
  data.table, digest, fs, jsonlite, memoise, rappdirs, rlang, bit64, htmltools, knitr, rmarkdown,
  stringi, yaml, lifecycle and their imports) took about 282 ms on ry and 290 to 350 ms on GNU R.
  Lazy loading already fetches only 763 of the 7,570 objects the namespaces hold, about 40 ms, and
  an eager image of the same namespaces would cost 30 to 45 ms to load, so an image would save
  nothing on that part. The rest was setup work an image would also have to replay or invalidate
  (S4 `cacheMetaData` 66 ms, S3 registration 26 ms, `.onLoad` hooks).
- **Decision.** No per-package image. Each setup step that dominates runs natively behind a
  hash-guarded prologue on GNU R 4.6.2's own function (`RY_PRINT_FAST_PATH_HASHES=1` prints the
  hashes), so a package that changes, a library edit or another R release needs no invalidation:
  `.checkGroupSigLength` (`ryS4CheckGroupSigLength` in `methods_core.rs`), `registerS3methods`
  (ported to 4.6.2, `register_s3_methods.R`) and utils' `osVersion` (`ryOsVersion`, which reads
  `/etc/os-release` instead of spawning shells). The heap tenures every object once a start-up image
  loads (`Heap::promote_young`), because the first minor collection otherwise traced all 10 MB of
  them (5.3 ms), and each intermediate collection would trace them again. The heap then holds
  little but the image, so tenuring every object keeps the rule that a tenured object points only
  at tenured ones without tracing. Lazy-load records inflate through one reused
  inflater straight into the output, and decompressing file connections are buffered, which halves
  `readRDS` of a gzip file.
- **Impact.** `library()` of the 31 namespaces: about 282 ms to about 187 ms (GNU R 350 ms); the
  whole script 341 ms to 250 ms (GNU R 440 ms); empty `ry run` 59 ms to 48 ms (GNU R 129 ms). The
  S4 cache step fell from 66 ms to 27 ms and S3 registration from 26 ms to about 11 ms. The
  re-run of the environment steps fell from about 20 ms to about 10 ms; what remains is GNU R's
  profile (`.expand_R_libs_env_var` about 3 ms, the `Autoloads` attach about 1 ms) and the first
  S3 dispatch of each generic (about 0.3 ms, method lookup through `UseMethod`).

## Package builds run `make` in parallel and link BLAS and LAPACK by versioned file

**Status:** built.

- **Previous shape.** `R CMD INSTALL` ran `make` with one job, as GNU R does, and the Linux
  default toolchain linked `-lblas -llapack`, which needs the unversioned `libblas.so` that only
  a distribution's development package installs. The runtime opens `libblas.so.3`, so a system
  without that package ran linear algebra but failed to link Matrix or quadprog. A relative `-l`
  library also broke dependency lookup, because the R half runs in the build directory.
- **Target shape.** `run_make` sets `MAKEFLAGS=-j<processors>` unless the user set `MAKEFLAGS`,
  and a failed parallel build runs again with one job (make reuses the built objects), so a
  `Makevars` that is not parallel-safe still installs (`install-serial-makevars.scenario`). Where
  the toolchain says exactly `-lblas` or `-llapack` on Linux, the home's Makeconf writes
  `-l:libblas.so.3` and `-l:liblapack.so.3` (`versioned_linear_algebra`), so the linker finds the
  file the runtime opens when the package links, and the package records that file's soname
  (`libblas.so.3` for Debian's reference BLAS; a distribution whose `libblas.so.3` is a link to
  OpenBLAS may record `libopenblas.so.0`). Nothing in the home depends on what the system had
  installed: a `dlopen` probe at home build would freeze the result into a cached home. A system
  with only an unversioned `libblas.so` fails to link (unverified whether any distribution ships
  that). `bfd`, `gold` and `lld` all take `-l:file` on the command line; `gold` rejects it inside
  a linker script, which ruled out a `libRblas.so` script. `install` makes the library path absolute.
- **What is not the bottleneck.** Installs never byte-compile (ry compiles closures when they
  run), and writing the lazy-load database is fast: 0.06 s for MASS against GNU R's 0.10 s on the
  same driver. The R half spends its time in Rd processing and help indices (MASS: 0.55 s and
  0.27 s, about 10% slower than GNU R) and, for an S4 package, in sourcing the code (Matrix: 2.7 s).
- **Impact.** Whole `R CMD INSTALL` wall time on a 4-processor container:

  | Package | ry | GNU R | GNU R, `MAKEFLAGS=-j4` |
  |---|---|---|---|
  | MASS | 2.0 s | 6.6 s | |
  | lattice | 1.5 s | 8.1 s | |
  | Matrix | 39 s (failed to link before) | 177 s | 74 s |

  Matrix's compile takes about 134 s serially and about 33 s with four jobs, so for packages with
  compiled code the C and C++ compiler is the floor. Analysis is unaffected.

## ry's base R ships the recommended packages

**Status:** built.

- **Shape.** `tools/base-r/build.sh` installs GNU R's recommended packages from the source
  release's `src/library/Recommended/*.tar.gz` into the base R tree it builds, with that tree's own
  `R CMD INSTALL`, so `library(lattice)` and the rest work without GNU R installed, as in GNU R.
  On Linux that is all fifteen. On macOS and Windows nlme, KernSmooth and mgcv (which imports nlme)
  are left out, because their Fortran code would link a Fortran runtime the archive does not carry
  (Homebrew's or Rtools' `libgfortran`); there they come from GNU R's own libraries.
- **Why not GNU R's system library on `.libPaths()`.** Debian's `/etc/R/Renviron` puts
  `/usr/lib/R/library` in `R_LIBS_SITE`, but site libraries come before `.Library`, so GNU R's
  `stats`, `utils` and the other base packages there would shadow ry's own. `gnu_r_site_libraries`
  in `crates/interp/src/session.rs` leaves out every library that holds `base` for that reason.
- **Cost.** On Linux the fifteen packages add about 85 s to the build and 34 MB to the base R tree (63 MB in all).
- **Windows.** The unversioned `Recommended/*.tgz` names are symlinks, which tar cannot create on
  Windows, so the build extracts only the versioned tarballs.

## Hot C API entry points take a direct success path

**Status:** built.

- **Previous shape.** Every `libr` entry point ran its body inside `boundary::enter`, which saves the frame state, catches panics and R conditions, converts an `ApiResult` error into `Rf_error`'s longjmp, and then hands the result to the native frame's handout window. That costs about 90 instructions before any work, and a body such as `Rf_setAttrib` then went through the interpreter's general attribute setters (`dim<-` and `dimnames<-` validate through R-level coercion and can warn), so a single `setAttrib` cost about 550 instructions and a `dim` set about 930.
- **Target shape.** A hot entry point is `boundary::direct(fast, slow)`. `fast` gets the runtime and returns `Option`: `Some` is the result, handed out like `enter`'s; `None` sends the call to `enter` with the original body, marked `#[cold]` so the success path stays small. `fast` must not raise, warn, panic, run R code or call native code, and it may decline only before it has changed anything visible, so the slow path always starts from the same state. It may allocate, rooting what it holds across the allocation, because a collection there is invisible to the caller. Results the collector already keeps alive (an attribute value, a symbol) come back as `Reachable` and skip the handout. Errors, warnings and longjmps happen only on the slow path, so `Rf_error` semantics are unchanged.
- **Coverage.** The attribute functions (`getAttrib`, `setAttrib` including `names`, `dim`, `dimnames` and `class`, `inherits`, and `classgets`, `namesgets`, `dimgets` and `dimnamesgets` for a non-`NULL` value), allocation and scalar constructors (`allocVector`, `Scalar*`, `mkString`, `cons`, `lcons`, `list1`-`list6`, `lang1`-`lang6` with one batched reservation), coercions of a plain first element (`asLogical`, `asInteger`, `asReal`), `nrows`/`ncols`, `install`, `charIsASCII`/`charIsUTF8`, `ExternalPtrAddr`, `GetOption1`, protect-with-index, the cell writers (`SETCAR` through `SETCAD4R`, `SETCDR`, `SET_TAG` on a plain cons or language cell), `R_existsVarInFrame`, `R_GetBindingType`, and `R_DelayedBindingExpression`/`R_DelayedBindingEnvironment` on an unforced promise whose environment exists (an argument reference or a pending frame materializes one, which allocates, so it takes the slow path). Reading `names` answers a one-dimensional array's `dimnames[[1]]` in the same walk, and reading `row.names` returns stored values directly. Expanding compact row names on read, setting `row.names`, and pairlist `names` (tag rewriting) stay on the slow path because their R semantics are not worth duplicating.
- **Resizable vectors.** `R_allocResizableVector` and `R_resizeVector` take direct paths. data.table allocates its column list and names about a thousand slots past the columns it has and shrinks them at once, in nearly every call that builds or modifies a table, so the shrink dominated: it visited every dropped slot to release what it held. The shrink (`Heap::resize_within_capacity`) now tests dropped slots eight at a time and releases only a chunk holding something other than `NULL` or the blank string. `NULL`'s count is pinned and the blank string is a rooted global, so neither needs a release. The direct allocation zeroes an atomic vector, unlike `Rf_allocVector`'s direct path, because the slow path (`alloc_resizable_vector`) zeroes it and the fuzz compares the two; a resizable vector is usually a list or character vector, where the fill is required anyway.
- **Duplicating captured calls.** `Rf_duplicate` takes a direct path for calls and pairlists without attributes whose leaves are shared values (symbols, environments, `NULL`) or plain atomic vectors; anything else, including a list, ALTREP, an attribute anywhere or a nesting past 64 levels, declines to the general `duplicate`. rlang copies every argument expression it captures (`capture`, `ffi_interp`), which made `Rf_duplicate` the largest boundary cost in cli's tests outside the string layer. The direct path copies post-order: it copies and roots a chain's elements first, then allocates the chain's cells back to front with their final element and successor under one `reserve`, so no cell is written twice and no barrier runs. The general path keeps one `duplicate` for every shape, because a second full copier would be a second source of truth for what a deep copy shares.
- **Why not make `enter` cheaper instead.** `enter`'s cost is the price of catching an unwind and saving state; it cannot be both general and free. Splitting success from failure is the shape GNU R itself has (its setters check and then store), and it keeps the one error path in one place.
- **Pinned.** `crates/libr/tests/test_api_boundary.rs` fuzzes every direct path against the slow path (`ry_set_slow_paths_only`) on random objects, with GC stress on a quarter of the seeds and base R loaded so it can build delayed and active bindings, comparing results, the mutated object and its attribute order with `R_compute_identical`. Its callgrind test holds every touched entry point to an instruction budget no higher than the same loop with direct paths switched off (`RY_API_BENCH_SLOW=1` measures that).
- **Heap allocation errors are built lazily.** The allocator's `ok_or(HeapError::...)` chains became `let ... else` returns: the eager form constructed and dropped a `HeapError` on every allocation, about a dozen instructions per `allocVector`.
- **Not on this path.** `Riconv`, which `processx` calls to convert child output to UTF-8 while `rlang`'s tests run, belongs to the string and encoding layer, not to the call boundary; UTF-8 to UTF-8 copies its valid prefix ("`iconv` goes through the C library's `iconv`").
- **Not changed.** Attributes stay a pairlist on the object. A dense attribute table would make lookups cheaper still, but it changes the image format; it is the follow-up in `backlog.md`.
- **Closed: no further entry point is worth a direct path.** After `Rf_duplicate`'s, the whole C API boundary is 0.44% of the `testthat` application's instructions, and its largest entry, `R_existsVarInFrame`, is 0.08%. In the package test suites (release build, one process with `TESTTHAT_PARALLEL=false`) the largest remaining entries outside the string layer are small:

  | entry point | cli | rlang | vctrs | Ir per call |
  |---|---:|---:|---:|---:|
  | `Rf_shallow_duplicate` | 0.41% | 0.37% | 0.44% | 1,030 |
  | `R_existsVarInFrame` | 0.58% | 0.23% | 0.35% | 475 |
  | `SETCAR`, `CAR`, `Rf_protect` | 0.1 to 0.3% each | | | |

  A direct path roughly halves an entry's cost, so the two largest together would save about 0.5% of cli and less elsewhere. Reopen this only if a profile shows one entry above 2% of a named workload. Two vctrs costs that look like boundary costs are not: `Rf_allocVector` at about 11,000 Ir per call is probably collection charged to the allocator (unverified), and `INTEGER_RO` at about 19,000 Ir per call comes from callers that build a fresh `1:n` on every call. A compact sequence is expanded once and then reused, as in GNU R, and building it costs about 37,000 Ir per call on both sides.
