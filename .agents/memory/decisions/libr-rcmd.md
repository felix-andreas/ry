# Decisions: C API, R home and packages

The `libR` C API, `R CMD`, package installation, the bundled base R, and the loader behind `ry load`, `ry test` and `ry document`.

## Packages and R versions in the runtime

**Status:** built, except the `[r]` keys of `ry.toml` and the renv pin of the corpus, which do not exist. The runtime choice, the per-package cache and the R home layout are superseded by "ry run and ry R: the command chooses the runtime", "Bytecode is build output, not a cache" and "ry ships its own base R, and GNU R through `R_HOME` is the opt-in".

Design and measurements: `development/design/packaging.md`.

- **One installed package directory serves GNU R 4.6 and ry,** because ry's `libR` is ABI-compatible with R 4.6's headers and reports the version of the GNU R it loads base R from. The `Built:` field is treated as GNU R treats it (only pre-3.0.0 builds are refused), so existing libraries load unchanged. A package ry builds also carries `RyBuilt:` and `RyAbi:`; the corpus is built only by GNU R.
- **ry's per-package data never lives inside the package,** so the differential oracle loads byte-identical packages under both runtimes and shared libraries (site libraries, renv's cache) are never written to.
- **Package management runs as R code under the runtime** (`install.packages`, renv, pak). A package manager of ry's own (`ry add`, `ry sync`, a lockfile) is deliberately left open, because the runtime does not need it; if it comes, it must read and write `renv.lock`.
- **The runtime is never chosen by failure,** because a fallback hides what a transition must surface. The `renv.lock` R version and `Depends: R` are advisory only, or most renv projects would stay on GNU R.
- **The analysis reads the installed library, not a running R,** found by a fixed rule (renv's project library, else `R_LIBS`/`R_LIBS_USER` and the selected R's libraries; `.Rprofile` is never evaluated). Per package it reads `DESCRIPTION`, `NAMESPACE` and the `.rdx` object names through a tracked by-name lookup, with stubs chosen by version range. Generated export manifests remain for base R and as the no-library fallback.
- **The corpus would be pinned with renv** on a dated P3M snapshot, because renv is in the benchmark set and R's own installer cannot backtrack to older compatible versions.

## The full C API surface and package installation

**Status:** built, except base R from GNU R's home, the header-only BLAS and LAPACK link line, the skipped byte-compile and the routines rewritten from published algorithms, which "ry ships its own base R, and GNU R through `R_HOME` is the opt-in", "BLAS and LAPACK come from the system, then from GNU R's Makeconf", "Bytecode is build output, not a cache" and "Numeric routines are exact ports of GNU R 4.6.2, and LINPACK lives in the interpreter crate" supersede.

- **Evaluation is a hook the interpreter installs, not a dependency.** `libr::eval::Evaluator` is set once with `install_evaluator`, and every evaluating C entry point (`Rf_eval`, `R_tryEval`, `R_forceAndCall`, `R_ParseVector`, `Rf_PrintValue`, finalizers, `R_GetCurrentEnv`, S4 class functions, namespace loading in `R_GetCCallable` and the unserializer) goes through it. The interpreter calls libR back as ordinary Rust (`native_call`, `connections::*`, `serialize::*`, `altrep::*`). Without one, a built-in evaluator of native calls runs, so libR is testable alone.
- **libR depends on zlib, bzip2 and xz** as GNU R's does, because binaries (gert, httpuv) call zlib without linking it and find it through libR.
- **The graphics engine C API is real** (`crates/libr/src/graphics.rs` over `crates/graphics/src/engine.rs`).
- **Names R 4.6 dropped are still exported and declared** (`legacy.rs`; the headers keep `SETLENGTH`, `SET_TRUELENGTH`, `FRAME` and an empty `R_ext/PrtUtil.h`), because older binaries import them and pinned sources (readr, isoband, yaml) use them; one missing symbol stops a library loading. A `TRUELENGTH` other than a shrunk vector's capacity lives in `Runtime::truelengths`, with the object preserved so no address is reused under a stale value. `SETLENGTH` makes the vector resizable; `FRAME` returns a snapshot pairlist (frames are hashed); `R_PPStack` is a real array that a root source scans up to `R_PPStackTop`, for code compiled with R's internal macros.
- **`.C` and `.Fortran` copy their arguments,** pass data pointers through the same dispatcher as `.Call`, and return the copies as a named list. `.Call` accepts `NativeSymbolInfo` objects (`R_forceSymbols` needs it).
- **`R_withCallingErrorHandler` runs its handler after unwinding;** GNU R runs it before. Only a handler that inspects the unwound native frames can tell.
- **Hash tables (`R_mkhashtab`) live entirely in heap objects** (an external pointer tagged `c(type, count)` protecting the bucket list), so the collector traces them and nothing is freed by hand.
- **ALTREP methods are default, native (C trampoline) or built in (Rust, no `setjmp`),** so compact sequences cost nothing at the boundary. A class handle starts with GNU R's method table, because vctrs calls `Extract_subset` through `STDVEC_DATAPTR(ALTREP_CLASS(x))`; each slot trampolines into ry's dispatch, the single source of truth. The handle has GNU R's layout (a raw vector whose attribute is class, package and type), registered by (class, package) symbols. `Rf_coerceVector` of a plain integer or double vector to character returns a deferred string, as GNU R does.
- **Serialization writes ALTREP objects as `ALTREP_SXP` in version 3** and reads them through the class registry (loading the package's namespace for an unknown class), so compact sequences round-trip byte for byte. GNU R bytecode reads back as the expression in its constant pool, because ry does not run GNU R bytecode. Environments are written unhashed; namespace and package references go through a resolver the interpreter replaces (the default creates registered placeholders).
- **A connection is identified by its `conn_id` external pointer object,** validated on use so a stale one errors with "invalid connection". The pointer's address is not the identity, because packages overwrite it (curl stores its multi handle there). Buffering lives in each connection kind, not in `Rconn`'s `buff`, so `R_ReadConnection` after `readLines` sees exactly the unread bytes.
- **Header export coverage is checked by the compiler** (`-aux-info` and `-E` in `crates/libr/tests/test_headers.rs`), because rename macros and struct fields defeat a text scan.
- **Package installation is the `rcmd` crate and runs on ry.** The R home's `bin/R`, `bin/Rscript` and `bin/Rcmd` reach `ry-rcmd`, which answers `R CMD INSTALL|SHLIB|config` and `R RHOME` in Rust and runs everything else on ry's interpreter, so GNU R's `R CMD` scripts calling back into `bin/R` also run on ry. Install compiles `src/` in Rust, then `crates/rcmd/src/install.R` runs the R half with GNU R's own `tools` functions in `tools:::.install_packages`' order (DESCRIPTION and `Meta/`, collated code, `sysdata`, lazy data, demos, `inst/`, the lazy-load database, Rd objects and help indices, vignette index, `nsInfo`), then test-loads in a fresh ry process, so the installed layout is GNU R's and GNU R can read it. No GNU R process runs at install time.
- **Rmath's oracle is GNU R itself:** `crates/libr/tests/data/rmath_cases.R` records answers and random streams through the `rmathtest` package, and `test_rmath` replays them with a per-function tolerance that names its reason. Random variates draw uniforms in GNU R's order, so seeded streams match.
- **An error raised while `dlopen` runs static initializers surfaces as a `dyn.load` error** (`ry_call_dlopen` sets a jump target; Rcpp's `Function c("c")` globals call `Rf_findFun` there). Longjmping out of an initializer leaves glibc's loader lock with that thread, as in GNU R, so a process exiting from another thread deadlocks in `_dl_fini`; R and `test_load_errors` run on the main thread.
- **`getNamespace` is a built-in of the native-call evaluator,** answered by the serializer's namespace resolver, because Rcpp's initializers look it up before any interpreter exists.
- **One L-BFGS-B, written from the BSD reference v3.0** (Zhu, Byrd, Lu and Nocedal's `lbfgsb.f`; `crates/stats/src/lbfgsb.rs` is the only implementation: `optim` calls it through `crates/stats/src/optimize.rs`, and libR's `lbfgsb` entry point only maps `nbd` and wraps the C callbacks), matching GNU R's hybrid of versions found by comparing every iterate bitwise (`%a`) against Rscript: the subspace step backtracks to the box without v3.0's Morales-Nocedal projection, and the Cauchy search keeps the `f2` floor (`eps * f2_org`) and is skipped unconstrained once memory exists. Addition order is pinned with comments where it is not the reference's (grouped in `cmprlb`, `cauchy`, the subspace product and `formk`'s (2,1) block, ungrouped in its diagonal blocks). Oracle case `stats__lbfgsb_paths` and `test_numerics` hold it exactly; the trace text for a line search that exhausts 20 evaluations is unverified (no case reached it in GNU R).
- **`nlminb` runs PORT's drivers written from netlib's reference** (`crates/stats/src/port_minimizer.rs`: `DRMNG`/`DRMNGB` BFGS and double dogleg, `DRMNH`/`DRMNHB` More-Hebden with a Hessian, `DRMNF`/`DRMNFB` finite differences, behind one reverse-communication `Minimizer`; GNU R's `portsrc.f` was never read), matching GNU R's driver by black-box tests: a Hessian without a gradient is ignored; the loop runs the objective once more, uncounted, at the final point; a NaN objective warns "NA/NaN function evaluation" and becomes Inf; the Hessian's lower triangle is read by rows; `trace` prints at `DITSUM` points when `niter % trace == 0`; a nonpositive `scale` gives code 18. Quirks kept: `DRMNGB`'s `G0` shares scratch with `DD7DGB` and `DQ7RSH`, `DD7DGB` leaves `NRED` unset with no free variables, `DS7BQN` keeps the last violating index. `DV2NRM` is BLAS `dnrm2` (LAPACK 3.12's Blue algorithm), because GNU R links PORT's norm to the BLAS; PORT's own norm differs in the last bit from the second step. Oracle case `stats__nlminb_port_paths`; open divergences in `backlog.md`. `nls(algorithm = "port")` still runs ry's own trust-region method (`crates/stats/src/port.rs`).
- **KalmanLike and `prod` accumulate as GNU R observably does:** the Kalman filter updates term by term (`y -= z_i a_i`, `V[i,j] += m T`), and `prod` multiplies each argument in long double, combines in double, and lets the first NaN win (`prod(NaN, NA)` is NaN).
- **`R CMD SHLIB` passes `-l`, `-L` and `.a` arguments to the link,** because configure scripts probe with `R CMD SHLIB conftest.c -lfoo` (Rglpk).
- **`Rf_rnbeta` is exported** though GNU R does not define it, because `Rmath.h` declares `rnbeta` and Rcpp's sugar refers to `::Rf_rnbeta`: a declaration with no symbol fails the header coverage test, and no declaration breaks Rcpp builds.

## The package-development loader

**Status:** built, except the ignored `if` in `NAMESPACE` and the console's own stat fingerprint, which "`if` in NAMESPACE is evaluated by the host, then resolved in text" and "Source-change detection lives in the loader, so both runtimes reload the same way" supersede.

User page: `reference/package-loading.md`; design: `development/design/r-runtime.md` section 6.

- **The index lives in `semantics`, the diff in `crates/loader`, and only a backend knows the runtime.** `semantics::load_index` derives per-file facts (`file_items`, `file_binds`), the binding join (`package_bindings`) and the package join (`package_index`) with its own input `PackageSources`, so a loader never perturbs a project's `ProjectFiles`. The diff produces a backend-neutral `loader::Plan` that both runtimes apply.
- **Out of process, a plan travels as an uncompressed RDS file written in Rust** (`r::rds`), because `readRDS` builds vectors in C without parsing R code; one format serves the in-process `.Call` routines on the embedding `DllInfo` and the `ry load --emit-plan` process a plain R session runs. The loaded state is a JSON `LoadState` in the session's `tempdir()`, committed by renaming `<state>.pending` only after R applied the plan, so a failed apply never records a load (in process, `Loader::commit` does the same).
- **The GNU R runtime is byte-compiled once per runtime hash and R version and cached** under `dirs::cache_dir()/ry` (`crates/ry/src/load.rs`), because R's JIT otherwise compiles each runtime function on first call, about 200 ms per session, more than a reload. The cache stores the whole environment, because `environment<-` strips bytecode from a closure.
- **A name is lazy only when its one writer defines a function.** Two writers make every writer eager in order, which keeps `f <- memoise(f)` correct without special cases.
- **Reload rules (each found by the reload property test):** statements match by content in order (longest common subsequence) within a file; a statement reruns when new, when it read a changed binding, or when it writes a changed name; a rerun statement reading a name rewritten at or after it reruns that name's earlier writers first; the rerun set is a fixpoint over the growing changed-name set; a `Collate` reorder falls back to a full load. Dependencies include calls: function items record the identifiers their body mentions, and the fixpoint closes over that call graph.
- **Durability, not a host-side cache, keeps a reload proportional to the edit.** Files enter at `Durability::HIGH` and drop to `LOW` on their first edit, so salsa verifies untouched files in O(1). A host reusing per-file results would duplicate salsa's bookkeeping.
- **The REPL reloads by feeding `.ry_autoreload()` as its own top-level command** before the user's, only at the top-level context (`R_GlobalContext` has no parent; comparing prompt text broke on `options(prompt =)` and fed `readline()`). A failed reload retries only after the next change on disk. It is the one place the console feeds input after start-up, a user-visible command like typing `load_all()`, and it keeps `.Last.value`.
- **testthat follows pkgload 1.3's `load_all()`:** with `tests/testthat` or `inst/tests` and testthat installed, `library(testthat)` attaches after the package code and before `.onLoad`, and `helper*.R` files are sourced with `NOT_CRAN=true` into the attached package environment (not the namespace, which would leak them into `pkg:::`), only when the load attaches and exports everything. Their names are kept in `state$helper_names` so a package-environment sync does not drop them. Exports sync after `.onLoad`, as pkgload's `setup_ns_exports` does, so a name `.onLoad` binds is attached too.
- **Deferred** (`backlog.md`): updating other development packages' import copies of a changed binding, and a `system.file()` shim for `inst/`.

## The loader on ry's runtime

**Status:** built.

- **One R runtime (`crates/loader/src/runtime.R`) with backend hooks.** GNU R's (`gnur.R`) keep the RDS plan and `makeLazy`; ry's (`ry.R`) call `.Internal(ryLoaderPlan/Commit/Bind/Run/Force)` in `libr::loader_backend`, which keeps the plan in process. `.Internal`s, not `.Call`, because the interpreter registers Rust builtins directly: no `DllInfo`, no RDS round trip, no source strings in R memory.
- **A lazy binding is a promise over `ryLoaderForce(tree, start, end)`,** where `tree` is an external pointer to the analysis's parse of the file, and forcing lowers the node at those byte offsets (`SourceSpan::source_start/end`, `interp::lower::lower_node`), so the node is found without text and no function is parsed twice. The analysis already parsed every file, so forcing skips the lexer and parser and costs no extra memory. A tree is dropped at the collector's next finalizer point (a native call's return, `gc()`, or exit) after no promise of its file remains, not immediately.
- **`ry load` defaults to ry's runtime;** `--runtime gnu-r` (or `RY_RUNTIME=gnu-r`) keeps the GNU R path.
- **Planning runs on its own thread** with `ANALYSIS_STACK_SIZE`, because the analysis recurses as deeply as the code nests.
- **A process's first plan is a full load, made cheap rather than cached.** The S4 scans use hash sets and one grouping pass (they were quadratic in generics × definitions), and a full load skips reload bookkeeping. A persisted plan was rejected: it would be a second on-disk copy derived from the index, which the index cache already keys by content. On a 591-file, 65,000-line S4 package (`tools/loader-bench/s4_package.py`) `ryLoaderPlan` takes 14 ms (38 ms quadratic); what remains is linear: `diff` (about 7 ms), reading source stamps (3 to 6 ms) and converting the plan to an R value (a third of the plan thread), while `prepare` deserializes the cached index (18 ms for 1.7 MB) in parallel with R's start.
- **Runtime fixes the load path needed, each pinned by an oracle case:** `readDCF`, `load()`/`save()` of XDR workspaces (`R/sysdata.rda`, gzip, bzip2 or xz through `gzfile()`), `readChar` on a connection, comparisons and `&`/`|` keeping only names, dim and dimnames, `match()` on interned ASCII strings by pointer, and `names(env)`/`ls()` from print names.
- **Measured against the GNU R backend:** on the 326,000-line bench package a cold load is faster and a one-function reload slightly slower (932 ms and 58 ms against 1,036 ms and 53 ms); on a 14,000-line package the cold load is slower (104 ms against 82 ms), because the interpreter evaluates eager statements more slowly than GNU R.
- `test_load.rs` runs its namespace, S3/S4, collation and reload tests on both runtimes.

## libr reads options from base's `.Options`

**Status:** built.

- `Rf_GetOption1` walks `interp::envir::options()`, the `.Options` pairlist `options()` writes, so it is the single store. A separate options environment in libr's `Runtime` that nothing wrote made it always return `NULL`, so data.table's `datatable.*` options read as defaults.

## One serializer writer, in libr

**Status:** built.

- **libr's writer (`crates/libr/src/serialize.rs`) is the only one.** It registers `serialize`, `serializeb`, `serializeToConn` and `saveToConn` through `interp::register_builtins`, because the interpreter cannot depend on libr. A second writer drifted (no version 2, ASCII, native byte order or refhooks), so `serialize(x, NULL, version = 2)` gave version-3 bytes and digest's version-2 hashes were wrong.
- **The header is written one field per output call,** as GNU R does, because digest's `spooky` algorithm counts the calls.
- The readers are still duplicated (`interp::serialize`'s `unserialize` and libr's `R_Unserialize`); `backlog.md` tracks merging them.

## A package ry compiled records its headers' binary interface

**Status:** built.

- **`rcmd::NATIVE_ABI` versions the binary interface the headers' inline accessors give compiled code.** `R CMD INSTALL` under ry writes it as `RyAbi:`, and `dyn.load` (`crates/libr/src/eval.rs`) refuses a package's shared library when `DESCRIPTION` has `RyBuilt:` and a missing or different `RyAbi:`, naming the reinstall command (`rcmd::stale_native_build`). The comment over the inline accessors in `Rinternals.h` says to bump it. Pinned by `install-stale-binary.scenario`.
- **Refuse, not reinstall:** reinstalling inside `library()` would need sources, repositories and compilers, and would rewrite a library (site library, renv cache) that may not be the session's to write.
- **Equality, not a version comparison:** `RyBuilt:` moves every release while the headers rarely change, so keying on it would force a reinstall per upgrade; equality refuses exactly the binaries whose inline code differs, newer ones included. The trigger was the inline `DATAPTR_RO` and `STRING_PTR_RO` gaining the write barrier, which left every earlier ry-compiled package (0.4.5 and older) with the unsafe inline code.
- **In-place builds are stamped too:** make rebuilds only what sources outdate, so objects from old headers would be reused. `rcmd::compile_in` keeps `src/.ry-abi` and deletes objects and shared libraries under `src/` when it differs; static libraries stay, since a package may ship one prebuilt. Pinned by `load-stale-objects.scenario`.
- GNU R's builds reach element pointers through libR's functions, carry no `RyBuilt:` and are not checked.

## ALTREP elements go through the class methods everywhere

**Status:** built.

- **`heap::object::install_altrep_elements` installs libr's read and write functions,** and `Sexp::element` and `Heap::set_element` dispatch to them for an ALTREP object, so there is one access path none of the roughly 370 interpreter call sites can forget (raw reads showed the instance's private triple, which crashed DBI's arrow round trip on nanoarrow columns).
- **The accessor cannot fail,** so an unwind from the method is parked (`interp::unwind::park`) and raised when the builtin that made the read returns (`call_builtin`), before its value reaches R code, so `tryCatch(x[[2]], ...)` catches it; a read outside a builtin raises it at the evaluator's next poll. Meanwhile the read returns `NA` (strings) or `NULL`. Making `element` fallible would touch every call site for a case only ALTREP reaches.
- `length()` asks `Length`, and `[[` on a string or list instance reads only the selected element, so `Elt` runs exactly where GNU R runs it (`r_code_reads_only_the_string_elements_it_asks_for` in `crates/libr/tests/test_altrep.rs`). Numeric instances are materialized whole for slice access.
- An ALTREP instance is never marked mutable, so the interpreter copies it before modifying in place, as GNU R's `DATAPTR` contract implies.

## The presented R version comes from the base package the runtime runs

**Status:** built, except the source of the revision and date, which "The runtime emulates the GNU R version it loads base R from" supersedes.

- **One source of truth: the R home's `library/base/DESCRIPTION`.** `R.version`, `getRversion()` (`interp::system::r_version`), the serialize writer version and the home's generated `include/Rversion.h` (`rcmd::assemble`) all read it. The revision is raised to at least 89194, because ry implements the API data.table guards on it.
- **Why the C and R versions must agree:** with C at 4.6 and R at 4.3, rlang's C made `ns_registry_env()` defunct while its R code still called it, breaking pkgload, generics and downlit. A version claim above the base R code actually running is false at the R level: presenting 4.6 over a 4.3 base made `is_installed("base", version = getRversion())` false and sent R code to 4.4+ base functions the base lacks.
- The checked-in `crates/libr/include/Rversion.h` serves only code built directly against `crates/libr/include`. Source-installed packages must be reinstalled when a home's base version changes. `crates/libr/tests/test_install.rs` pins that `getRversion()`, `packageVersion("base")`, `Rversion.h` and `R --version` agree.

## ry run and ry R: the command chooses the runtime

**Status:** built.

- **The command names the runtime** (`crates/ry/src/command_line.rs`): `ry run [FILE] [ARGS]` runs on ry's runtime, `ry R [FILE] [ARGS]` on GNU R, and without FILE each opens the console; `-i FILE` runs FILE then opens the console. Both consoles load the package the working directory belongs to unless `--no-load`. `resolve_runtime` is the one resolution function (`Command::session` and `ry load` call it).
- **Compatibility:** `ry repl` is a hidden alias with its GNU R default; `--runtime` and `RY_RUNTIME` are honoured but undocumented, the flag beats the variable, and `ry R` ignores the variable because a command named after GNU R must run GNU R.
- **FILE and its arguments are one `trailing_var_arg` vector,** so everything after FILE reaches the script verbatim, as with `Rscript` (`ry run s.R --runtime gnu-r` passes the flag to the script; accepted deliberately). On GNU R they follow `--args`, so `commandArgs(trailingOnly = TRUE)` agrees. `crates/ry/tests/test_command_line.rs` fuzzes this. `--keybindings` and `--no-load` conflict with a script because they would be silently ignored.
- **Exit codes:** 0 success; 1 top-level error or strict refusal; 2 usage error, unreadable script (read before R starts) or a runtime that cannot start; `q(status = n)` exits n. A GNU R batch run starts non-interactive with `--no-echo`, as `Rscript` does: `interactive()` is FALSE, a top-level error prints "Execution halted" and exits 1 through R's own path, and R prints no trailing newline at session end.
- **`-i FILE`** is checked under its own name with the same `Preflight` the console continues (`Preflight::rename`), so a refusal exits 1 before the console opens, later commands see the file's definitions, and the file is fed as already-checked console input.
- **Options come before FILE,** because clap stops reading options once a trailing positional has its first value.

## Source-change detection lives in the loader, so both runtimes reload the same way

**Status:** built.

- **`Loader` owns the fingerprint** of the sources a plan read (taken before reading, so a mid-read edit counts later) and promotes it on `commit`; `Loader::changed_roots` reports each moved package once. GNU R's `ReloadWatcher` and ry's `RuntimeReloadWatcher` (through libR's `ry_loader_changed`) both ask it, because libR's `Loader` (`crates/libr/src/loader_backend.rs`) is invisible to the binary and a second fingerprint beside it was duplicated state. Both runtimes reload through the same `.ry_autoreload()`, and `ry_repl` attaches `tools:ry` and runs a start-up load, so `ry run` in a package loads it as `ry R` does.
- **The fingerprint stats `DESCRIPTION`, `NAMESPACE` and `R/` code files without reading them** (the extension test of `collated_r_files`, so editor swap files do not reload). It overlaps the per-file `stamps` on purpose: stamps decide which texts a plan rereads, the fingerprint whether to plan at all, and merging them would make the per-command check walk `Collate`.
- Pinned by `crates/loader/tests/test_reload_fuzz.rs` and `test_repl_e2e.rs`.

## A reload re-derives one file and rewrites only the bindings it changed

**Status:** built.

- **Binding facts are their own firewall:** `file_binds` per file and `package_bindings` joining them behind an `Arc`. A body edit leaves them equal, so `diff` sees the same `Arc` (`Arc::ptr_eq`) and skips the laziness-flip scan. An item's load facts are computed inside `file_items`, not memoized per item.
- **Per file, not per item,** because salsa re-verifies every memo the plan reaches on a file's first edit (files start at high durability); one memo per file is far cheaper to re-verify (a file's first edit still re-verifies about 8 memos per file and 5 per eager item). Starting files at low durability was rejected: every edit would pay that cost. On the 326,000-line package (25,000 items) a repeated body edit reaches output in 9 to 12 ms (plan 6 to 8 ms) and a file's first edit in about 30 ms. The cold load warms `file_binds` across cores.
- **The plan carries `keeps_names`** (every binding written was bound before and none goes away), which lets `runtime.R` skip the exports resync and copy only the touched names into the package environment.
- `test_incremental.rs` pins that a body edit re-runs one file's layer and stops at `package_bindings`.

## A cold load is analyzed while base R starts

**Status:** built.

- **`Loader::prepare` derives the whole index without making a plan,** on a thread the host starts before R (`ry_loader_prepare` for ry's runtime, `load::prepare` for GNU R), so parsing overlaps R's own start-up (about 400 ms of indexing against about 140 ms of R start-up on the 326,000-line package; cold `ry run` there takes about 0.74 s, down from 0.95 s run serially). The plan then finds everything memoized and only diffs. Prepare and plan share one salsa database, so a file edited between them is re-derived as usual.
- Strict mode lives in an atomic beside ry's runtime session, so applying session flags never waits on the prepare's lock.

## ry refuses a `libR.so` built from other sources

**Status:** built.

- Cargo builds `libR.so` separately from `ry`, so a stale library silently ran old code. `crates/libr/build_identity.rs` (included by the `libr` and `repl` build scripts) hashes the sources (`src/`, `include/`, `build.rs`) of `libr` and every workspace crate it links, found by following `path` dependencies; `libR.so` exports it as `ry_runtime_id`, and `ry` compares it after `dlopen` and exits 2 naming `just build` and `cargo build -p libr`. Edits to crates `libr` does not link (the formatter, the IDE layer) leave it alone.
- Contents, not timestamps or a git commit: a no-op rebuild or `touch` keeps the identity, git is not needed, and the two binaries of one `cargo install` or release build always agree. A dependency bump alone does not trip the check. `Cargo.toml`, `Cargo.lock` and the compiler version are excluded on purpose (adding a test target must not look like a runtime change).

## The C headers declare the API of the presented R version

**Status:** built, except the default version 4.3.3, which "ry ships its own base R, and GNU R through `R_HOME` is the opt-in" supersedes.

- Packages branch on `R_VERSION` to backport newer API, so headers declaring everything ry implements would collide with the backports (rlang's `ANY_ATTRIB`, `CLEAR_ATTRIB` and `R_mapAttrib`, yaml's `R_mkClosure`) and miss names newer R removed (Rcpp's `VECTOR_PTR` below 4.5).
- Every declaration GNU R added in 4.4 to 4.6 sits behind `#if R_VERSION >= R_Version(...)`, versions taken from GNU R's headers; the headers include the home's generated `Rversion.h`. `VECTOR_PTR` is declared below 4.5. The crate's fallback `Rversion.h` says 4.6, so ry's own tests and code built against `crates/libr/include` see every name. `libR.so` exports every name regardless.

## macOS packages bind to ry's libR

**Status:** built.

- **The problem:** Mach-O's two-level namespace binds each undefined symbol to one named library, so a CRAN binary (`-framework R`, naming `/Library/Frameworks/R.framework/Versions/4.x-<arch>/Resources/lib/libR.dylib`) or Homebrew binary (`-lR`) would load GNU R's libR beside ry's and run on GNU R's heap. Linux has no such problem: the package records `libR.so` by soname and ry's library, loaded first with `RTLD_GLOBAL`, satisfies it.
- **Rejected:** a flat namespace or `DYLD_INSERT_LIBRARIES` (need variables before ry starts, stripped by SIP, change binding process-wide); rebuilding every package (gives up CRAN binaries).
- **Shape:** ry's `libR.dylib` has install name `@rpath/libR.dylib` and a second `-exported_symbols_list` for the `RY_EXPORT` C symbols (ld64 and ld64.lld merge it with rustc's list). `dyn.load` of a Mach-O naming GNU R's libR (`crates/libr/src/macho.rs`) opens a cached copy with that load command rewritten in place to `@rpath/libR.dylib`, its compatibility version zeroed (GNU R records its own, ry's libR has none, so dyld would refuse it), and re-signed ad hoc (`codesign --force --sign -`), because arm64 refuses a modified unsigned image. dyld satisfies the load command from the already loaded image with that install name. The cache key is `REBIND_VERSION` (`dynload.rs`, bumped whenever the rewrite changes), path, size and mtime; a library naming no libR gets a marker so it is read once. The DLL record keeps the original path.
- **Relative dependencies:** the copy's cache directory mirrors the original's surroundings with links (`mirror_surroundings`), climbing as many ancestors as the longest `@loader_path` prefix (`macho::Rebound::climb`). `.target` in the cache entry records where the copy sits in the mirror. Rewriting `@loader_path` to an absolute path was rejected: the longer string does not fit in place, and growing load commands needs header padding and moves offsets.
- **ry-built packages** link with `-undefined dynamic_lookup` and an empty `LIBR`, resolving R's API in the flat lookup over every loaded image, so one build loads under ry and GNU R unrewritten.
- **Known gaps:** CRAN's `libRblas.dylib` could pull in GNU R's libR if linked directly (unobserved); `@rpath/../x` with an `@loader_path` rpath is not counted in the climb; the install-name match is reasoned from dyld's source, verified only by the release smoke test on a Mac.

## Windows runtime: `R.dll` and `ry.exe` for windows-gnullvm, linked with lld

**Status:** built, except shipping `R.dll` beside `ry.exe`, which "A release is `ry` alone, with the runtime embedded and the R front end folded in" supersedes.

- **Target `x86_64-pc-windows-gnullvm`,** the mingw-w64 ABI Rtools builds packages with, on the UCRT. `windows-gnu` links `msvcrt.dll` and an MSVC `ry.exe` mixes runtimes too, so `errno`, `FILE *` and `malloc`/`free` crossing the API would hit two C runtimes. Package DLLs import `R.dll` by module name and Windows reuses a loaded module of that name, so naming ry's library `R.dll` binds them with no rewriting. Exports come from `__declspec(dllexport)` in `native.c` plus rustc's, so no `.def` file can drift; the linker writes `libR.dll.a` for `rcmd`. `LibExtern` is `__declspec(dllimport) extern` under `_WIN32`.
- **One platform seam:** every OS call the interpreter makes goes through `crates/interp/src/platform.rs` (Unix and Windows `imp`), so a new libc use (`dlopen`, `umask`, `utimensat`, `nl_langinfo`, `kill`, `getpwnam` and the like) is a Windows build error in one file. Platform conventions (`.Platform`, the platform string, `libs/x64`, the routine table) still branch on `cfg!` where used. The heap's reservation has its own seam (`virtual_memory` in `crates/heap/src/space.rs`: `mmap` or `VirtualAlloc`). `PortableIconv` uses `encoding_rs` behind the interface `Riconv` exports, because Windows has no `iconv(3)`.
- **Windows-only routines** that `utils` and `grDevices` reference at load are in `WINDOWS_ROUTINES` (`builtin_libraries.rs`), chained in only on Windows so Unix keeps GNU R's Unix routine table.
- `getwd()` answers with `/` separators on Windows, and `basename` and `dirname` split at both separators.
- **lld is required:** GNU ld 2.41 emits a `HIGHLOW` relocation per `SECREL32` thread-local offset, so a relocated `R.dll` computes garbage on every `thread_local` access. mingw's `gcc` ignores `-fuse-ld=lld`, so `clang --target=x86_64-w64-mingw32 -fuse-ld=lld` drives the link (`development/design/r-runtime.md`, section 11).
- **A 64 MiB main-thread stack** for `ry.exe` (`crates/ry/build.rs`), because base R's start-up overflows the 1 MiB default. The UTF-8 code-page manifest goes in as a `windres` resource object, so the build needs `windres` (`WINDRES`, else `llvm-windres` or `windres` on `PATH`).
- **Jumps use `_setjmp(buffer, NULL)` and `longjmp`:** with a frame, mingw's `longjmp` unwinds through SEH, and clang's `__builtin_setjmp` resumed `R_UnwindProtect` with garbage locals. A null frame makes `longjmp` a plain register restore, as on Unix.
- **`xerbla_` never jumps over Rust:** it raises an R error for a package's call but returns while the interpreter's `DirectFortran` guard (`linalg.rs`) is alive.
- **Toolchain:** MSYS2's clang64 on the release runner (`msys2/setup-msys2`); Linux cross-builds use host clang and lld over that sysroot, and test packages build with the Rtools ABI from MSYS2's ucrt64 GCC 14 headers and static `libstdc++`, because GCC 16's needs a newer clang than 18.
- **A cross-link for `x86_64-pc-windows-gnu` needs an import library for `synchronization`,** which mingw does not ship; the flake's `preBuild` synthesizes it with `zig dlltool`. A crane build of the dependencies alone links nothing, so it cannot catch a break there.
- Base R and Rtools-ABI package DLLs (jsonlite, data.table, digest, cli, uuid, Rcpp with exceptions crossing R errors) run under Wine with no GNU R binary executed. Open items are in `r-runtime.md`, section 11.
- **Two modules named `R.dll`:** `ry R` loads GNU R's `R.dll` first by full path, so later by-name imports bind to it.
- **Testing under Wine:** install `wine64` (binaries in `/usr/lib/wine`), set `R_HOME` to `Z:<home>` and unpack a GNU R home with `innoextract`; a package without its native library needs a zero-byte `library/<pkg>/libs/x64/<pkg>.dll`; `WINEDEBUG=+seh,+loaddll` shows exceptions and loads, `llvm-objdump --disassemble-symbols` reads a faulting function, and `pip install zstandard` lets Python unpack an MSYS2 `.pkg.tar.zst`.
- `do_system` picks its contract by argument count: seven is the Windows form (no shell), three the Unix form (`sh -c`). `crates/interp/tests/test_system.rs` exercises both on Linux.

## ry ships its own base R, and GNU R through `R_HOME` is the opt-in

**Status:** built.

- **ry pins R 4.6.2** (`tools/base-r/VERSION`, `SHA256SUMS`, `patches/`). `just base-r` runs `ry-rcmd build-base` (`crates/rcmd/src/base.rs`), which replays GNU R's `src/library/Makefile` on ry's interpreter, one `ry-rcmd R --vanilla` process per step as make runs `R_EXE`. The output (`target/base-r`, about 23 MB) is GNU R's layout, so `system.file`, `R.home()`, help and `tools:::` work unchanged. `just package` embeds it zstd-compressed (`RY_EMBED_BASE`, `crates/repl/build.rs`) and `ry` extracts it to `cache/ry/base/<key>` on first use. This removed the only reason ry's runtime needed an installed GNU R.
- **No precompiled bytecode in the lazy-load databases:** ry's interpreter compiles on its own terms.
- **Lookup order** (`repl::base_r`, `interp::system::find_r_home`): a non-empty `R_HOME` is authoritative with no fallback (a typo is an error, not silent R 4.6.2); then `RY_BASE_R`; then a complete tree (`ry-base.txt` present) beside the executable; then the embedded copy. GNU R's search paths and `R RHOME` are only for `ry R` (`crates/repl/src/gnu_r_home.rs`).
- **No GNU R compiled code:** the default packages' routines are ry's (`builtin_libraries.rs`), with a placeholder `libs/<pkg>.so` for `library.dynam`; grid and splines compile from the tarball's C against ry's headers (`compile_native`).
- **The makebasedb check ignores `tools:ry`, `package:std` and `package:ry`,** which every ry session attaches.
- **BLAS and LAPACK come from the system** (Linux `libblas.so.3`/`liblapack.so.3`, macOS Accelerate). Windows has no system LAPACK and no bundled reference build, so linear algebra there needs `R_HOME` naming GNU R (`backlog.md`).
- **Embedded archive format:** sorted entries of a kind byte (0 directory, 1 file, 2 executable), u32 LE path length and path, and for files u64 LE length and bytes, zstd-compressed as a whole and keyed by the FNV hash of the compressed bytes. Paths with empty or `..` parts are rejected on unpack.
- **`R_HOME` naming GNU R 4.3 or later makes ry emulate that release** ("The runtime emulates the GNU R version it loads base R from"). The oracle's expectations are GNU R 4.6.2 output, so CI sets `R_HOME` to 4.6.2.

## The runtime emulates the GNU R version it loads base R from

**Status:** built.

- **Applies when `R_HOME` names a GNU R installation;** the default is ry's own 4.6.2.
- **The version comes from that home** (`interp::system::r_version`: `library/base/DESCRIPTION`, plus `SVN-REVISION` for the revision and date, clamped to at least 89194), and `R.version`, `getRversion()`, the `Built:` check, the serialize version and the generated `Rversion.h` report it. Behaviour GNU R changed between versions is gated with `interp::system::r_at_least(major, minor)` at the one place it differs (strptime in a DST gap, `psignrank` rounding, `%*%` argument counts, `rbind`/`cbind` passing `deparse.level` to S3 methods, `is.atomic(NULL)`, complex formatting, short hex colours, the graphics engine version). Base R 4.6's own code fails its `Built:` check or calls changed internals when the presented version and the internals disagree. Additions (`Tailcall`, `Exec`, `declare`, zstd) exist under every version, because no older base R calls them.
- **Range: GNU R 4.3 and later;** an older home stops `ry run` with an error naming the version. 4.3.3 is checked against its own oracle recording (`ORACLE_R` and `R_HOME` select it); the 4.4 and 4.5 gates were placed from GNU R's sources and NEWS, not by running those versions.
- **The default packages' routine table is the union over versions,** with routines a release does not register hidden from lookup and `getDLLRegisteredRoutines` (`registered_in_release`: stats' `pKolmogorov2x` and `pKS2` before 4.4, `pkolmogorov_two_exact` and `pkolmogorov_two_limit` from 4.4). Positions stay fixed, because routine pointers and image tokens index the table.
- **The C API is R 4.6's whatever the version,** because a newer API is a superset; only the generated version macros follow base. A source-installed package must be reinstalled when the linked base version changes.
- **`Tailcall` re-evaluates in the caller's frame without reusing it:** right result, not constant stack depth (`backlog.md`).

## The C API draws from the interpreter's generator

**Status:** built.

- **`unif_rand`, `norm_rand`, `exp_rand` and `R_unif_index` call `interp::random`,** the `Rf_r*` deviates are `interp::distributions`' drawn through `SessionDeviates`, and `GetRNGstate`/`PutRNGstate` are the interpreter's. A separate C-side Mersenne Twister shared only `.Random.seed` and could not parse other kinds' seeds (after `RNGkind("Marsaglia-Multicarry")`, doParallel's stress test failed with "'.Random.seed' is not a valid integer vector"). No deviate algorithm exists twice, and every kind the interpreter supports works from C. One generator means a C draw and an R draw interleave exactly as under GNU R.
- `R_sample_kind` reports `RNGkind(sample.kind =)`; `R_binom_kind` hard-codes pre-fix BTPE, the only `rbinom` stream ry implements. `RNGkind("user-supplied")` finds `user_unif_rand` and its seed routines through the `find_native_symbol` hook, which libr fills with `dynload::find_symbol` (randtoolbox's WELL tests).

## Native code reads an environment as a cell

**Status:** built.

- GNU R lays an environment out as a cell, and nanonext walks `CAR(env)` because `findVarInFrame` is not API. `CAR(env)` returns the same read-only frame snapshot as `FRAME`, `CDR(env)` the enclosure, `TAG(env)` `NULL` (as `HASHTAB`). `R_NewEnv(R_NilValue, ...)` gets the empty environment, because GNU R stores the enclosure unchecked. mirai and purrr's `in_parallel` depend on this. A snapshot costs one cell per binding on each `CAR(env)`, fine for nanonext's small aio environments but not for large ones.

## The strict load check overlaps the runtime start

**Status:** built, except the whole-root walk, which "The strict load check analyses only the package" supersedes.

- **On ry's runtime the check runs on a thread while the runtime starts** (`run_load` in `crates/ry/src/main.rs`). The loader calls a verdict callback (`ry_loader_set_verdict`, `.ry_await_verdict()` in `runtime.R`) before any statement that is not effect-free, before compiled code, `.onLoad`, `sysdata.rda` and `data/*.R` (ordinary code), after the load, and before a held-back S4 statement that is not effect-free; a refusal exits with the verdict's status. GNU R checks before it starts, because its loader cannot call back into ry. Run serially, a cold typed load cost the check plus the load (4.8 s against 2.4 s untyped on finbench).
- **Effect-free** (`EagerItem.effect_free`, `load_index.rs`): binds a function literal, or `item_effect_free` (`top_level.rs`) holds. Strict mode's `Definition` alone is not enough, because it leaves a declaring call's arguments to R (`setRefClass(..., methods = list(m = { cat(1); function() 1 }))` passes strict mode yet prints). Loading an imported package (NAMESPACE imports, `library()` with effect-free arguments) is allowed before the verdict; this package's own code with an effect never is. A constant may call a user `@pure` function, whose purity is trusted by the `@pure` contract.
- `package_verdict` is `extern "C"`, so a panic exits 101 instead of unwinding into libR. The check thread lowers its priority (`lower_priority`), and the analysis workers inherit it, so on a loaded machine the runtime start keeps its cores; the check has the whole load to finish.

## Every load of a strict package is checked

**Status:** built, except the full analysis on every reload, which "A strict reload re-checks through a persistent salsa database" supersedes.

- **Every plan carries `Plan::strict`** (`PackageIndex::strict`, the same test as `prepare_purity`), and a host checks every plan that changes something: GNU R's `.Call` routine and `ry load --emit-plan` through `load::strict_value`, ry's runtime through a host callback (`loader_backend::set_check`, registered by `repl::set_package_check_on_ry_runtime`), because the loader cannot depend on the CLI's analysis. A refusal turns the plan into an error before R sees it, so the old bindings stay.
- **Strictness is decided per package root from its own `ry.toml`** (`load::strict_config`): GNU R's routine reads it directly, and ry's runtime asks a second host callback, the strict query, before each plan. Taking it from the command's configuration would let a lax script `load_all()` a strict package unchecked.
- The first plan of a session with a verdict (`ry load` on ry's runtime) skips the callback, because the concurrent check already covers it and checking again would serialize what the verdict overlaps.

## Lazy definitions keep source references

**Status:** built.

- A plan's files become per-file handles (`FileSource`) holding the source and the `keep.source` option. The first force from a file that keeps source builds its shared `srcfile`, so an unused file costs nothing.
- The definition is reparsed alone, padded to its line and column, and lowered with a line offset (`interp::lower::lower_node_at_line`), so srcrefs match GNU R's (bytes and character columns) without parsing the whole file. Without a srcref the handle's tree is used directly.

## GNU R's public C API is a checked list, not a measured subset

**Status:** built.

- **`crates/libr/tests/headers/r_api_names.txt` lists every name GNU R's public headers declare and its `R.dll` exports,** plus a section of non-header names packages link; the test fails when ry's headers miss one or libR does not export it. Checking only what ry declares let a name missing from both pass (diffing GNU R 4.5's `R.dll` found `R_findVar`, `R_findVarInFrame`, `R_GetVarLoc*`, `Rf_formatReal`, `R_AllocStringBuffer`, `utf8clen`, `Rf_mbrtowc`, `R_MissingArgError`). Declared-only names are excluded, because they vary by platform and are often macros. Regenerate with `RY_GNU_R_INCLUDE`, `RY_GNU_R_EXPORTS` and `RY_UPDATE_API_NAMES=1` (`development/testing.md`). The 265 `R.dll` exports ry lacks are GNU R internals and RGui entry points no package in the measured closure uses (`tools/research/data/symbols.csv`). Front-end headers (`ga.h`, `graphapp.h`, `iconv.h`, `libintl.h`) are excluded: packages get gettext's from Rtools, the rest is RGui.
- `R_varloc_t` is a struct of one pointer, as in GNU R, so `R_findVarLoc` returns the binding cell by value through the same ABI.
- `VECTOR_PTR` is declared without `NORET`: ry returns the data pointer, and `noreturn` would let a compiler drop the code after the call.
- `NORET` is `[[noreturn]]` only for C23; C++ gets `__attribute__((noreturn))`, because clang rejects the standard attribute in Rcpp's `inline void NORET stop(...)`. A package's own `NORET` wins.
- **Test packages compile against ry's headers and GNU R's,** so their C must be valid for both. GNU R 4.6 does not declare `ATTRIB`, `DATAPTR`, `FRAME`, `fdhess` or the contents of `R_ext/PrtUtil.h` and hides `ALTINTEGER_SUM`, `LOGICAL_IS_SORTED` and `rexitc`: declare an undeclared entry with a parenthesized name (`SEXP (ATTRIB)(SEXP x);`, valid whether or not a macro exists) and reach a hidden one through a weak reference or an exported path. ry's libR keeps exporting the hidden entries for old binaries.
- gcc 14 makes an implicit function declaration an error; on older compilers use `-Werror=implicit-function-declaration -Werror=int-conversion -Werror=incompatible-pointer-types` for the same verdict.

## Windows `R CMD INSTALL`: the home's `Makeconf` decides the platform

**Status:** built, except the front ends as plain copies of `ry-rcmd.exe`, which "A release is `ry` alone, with the runtime embedded and the R front end folded in" supersedes.

- **`rcmd::Platform::of_home` reads `SHLIB_EXT` (and `DYLIB_EXT`) from the home's `etc/Makeconf`,** so the home is the single source of truth and a Linux test can drive Windows conventions with a Windows `Makeconf`. From it follow the suffix, `libs/x64/`, `R_ARCH` and build-file precedence (`.ucrt`, then `.win`, then portable `Makevars`; `configure` and `Makefile` have no Unix fallback, as in GNU R). `makeconf` writes the Rtools ABI: `-shared -static-libgcc`, no PIC flags, forward slashes, `-L"$(R_HOME)/bin/x64" -lR`.
- **The SHLIB is `<package><ext>`,** the name unchanged, as GNU R links it, and every shared object `src/` holds is installed, so one a Makevars renames is found (data.table's renames `data.table.dll` to `data_table.dll` itself).
- **Windows home layout:** `R.dll` and `libR.dll.a` in `bin/x64/`; front-end `.exe`s pick their role from their file stem (Windows runs no `#!` scripts). Directories are symlinked where allowed, else copied; Wine reports a successful symlink without creating it, so the link is checked.
- `ry-rcmd` uses `crates/ry/build.rs` as its build script, so both executables get the 64 MiB stack and the UTF-8 manifest from one place; the manifest is `include_str!`-ed so the script works from either package.
- **Verification limit:** MSYS2's `gcc`, `make` and `sh` cannot run under Wine in the agent environment, so the six test packages were installed cross-hosted and only loaded under Wine; the release smoke test is the first native Rtools run.

## The Windows session's native encoding is UTF-8

**Status:** built.

- `init_locale` swaps a non-UTF-8 codeset for UTF-8 on Windows (`use_utf8_codeset` in `crates/interp/src/system.rs`), as GNU R from 4.2 does under its manifest, because the manifest is ignored in places (Wine). `init_locale` first sets every category from the environment. Locale names spell it `.utf8`, GNU R's spelling, including in the composite `LC_ALL` string (Wine's CRT says `.65001`). An explicit `Sys.setlocale` to another code page is honored.
- `.Internal(Sys.which)` exists because GNU R's Windows base code calls it (Unix base R uses `command -v`); it searches `PATH` with `PATHEXT` and returns forward-slash paths.

## A load holds back S4 definitions and caches its index across processes

**Status:** built.

- **A package's S4 definitions are held back on ry's runtime** (`loader::deferred_s4`, `.ry_force_s4` in `runtime.R`): each name they bind becomes a placeholder promise, and the class-cache miss and `R_getGeneric` hooks (`interp::methods`) force them. A force during the load runs only definitions before the current statement, so order is GNU R's. Unit 0 (classes, unions, coercions, methods on other packages' generics) is all-or-nothing, because S4 registration is global state no per-name promise reproduces. The rules that keep it eager are in `reference/package-loading.md`, "S4 definitions", pinned in `crates/loader/tests/reload/deferred_s4.test`. GNU R's backend never defers (no hook); `options(ry.load.lazy_s4 = FALSE)` turns it off. A load that keeps S4 eager reports the first definition that forces it (`deferred_s4` returns `Err(reason)`, printed under `ry.load.verbose`).
- **Measured** on the seeded S4-heavy package (`just bench-s4-load`, `tools/loader-bench/s4_package.py`, 2,863 S4 definitions): eager, the first load took 5.7 to 5.9 s under `ry run`, 92% of it S4 (setMethod 2.8 s, setClass 2.3 s, setClassUnion 0.4 s, setGeneric 0.2 s), about 1.3x GNU R per call (`pkgload::load_all` 6.3 to 7.1 s, `library()` of an installed copy 0.6 s). Held back, the load takes about 0.27 s. On the 326,000-line bench package eager S4 cost about 300 ms.
- **S4 calls are matched as R matches arguments** (`match_arguments` and `s4_formals` in `crates/semantics/src/load_index.rs`: exact names, unique partial names, then positions), because reading names alone missed `setAs(from =, to =, function(from) ...)` and `setClassUnion("U", mem = ...)` and kept whole packages eager.
- **Methods on other packages' generics are held back when their signature names one of the package's classes** (nearly every S4 package defines `show`). Each kind of generic needs its own door, because forcing on every `initialize` or `length` dispatch would fire during the load (`.ry_foreign_generic_kind`, `.ry_defer_s4`):
  - a generic function (`show`, `initialize`, any `standardGeneric`): one placeholder per dispatch label in `.MTable`/`.AllMTable`, labelled as `methods` labels them (`.ry_s4_label`: `matchSignature` padded with `ANY` to `.SigLength`), so dispatch for another class never forces; `getMethod`, `existsMethod` and dispatch for a held-back class read the label and force. A force during the load from a method after the cursor reads the label with `get0`. A signature longer than `.SigLength` stays eager. Another package's `setMethod()` on the generic reads every label and forces. Forcing restores each table first (`.ry_restore_s4_labels`).
  - a primitive: `s4_dispatch` forces only when a non-promise S4 argument's class is the unit's (`interp::methods::Lookup::Primitive`), which is exact.
  - a not-yet-generic function from another package (`as.list`, `toString`): `setMethod` would make it generic in the package namespace, so the name and `.__T__name:pkg` become placeholders. One of the package's own functions keeps the unit eager.
  - group generics keep the unit eager: members dispatch through the group's table.
- **Class unions are held back, mixed (`c("Cls", "NULL")`) or foreign (`c("numeric", "NULL")`),** although a union naming other packages' classes edits each member's global class-cache entry (`.getClassesFromCache("numeric")@contains`, not `methods::.__C__numeric`). Run in place, each cost about 10 ms (150 ms for 15 unions on a real package), and a mixed one kept the whole unit eager. Holding back is sound because every membership question names the union (`is(NULL, "U")`, a method for `U`) and so forces, and `possibleExtends` falls back on `@subclasses`. Only the reverse listing (`extends("NULL")`) is wrong until the force (documented under Limits). Forcing on any member-class lookup was rejected: `methods` calls `is(NULL, ...)` constantly (`setOldClass()` at load), so nothing stayed deferred. A restore adds unions to members in definition order, the order `extends()` lists after an eager load. A foreign-generic method for a mixed union keeps the unit eager, because dispatch on `NULL` can skip class lookup.
- **`setAs` is a method on `coerce`** (and on `coerce<-` with `replace`; `S4Definition::Coerce`), held back with signature `(from, to)` when either side is held back; `as()` reaches it through `.quickCoerceSelect`, which reads the label placeholder. A coercion between other packages' classes runs in place.
- **No `cacheMetaData` while held back,** because it reads every `.__C__`/`.__T__` binding and would force the unit; each definition caches what it defines and `.ry_force_s4` ends with the call.
- **Each package generic is its own unit; there is no S4 metadata cache** (`force_deferred_unit`, `run_unit` in `interp/src/methods.rs`). A generic whose methods name only earlier classes runs on its first lookup or dispatch; one a class definition reads stays in unit 0. Forcing a lazy generic runs unit 0 first and drops that generic's placeholders before unit 0's `cacheMetaData`, since the placeholder may be the promise that started the force. A generic whose methods name a later class also stays in unit 0. The native `cacheMetaData` skips generics still held back, and a lazy unit ends with `ryS4CacheGeneric` (`.ry_cache_generic` in R) for its one generic. On `target/s4heavy` unit 0 is about 416M instructions and lazy generics cost nothing until called. A persisted metadata cache (a lazy-load database keyed on definition text, build id and import versions) was rejected: derived state across processes, it needed every definition to run together, and a miss still paid the whole run.
- **Cross-checked against eager** as whole processes, because laziness changes when definitions run, not what they compute: `a_held_back_method_for_another_packages_generic_is_there_when_that_generic_needs_it` in `crates/ry/tests/test_load.rs` runs seven first-use probes (`show`, primitives, `as.list`, `print`, union membership, `initialize` with `callNextMethod`, `readRDS` objects) lazily and eagerly, and `tools/loader-bench/s4_check.R` compares lazy, eager, cache-hit and GNU R results.
- **The load index is cached per package root** (`index_cache`, `read_index_cache`, `write_index_cache` in `loader.rs`), keyed by xxh3-128 over a build id and every input (strictness, DESCRIPTION, NAMESPACE, each path and text), because a cold parse and derivation cost about 280 ms on 4 cores. Package-level, not per file, because the per-file salsa queries chain off the parse and cannot take a stored result without re-parsing; a hit skips analysis and the runtime parses each item's text on first force. The build id hashes the analysis crates' sources (`crates/loader/build.rs`), so no hand-maintained version is needed. A hit warms the salsa parse on a cancellable clone for the first reload.

## The strict load check shares the check record

**Status:** built, except the whole-root walk, which "The strict load check analyses only the package" supersedes.

- **The load check is `ry check`'s analysis (`cli::analyze_project`) asked about `R/` alone, sharing its record,** so a package `ry check` or the editor just checked loads without re-checking. A recorded file's `diagnostics` is `None` when the writing run did not check it, and `check_cache::plan` treats it as stale. Kind isolation makes this sound: a name only a test defines stays unresolved in `R/`, and a test's `library()` does not attach in `R/`. A separate fingerprinted verdict beside the check record recorded the same fact twice and was invalidated by any edit.
- **A verdict-only pass was rejected:** a lint configured as an error counts (`is_strict_error`), so lints cannot be skipped, and what a verdict could skip (rendering, annotation agreement) is about 0.3 of 5.3 CPU-s on typed finbench.
- **Measured on typed finbench** (4 cores, release): a load after `ry check` takes 0.55 to 0.64 s, the same as untyped, and an edit inside one body rechecks one file. A verdict-cold load is analysis CPU (parse about 1 s, item HIR 0.9 s, folds 0.5 s, type check 2.2 s), which belongs to the type checker's own speed.

## `ry test` is `ry load` plus testthat, with affected tests first

**Status:** built.

- **`crates/ry/src/test.R` calls `testthat::test_dir(load_package = "none", package =)` against the namespace `load_all()` built,** so testthat's reporters, helpers and setup behave as under `devtools::test()` with identical output for CI, without its full reload (about a minute on finbench before every run). Nothing in the reporter path is reimplemented.
- **`--watch`** waits on OS notifications for `R/`, `data/`, `src/`, `inst/`, `tests/`, `DESCRIPTION` and `NAMESPACE` (`loader::watch`, `Loader::wait_edits`; `_snaps/` is ignored because testthat writes there), reloads, and reads the touched names from `.ry_packages[[root]]$changed` (`NULL` after a full load or one that ran code). `Loader::callers` closes them over reverse mentions and reads; a test file is affected when it changed or its `all.names()` meet the closure. Affected files run first; a changed helper or setup file, or `NULL`, reruns everything.
- **No test index in the loader:** parsing tests with `all.names()` and caching by stamp costs a few milliseconds per file and keeps the loader ignorant of testthat.

## `ry test --jobs` runs test files in testthat's workers, loaded by ry's loader

**Status:** built.

- **testthat's parallel runner is the scheduler,** so reporters, snapshots and output order are its own. `--jobs N` sets `TESTTHAT_PARALLEL` and `TESTTHAT_CPUS`/`Ncpus` (`N > 1` forces workers, `1` forces one process); otherwise testthat's switches decide, with every core (`thread::available_parallelism`) unless `Ncpus` or `TESTTHAT_CPUS` is set. The count is set only for runs that start workers, so tests in one process see the settings they see under devtools.
- **Workers load with ry's loader:** `load_workers_with_ry` in `test.R` rewrites `testthat:::queue_setup` so the worker hook runs `.Internal(ryLoaderAttach()); load_all(dir, quiet = TRUE, helpers = FALSE)` instead of `pkgload::load_all()`, which loaded eagerly, needed pkgload and every import, and ignored `ry.toml` (finbench's workers failed at start-up, and a declared-parallel package ran under different checks than the main process). testthat sources helpers in each worker itself, which is why `load_all()` has pkgload's `helpers` argument. testthat offers no hook; if a future testthat has no such call, the driver says so once and leaves pkgload. Under `--runtime gnu-r` workers keep pkgload. Runtime switches pass in `RY_RUN_FLAGS` (`rcmd::front_end::PROJECT_FLAGS_VARIABLE`), which ry's `bin/R` reads and the hook unsets, so tests never see it and grandchildren run with R's defaults.
- **Workers are opt-in for an undeclared package,** because its test files may share state (temp names, ports, options). `--jobs N` is the explicit request; `Config/testthat/parallel` is the package's promise.
- **`--profile-summary` forces one process,** since it measures the process that writes it; with `--jobs N > 1` it is an error.
- **Measured on finbench** (501 test files, 43,011 expectations, 4 vCPUs): 251.5 s in one process, 122.5 s with 2 workers, 84.3 s with 4 (2.98x). CPU time rises from 249 s to 300 s, each worker binding its own lazy definitions.

## A C API argument that may be a C `NULL` is typed `Option<Sexp>`

**Status:** built.

- `Sexp` wraps `NonNull`, so `Some(NULL)` and `None` share a bit pattern and a C `NULL` silently became "absent". Any parameter or result C may legitimately pass as `NULL` is `Option<Sexp>` in the `extern "C"` signature (same ABI), with `Handout for Option<Sexp>` in `boundary.rs`. Rcpp's `Environment::exists()` and `find()` call `R_getVarEx(sym, env, inherits, NULL)` on R 4.5+, so a `NULL` default read as "no default" made every Rcpp assignment of an unbound name fail; httpuv builds its request environment that way, which broke every HTTP request and with it plumber. `get_var` returns `Option<Sexp>` for "unbound", which `R_getVar` turns into the error and `R_getVarEx` into the caller's default. `rytest_get_var` pins it (`crates/libr/tests/test_native.rs`).

## Source edits arrive by OS notification, ordered by a barrier file

**Status:** built.

- **One `notify` watcher per loaded package** (`crates/loader/src/watch.rs`) files relevant paths into an inbox. `SourceWatcher::take` writes a barrier file into a private watched directory and waits for its event; one watcher delivers in order, so every earlier edit is then in the inbox and a reload depends on the order of writes, not thread timing, keeping the reload fuzz deterministic. The stat fingerprint is the fallback when notifications are unavailable or lost.
- **A `src/` edit reloads and rebuilds.** `dyn.unload` retires the library into `unloaded_dlls` (kept mapped, so stale pointers cannot crash), and the next `dyn.load` maps a fresh copy from a temp directory, because `dlopen` returns the open handle for a path it has seen.

## `ry document` is roxygen2 with block and topic caches and linear topic ordering

**Status:** built.

- **`crates/ry/src/document.R` runs `roxygen2::roxygenise(load_code = )` with ry's `load_all()`,** so output is roxygen2's byte for byte. For the run it swaps `roxygen2:::parse_package` for one caching each file's tokenized blocks by md5 (keyed by roxygen2 version, R version and roxygen options; `package_cache_path(root, "roxygen")`), and replaces roxygen2's quadratic steps (on a 22,000-topic package `RoxyTopics$topo_order`, run 15 times by `@inherit` processing, and `RoxyTopics$add` dominate) (`roclet_process.roclet_rd`, `RoxyTopics$topo_apply`, `topo_order`, `drop_invalid`) with linear ones, each only when the installed function's deparsed body equals the embedded original, so another roxygen2 version runs unpatched. Everything is restored on exit. `keep.source = TRUE`, because roxygen2 reads R6 methods' srcrefs (as pkgload's load does).
- **Topic cache** (`<cache>-topics`: each topic's finished Rd text, aliases, family and `@inherit*` sources, plus each block's namespace directives), because a one-file edit on finbench still took about 2 minutes of per-topic work. A block is clean when its file's md5 is unchanged and it is trackable (a `name <- function` block whose loaded object's srcref names the same file and line, `defined_here`; no dot in the name, because S3 dispatch is not tracked; none of `uncacheable_tags` such as `@eval`, `@template`, `@example`, `@describeIn`). A clean block skips `block_find_object`. The rebuilt set is every topic touching a dirty or untrackable block plus the package topic, closed under reverse `@inherit*` edges and family co-membership; the blocks needed to rebuild it add the forward closure. Everything else is emitted from the cache through a stub `roclet_output`'s `write_if_different` skips when size and mtime match. `namespace_imports` is cached per file md5 (`<cache>-imports`).
- **Measured on finbench** (`generate.py --roxygen`, 22,000 Rd files): 385 s cold, about 11 s for a no-op rerun, 20 s after a one-file edit (50 topics rebuilt), against 1,447 s and 1,280 s for `devtools::document()`, byte-identical on both runtimes, R6 included. Most warm time is `readRDS` of the 29 MB compressed block cache (about 3 s).
- **Dependency tracking is in R and conservative:** rather than reimplement roxygen2's merging, the cache only decides which topics roxygen2 rebuilds, over-approximating; generated blocks, S4, R6 and data are always rebuilt. The block cache stays compressed, because uncompressed it is 214 MB.
- **Known staleness:** a cached block keeps markdown links resolved at parse time, and a cached topic keeps documentation inherited from another package.

## A strict reload re-checks through a persistent salsa database

**Status:** built.

- **`preflight::PACKAGE_CHECKS` keeps one `cli::PackageCheck` per canonical package root for the process,** and `check_package_code` updates only changed inputs, because a from-scratch `analyze_project` over `R/` took about 2 s on the 326,000-line bench package. It lives in the ry binary, because the loader runs inside libR on ry's runtime and cannot link the analysis.
- **The first check goes through `analyze_project` with `CacheUse::ReadWrite`** to reuse `ry check`'s record; that database replays findings it never derived, so it is not kept (`PACKAGE_CHECKS` holds `None`), and the first reload builds the kept one.
- On the typed finance bench package in a debug build, a one-file reload with only the record takes about 22 s; with the kept database the first reload takes about 20 s and later ones under 1 s, load included.
- `namespace_spec` is a tracked query shared as `Arc<NamespaceSpec>`, and the reload fingerprint comes from the plan's read stamps, so a one-file reload plans in about 3 ms and prepares in 4 to 5 ms (re-joining every file's names cost about 28 ms). Remaining costs: `backlog.md`, "Open: the package loader".

## Compiled code under `ry load` builds with ry's own R home

**Status:** built, except that `ry-rcmd` sits beside libR, which "A release is `ry` alone, with the runtime embedded and the R front end folded in" supersedes.

- **`.ry_compile` calls `.Internal(ryLoaderCompile(...))`,** running `rcmd::compile` in process against a home from `rcmd::cached_home`, so no pkgbuild and no GNU R `libR` link (pkgbuild through `callr` ran GNU R's `bin/R`, and a package without pkgbuild could not load). The home is under the user cache (`ry/home/<hash>`), keyed by ry's version, libR's and the front end's stamps and base, built from headers embedded at build time (`embed_headers` in `crates/ry/build.rs`), so an installed ry needs no source tree. libR is hard-linked in (copied only across file systems), so a home costs about 300 KB beyond the shared libR inode.
- **Pruning:** processes hold a shared `flock` on a home's `.lock`; a new build prunes only homes whose recorded inputs (`.libr`) no longer hash to their name and no process holds, renaming to `.removing-*` first so a half-deleted home is never found. Pruning every unheld home was rejected: two builds sharing a cache would delete each other's homes. Stale homes each pin an old libR inode (about 190 MB), which is what grew the cache.
- **`dyn.unload` is real** (`libr::dynload::unload_library`): it calls `R_unload_<pkg>` and keeps the library mapped in `unloaded_dlls`. A reload copies the rebuilt library to a fresh temp directory under its own name (which names `R_init_<pkg>`), since `dlopen` reuses an open path.
- **A registered routine whose R name is already bound is skipped** with GNU R's `loadNamespace` warning, and a reload removes only the symbols the previous load bound and that are still `identical`, so a user function with a routine's name survives. `unloaded_dlls` keeps the library mapped because R objects may still hold pointers into it.
- The GNU R runtime keeps pkgbuild, matching `load_all()`.

## The strict load check analyses only the package

**Status:** built.

- **A run whose requested files are all package files analyses only package files** (`ry check R`, the load's check), because tests and scripts read the namespace but add nothing to it. Building them was about a quarter of the derive work on the benches; the verdict-cold typed load on finbench (60K lines) went from 0.52-0.55 s to 0.43-0.45 s, the rest of the gap being checker CPU.
- **The record keeps unanalysed files:** an unchanged one keeps its recorded content, surface, findings and imports used; when one changed, it gets no surface (`RecordedFile.surface` is an `Option`) and every unanalysed file is marked unchecked. `check_cache::plan` compares surfaces only for analysed files. `NAMESPACE` findings are recomputed on every analysis run (a few milliseconds) from the analysed files' imports used, and a replay recomputes them when package and all-file imports differ, since the record does not say which kind of run wrote it.
- `preflight::check_package` frees the analysis on its own thread (85 ms on typed finbench at 60K lines, which blocked the loader), and the check record's build identity is hashed on a thread overlapping the walk.
- **Attach calls cross kinds of run.** A test's `library()` of a package no stub describes tolerates every bare read, in the package too, and a package-only run never sees it. The record keeps each file's attached namespaces and the set its findings were computed under, and `check_cache::plan` checks everything when the analysed files' set differs in a namespace that can change a finding (`metadata::attaching_changes_findings`: not the own package unless the attach switches its shipped stubs on, not a stubbed namespace active without the attach). The server's set counts only the files it loaded, whose attach calls are the ones its analysis sees. A test's `library(testthat)` and `library(<own package>)` change nothing, so `ry load` and `ry check` still share the record. Splitting the attached set per layer, so tests could not reach package findings at all, would need a stub library per layer; not taken.
- The seeded simulation in `cli.rs` mixes package-only runs into its edits; `tests/scenarios/check-cache-package-only.scenario` pins the three traps found (a test helper edited between a load and a full check, the `NAMESPACE` verdict across kinds of run, and a test's attach of an undescribed package).

## A reload diffs S3 registrations per table entry

**Status:** built.

- R registers `S3method` directives in order into one entry per generic and class, so the last wins. `loader::diff` maps each pair to its winning directive and registers or unregisters only winners; a set comparison re-registered rebound losers and dropped entries that still existed (found by `test_reload_fuzz`, pinned in `crates/loader/tests/reload/edits.test`).
- `keeps_names` also requires every name a rerun statement may write to be bound already; a statement whose writes are unknown rules it out.

## `R CMD` make variables expand as GNU make expands them

**Status:** built.

- **`make_assignments` and `expand_make` (`crates/rcmd/src/makeconf.rs`) follow GNU make 4.3:** `=` expands at use, `:=` at assignment, `?=` assigns once, `+=` keeps the original flavour and adds no space to an empty value, backslash-newline runs collapse to one space, `$$` is a dollar, a lone trailing `$` stays, and a self-reference expands to nothing at the inner reference (make errors there). No depth bound, so a non-cyclic doubling chain expands exponentially, exactly as under make. `R_HOME` is ry's home whatever the makefile assigns, because R passes it on make's command line.
- **Why:** a `Makevars` is written against GNU make, and any difference changes compiler flags silently. `make_expansion_agrees_with_gnu_make` checks generated makefiles against real `make`, includes too.
- **Includes** (`include`, `-include`, `sinclude`) read each file through a caller-supplied `read` at that point; an unreadable file is skipped, and nesting stops at 32 so a self-include ends. GNU R's `Makeconf` includes `vars.mk`.

## BLAS and LAPACK come from the system, then from GNU R's Makeconf

**Status:** built.

- **`library_candidates` (`crates/interp/src/linalg.rs`) tries, in order:** the sonames (so `LD_LIBRARY_PATH` is the user's override and a distribution's R gets what it links); Accelerate on macOS; each library `BLAS_LIBS` (for LAPACK, `LAPACK_LIBS` then `BLAS_LIBS`) names, in its `-L` directories then by soname; `R_HOME/lib/libRblas`/`libRlapack` last, because R's own libraries leave `xerbla_` to GNU R's libR. A candidate must define every routine of the table, so a BLAS tried for LAPACK is passed over. Opening LAPACK loads BLAS first, so a LAPACK whose BLAS sits in its own directory resolves it by soname. A failure lists every place searched and the fix. NixOS has no global library path, which is why the Makeconf step exists. `La_library()` and `extSoftVersion()["BLAS"]` report the file the routines came from. The order is pinned by unit tests in `linalg.rs` and `rcmd.rs`.
- **One Makeconf reader:** `crates/rcmd/src/makeconf.rs`, which `interp` includes by path. `Toolchain::for_base` on Linux keeps the default compilers but takes GNU R's `BLAS_LIBS`/`LAPACK_LIBS` (`Toolchain::with_makeconf_libraries`), so compiled packages link what the interpreter loads.
- **ry carries the last resort on Linux:** `tools/base-r/lapack.sh` builds GNU R's reference `libRblas.so` and `libRlapack.so`, embedded with the runtime and linked into the home's `lib/`. They define `xerbla_` in libRblas (so libR does not interpose on a system BLAS's handler) and link no Fortran runtime (`tools/base-r/fortran.c` supplies `_gfortran_concat_string` and `xerbla_`), because libgfortran's static archive is not PIC on Ubuntu and a system without LAPACK rarely has the shared one. The smoke test hides the system's libraries in a user and mount namespace and runs `solve()`. libR links with `-z defs`, so a new undefined runtime dependency is a link error.
- `open_reference_blas` (`crates/libr/src/dynload.rs`) opens them with `RTLD_LOCAL` before the first package `dlopen`, so a package linked against GNU R's copies, with `NEEDED libRblas.so` (quadprog, PerformanceAnalytics), resolves by soname without the symbols interposing on a system BLAS.
- An OpenBLAS result can depend on its buffer's address modulo 16, which is why `linalg.rs` places every buffer through `Placed`. Haswell and newer kernels hide the dependence; `OPENBLAS_CORETYPE=Zen` reproduces it; `RY_TEST_REFERENCE_LIBRARIES=1` runs the tests against the reference libraries.

## `ry test --watch` selects callers through S3 dispatch

**Status:** built.

- `Loader::callers` (`crates/loader/src/loader.rs`) walks reverse mentions from the changed names, and every name reached also reaches the generics its `S3method()` registrations name and every dotted prefix (`format.shape` reaches `format`), because a test calls the generic, never the method. The dotted prefix covers methods registered by naming convention or `registerS3method`; the registration edge covers `S3method(gen, cls, fun)` with any function name. `Plan::changed_names` adds the function and generic of each registration a reload made or removed. `check_reload` (`crates/loader/src/testing.rs`) models both; the `test-watch` scenario pins a three-argument registration.

## A release is `ry` alone, with the runtime embedded and the R front end folded in

**Status:** built.

- **Embedding:** `crates/repl/build.rs` embeds the file `RY_EMBED_RUNTIME` names, zstd-compressed (level 12 in release, 1 otherwise, since a debug runtime is about a gigabyte), plus `libR.dll.a` on Windows, keyed by a hash of the compressed bytes; `just package` sets it. A build-time variable, not a cargo feature or artifact dependency: a plain `cargo build` stays fast and no unstable `bindeps` is needed. A section written after linking (Bun's approach) was rejected: it needs per-platform object rewriting, while `include_bytes!` leaves macOS signing and loading untouched.
- **Lookup order:** `RY_LIBR`, beside the executable, `deps/`, `../lib/ry/`, then the embedded copy (`repl::ry_runtime_library`).
- **Extraction** into `ry/runtime/<key>/` of `XDG_CACHE_HOME`, `%LOCALAPPDATA%` (Windows) or `~/.cache` (`rcmd::cache_directory`, shared with the R homes): each file is written under a per-process name and renamed into place, the library last, so a complete library means a complete directory and concurrent first runs both succeed (on Windows a failed rename over a loaded, complete library counts as success). Errors name `XDG_CACHE_HOME` and `RY_LIBR`, plus the `noexec` hint on load failure.
- **Multi-call:** `rcmd::front_end` runs when `ry` starts as `ry-rcmd` or, on Windows, as a home's `R.exe`, `Rscript.exe`, `Rcmd.exe` or `Rterm.exe`. The home's scripts exec `bin/exec/ry-rcmd`, a symlink to the front end, and the Windows front ends are hard links where possible, since `ry.exe` carries the runtime. `rcmd::runtime_home` uses an `ry-rcmd` beside the runtime when there is one and the running executable otherwise; the `ry-rcmd` binary remains for development builds and tests. A home records its runtime and front end in `.libr`, and pruning removes only homes with both stale, so two `ry` copies sharing one runtime never remove each other's home. Staging names carry `rcmd::staging_suffix` (pid plus clock nanoseconds), since containers sharing a cache volume often share a pid.
- **Tests:** `crates/ry/tests/scenarios/single-binary.scenario` runs `single-ry`, which the harness builds with `RY_EMBED_RUNTIME` in a directory of its own; `tools/smoke-test.sh` checks an archive holds `ry` alone, that the runtime was extracted, and (with `RY_SMOKE_PACKAGES`) that `ry install` compiles C code.

## S-era package sources are R sources

**Status:** built.

- `R CMD INSTALL` collates `R/*.R`, `.r`, `.S`, `.s` and `.q`, so S-era packages (Hmisc, VGAM, boot, cluster) define exports there; reading only `.R` reported 627 undefined exports on a fresh 99-package sample. `cli::is_package_source` is the one predicate for the CLI and the server; a `.s` file outside `R/` (`src/fast.s`) is assembler, not R.

## Start-up and imports run GNU R's code, with its hot loops native

**Status:** built. The heap image weighed here for base alone exists ("The start-up image is build output of the base tree, and every start re-reads the environment").

- **The problem:** a warm `ry load` of a large S4 package spent most of its time before the package's code ran: process start (base, `methods` and default packages, about 130 to 170 ms) and the imports' `loadNamespace` (about 230 to 300 ms for lubridate, data.table and xts), both GNU R's own R code.
- **Chosen: native hot loops behind GNU R's own code.** The runtime replaces one base R function at a time with a copy whose per-item loops run natively, guarded so it applies only to the exact GNU R code it was copied from.
- **Rejected: a fork server.** Not exact: the child inherits the server's environment, profile, `tempdir()` and library paths unless replayed, thread pools started at load (data.table's OpenMP) drop to one thread after a fork, and an updated package leaves the server stale. A fork server would cut start plus imports to a few milliseconds. An image of imports was rejected because the interpreter's own state (symbol and string tables, `Globals`, builtin and routine tables, connections, external pointers into DLLs) lives outside the heap, each needing a serializer, and an import with compiled code cannot be imaged.
- **`registerS3methods`** (`crates/interp/src/register_s3_methods.R`, `do_register_s3_methods` and `do_local_s4_generics` in `envir.rs`, `install_register_s3_methods` in `session.rs`) performs GNU R's steps in GNU R's order (generic lookup, `genfun@default` for an S4 generic, table creation, the overwrite check with `identical()`, the `delayedAssign` promise in the environment `assignWrapped` makes) and calls back into R for every step that can signal or dispatch (`asNamespace` of an unloaded package, the not-found error, `methods::is` on an object, `@default`, creating a table, reading an unforced binding, `environmentName`). Its per-method `tryCatch`, `methods::is` and `get0` cost 41 ms of the default packages and 34 ms of the imports (37 ms each on GNU R); natively 8 ms each. `methods::is(f, "genericFunction")` is skipped for a value with no class attribute and no S4 bit, where it is always `FALSE`.
- **The hash guard:** the copy follows R 4.6.2, so it replaces the installed function only when an FNV-1a hash of its formals and body (symbols and strings by text, attributes ignored) equals 4.6.2's; another release keeps its own function, and a mismatch on the bundled 4.6.2 is an error (the copy drifted). `RY_PRINT_FAST_PATH_HASHES` prints the hash. The hash skips a nested `function` call's srcref element, because a parse fills it and a saved namespace omits it, which made the pristine function hash differently in `build-base` and in a built home, and pinning the second broke every clean build. A built base namespace saves the replacement, which the guard recognises by its own hash. `stable_hash_ignores_nested_function_srcrefs` in `session.rs` pins the rule; CI's `just base-r dev` is the end-to-end check.
- **Pinned by** `namespaces_register_s3_methods_as_gnu_r_does` in `crates/libr/tests/test_install.rs`, comparing output under ry and GNU R across every branch (known generics, another package's generic, an overwrite and its message, local plain and S4 generics, delayed registration) and asserting the replacement is in place under the bundled release.
- **Next by cost was `methods::cacheMetaData` of the imports** (about 150 ms, mostly lubridate), dominated by `.checkGroupSigLength` rescanning the whole group tree for every group member (1.2 ms per call on ry, 0.8 ms on GNU R); it runs natively ("Installed packages load through native fast paths, not an image").

## Installed packages load through native fast paths, not an image

**Status:** built.

- **No per-package namespace image** written by `ry install`. Measured on `library()` of 17 benchmark dependencies (31 namespaces) at about 282 ms (GNU R 290 to 350 ms): lazy loading fetched only 763 of 7,570 objects, about 40 ms, while an eager image would cost 30 to 45 ms to load, so it saves nothing there; the rest was setup an image would have to replay or invalidate (S4 `cacheMetaData` 66 ms, S3 registration 26 ms, `.onLoad`).
- **Each dominant setup step runs natively behind a hash-guarded prologue on GNU R 4.6.2's function,** so changed packages, edited libraries or other releases need no invalidation: `.checkGroupSigLength` (`ryS4CheckGroupSigLength` in `methods_core.rs`, which rescanned the whole group tree per member), `registerS3methods`, and utils' `osVersion` (`ryOsVersion` reads `/etc/os-release` instead of spawning shells). `RY_PRINT_FAST_PATH_HASHES=1` prints the hashes.
- **The heap tenures every object once a start-up image loads** (`Heap::promote_young`), because the first minor collection otherwise traced all 10 MB of them (5.3 ms) and each intermediate collection again, and with the heap holding little but the image, tenuring everything keeps "tenured points only at tenured" without tracing.
- Lazy-load records inflate through one reused inflater straight into the output, and decompressing file connections are buffered, which halves `readRDS` of a gzip file.
- **Result:** `library()` of the 31 namespaces about 187 ms (GNU R 350 ms), the whole script 250 ms (GNU R 440 ms), empty `ry run` 48 ms (GNU R 129 ms); S4 caching 27 ms, S3 registration about 11 ms. The environment re-run is about 10 ms: GNU R's profile (`.expand_R_libs_env_var` about 3 ms, the `Autoloads` attach about 1 ms) and each generic's first S3 dispatch (about 0.3 ms, method lookup through `UseMethod`).

## Package builds run `make` in parallel and link BLAS and LAPACK by versioned file

**Status:** built.

- **`run_make` sets `MAKEFLAGS=-j<processors>`** unless the user set it, and a failed parallel build retries with one job (make reuses the built objects), so a non-parallel-safe `Makevars` still installs (`install-serial-makevars.scenario`). For packages with compiled code the compiler is the floor (Matrix's compile is about 134 s serially, 33 s with four jobs). Installs never byte-compile, and the lazy-load database is fast (0.06 s for MASS against GNU R's 0.10 s); the R half spends its time in Rd processing and help indices (MASS 0.55 s and 0.27 s, about 10% slower than GNU R) and, for an S4 package, sourcing code (Matrix 2.7 s).
- **Whole `R CMD INSTALL` wall time** on a 4-processor container: MASS 2.0 s (GNU R 6.6 s), lattice 1.5 s (8.1 s), Matrix 39 s (177 s; 74 s with `MAKEFLAGS=-j4`).
- **On Linux an exact `-lblas`/`-llapack` becomes `-l:libblas.so.3`/`-l:liblapack.so.3`** (`versioned_linear_algebra`), because the unversioned `libblas.so` exists only with a distribution's development package while the runtime opens the `.so.3` (without it Matrix and quadprog failed to link). The package records that file's soname (`libblas.so.3` for Debian's reference BLAS; a distribution linking it to OpenBLAS may record `libopenblas.so.0`). Nothing in the home depends on what the system had: a `dlopen` probe would freeze into a cached home. `bfd`, `gold` and `lld` take `-l:file` on the command line, but `gold` rejects it in a linker script, which ruled out a `libRblas.so` script. A system with only an unversioned `libblas.so` fails to link (unverified whether any ships that).
- `install` makes the library path absolute, because the R half runs in the build directory.

## ry's base R ships the recommended packages

**Status:** built.

- `tools/base-r/build.sh` installs GNU R's recommended packages from `src/library/Recommended/*.tar.gz` with the tree's own `R CMD INSTALL`, so `library(lattice)` works without GNU R. On Linux that is all fifteen, adding about 85 s to the build and 34 MB to the tree (63 MB in all). On macOS and Windows nlme, KernSmooth and mgcv (which imports nlme) are left out, because their Fortran would link a Fortran runtime the archive does not carry (Homebrew's or Rtools' `libgfortran`); there they come from GNU R's libraries. Windows extracts only the versioned tarballs, because the unversioned names are symlinks tar cannot create there.
- **GNU R's system library is not on `.libPaths()`:** Debian's `/etc/R/Renviron` puts `/usr/lib/R/library` in `R_LIBS_SITE`, and site libraries precede `.Library`, so GNU R's base packages there would shadow ry's. `gnu_r_site_libraries` in `crates/interp/src/session.rs` leaves out every library holding `base`.

## Hot C API entry points take a direct success path

**Status:** built.

- **A hot entry point is `boundary::direct(fast, slow)`.** `fast` returns `Option`; `None` falls to `enter` with the original body, marked `#[cold]`. `enter` (save frame state, catch panics and conditions, longjmp on error, hand out the result) costs about 90 instructions before any work and cannot be both general and free; with the general attribute setters behind it, one `setAttrib` cost about 550 instructions and a `dim` set about 930. Splitting success from failure is GNU R's own shape and keeps one error path; errors, warnings and longjmps happen only on the slow path, so `Rf_error` semantics are unchanged.
- **Coverage:** the attribute functions (`getAttrib`, `setAttrib` including `names`, `dim`, `dimnames` and `class`, `inherits`, and the `*gets` setters for a non-`NULL` value), allocation and scalar constructors (`allocVector`, `Scalar*`, `mkString`, `cons`, `lcons`, `list1`-`list6`, `lang1`-`lang6` with one batched reservation), `asLogical`/`asInteger`/`asReal` of a plain first element, `nrows`/`ncols`, `install`, `charIsASCII`/`charIsUTF8`, `ExternalPtrAddr`, `GetOption1`, protect-with-index, the cell writers (`SETCAR` through `SETCAD4R`, `SETCDR`, `SET_TAG` on a plain cell), `R_existsVarInFrame`, `R_GetBindingType`, and `R_DelayedBindingExpression`/`R_DelayedBindingEnvironment` on an unforced promise whose environment exists (an argument reference or pending frame must materialize one, which allocates, so it is slow). Reading `names` answers a one-dimensional array's `dimnames[[1]]` in the same walk, and reading `row.names` returns stored values directly.
- **`fast` must not raise, warn, panic, run R code or call native code, and may decline only before changing anything visible,** so the slow path starts from the same state. It may allocate if it roots what it holds. Results the collector already keeps alive come back as `Reachable` and skip the handout.
- **Compact row names, setting `row.names` and pairlist `names` stay slow,** because their R semantics are not worth duplicating.
- **Resizable vectors:** data.table over-allocates its column list by about a thousand slots and shrinks at once, so `Heap::resize_within_capacity` tests dropped slots eight at a time and releases only chunks holding something other than `NULL` or the blank string (both need no release). The direct allocation zeroes atomic vectors because the slow path does and the fuzz compares them.
- **`Rf_duplicate`'s direct path** handles attribute-free calls and pairlists whose leaves are shared values (symbols, environments, `NULL`) or plain atomic vectors, up to 64 levels deep (rlang's `capture` and `ffi_interp` copy every captured expression, which made it cli's largest boundary cost outside the string layer), copying post-order so each cell is allocated once with its final contents and no barrier runs; anything else declines to the general `duplicate`, which remains the one definition of what a deep copy shares.
- **Pinned:** `crates/libr/tests/test_api_boundary.rs` fuzzes every direct path against the slow path (`ry_set_slow_paths_only`) on random objects, with GC stress on a quarter of the seeds and base R loaded to build delayed and active bindings, comparing results, the mutated object and its attribute order with `R_compute_identical`, and holds each entry to an instruction budget no higher than with direct paths off (`RY_API_BENCH_SLOW=1`).
- **Heap allocation errors are built lazily** (`let ... else`, not `ok_or(HeapError::...)`), because the eager form built and dropped a `HeapError` on every allocation, about a dozen instructions per `allocVector`.
- **`Riconv` is not on this path:** it belongs to the string and encoding layer ("`iconv` goes through the C library's `iconv`").
- **Not changed:** attributes stay a pairlist; a dense attribute table changes the image format (`backlog.md`).
- **Closed:** after `Rf_duplicate`, the whole boundary is 0.44% of the testthat application's instructions, its largest entry `R_existsVarInFrame` 0.08%. In package suites (release, `TESTTHAT_PARALLEL=false`) the largest remaining entries outside the string layer are `Rf_shallow_duplicate` (0.37 to 0.44%, 1,030 Ir per call) and `R_existsVarInFrame` (0.23 to 0.58%, 475 Ir per call), with `SETCAR`, `CAR` and `Rf_protect` at 0.1 to 0.3% each; a direct path roughly halves an entry, so the top two would save about 0.5% of cli. Reopen only if a profile shows one entry above 2% of a named workload. Two vctrs costs are not boundary costs: `Rf_allocVector` at about 11,000 Ir per call is probably collection charged to the allocator (unverified), and `INTEGER_RO` at about 19,000 Ir per call comes from callers building a fresh `1:n` each call (expanded once and reused, as in GNU R; about 37,000 Ir per call to build on both sides).

## Measured and not taken: a builtin path for `Rf_eval` and a cheaper protect stack

**Status:** built (not taken; the measurement stands).

- On timeseries, xts's `xts_get_attrs` evaluates `attributes(x)` through `Rf_eval` about 97k times per repeat (2.87% of warm instructions, 1,685 Ir per call: 867 in `do_attributes`, 818 in the evaluator around it), and protect, unprotect and reprotect cost 1.13%. A direct builtin path must still look the function up from `rho` and hand the result out, and protect at about 10 Ir saves about 0.8%, so the realistic saving is about 1.8%, under the 3% bar.

## Measured and not taken: libdeflate for lazy-load records

**Status:** built (not taken; the measurement stands).

- Inflating whole records with one `libdeflate_zlib_decompress` call in `serialize::decompress_record` (a record states its decompressed length) saves 1.1% of testthat (41.5M to 18.3M Ir), under the 2% bar. Only GNU R-installed packages benefit (ry-installed ones read records uncompressed from the code image), and it adds a C dependency. The branch `perf/libdeflate-records` holds the change, a size check against deflate's 1032:1 expansion, and a bounded fuzz test.
