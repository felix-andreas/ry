# Backlog

The standing goal, which is a user mandate, is to empty this list and keep the project at
rust-analyzer quality. Every open item sits in one of the sections below. Shipped work lives as a
one-line ledger entry at the bottom, with the rationale in `decisions.md` and the contract in the
docs.

The quality bar is four things.

- **Sound on idiomatic R.** There are no known accepts-then-crashes holes on a supported construct.
  An unsupported construct may be refused loudly, because sound-by-refusal is acceptable, and must
  never be silently mistyped.
- **No false positive on the roughly 200 most-used base functions**, with `[check] typing = true`,
  on an idiomatic call form. `crates/semantics/tests/typing/base-functions.R.test` measures it: the
  functions the corpus calls most, one idiomatic call each, with no diagnostic allowed. Its
  remaining `Unknown` results (`Reduce` over an operator, a replacement call's value) are strict-mode
  gaps, not false reports.
- **Keystroke to diagnostics at a median of 30 ms or less and a 95th percentile of 100 ms or less,
  at 300k lines.** Read those against the raw-parse floor the instrument prints, because a latency
  number swings by about 1.4 times with machine load. `crates/semantics/tests/test_perf.rs` pins
  the budgets, as per-line ceilings on wall time, memory and resolve steps plus keystroke latency,
  and `just perf` runs it.
- **No input kills the server.** There is no `unwrap` panic on a protocol-legal message.

## Open: two Renviron readers

- ry has two Renviron readers, the interpreter's startup one in `crates/interp/src/session.rs` and `rcmd::command_environment`. The interpreter's lets an already-set variable win, which differs from GNU R's `process_Renviron` (a file line always overrides). Merge them into one reader with GNU R's semantics.

## Open: `test_linalg` library cases fail on some OpenBLAS kernels

- On CI (ubuntu runner, OpenBLAS, probably an AMD Zen CPU) `linear_algebra_matches_gnu_r` fails on `dtrco` by one ulp against a live GNU R on the same libraries. It reproduces locally with `OPENBLAS_CORETYPE=Zen`, `Sandybridge` or `Nehalem` (and `Prescott` fails on `dqrcf`); the default, Haswell and SkylakeX kernels pass.
- Cause: ry's vector data starts on a 16-byte boundary (`VECTOR_DATA_OFFSET == 32` in `crates/heap/src/object.rs`), GNU R's 8 bytes past one. Those OpenBLAS kernels peel a misaligned first element, so the summation order differs. Copying `dtrco`'s buffers to an 8-byte offset made it match exactly; `La_dtrcon` then failed the same way.
- Options: (a) pass BLAS/LAPACK buffers at GNU R's alignment (matches GNU R bit for bit on every kernel; heap layout or copy cost), (b) compare live only on kernels that ignore alignment, (c) install `libblas3 liblapack3` in ci.yml so CI uses the reference libraries and the pinned bits (workflow edit, human only). (c) is the quick unblock; (a) is the faithful fix.

## Open: bundle base R into the `ry` binary

- Today `ry run` reads the R code of the base packages (base, stats, utils, methods, graphics, grDevices, datasets, tools, grid, splines, stats4, parallel, compiler) from an installed GNU R (`rcmd::runtime_home`), which is the only reason a user needs GNU R for it. Both are GPL-2.0-or-later, so bundling is licence-clean.
- Plan: a build step, not vendored sources. Download one pinned R source tarball (SHA-256 in the repo; patches as files in git), parse `src/library/*/R` per platform (base selects unix/windows code at build time), and write one preloaded base image in ry's own format, which can also start faster than R's lazy-load DBs (relevant to S4 first load). Embed it zstd-compressed and extract it to the cache like libR. Nix fetches the tarball as a fixed-output derivation.
- About 25 MB on disk for those packages on Linux, roughly 8-10 MB compressed (estimate). Check whether ry loads any of their compiled `.so` files.
- Unchanged: compiled packages still need a C/Fortran toolchain, and `ry R` still needs GNU R.

## Open: the benchmark packages under ry's runtime

`just bench-compare` gives the current numbers; `MEMORY.md` records them.

- **Quoted byte strings with a quote character.** GNU R prints a quoted `"bytes"` string that contains `"` with a stray trailing `\0` (`"q\"\\xff\0"`), an artifact of its escape buffer. ry prints it without the `\0`; this is deliberate.
- **Per-test time.** With the JIT off, finbench's tests take 1.15x GNU R's time. `sd(x)` on 20 doubles costs about 104.5k instructions against GNU R's 70.6k. Measure GNU R marginally (a run of N2 calls minus a run of N1, both above zero): GNU R byte-compiles a closure on its first calls, so subtracting an empty baseline charges that compilation to the loop and roughly doubles small per-item costs. ry's cost is spread over bytecode `execute_code` (its self cost is level with GNU R's `bcEval`), function lookups (93 per `sd`, through `frame_cell` and `cached_cell`), allocation (`allocate_uncollected`, `alloc_triple_keeping`, `define_formals`, `root`, the write barrier) and `mk_char`. A suite profile (`perf record` over `oracle/src/bench.R` with `RY_BENCH_FILTER`) is flat: `execute_code` 12%, `frame_cell` 4%, `allocate_uncollected` 4%, `Heap::drain` 3.4%, `mk_char` 2.7%. Promise code is compiled (`decisions.md`, the promise paragraph under the object model), but `sd` passes few compiled arguments, so it gains little there; arithmetic arguments run about 1.6x faster. `execute_code`'s self cost per `sd` grew by about 4.4k instructions when the record and `match` opcodes joined `step`, at the same instruction counts; a profile with debug info (`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`) should show whether the larger dispatch or register pressure in `step` is the cause. Next levers: fewer lookups per call (about 79 `lookup_function` calls per `sd`), fewer cells and promises per call (`define_formals`, `allocate_with`), and the callers of `mk_char` in the test code. The untyped stdlib templates (`tools/bench/templates`) show the same gap: the ledger fold costs about 15.8k instructions per fill on ry against about 6.1k on GNU R (both marginal), and readings generation about 7.4k per reading against 4.5k, now that compiled `out[[i]] <- v` updates a local vector in place (`assign_element_in_place`) instead of running `complex_assign`. `apply(m, 1, f)` on a 4-column matrix costs about 13.8k instructions per row against GNU R's 16.8k, after the `m[, j]` column fast path; the kernels session measured it at 1.9x GNU R's time before that, and xts `period.apply` (about 33k closure calls through `[.xts` and `mean.default`) at 1.3x, which is still to be re-measured. In the ledger fold, `frame_cell` runs about 25 times per fill for about 14 variable and 7 function lookups, because `lookup_variable` searches the local frame before consulting its cache; the per-instruction dispatch lines (opcode fetch, operand decoding, the handle-mark check) cost about as much as the work. `frame_cell`'s cost per `sd` moved by about 1k instructions between two builds at the same call counts; the likely cause is that the frame filter hashes symbol addresses, so its collisions change when allocation does (unconfirmed).

- **Old runtimes and R homes stay in the cache after an upgrade.** Each release extracts its runtime into a new `ry/runtime/<key>/` (tens of MB) and builds a new `ry/home/<hash>/`, and nothing removes the previous release's directories: a home is pruned only when a newer home is built for the same runtime and front end (`rcmd::cached_home`). Prune runtime and home directories that no `ry` on the machine can name, without removing one a running session still maps.

## Open: collector throughput on the benchmark suites

- **Measure first.** Generate the suites (`python3 tools/bench/generate.py --out .bench --lines 300000 --files 500`), install the corpus library (`just corpus`; the polars R package builds from source and takes the longest), then run `just bench-compare .bench/finbench .bench/dsbench .bench/biobench` with `RY_GC_LOG=<file>` set. The report's last block gives each runtime's `gc.time()` total and share of suite time; the log gives every pause (kind, nanoseconds, young small and large bytes, old bytes), from which pause p50 and p99 and collections per second follow. Fix the largest cost the numbers show, then check each object type's allocation path against `decisions.md`, "Data-oriented design and explicit allocators" (open audit items: "Open: data-oriented layouts and explicit allocators").
- **GC torture over the package test suites** is owned by the robustness work (`oracle packages` under `RY_GC_STRESS` and `gctorture(TRUE)`, `contributing/robustness.md`).
- **String-heavy C code is bound by memory traffic** (`str_split`, see "Package kernels" below): suspects are the heap's allocation slow path and string-table probes that miss cache with about 2e7 live CHARSXPs. A character subset of 1e6 unique strings takes 0.012 s against GNU R's 0.003 s.

## Open: base R kernels against GNU R

`just bench-base` times base R's kernels against GNU R with checksums (`tools/bench/base`). Kernels that take a few milliseconds swing by 2x between runs, so judge them with `--rows 1e7`. At 1e7 rows these are still slower:

- **Strings:** everything measured is at or below GNU R (`paste(collapse =)` 0.80, `format` 0.90, `sub` 0.93, `grepl(fixed = TRUE)` 0.97). `as.character(double)` is deferred on both, so it is timed forced. data.table's `tstrsplit(s, "_", fixed = TRUE)` on 2e6 strings took 1.1 s against GNU R's 0.46 s (one thread); re-measure.
- **Grouping:** `order` on two keys runs at 0.88 of GNU R's time at 1e6 rows. Carrying the second key's values through the first key's scatter would avoid the random gather that dominates it.
- **Matrix and time series:** xts `period.apply(xt, endpoints(xt, "hours"), function(x) mean(x))` on 2e6 minutes (33,335 periods) takes 1.4 to 1.5 s against GNU R's 0.89 s, at 250k instructions per period against 204k, so part of the gap is not instruction count. Callgrind's cache simulation (`--cache-sim=yes`, marginal over two run counts) puts it in the instruction cache: about 7,040 L1 instruction misses per period against GNU R's 3,716, while data misses are close (2,382 against 1,872 reads, 700 against 822 writes). The extra misses cost roughly 0.4 s. They are spread thinly (`execute_code` about 460 per period, then `run_closure`, `native_call`, `bind_arguments`, `match_args` and `mk_char` at 120 to 190 each), so outlining cold arms of `step` alone has little room; the open lever is function ordering and keeping cold paths out of the whole call path. Profile-guided optimization made things worse (`decisions.md`, "PGO is not used for the release build"), and BOLT is not installed in the cloud environment. Closure-call overhead is a broader target than this one case: a closure call costs 0.75 µs against GNU R's 0.35 µs, `vapply` 8.2 µs against 3.4 and `pmin` 15.7 µs against 6.2, and this per-call cost is why ggplot2, lattice and PerformanceAnalytics take 1.2 to 1.9x GNU R's time to render. The aim is a closure call at or below GNU R, measured with `oracle gallery` (149 timed figures) beside period.apply. Two more levers are queued: the `frame_cell` lookups in the fill loop of the finance benchmark's `ledger` template (`tools/bench/templates/finance/ledger.R`, cost not yet measured in isolation), and the S4 metadata-cache-miss path (`slot<-`, `list()`, `possibleExtends`; see the start-up item in `MEMORY.md`). The remaining instruction cost is spread: `execute_code` 27.5k (level with GNU R's `bcEval`), `method_symbol` formatting and interning about 21 method names per period (4.3k), `is_top_env` probing `.packageName` (2.4k), and xts' own `Rf_install` calls (4.4k). `apply` over rows is below GNU R per row; zoo `rollmean` 1.4x.
- **Matrix products** (`linalg.rs`, `real_product`) are ry's own kernels in reference BLAS's summation order (`decisions.md`, "base R's linear algebra"), so they run at reference BLAS's speed, not OpenBLAS's; not yet timed against a GNU R on OpenBLAS. A faster kernel may block over rows and columns and thread across columns, but must keep each entry's sum sequential in the inner index and use no FMA, or the bits move. CI has only OpenBLAS (setup-r installs it), so `test_linalg.rs` compares its LAPACK cases there with live GNU R; adding `libblas3 liblapack3` to the workflow's apt line (a human edit: workflow scope) would make CI check the pinned reference bits too.
- **Package kernels** (`kernels.R`, zoo, xts, lubridate, stringr, jsonlite, readr) at 1e7 rows: `rollmean`, `rollapply`, `na.locf`, `endpoints`, `to.period`, `str_detect`, `str_replace_all` and `read_csv` are at or below GNU R. Still slower: `str_split` 1.54x, xts `period.apply` 1.39x (per-call interpreter cost, tier-0 work), `ymd` 1.29x, `fromJSON` 1.26x and `toJSON` 1.20x. `str_split` executes only 1.14x GNU R's instructions, so the rest of its gap is memory traffic in `allocVector` and `mkCharLenCE` (the heap's allocation path and string table). The jsonlite simplify step and lubridate's format training are R-level interpreter cost. At 1e6 rows these ratios swing by 30% between runs.
- **Peak memory** (`run.py --memory`) at 1e6 rows is within 1.13 times GNU R's for every kernel (`decisions.md`, "String keys sort by rank"). The highest are `unsplit` 1.13 (184 MB against 163), `gsub(perl = TRUE)` 1.08, `xts period.apply` and `as.character(double)` 1.07. `make.unique` on 1e6 names peaks at 127 MB against GNU R's 92 MB; names that are not ASCII still take the `String` path.

## Open: the package loader

The loader runs on ry's runtime (the `ry load` default) and on GNU R (`crates/loader`,
`docs/.../guides/running.md`). Gaps, roughly in order of how often a real package hits them:

- **Every rebuild of a shared library maps one more copy** on ry's runtime: `dyn.unload` keeps
  the retired library mapped (`libr` `unloaded_dlls`) so a stale pointer into it does not crash.
- **`ry document`'s warm run is dominated by reading the block cache**: about 3 s of a 11 s no-op
  on finbench is `readRDS` of the 29 MB compressed cache. Splitting it per file, or a faster
  format, would cut it (`decisions.md`, "`ry document` is roxygen2 with block and topic caches").
- **Tarballs built on ry's runtime record owner 0/0** where GNU R's record `root/root`: ry's
  `file.info` has no `uname`/`grname` columns, so `utils::tar` falls back to ids. Harmless for
  installing, but `ry build` output is not byte-identical to GNU R's.
- **Remaining one-file reload costs** on the 326,000-line bench package: planning takes about
  3 ms and preparing 4 to 5 ms. A file notification finds the edit (without one, a `stat` per file takes about 2 ms), and the diff still
  walks every eager statement. The first reload after a load that read the cached index pays for
  building the salsa inputs once. A strict reload spends about 100 ms of its 0.1 s re-verifying
  diagnostics, plus a fingerprint of about 16 ms taken twice (before and after the check).
- **Compiled code under `ry load --runtime gnu-r`** still needs pkgbuild; only ry's runtime
  compiles with `rcmd`. A non-gcc platform inherits `rcmd`'s gaps (see "Platforms").
- **Attaching testthat is eager**, as pkgload's `load_all()` is: a package with `tests/testthat`
  pays for loading testthat on every cold load (about 0.2 s on GNU R; a tiny package's load went
  from 14 ms to 1.2 s on a debug build of ry's runtime). A `package:testthat` of promises over its
  exports would keep the search path identical and load testthat on first use.
- **Start-up and imports are the floor of a warm `ry load`.** With the S4 unit restored lazily, a
  warm `ry load` of the generated S4 package (`just bench-s4-first-use target/s4heavy
  lubridate,data.table,xts`) spends about 130 ms reaching the load and about 200 ms importing,
  against under 60 ms for the first `new()`. `registerS3methods` runs natively (`decisions.md`,
  "start-up and imports run GNU R's code"), so what remains is GNU R's other R code:
  - Process start, best of five: base alone 33 ms (`R_DEFAULT_PACKAGES=NULL`), with methods 74 ms,
    with utils 93 ms, with stats 119 ms, all defaults 130 ms (GNU R's `Rscript`: 153 ms). The levers
    are interpreter speed, or the preloaded base image of "bundle base R into the `ry` binary"
    above, which could carry the default packages already loaded.
  - Imports: `methods::cacheMetaData` is about 150 ms of them, and `.checkGroupSigLength` about
    70 ms of that (an upper bound measured by stubbing it out): `.updateMethodsInTable` runs it for
    every member of a group generic, and each call rescans the whole group tree through
    `getGeneric` (1.2 ms per call on ry, 0.8 ms on GNU R; 67 calls for lubridate, data.table and
    xts, none of which reset anything). A replacement guarded like `registerS3methods` could skip a
    rescan whose generics and `.SigLength` values cannot have changed, but proving that cheaply needs
    an invalidation signal for `.genericTable` and every `.SigLength` assignment. Deferring an
    import's `loadNamespace` until a name is used is not exact: `.onLoad` side effects (options, S3
    registrations that dispatch on objects read back with `readRDS`) would move.
- **A statement reading a name before its defining statement** sees, on reload, the previous load's
  value (a fresh load fails on such code); documented as a limit.

## Open: false reports and gaps found by the documentation review

An adversarial review of every docs page ran each claim against the binary. The docs now state the
behavior below truthfully, most of it under "Where correct code is reported" in
the Limitations section of `type-checking/unknown.mdx`, so fixing an item means updating that page too.

False reports on correct R, worst first:

- **Platform-conditional `pkg::name` reads are reported "not exported".** 17 corpus reads, such as
  `utils::shortPathName`, `utils::readRegistry` and `grDevices::win.metafile`, name Windows-only
  exports the Linux-generated manifests lack, and `parallel::splitList` and `base::check_tzones`
  are feature probes inside `try()` or `tryCatch()`. Adding the Windows-only exports to
  `scripts/export-manifests.R` would fix the first class; a qualified read inside a `try()`
  argument is a probe and could skip validation.

Smaller gaps:

- **Writes cut off by `break` or `next` are lost at loop joins.** `x <- "a"; if (c) break; x <- 1L` in a loop body leaves `x` without `character` after the loop, for loop-carried variables and for the loop variable alike (`check_loop_body` joins only the end-of-body state). Naming also calls the write before `next` unused. A nested loop that reuses its outer loop's variable name drops the outer loop's `NULL` (`for (m in xs) { for (m in 1:2) {} }; m` is `integer`).
- Found in passing by the IDE work, not yet reproduced: `sales[, amount := "x"]` flags `sales` as
  unused; `p2$x <- "a"` and `p2[["w"]]` go unreported after an earlier error in the same item; the
  message for `Result.map(:Ok(1L), 3)` leaks the type variable `T`; `Str.lenght` gets no
  "did you mean".
- Two broken annotations still cause follow-on findings: `#: Rec[]` over an alias of a record (also
  coded `type-mismatch` instead of as an annotation error), and `fn(integer, [character])`.
- A `vapply` whose callback's return type disagrees with `FUN.VALUE` reports only the two types, as
  `vapply(words, toupper, numeric(1))` gives "expected `character[] | character`, found `double[]`"
  on `numeric(1)`, whether the callback is a name or a `match.fun` string. Naming the callback needs
  the generic argument check to carry which argument fixed the type variable.
- After an invalid field write, a later field typo on the same record goes
  unreported. Hover on `m2` in `m2$amount <- 5` shows `double` instead of the nominal type.
- From the final type-system review, each now documented as a known gap in the reference:
  - an elided return in a trusted coercion or on a non-literal value (`#: fn(integer)` on `g <- f`)
    is `Unknown` rather than `f`'s known return;
  - assignments in a bare top-level `{ }` block are invisible to other package files;
  - an optional `[label]` parameter over a formal with no default is checked in annotations but not
    at an argument position;
  - a bare read satisfied only by an import or manifest is not a strict origin, while a qualified
    one is;
  - `maybe-undefined` ignores a binding of the same name outside the function, which R would read;
  - `[[` with a missing name on a named atomic vector is typed `| NULL`, where R fails;
  - strict mode can still report inside a statement that failed to parse.
- Console completion: `df$` offers only `df`, and an unclosed `x[["` completes keywords instead of
  fields.
- The Zed extension downloads the newest pre-release's archive (`editors/zed/src/lib.rs`), and
  0.4.0-beta has only the Linux one, so on macOS and Windows the download fails until a release
  ships every platform. The install page tells users there to put `ry` on `PATH`.

## Open: what `ry check` still reports on the GNU R corpus

Running `ry check` with typing on over the `R/` sources of the corpus packages (`corpus-and-oracle.md`)
gives 39 errors: 25 true package bugs, 2 known limitations and 12 false reports on correct R. The
three generated benchmark packages (`tools/bench/generate.py`) report nothing. To reproduce, give
each package a `ry.toml` with `[check]` `typing = true` (typing is off by default) and run
`ry check --output json R` inside it. A fix also deletes its line here and, for a user-visible
gap, in the Limitations section of `type-checking/unknown.mdx`.

### True bugs, left reported (25)

- DBI `data.R` (5): coerce calls that leave out the required `to`.
- GeneralizedHyperbolic `nigFit.R` 99 and 110: `llfunc(paramStart, x)` on a one-parameter `llfunc`.
- GeneralizedHyperbolic `hyperbFit.R` 285: `logHypDens <- function(y) log(dhyperb(t, ...))` reads
  base `t` instead of `y`.
- NlcOptim `solqp.R` 297 and 528: calls the vector `indall` and the matrix `A` as functions.
- NlcOptim `NlcOptim.R` 261 and 262: indexes a value that is a function on one path.
- diffobj `core.R` 492 and `check.R` 476: `!is.valid.*(...)` negates a `character` message, which R
  rejects with "invalid argument type".
- ff `ordermerge.R` 3006, 3009, 3012: passes `verbose`, which the callee does not take.
- gmp `Stirling-n-etc.R` 277: `if (log)` with no `log` formal reads base `log`.
- httr `oauth-token-utils.R` 34: `revoke_oauth2.0(x)` leaves out `credentials`.
- kernlab `kqr.R` 173 and 263: `ramloss(...)` is a typo for `ramploss`.
- kernlab `gausspr.R` 194: passes a `double` where the callee multiplies a `matrix`.
- mclust `toremove.R` 259: leaves out `k`.
- processx `process.R` 748: `new_not_implemented_error` without `call`.
- rugarch `rugarch-startpars.R` 518: reads `temph` for `tmph`.

### Known limitation (2)

- devtools `test.R` 72 and knitr `citation.R` 163: a vector of statically unknown length
  (`p[file.exists(p)]` returned through a function) reaches a stub's scalar `character` parameter.
  The value has length one at run time, but no type says so.

### False reports (12), one cause each

- PerformanceAnalytics `legend.R` 293: a missing `angle` is passed to `rect(angle = angle)`, which
  forces it only when `density` is not `NULL`. Laziness that depends on a value.
- PerformanceAnalytics `PortfolioRisk.R` 387: `VAR$VaR` on a value inferred as a tuple.
- Rsolnp `benchmarks.R` 470: `eqB = NULL` to a project parameter typed numeric from a use outside
  the callee's `is.null()` test.
- bit64 `highlevel64.R` 309: an `if` joining `table()` and a list return reaches a stub union.
- diffobj `diff.R` 654: a one-parameter callback against an interface that names more parameters.
- ks `kdde.R` 304: a rigid `T` meets `double[] | integer[]`; not reduced.
- ks `kdde.R` 568: `if (missing(col.fun)) col.fun <- function(n) ...` binds the lambda's `n` to
  the first call's `integer`, so a later `double` fails.
- renv `python.R` 109: a union parameter that flows to the return keeps every member at the call
  (`path: list[T] | T[]` from `map_lgl(path, ...)`). Needs a bounded variable so the return names
  the argument's own type.
- renv `r.R` 137: a callback of a project `lapply` wrapper receives the whole tuple of records
  instead of one element.
- renv `use.R` 100: `records[!compat]` where `compat` is a named list from a project `enum_lgl`.
- roxygen2 `rd-usage.R` 88: `format_name(name)` after `identical(format_name, identity)` sees an
  over-wide `name`.
- snakecase `to_any_case.R` 266: `vapply` over a union with a `factor` member, which the stub's
  `list[T] | T[]` does not accept.

### Scalar-claim chains

A stub's scalar return (`round`, `sum`, `nchar`) or a scalar use far down a call chain pins an
unannotated project parameter, and a caller passing a vector is reported. The corpus no longer
shows an instance: gmp's `x <- 1:n; lapply(n, f)` was one, and a variable bounded by a scalar
constraint now fills a vector parameter. The forms once seen in ks `kfe.1d`, pracma
`.adaptsimstp` and rugarch no longer report, and no isolated repro exists. Reopen with a repro.

### A fresh sample outside the corpus

The top 100 CRAN packages by reverse dependencies that are not in the corpus (99 checked,
1,210,157 lines, sources only) first gave 1,449 errors, of which 1,384 were false (114 per 100,000
lines). After the fixes they give 468, about 400 of them false (33 per 100,000 lines); the rest are
50 true bugs and 15 debatable ones (psych's `print(quote = "FALSE")` works by coercion). Remaining
false classes, largest first:

- Return types selected by a flag the body reassigns or nests (VGAM `iam`, about 60 in `.q` files
  that are now checked).
- Dead code after `stop()` still binds names that shadow outer ones (psych, 44).
- An unannotated parameter over-inferred from one use, a default or one branch (21).
- A variable correlated with a flag tested elsewhere (20).
- A union of function types where a function is expected (lme4, kableExtra, 18).
- A missing argument forwarded to `[` or to a callee that tests `missing()` (flextable, 10).
- An element write `d[[i]] <- v` does not update the container type (psych, 10).
- `vapply` over a parameter turns it into `list[T] | T[]`, so a later `$` fails (ggplot2, 10).
- A project S3 method for a stubbed generic is ignored (R.utils, 9); NSE arguments checked (9);
  narrowing gaps after `tryCatch` and inside `&&` (8); Windows-only NAMESPACE imports (8).

### Other open checker items found on the corpus

- **A flag-selected return stops at the first guard.** `flag_return` in `crates/semantics/src/check.rs`
  reads only the first `if (flag) return(...)` on a parameter, so a second flag guard later in the
  body falls back to the union of every return. Low priority: no corpus or fresh-sample report
  depends on it.
- **A NULL default written back as an annotation is checked.** An inferred `[H]: T | NULL` passes
  on as a placeholder, but the same signature declared reports the call, so the round-trip test
  lists `a_null_default_is_a_placeholder_at_a_numeric_parameter` as known unwritable. Either a
  declared optional `NULL` default should read as a placeholder too, or the renderer should mark
  it. Flag-selected returns (`p_dist`, `iam`) are listed there for the same reason: an annotation
  cannot say that a literal flag picks the branch.
- **A generic's dispatch formal, forwarded, reads as optional** (missed error).
  `f <- function(fit) persistence(fit)` over a `UseMethod` generic gives `f` an optional `fit`, yet
  `f()` errors in R (`a_generic_does_not_require_the_formals_it_passes_on`).
- **`m * tol` after `if (missing(tol)) tol <- 1e-8` leaves both parameters unbounded**, so
  `fast_svd("a", 1)` is not reported (missed error).
- **A number passed to a parameter tested as a condition.** `if (x)` and `!x` bind an open
  parameter to `logical`, and R takes `0L` there. Bounding it by `scalar numeric` was tried and
  rejected for hover readability; a call-site relaxation for an inferred `logical` is the likely
  shape, but schemes do not record where the `logical` came from.

## Open: where findings point, and what the type system still refuses

Three simulated users probed the type checker itself rather than package coverage, working from the
docs and `--help` only. One worked on parametric polymorphism and higher-order code, one on domain
modelling and nullability, and one adversarially on the `#:` surface and where carets land. Each
judged location and message as separate verdicts, which is what made this round different. A
diagnostic that reads perfectly while blaming the wrong expression counted as a failure.

Only the open findings are below. `test-user-reports.md` holds the closed ones with what each
measurement showed.

### Placement

Caret placement is good for ordinary nesting, which covers four-deep calls, multi-line arguments,
lambdas and pipes. The renderer is display-width aware while JSON stays in codepoints. Every failure
found was the same shape, which is a collapse to the outermost node, and one rule now covers them in
the reference under "Where a finding points": a finding underlines the smallest expression its
message is about. Comparison operators, a return-type mismatch with an `if` and `else` tail, `$` and
`[[`, a surplus positional argument, a record mismatch and an annotation range ending at
end-of-line all follow it now.

**One shape is still open.** A parse error was reported past the end of the file. The filed shape,
which was line 10 of a 9-line file, does not reproduce. An unterminated `f <- function(x,` in a
one-line file reports at `1:14-1:15`, which is in range. Either an earlier fix covered it or the
note was imprecise. This needs a fresh reproduction before it can be worked, and it should be
re-derived rather than trusted.

### Should strict mode report a binding whose exported type contains `Unknown`?

This is a design question, not a bug fix, and it needs volume measured before it is chosen.

The gap is real. `g <- f` closes to `g: fn(p: Unknown) -> Unknown`, which the
`aliased_function_reference_exports_closed` fixture pins, and strict reports nothing. Calls through
`g` are therefore unchecked while the run looks like a pass.

Two earlier diagnoses of it were wrong, and the measurements are the keepable part.

- **`Any` is not what blinds strict mode.** Changing `do.call`'s return from `Any` to `Unknown`
  produces no strict finding at all. This was tried: the stub was edited, the binary rebuilt, and a
  strict project run over `do.call(fun, args)`. Nothing.
- **Strict mode has no binding-level `Unknown` test to widen.** It reports origins recorded at
  construct sites, which are `UnsupportedConstruct`, `UndeterminedReference`, `LoopWidened` and
  `RecursiveUnknown`. Nothing inspects a finished type.
- **`Any` in the corpus is deliberate.** The shipped stubs declare 176 entries whose return is
  `Any`, across nine files rather than just `base`, and none returning `Unknown`. The stub header
  names each compromise.
- The one behavioral difference between them is that `@if-unknown` coerces an `Unknown` and is
  refused on an `Any`, saying that the value is already `Any` so the annotation should be dropped.

Measure before deciding. Strict mode already emits 2879 findings on dplyr and 2458 on shiny with
typing and strict forced on, and `erase_residual_vars` gives every aliased function `Unknown`
parameters, so a containment sweep would fire on `g <- f`, an idiom ordinary R uses constantly.
Decide it as a design note with numbers, not as a one-line widening.

### Promotion needs to live in the type, not in the binding

Two findings are the same missing piece, and they should be designed together.

**A `logical` is accepted at a declared `integer` parameter and refused at an inferred numeric one.**
Both halves reproduce. `bump <- function(x) x + 1L; bump(TRUE)` is refused, and the same body under
`#: fn(n: integer) -> integer` accepts it. R does promote, because `TRUE + 1L` is `2L`, and the
checker's own arithmetic rules say so. Verified against R 4.3.3.

Do not just widen the constraint. The obvious repair is to bind the numeric variable to `integer`
when a `logical` argument arrives, which is exactly R's promotion, and it is wrong. The
counter-example is ordinary R.

```r
bump    <- function(x) x + 1L                          # R: bump(TRUE) is 2L, integer
checked <- function(x) { stopifnot(x + 1L > 0L); x }   # R: checked(TRUE) is TRUE, logical
```

Both infer `<T: numeric> fn(x: T) -> ...`. One binding of `T` cannot be `integer` for the first and
`logical` for the second, so the promotion produces a wrong return type. This project ranks a wrong
answer below a refusal, because a gap means checks are skipped rather than that wrong answers are
produced.

**A numeric variable shared by two parameters refuses ordinary mixed arithmetic.**
`add <- function(a, b) a + b` infers `<T: numeric> fn(a: T, b: T) -> T`, which ties both operands to
one variable, so `add(1L, 1.5)` reports ``expected `integer`, found `double` `` where R gives `2.5`.
That is a false positive on about as plain a piece of R as exists. The operand types need a numeric
join, where `integer` with `double` is `double`, rather than unification.

Closing either properly needs a scheme like `<T: numeric> fn(x: T) -> promote(T)`, which is a
type-level function the annotation language does not have.

### Smaller open items

- `@new` is unrestricted project-wide, so a nominal type's invariant holds only by convention. The
  `type-checking/custom-types.md` says so conditionally ("put `@new` in one constructor"). An
  encapsulation modifier would make it a guarantee.
- A narrowing failure on a field, or behind `&&`, produces a message byte-identical to the one for
  writing no guard at all. The docs know the fix, which is to lift the value into a local first. The
  diagnostic should say it.
- A nominal union is unchecked through `$` while a structural union is exact. There is also no
  discriminator for a nominal type, because `is.list` is true of both arms, so a tagged union cannot
  be narrowed at all.
- A generic lambda passed as an argument keeps its binders rigid, so
  `lapply(xs, function<T>(v: T) -> T v)` reports `its parameter v accepts T`. An inline signature
  on a function that is not bound should instantiate its binders like a call does.
- A call that fails against a generic signature types its statement as the leaked rigid name, so
  `twice("a")` against `<T: numeric> fn(x: T) -> T` renders `T` in the typing fixtures. It should
  be `Unknown`.
- Signature help misses R6's own helpers (`assign_func_envs`, `merge_vectors`, `list2env2`),
  which R6 defines inside `encapsulate({...})` blocks; go-to-definition already finds them.
- httr2's argument-name definition role (about 90%) counts arguments collected into `...` and
  passed on to a list or a form body (`req_body_form(req, scope = scope)`), which have no
  definition site; the role should leave them out rather than count them as misses.
- The add-annotation code action writes a `#:` comment even in a project that uses inline
  annotations. It could offer the inline form where inline annotations are on.
- Editor gaps left by the completeness pass against rust-analyzer (`ide/tests/ide/editor-completeness.R.test`
  pins what works). `df$` on a base `data.frame` completes nothing, because its columns are not
  typed (the `DataFrame` type is the typed alternative). A local whose
  type is a generic parameter (`total <- a + b` inside `<T> fn(a: T, ...)`) gets no inlay hint,
  because a bare `T` without its binder would mislead.
- `contributing/design/r-runtime.md` section 8 keeps `declare(type(...))` as a third annotation
  spelling for code that must stay valid R, and a strip step (`ry build`) that moves inline types
  into `#:` comments. Neither exists yet; `syntax::erase_inline_annotations` is the starting point
  for the strip step.

### What the round said overall

Both testers who could reach a verdict said the core is real. They found genuine Hindley-Milner with
generalization, an occurs check, per-parameter variance, working generic nominals, airtight nominal
distinctness, and no cascades outside the `@param` case. The gap is not the engine. Diagnostics
render the artifact unification left behind rather than the fact that failed, and the nominal story
protects construction but nothing after it.

## Open: ry's standard library

- Under GNU R (`ry load`, `crates/ry/src/load.rs`), `std::List.map` fails with R's "there is no
  package called 'std'" instead of the explanation bare names get, bare names are stubbed whatever
  `[check] prelude` says, and the stub message calls every `ry.exports` name (DataFrame, Arrays) part
  of the standard library.
- Qualified library calls are not fast-pathed: `library_function`/`library_call` in
  `crates/interp/src/bytecode.rs` recognize only a bare symbol, so `std::List.map` (the only spelling
  with `[check] prelude = false`) misses the compile-time leaf resolution and the JIT loops.
- A check-time diagnostic for library calls in a project that targets GNU R, once a project can
  declare its runtime (`decisions.md`, "The standard library runs only on ry's runtime"). At run
  time, `ry R` already stops with an error that names the cause.
- A helper that calls a closure it did not create (a parameter, a global) ends every move early,
  so each call copies. Ending only when the closure can reach the moved value would need an
  escape check on the closure's body; closures the helper creates itself are already covered
  (`decisions.md`, "Moving a collection into a helper's parameter").
- `List.walk(ks, d, fn)` with `fn` updating `d` copies `d` on the first iteration.
- `eval::rebind` re-implements part of `eval_call` (interrupt polling, `visible`, error
  signalling) for the reuse path. Fold it into `eval_call`.
- Compiled loops cover the `List` walks (backwards, until, with index, from), `map`,
  `map_with_index`, `keep_if`, `drop_if`, `any`, `all`, `count_if` and the `find` family, plus
  `Dict.walk`, `Dict.walk_until`, `Set.walk` and `Set.walk_until` (`decisions.md`, "Library
  traversals compile to loops"). Still on `standard_library::invoke`, by reason:
  - a rebuilt collection: `Dict.map`, `Dict.keep_if`, `Dict.drop_if`, `Set.map`, `Set.keep_if`
    (the loop would have to rebuild the hash index);
  - several lists: `List.map2`, `map3`, `map4`, and `join_map` (the callback returns a list);
  - `Result` callbacks: `List.keep_oks`, `map_try`, `walk_try`, `keep_if_try` and the `Result.*`
    functions, whose callbacks return tags the inliner cannot build unboxed;
  - `Dict.update`, `List.update`, `List.sort_with` and `Str.walk_utf8`;
  - `List.for_each`: an inlined body has no effects, so a looped `for_each` would do nothing.
  A callback that cannot inline (it calls a function, returns a non-scalar, or reads a defaulted
  formal of the caller that has not been forced yet) stays generic too; a guarded fast call for
  those is the next step. No test builds an ALTREP list, which R code cannot create. The stdlib
  fuzz feeds Arrays instead, and they fail the same `ALTREP_FLAG | ARRAY_FLAG` test in the loop's
  type guard, so the hand-back to tier 0 is covered but a genuine ALTREP list is not.
- When a constrained type parameter fails (`List.sort_asc(list("b", "a"))`), the mismatch says
  "expected `list[T]`, found `list{character, character}`" without naming the `scalar numeric`
  constraint. Belongs to the checker diagnostics work.

## Open: standard library rows slower than GNU R

`just bench-stdlib` compares each function with its fastest R equivalent at n = 1e6, and
`tools/bench/stdlib/results.md` has the numbers. These rows are still slower, grouped by cause.

- **Rerun the full bench.** `tools/bench/stdlib/results.md` predates the materialized-double R
  inputs and the Array paths of `List.min`/`List.max`; regenerate it with a full `just bench-stdlib`
  (about 75 minutes) and update the rows below.
- **One string call per element against one vectorized call.** `Str.starts_with`,
  `ends_with`, `contains`, the `trim`, `replace`, prefix and suffix functions, `repeat`,
  `with_prefix`, the case conversions, `split_first`/`split_last` and the UTF-8, 16 and 32
  conversions run through `List.map` or a closure, one builtin call per string with a boxed
  result each (about 400 to 1,200 ns), while `startsWith`, `grepl(fixed = TRUE)` or stringi make
  one C call over the whole character vector (15 to 350 ns per string). A leaf callback already
  skips building a call (`decisions.md`, "The standard library is measured against GNU R"), and
  the leaves borrow their input. The next step is a kernel per unary `Str` leaf that `List.map`
  and the predicates run straight over a character Array's storage into one result vector, then
  the same inside compiled loops.
- **A callback loop entered from tier 0.** `List.walk`, `walk_until`, `walk_with_index`,
  `walk_backwards`, `walk_from`, `for_each`, `Dict.walk_until` and `Str.walk_utf8` are 1.1 to
  3.5 times GNU R's `for` loop when called once from code that is not compiled: the loop only
  compiles inside a compiled body (`decisions.md`, "Library traversals compile to loops"), so
  a one-shot call runs the callback as a full closure call per element (about 3,800
  instructions: a frame, argument binding, entering the compiled body, loading the arguments
  back from the frame). Fix: let the tier-0 builtin ask the JIT for the traversal loop over the
  callback once the list is long enough to pay for compiling it.
- **A callback against a vectorized whole-vector answer.** `List.sort_with` calls its comparator
  about n log n times (12 s against `order` at 1e6), and `Dict.update` counting words calls its
  callback once per key (3.6 s, linear, against `tabulate(match())`). The callback API cannot
  reach one C call; sort by a key with `List.sort_asc` where the order is numeric.
- **One lookup per key against a vectorized index.** `Dict.get` and `Dict.contains` in a `for`
  loop pay an interpreted iteration, a builtin call and a `Result` per key (about 1 to 1.9 us),
  against `v[keys]` or `keys %in% names(v)` as one call. A bulk lookup function would close it.
- **Boxed lists against atomic vectors.** `List.range`, `from_vector`, `concat`, `prepend`,
  `reverse`, the `take`/`drop` family, `drop_at`, `split_*`, `starts_with`/`ends_with`,
  `join`, `concat_utf8`, `sort_asc`/`sort_desc` and `min`/`max` on a list copy or compare
  boxed elements where R copies or scans 8 bytes per element (and `seq_len` is a compact ALTREP
  sequence that costs nothing). They are 1.1 to 2.4 times R; on an Array the same functions
  work on unboxed storage.
- **Quadratic, by design for now.** `Dict.remove` rebuilds the dictionary per call, because a
  `Dict` keeps insertion order and removing one entry in place shifts every later entry and
  renumbers the index. Tombstones (a removed marker plus a live count, compacting when half
  the entries are dead) would make it O(1), but every reader of the key vector (`Collection`,
  `entries`, `collection_parts` in compiled loops) must then skip them. `List.replace` returns
  a record, so `r <- List.replace(xs, i, v); xs <- r$list` leaves `xs` held twice and every call
  copies; in-place reuse would need the reuse analysis to see through the record. `List.get`
  followed by `List.set` updates in place.
- **Small fixed costs.** `List.len`, `Str.is_empty` and `Str.count_utf8_bytes` per element are
  1.7 to 6 times R, which is the builtin call against R's `length()` or vectorized `nchar`.

## Open: Arrays

`decisions.md`, "An Array is a flagged R vector". Known gaps:

- The runtime checks that atomic items share one type but not that ry-value items do, so
  `[(x = 1), [1]]` builds at run time; only the checker rejects it.
- `List` functions added to `standard_library.rs` need an Array overload in `types/std.Rtypes`, and
  any that reuse their first argument's storage must not reuse an Array's.
- `class(xs)` is R's implicit class (`numeric`), and serialization (`saveRDS`) drops the flag, so
  a reloaded Array is a plain vector.
- C code can write `NA` into an Array through `REAL()` or `INTEGER()`; nothing re-checks it.
- The JIT and the bytecode fast paths treat an Array as an unknown value. A dedicated feedback kind
  would let compiled loops run strict arithmetic without the generic call.
- The empty `[]` is a flagged `logical(0)` at run time, so its item type is known only statically.
- An integer overflow in an Array operator prints R's "NAs produced by integer overflow" warning
  as well as the Array error, because the check runs after R's operator. Muffle the warning when the
  result is rejected.

## Open: strict mode's top-level rule

The rule ships (`decisions.md`, "A strict library file's top level holds only definitions"). Follow-ups:

- **Higher-order calls.** `lapply(1:3, function(i) i * 2)` is refused, because `lapply` is not pure for every callback. A constant could accept a higher-order call whose function arguments are pure, which needs a per-parameter marker in stubs.
- **Calls to functions bound inside `local()`** are effects today, even when the local function is pure. Accept them when the local binding is a function literal whose body has no effect.
- **`@pure` on a non-binding** (`#: @pure` above `cat(1)`) is ignored silently; warn instead.
- **Evaluate pure constants once and cache them** with the bytecode, as Roc evaluates top-level constants at compile time.

## Open: strict-mode friction from typing finbench

Porting the synthetic finance benchmark to fully typed strict code hit these. Each repro is a project with `[check] strict = true` (and `Imports:` naming the packages it uses); the quoted output is what `ry check` prints.

- **A call returning a record inside `expect_error` is an error.** With `g <- function(n: integer) -> Record{x: integer}`, `expect_error(g(-1L), "neg")` reports "`expect_error` is an R function, and this value is a ry record, not an R object", although `expect_error` only evaluates the expression and never keeps its value. The typed bench tests work around it with `expect_error({ g(-1L); NULL })`. The fix is to let a parameter the callee only evaluates or quotes accept native values (testthat's `expect_error`, `expect_warning`, `expect_message`, `expect_silent`).

- **`data.frame` and `data.table` columns are `Unknown`.** Both constructors type as their opaque nominal, so every `$` read is reported, and an annotation on the enclosing binding does not cover a read inside it; each column needs its own annotated binding, the largest annotation cost in typed tests. Track columns as `DataFrame{...}` does.
  ```r
  #: fn() -> double
  f <- function() {
    dt <- data.table::data.table(pnl = c(1, 2))
    sum(dt$pnl)
  }
  ```
  Reports `Unknown` twice, on `dt$pnl` and on the `sum(...)` around it.
- **`ry check --annotate` writes top-level functions and bindings only.** A lambda passed as an argument (`vapply(x, function(v) v * 2, numeric(1))`) keeps no annotation, so the last unannotated functions in the benchmark code bases, nearly all callbacks to `vapply`, `lapply`, `tryCatch` and `purrr`, were annotated by hand. Write the lambda's contextual signature when the callee's parameter type fixes it.
- **The stats distribution functions return `double[]`.** `pchisq`, `pnorm`, `dnorm` and their siblings in `types/stats.Rtypes` claim a vector, unlike base's scalar-claim convention, so `function(q: double) -> double stats::pchisq(q, df = 1)` is a `type-mismatch` (`expected double, found double[]`) and a p-value lambda must return `double[]`. Moving them to the scalar claim needs the corpus false-report count.
- **An annotation cannot attach to a lambda argument** in `#:` comments (inline syntax can annotate one). Every callback whose parameter needs a type, or whose callee types it `Any` (`apply`, `uniroot`), must first become its own binding. Let a `#:` line inside an argument list annotate the argument that follows.
  ```r
  #: fn(m: matrix) -> Any
  f <- function(m) apply(
    m, 1,
    #: fn(row: double[]) -> double
    function(row) sum(row)
  )
  ```
  Gives `` A `#:` typing comment annotates the statement that follows it, and an argument is not a statement, so this one annotates nothing. `` plus the `Unknown` on `sum(row)`.
- **A lambda default gets no type from its declared parameter.** The parameter's declared function type should be the context for its default, as it is for a call argument.
  ```r
  #: fn(dsn: character, [connector]: fn(dsn: character) -> character) -> character
  connect <- function(dsn, connector = function(dsn) toupper(dsn)) connector(dsn)
  ```
  Gives `` strict mode: parameter `dsn` has no type. `` on the lambda's `dsn`.
- **Stubs cannot declare a structural nominal or a subtype.** `@type DEoptim {list{bestmem: double[]}}` and `@type fs_path : character` both give `... is not a valid type name.`, so S3 list results become aliases that lose their class, and `xts` (class `c("xts", "zoo")`) is not a `zoo`: `#: fn(x: xts) -> zoo` over `f <- function(x) x` gives `` expected `zoo`, found `xts` ``. readr's `tbl_df` and fs's `fs_path` are declared as their parents for the same reason.
- **One flat stub name map.** With rugarch imported, its `sigma` overloads replace `stats::sigma` even for a qualified call: `#: fn(fit: Any) -> double` over `f <- function(fit) stats::sigma(fit)` gives `` expected `double`, found `matrix` ``. Needs per-namespace stub tables.
- **`!!!` splice is `Unknown`.** In the `...` position it should splice the elements' type. `httr2::req_url_query(httr2::request(url), !!!query)` with `query: list[named: Any]` reports `Unknown` on `!!!query`.
- **foreach's `%do%` body does not see the iterator.** `foreach::foreach(g = groups, .combine = c) %do% sum(g)` with `groups: list[integer[]]` reports `Unknown` on `sum(g)`. A stub attribute that binds a call's named arguments in the right operand of `%do%`/`%dopar%` would fix it.
- **A chained `$` is reported at the innermost access, and `@if-unknown` on the statement does not clear it.** The user ends up annotating an intermediate they never named.
  ```r
  #: fn(v: environment, name: character) -> character
  get_secret <- function(v, name) {
    #: @if-unknown character
    out <- v$secrets$get(name)$value
    out
  }
  ```
  Reports `Unknown` on `v$secrets` only.
- **The `NULL`-default suggestion prints an unparenthesized function type.** `-> character | NULL` parses as the return type, which is right, but the fix it suggests is unparseable and pasting it reproduces the error; the printer must write `(fn(x: character) -> character) | NULL` inside a union. `#: fn(x: character, [f]: fn(x: character) -> character | NULL) -> character` over `apply_or <- function(x, f = NULL) if (is.null(f)) x else f(x)` gives `` `f` defaults to `NULL`, which its declared type `fn(x: character) -> character | NULL` does not admit. ... Declare it `fn(x: character) -> character | | NULL | NULL` ``.
- **zeallot `%<-%` targets are silently untyped.** The targets are neither typed from the right-hand side nor reported, so a wrong return type passes.
  ```r
  #: fn(x: double[]) -> character
  spread <- function(x) {
    c(lo, hi) %<-% list(1, 2)
    hi - lo
  }
  ```
  With `importFrom(zeallot, "%<-%")`: `1 file checked, no problems`.
- **A body annotation cannot name the signature's type parameter.** Bring the enclosing signature's type parameters into scope for annotations in the body. `#: <T> fn(fn: fn() -> T) -> T | NULL` over a body with `#: T | NULL` above `result <- tryCatch(fn(), error = function(e) NULL)` gives `` I do not know the type `T`. ``
- **No record width subtyping.** An injected fake with extra fields is refused, so a dependency cannot be typed as "any record with at least `tick`". `#: @alias Ticker {list{tick: fn() -> NULL}}` and a call `run(list(tick = function() NULL, finished = TRUE))` give `` this list has a field `finished`, which is not expected here ``.
- **A mismatch message leaks an uninstantiated type variable.** Against a field typed `fn([len]: double) -> Any`, `list(tick = function() invisible())` gives `` expected `fn([len]: double) -> Any` for field `tick`, found `fn() -> T` ``; the found type should not print the bare `T` of `invisible`'s signature.
- **Recursive aliases are refused**, so chaining methods (lgr setters, `progress` `tick()`) return `Any`. `#: @alias Logger {list{set_threshold: fn(level: character) -> Logger}}` gives `` Type alias `Logger` expands in a cycle. ``, filed under `type-mismatch` and underlining the whole function that uses it. Allow recursive record aliases, or a `Self` type for R6-style records.
- **No typed mutable environment.** A counter in `new.env()` cannot declare its fields: `calls <- new.env(); calls$count <- 0L; calls$count <- calls$count + 1L` reports `Unknown` on each `calls$count` read.
- **S4 slots and generics are untyped.** A slot read is `Unknown` even on `Any` (`out <- b@v` with `b: Any` gives `` could not determine the type of `out` ``), slots of a class with a `representation` are not typed, and a generic made by `setGeneric("scale_by", ...)` is silently `Any` at its call sites (`out <- scale_by(q, 2)` returned as `double` checks clean).
- **R6 method bodies are unchecked.** In `R6::R6Class("Counter", public = list(add = function(k) { self$n <- self$n + k; private$check(); invisible(self) }))` the untyped `k`, `self` and `private` produce no strict report: `1 file checked, no problems`.
- **The summary counts checked files, not files with problems.** A project with one finding in `R/a.R` and two clean files prints `1 problem in 3 files`.
- **Vectorised math types are wrong.** `exp`, `sqrt` and `nchar` return a scalar for any input, and there is no `array` type (an `array` annotation suggests `Array`, ry's own type). `#: fn(x: double[]) -> double` over `exp(x)` or `sqrt(x)` checks clean, as does `#: fn(x: character[]) -> integer` over `nchar(x)`; `#: fn(rates: matrix) -> matrix` over `exp(-rates)` gives `` expected `matrix`, found `double` ``.


## Open: the JIT

`crates/jit` compiles hot bodies and loops (`decisions.md`, "the JIT tier"). In priority order:

- **Typed natives, the rest of the end shape** (`decisions.md`, "Typed closures run as native
  functions over records, tags and match"). Typed closures run as natives at their first call over
  scalars, plain vectors, records, tags and `match`. Not built, by expected gain:
  scalar replacement of records that never escape a native (each construction allocates today);
  vector results and vector builtins (`c`, `numeric`, `sum`) inside natives; strings and the
  standard library (`Str`, `List`, `Dict`) inside natives; uniqueness in-place update of a record
  or vector whose count is one; recompiling when a slot profile widens (a mixed profile refuses
  for good); guards, `|`, `..`, literal and nested patterns in `match`.
  The why-ry programs that gain nothing typed show the order: bond_book needs `seq_len`, `rep` and
  vector results, report needs `paste`, closures needs captured variables (`a * scale`).

- **Compiled-to-compiled calls of general functions.** A closure whose body is scalar arithmetic,
  comparisons, `if` and calls to other such closures compiles to a frameless native function
  (`decisions.md`, "Native scalar functions"), so `fib(30)` runs in 0.007 s against 1.12 s on GNU R.
  Every other call from compiled code still goes through `jit_call`: an environment, a binding cell
  per formal and the boxed arguments, and about 440 instructions per formal read (`jit_load`).
  Natives now loop, read plain vectors and inline small native callees (`decisions.md`, "Native
  functions loop, read vectors, and inline small callees"). Next: local closures (`Ty::Closure` is
  only inlined today), vector results and `Array<T>` arguments (`annotations::plain_admits` rejects
  an Array mask), then a direct entry with no environment for non-native compiled callees whose
  frame cannot escape.
- **Native function cache lifetime** (`crates/jit/src/native.rs`, `native_for`). A native roots
  its closure for the session, so a helper defined inside a function keeps that function's frame
  (and every large local in it) alive. A recompile after `still_resolves` fails leaks the old
  `Native` and roots the closure a second time. Refusals are never retried, so mutual recursion
  (`ev` calls `od` calls `ev`) is refused for good: the inner call hits the `COMPILING` cycle guard.
  Compile a strongly connected group of functions together instead, and root closures weakly.
  Typed closures compile at their first call, so a typed closure created per iteration
  (`lapply(xs, function(i) { g <- function(x: double) x + 1; g(i) })`) compiles and roots once per
  closure and reaches `MAX_NATIVES` (4096), after which natives are off for the session. Key the cache
  by the closure's body and formals, not the closure, and pass the environment at call time.
- **Native recursion has no machine-stack check.** It is bounded only by `max_depth`
  (`jit_depth_budget`). That is safe today only because `options(expressions =)` never raises
  `max_depth` once the option exists (`set_option` in `crates/interp/src/envir.rs` returns early)
  and tier 0 itself overflows the machine stack at a depth of about 2,000 to 3,000. Fix both, and
  give native entry a stack-limit check, before raising the depth limit.
- **Tier 0 makes `return(y <- x)` invisible.** GNU R and native functions keep it visible.
- **Records, tuples and tagged unions in native functions.** `Intrinsic::Field` reads a field from
  compiled bodies; natives refuse them. A layout id fixes kind, field names, order and slot count,
  but whether a slot is stored unboxed is per object (only length-1, attribute-free, non-ALTREP
  double, integer or logical values, and never slots at index 64 or above). Key a native on layout
  id plus per-slot feedback and guard each read on the mask bit at `RECORD_UNBOXED_OFFSET` and the
  NaN-box prefix, as `Intrinsic::Field` does. Records are immutable (`$<-` copies), so one guard
  holds for the object's lifetime.
- **Calls dominate the benchmark packages' test suites.** A `perf` profile of full-size dsbench's
  testthat suites (JIT on) puts 12% of samples under `jit::`, and about 85% of that is
  `jit::runtime::jit_step` handing a call back to tier 0. Each closure call there still matches
  arguments, allocates an environment, defines each formal and makes a promise per argument, which
  together with the collections they trigger is most of the 13% spent in allocation and GC.
  Global function lookups (`bytecode::lookup_function`, `eval::find_fun`) walk 3 to 5
  environments with a hash probe each (`Heap::frame_cell`, 3.7% self): an inline cache checked
  against the binding epoch, or a direct cell for namespace and import lookups, would make that
  one load. `bytecode::step` dispatch is 6.6% self. Reproduce with `tools/bench/generate.py` and
  `just bench-compare`.
  Microbenchmarks against GNU R 4.3 (JIT on) show the per-call gap: 200k closure calls take
  190 ms against 83 ms, 50k S3 dispatches 100 against 59, 20k `tryCatch` 415 against 102, and a
  two-expectation `test_that` block about 60 ms against 28. The likely wins, in order: no binding
  cell or promise per argument for a closure whose frame cannot escape, a cached global function
  lookup, and less `Heap::root` and handle traffic per call.
- **Static types as early profiles** (`r-runtime.md`, "How the static types feed the JIT"). A
  direct call's result type comes from the callee's declared return type when the call site has no
  feedback yet (`call_result_at` in `crates/jit/src/analysis.rs`), so typed `fib` compiles on its
  first try, and typed Arrays compile to direct item loads and stores (`decisions.md`, "Arrays are
  their own JIT types"). A native call skips the argument and return checks because its
  parameter types satisfy the callee's masks at compile time (`annotations::return_admits`); other
  compiled calls still run `annotations::enter` and `check_return`. Typed and untyped code run
  equally fast: the profile proves the same facts the annotations do.
  Not started: passing typed scalars and vectors unboxed across calls between typed closures, and
  inlining small typed closures (not only natives) at their call sites. Strict mode keeps the
  guards either way, because a strict-clean package is a hint, not a contract.
- **Free invalidated code.** The machine code and `Compiled` box of an invalidated body are leaked,
  and invalidated external pointers stay rooted.
- **Closures from one site that speculate on an enclosure-local function compile for one
  enclosure only.** Since such closures share one body, code that is not portable (it calls a
  helper defined in the parent function's frame) runs for the first enclosure, every other
  enclosure stays in tier 0, and each recompile after the first enclosure dies spends one of
  the body's compiles. Per-enclosure code for a shared body, or speculating on the helper's body
  rather than its binding, would fix it. Alternating enclosures of portable code also re-walk
  every speculation on each switch.
- **Recompile churn.** The global binding epoch invalidates every speculation when any watched name
  is defined in a function frame, so a program that reuses a common function name as a local
  variable recompiles up to the per-body cap. Count invalidations per body before tightening it.
- **First-call compiles stop at multi-index subscripts and unknown builtins.** A fully typed
  closure compiles at its first call, and base builtins with a known result type (`seq_len`,
  `sum`, `sqrt`, `[` on a vector, element-wise arithmetic; `analysis.rs`, `builtin_result`) no
  longer block it, so `vasicek_paths` compiles at its first call. A builtin outside that table
  whose result feeds arithmetic, or `m[i, ]`, still declines as COLD until feedback arrives.
- **ry's tier 0 trails GNU R on whole test suites.** Typed beats untyped at runtime (finbench
  30-file slice, callgrind, analysis excluded: typed 71.5G against untyped 72.1G with the JIT on,
  71.7G against 73.7G off), but wall clock is level with GNU R or up to 15% behind (finbench about
  105 s on GNU R). The gap is interpreter cost spread across all test families, not the JIT. With
  the JIT on, a 3-file finbench slice runs 0.7% more instructions than with it off, 0.4% of it
  Cranelift, and wall clock is 1 to 3% slower, within run-to-run noise; disabling every compile
  but the first-call ones closes it, so the residue is hot bodies whose compiled code does not
  pay for its compile. Raising the entry threshold to 10,000 or 30,000 showed no clear win. A
  cold strict check of the typed package costs about 4G instructions (0.5 s) once per binary,
  and a first strict `ry load` of finbench about 1.3 s more than a warm one.
- **Vasicek's remaining per-step cost** (87k instructions against GNU R's 118k): about 7.6k in
  `complex_assign` beyond the subassign itself, 13k in scalar and vector arithmetic, 3.7k for the
  `stats::` lookup, and 7k for the `rnorm` R closure around the builtin.
- **Run the oracle against a release build** with `ORACLE_RY=target/release/ry` after building
  both `ry-lang` and `libr`: the JIT lives in `libR`, so building `ry-lang` alone tests a stale JIT.
- **Base gaps seen while benchmarking:** `sum()` of doubles does not follow GNU R's NaN-before-NA
  precedence exactly; a factor inside a list prints its codes;
  `cov` and `sd` route through an R-level fallback instead of a native `C_cov`; `pkg::name` for a
  name the namespace does not export gives a different message from GNU R.
- **Strict users must annotate `seq()` results.** The `seq` stub returns `Any`, so
  `x <- seq(...)` passes strict mode only with an annotation such as `x: double[]`. A precise
  overload set for `seq` and `seq_len` would remove that.
- **The oracle's `stats__*` cases depend on the BLAS build.** Their expectations come from
  reference BLAS; under OpenBLAS, `stats__formulas_and_least_squares` and
  `stats__models_and_tests` differ in the last printed digits and `stats__lbfgsb` warns in
  `HoltWinters`, so `recorded_outcomes_match_gnu_r` fails there.
  Force reference BLAS with
  `R_LD_LIBRARY_PATH=/usr/lib/R/lib:/usr/lib/x86_64-linux-gnu/blas:/usr/lib/x86_64-linux-gnu/lapack`
  (Debian's `ldpaths` otherwise puts the OpenBLAS alternative first), or make those cases print
  rounded values. The `datatable__*` cases also need data.table in GNU R's library.

## Open: data-oriented layouts and explicit allocators

The rule and its limits are in `decisions.md`, "Data-oriented design and explicit allocators". This
is the audit list, highest expected win first. The layouts below were read from the code; every
expected win is unverified until measured, so profile before and after each change, and revert a
change that does not pay.

1. **The checker's types: a per-item store instead of interning every intermediate type.**
   Measured, it is not the structural cut it looked like. A cold check of the finance bench interns
   only 2,362 distinct types (about 0.3 MB, so the table is no memory problem), and interning,
   `TyKind` hashing and equality together are about 2.5% of cold-check samples; `resolve_rec` is
   another 2%, and content flags already let it skip variable-free types. A per-check SoA store in
   the style of Roc's `Subs` would win at most those few percent, so it waits until a profile
   shows the type table on top. The remaining cost of item checking (about a third of samples) is
   spread over `infer`, call matching, formal-use collection and salsa reads.
2. **The JIT's analysis plan.** Done: flat state rows, bitset liveness and a bump arena
   (`crates/jit/src/analysis.rs`).
3. **Side mark bitmaps in the collector.** Done (`decisions.md`, "Small objects live in bitmap
   pages").
4. **The allocation fast path.** Done: a bump cursor per size class, inlined, with the OLD
   header bit for the barrier and a batch `Heap::reserve` for call frames (`decisions.md`, "The
   young generation allocates by bumping a cursor"). The interpreter's closure call adopting
   `reserve` is the next cut.
5. **Scratch storage for argument matching.** Done with inline vectors instead of a scratch
   stack: `match_args` returns `eval::Matched` and calls fill an `eval::Args`, both `SmallVec`s,
   so a typical closure call allocates nothing on the Rust heap (`decisions.md`, "the
   interpreter's call path, lookups and allocation").

- **Columnar record Arrays.** Store `Array<record>` as one buffer per field once the JIT fuses `xs[[i]]$field`; decided and deferred in `decisions.md`, "Array storage stays row-major". Measure a million-item loop both ways first.
- **Typed Array loops trail untyped ones slightly.** `python3 tools/bench/why/run.py --filter array` (GNU R / ry untyped / ry typed and strict, seconds, 4-core Xeon): array_dot 0.188/0.021/0.022, array_filter 0.167/0.022/0.026, array_map 0.247/0.038/0.041, array_sum 0.116/0.024/0.027. The JIT compiles Array element loops natively, but the typed program still pays 5 to 15 percent over the plain-vector one; find where (the strict operator check, `as.Array`, or the annotation checks) before calling Arrays a speed feature.

## Open: performance and memory

An independent review profiled before it proposed. Every number below was taken on a 4-vCPU
container with other work running, so treat each as an upper bound. Every in-container timing is
effectively a two-core number.

### A first strict `ry load` costs the strict check's wall time

- On typed finbench (4 cores), a load with no usable check record takes 1.6 to 1.9 s at full size, against 0.4 to 0.9 s untyped and 0.6 to 0.7 s after `ry check`; at 60,000 lines it is 0.45 to 0.55 s against 0.25 to 0.35 s untyped (`decisions.md`, "The strict load check analyses only the package"). The check skips tests and scripts and frees its memos off the verdict path, so what is left is checker CPU: at 60,000 lines the check alone is about 0.85 CPU-s (parse about 100 ms, `item_annotations` 63 ms, `file_annotation_references` 48 ms, folds 43 ms, parse-stage diagnostics 40 ms, HIR 39 ms, lints 37 ms). Lowering the thread's priority and changing the worker count were measured without gain, so the lever left is the analysis core's own speed. At full size the loader reports about 1.26 s, most of it waiting for the verdict, and the check of the 502 package files alone costs about 3.0 CPU-s (`ry check R --no-cache` takes 0.9 to 1.0 s; an `R/`-only copy of the project costs the same, so tests are already out of it). A `perf` profile of that check is flat: salsa bookkeeping (`QueryEdge` inserts, `SyncTable::try_claim`, `add_read`, interning) is about 21% of self time, mimalloc and page faults about 12%, the lexer and tree building about 6%, and no checker function exceeds 2%. So the next cut is structural: fewer tracked queries per item (each of the 50,000 items mints about a dozen memos, plus per-name `frame_binder`, `package_name` and `global_scheme` memos), not a hot loop.

### Corpus reports that more precise types now reach

- `if (identical(info, NA)) stop(...)` does not remove the `NA` member, so httr's `oauth_listener(url)$code` reports `$` on `logical | list[Any] | NULL`. Removing `logical` is unsound for `TRUE` in general, so it needs a literal `NA` member or a sentinel rule.
- knitr's `split_file()` infers a `character` member in its groups, so `lapply(groups, function(g) g$params)` matches no overload (`R/output.R`).
- gt's `vapply(..., FUN.VALUE = character(1))` callbacks return `character | NULL` (`R/fmt.R`, `R/helpers.R`, `R/utils_render_latex.R`).

### Writes inside a test block enter the package namespace, and that is a correctness question

This was framed as the interface fixpoint being superlinear in the size of the cyclic definition
group. It is not. Profiled on `targets` 1.12.0, where `ry check` took 16.6 s on the JSON path so the
reporter was not involved, `analysis-stats` attributed 16,341 ms of the 16,681 ms typecheck to one
245-line file, `R/class_active.R`.

What it scales with is not the file count. 284 package files cost 0.35 s. Adding 238 unrelated
package files costs 0.37 s. Adding the 238 `tests/testthat` files costs 16.6 s, and reclassifying
those same files as scripts brings it back to 0.99 s. Bisecting by test-file count gives 384 ms at
zero, 1.6 s at 30, 3.7 s at 60, 9.7 s at 120 and 22 s at 238, which is about 90 ms per added test
file.

The mechanism comes from per-item execution counts and times. Query executions grow only 3.6 times
while wall time grows 25 times, so re-execution volume is not the cause. One statement item was
re-executed 139 times and its own cost grew with the file count. A write inside a `test_that` or
`tar_test` block binds at the item's top level, because a bare `{...}` is not a scope, so
`conditional_slot_items` publishes every one of them as a package-namespace conditional slot. A
cross-file deferred read of a common local name such as `out` or `envir` then joins over every
conditional writer of that name project-wide, which is hundreds of statement items across 238 files,
and each join needs that item's check, which drags in the R6 record type.

Two causes were ruled out by direct experiment, so nobody should repeat them. It is not the fixpoint
round cap, because setting `SCHEME_ROUND_CAP` from 16 to 2 changed 9,690 ms to 9,760 ms. It is not
the type-size ceiling, because lowering `TYPE_SIZE_CEILING` from 100,000 to 2,000 changed nothing.

The performance half is fixed, and the ledger records both parts. What remains is the semantics.
A write inside a `test_that` or `tar_test` block still enters the package namespace, which is wrong
for those callees.

**The rule has to key on the callee, and R decides it that way.** This was checked rather than
assumed.

- The block's write does bind outward for `suppressWarnings({v <- 1})`, `invisible`, `system.time`,
  `try` and `withCallingHandlers`, because a promise is forced in the caller's frame. A blanket rule
  would manufacture a false `unresolved` finding on `try({cfg <- read()}); use(cfg)`.
- It does not bind for `local({v <- 1})`, or for the `eval(substitute(b), new.env())` pattern that
  `test_that` uses.

The honest options are a known-verb list, covering testthat's `test_that`, `describe` and `it` and
matching the existing `library`, `on.exit` and `local` precedent, or a stub annotation in the vein of
`@masked`. A verb list alone is not enough for a real project, because `targets` wraps `test_that` in
its own `tar_test`, and hardcoding a package's private wrapper is not a rule.

One blocker comes with whichever is chosen, and it is measured. Scoping those blocks adds 87
`unused` warnings to `targets`. They are a mix of genuine dead stores, such as
`expect_silent(tmp <- f(x))`, and cases that only look dead because a name is used in a nested
closure. That needs its own answer before the change can land.

### Memory, and the rest of the package-path measurements

A review-authored synthetic of 1,550 files, 277,586 lines and 14,771 items reported 55.9 s and
5,488 MiB peak as package documents against 5.95 s and 343 MiB as scripts, superlinear in file count
at 6.1 s for 400 files, 10.8 s for 800 and 68 s for 1,550. **That does not reproduce, and the
generator's shape was never recorded.** A fresh synthetic package of 1,500 files, 42,000 lines and
10,500 items, holding five functions plus a shared top-level conditional write per file, which is
the shape the join punishes hardest, costs 0.49 s and 111 MiB peak after the bound and 0.85 s
before it. Treat the old figures as unverified unless someone reconstructs the generator.

Other packages move much less between the package and script shapes, at 1.70 against 1.21 for
ggplot2, 0.72 against 0.58 for dplyr, and 0.65 against 0.59 for shiny. That fits the cause above,
because they have far fewer test-block writes landing in the namespace.

Sampling had put the time in cycle bookkeeping around `statement_binding_scheme` and `item_check`,
with 54 of 72 sampled thread stacks in `DependencyGraph::block_on` and `targets` getting no parallel
speedup at all, at 17.43 s and 17.81 s on one core against 17.43 s and 16.99 s on four, while
ggplot2 managed 1.15 times. That was a symptom of the unbounded conditional-slot joins, and bounding
them fixed the parallelism too. Re-measured with `taskset` pinning core counts, best of two:
`targets` goes 998 ms to 778 ms to 599 ms at one, two and four cores, which is 1.67 times;
data.table goes 420 ms to 309 ms to 278 ms, which is 1.51 times; ggplot2 goes 1,388 ms to 1,255 ms
to 1,033 ms, which is 1.34 times. Read those against the container's ceiling, because its 4 vCPUs
deliver only about 1.8 times native compute at four threads. `targets` is therefore at roughly 93%
of what is achievable here, and a number from real hardware would be worth having.

Two facts about the fan-out are worth knowing. `check` fans out over `available_parallelism()` while
`fmt` is a plain loop, measured at 646 ms on one core against 644 ms on four. There is also no flag
to control concurrency. `available_parallelism()` honors CPU affinity and cgroup quotas, so
`taskset -c 0-N` is the only lever today. That is enough for measuring, and not something a user
would find.

The memory note of about 300 MiB at 302k lines holds for the script shape, at 343 MiB for 278k
lines, and is sixteen times off for the package shape. Memory attribution in the healthy shape is
parse at 82 MiB, lowering and naming at 158 MiB, typecheck at 47 MiB and diagnostics at 28 MiB. HIR
and naming dominate resident memory at rest, not interned types.

### Open from a fixed entry: the `BTreeMap` choice in `ItemNaming`

`resolutions`, `bindings`, `non_locals`, `quiet_reads` and `namespace_reads` are pure lookups with
no ordering requirement, and `BTreeMap::get` showed up under `infer_read`. Switching `item_hir` and
`item_naming` to `returns(ref)` removed the node-walk half of that cost, so what is left is lookup
only. Measure before switching, and check iteration order wherever any of these is walked to build
diagnostics.

### Judged fast enough, so do not invent work here

Single-package cold analysis is 1.9 s and 88 MiB peak for ggplot2's 68k lines, 1.3 s and 72 MiB for
mgcv's 37k lines after the chain fix, and 1.4 s for the 64k lines of `targets` as the instrument
classifies it. Parse is only 2% to 7% of the pass, at about 0.9 microseconds per line.

`item_spans` identity is clean. `item_span_positions` is a memoized index, so `item_node` finds an
item's span in constant time, and `item_spans` itself is consumed per file. Incrementality genuinely
holds:
every project reported zero item rechecks per keystroke and zero resolve steps, with edited-file
diagnostics at a 2.5 ms median on ggplot2 and a 21.3 ms median at 277,586 lines, which is inside the
stated bar. Re-anchoring is not a quadratic either, because `child_or_token_at_range` is a
binary search.

### Two measurement lessons from this review

**Measure both sides through the same output mode.** The review reported a fixed case at 0.07, 0.13,
0.26 and 0.56 s against baselines of 0.28, 0.61, 2.15 and 6.22 s. The fixed figures came from the
JSON path and the baselines from human output, so the reporter's own quadratic sat inside the
comparison.

**Split analysis from reporting before drawing a conclusion about either.** An earlier entry claimed
that position barely matters, from two 20,501-line projects with 500 findings each costing 6.9 s and
8.0 s. Those projects are analysis-bound, at 4.6 s for JSON against 41 ms for the reporter, so they
were never evidence about the reporter at all.

## Open: oracles, or what the fuzzers never ask

The next section measures what the fuzzers *feed* the code. This one is about what they *ask* of it.
A crash class shipped past every battery: an inference variable escaping the item whose table
created it. The output stayed well formed (ranges in bounds, output deterministic, incremental equal
to fresh), so all four batteries were green while every package in the pinned CRAN manifest leaked
such a variable and a quarter of them crashed `ry check`. An oracle that only asks "is this answer
well formed?" cannot see a wrong answer. These are the concrete follow-ups.

### Assert invariants in the code, not only properties in the harness

A `debug_assert` at a seam turns every input of every suite, fixtures included, into a test of that
invariant, at no release cost. The export-edge assertion (a closed scheme carries no inference
variable) landed with the fix. Candidates for the next ones:

- Every expression type an item records resolves in the table that produced it. This is the same
  defect seen from the other end.
- An item's declared-name range lies inside the item and spells the item's name. The fuzzer needed
  1,200 iterations to stumble on a jump that landed on the wrong token. This assertion would catch it
  on the first fixture that uses `->`.
- HIR expression ranges and naming binding ranges lie inside their item's range.
- The occurrences returned by references and rename are all spelled alike. The IDE battery checks
  this locally. Promote it so that every caller is covered.

### Add more relational oracles

The round trip from a definition to its references and back to the cursor is the only relational
oracle in the tree, and it found a wrong jump in ordinary R. Each of these costs one extra call and
needs no expected output:

- rename edits equal the references;
- hover at the definition target names the same symbol as hover at the cursor;
- completion at a name's first character offers that name;
- goto-definition is idempotent.

### Run the pinned CRAN corpus as a test, one project per package

`scripts/corpus-manifest.txt` pins 69 packages, but `corpus/` is gitignored and nothing fetches it.
The review in the next section found two arms silently contributing zero inputs for that reason.
Twenty packages found five distinct crashes in ten minutes of wall clock. Two details decide whether
the arm is worth running:

- Each package must be its own project, with one `ry.toml` and its own `R/`. The defects live on the
  interface between items, which a per-file arm cannot see. One shared database over every file
  distorts the diagnostics instead: `duplicate` explodes, as measured below.
- Typing and strict mode must be on, or most of the checker never runs.

### Make a corpus finding reproducible before minimizing it

The same package panicked on one run and not on the next. Item checks run across threads, and the
order in which items are visited decides whether a dangling id lands out of range. A delta-debugging
pass over a non-deterministic reproducer wastes an hour and can conclude the opposite of the truth.
The corpus arm should force a single thread, and a minimizer (file level first, then statement level)
belongs in `scripts/` instead of being rewritten for each crash.

### Mutate for the shape that found these, and keep the default budget cheap

The IDE round trip needs about 1,200 iterations to surface this class, and the default 300 never
does. Raising the default is the wrong lever, because that battery already costs 216 s. Every
failing input had the same shape: two tokens abutting with no separator, as in `"s"broken <- ...`.
The seed-mutation arm produces that shape by accident, and a grammar generator almost never does.
Add "delete a separator" as an explicit mutation, and run the high budget nightly.

## Open: what the fuzzers actually feed the code

An independent review measured the generators rather than reading them. Every number came from a
probe that reimplements each generator arm byte for byte, with the same RNG constants, seeds and
budgets, and from real `-C instrument-coverage` region counts. A default battery run generates
11,735 inputs, and 98.7% of the wall clock goes to the two batteries with the worst input quality.

### The format battery's 4,500 generated inputs add 8 regions of 2,951

Leave-one-out region coverage of `format.rs`, where the whole battery reaches 2,771 of 2,951:
dropping `fuzz_random_bytes` loses none, dropping `fuzz_seed_mutations` loses none, and dropping
token soup loses 4. Dropping all three as a block takes 2,771 to 2,763. `fixture_sources_hold_invariants`
alone covers 2,762 and contributes 410 unique regions.

The earlier figure of 14 in 1500 for the random-byte arm reaching the formatter body is confirmed,
and it is worse than it reads. Eleven of the 14 are empty or whitespace, so the arm formats a
program with at least one token three times in 1500, and never one with ten tokens.

Delete the random-byte arm. Cut token soup to about 200, because it is the cheapest source of
parser-error shapes at 33 of 35. Seed the mutations from the fixture corpus instead of from the 35
hand-written seeds.

### The generators cannot express most of the type system

Normalizing diagnostics to message shapes, all three semantics fuzz arms reach 60 shapes, of which
33 are parser errors, 9 are distinct `type-mismatch` shapes and none are lints. The program corpus
reaches 110 and the typing fixtures reach 80.

In 250 generated programs the annotation grammar produces `TYPE_REF`, `TYPE_FUNCTION` and
`TYPE_RECORD`, and no unions, binders, applications, vectors, `list[T]`, tuples, parens, optional
`[x]:` parameters or rest `...r:` parameters. The generator calls 6 of 872 declared stub names and
reaches 1 of 37 overload sets. No harness emits `library(...)`, so every conditional namespace and
the whole non-standard-evaluation ladder is unreachable, and nothing anywhere emits `setClass`,
`setGeneric`, `R6Class` or `new()`. `metadata.rs` sits at 7.63% of regions, and `lints.rs` produces
no findings in 250 programs.

### Grammar-directed generation, prototyped and measured

A 250-line recursive generator over the R grammar crossed with the `#:` grammar produced 1,500
programs in 0.338 s.

| metric | best current arm | grammar prototype |
|---|---|---|
| parses clean | 35.3% | 100% |
| formats with 10 or more tokens | 8.0% | 41.3% mutated |
| `parser.rs` regions | 87.27% | 93.46% |
| semantic regions across 9 files | 11,073 | 12,888 |
| diagnostic shapes | 60 | 95 |
| distinct `type-mismatch` shapes | 9 | 17 |
| lint shapes | 0 | 5 |
| `ide::type_definition` hits | 0 of 7,884 | 80 of 23,826 |

Add it alongside the existing arms rather than replacing them, because token soup still owns 24
parser-error shapes. Semantic coverage then goes from 11,073 to 13,306 regions. The typing fixtures
still lead at 14,580, so the corpus beats synthesis and both beat noise.

### The best inputs are already in the tree, and only one crate uses them

`fixture_sources_hold_invariants` exists only in `format`. The same 388 typing-fixture sources run
through the semantic pipeline reach 86.92% of `check.rs` and 80 shapes, against the entire generated
battery's 54.35% and 60. Wire it into `syntax`, `semantics` and `ide`, and use both corpora as
mutation seeds rather than only as fixed inputs.

The program-corpus arm as landed deserves one fair criticism. It runs one shared database over all
1,967 files, calls `file_diagnostics` only, and asserts never-panic plus range geometry. That shared
project changes what is tested: `unresolved` collapses from 284 to 148 while `duplicate` explodes
from 20 to 2,182, because 1,967 unrelated files redeclare each other's names. Run per file with the
full battery and it reaches 110 shapes. Fix it by batching into projects of a sane size and running
the full `check_semantics_invariants`. The cost is fresh databases, at about 9 ms each in release
and 113 ms in debug, so this wants the batteries in release or a lower count of databases per input.

### The IDE battery costs 216 s and never reaches the type-driven features

`type_definition` returns `Some` zero times in 7,884 offsets. That is structural, because it needs a
`TyKind::Named` and no IDE seed declares a `@type`. `signature_help` fires at 0.5%, and 86.7% of
inputs do not parse. Swapping the 10 hand-written seeds for grammar-generated programs that declare
and use nominals took `ide.rs` from 52.58% to 68.38% on fewer inputs. Cap a generated program at
about 10 statements, because a 150-input grammar sweep cost 81 s against 45 s for 300 tiny mutated
ones.

### The semantics incremental invariant never performs a small edit

The fuzz harness's `run_single_file` derives its edit by generating an unrelated program, so the
computed splice covers 97.3% of the old text on average and only one pair in 250 touches 10% or less.
`syntax::reparse` therefore takes the full-parse fallback essentially always, and the engine sees
whole-file invalidation every time. The splice-reuse path and per-item early cutoff, which are the
architecture's core claim, are never exercised. Derive the edit from the source instead, by
replacing or inserting one statement at a boundary.

### Smaller items, each with its measurement

- **The syntax edit stream degenerates, and its coverage is fine anyway.** No buffer of 400 parses
  clean, only 89 distinct shapes appear, and 60 steps lex to two tokens because an inserted `"`
  swallows the buffer. Yet `reparse.rs` sits at 96.12% against a grammar-directed stream's 96.44%.
  Reset the buffer every 20 steps or so and skip a near-empty one. Do not rewrite it.
- **Coverage-guided fuzzing has never run.** `cargo fuzz` is not installed, `fuzz/corpus/` does not
  exist, no CI job invokes it, and the root `cargo test` resolves to the product crate so
  `fuzz_deep` never runs either. Meanwhile the `REGRESSIONS` arrays, 21 in `format` and 2 in
  `semantics`, are documented finds from exactly that mechanism. `fuzz/` does still compile, in 83 s
  under `cargo +nightly check`. Judging coverage costs one component, because
  `rustup component add llvm-tools-preview` produced every number in this review.
- **Two arms silently contribute no inputs.** `corpus/` does not exist and nothing in CI or the
  justfile fetches it, so `fuzz_corpus_seeded` in `syntax` and in `format` returns early. That is
  about 1,050 budgeted inputs that never run, behind a skip line `cargo test` hides. It is the same
  "green means nothing ran" species the `FIXTURE_FILTER` guard already fixed.
- **Three surfaces have no fuzz coverage at all.** Generated `.Rtypes` text never reaches the stub
  loader, which leaves `stubs.rs` at 60.67%. `PackageMetadata` appears in no harness. The IDE arm
  only ever uses `DocumentKind::Package`.
- **The review's fourth item here was wrong, and the correction is the useful part.** It reported
  `syntax::literate::r_source_of_literate` as having no test of any kind. It has eight unit tests,
  and three of them already assert the byte-length invariant, including one for multibyte prose. The
  real gap was only that the invariant was pinned on four hand-written documents rather than over
  generated input, which the `syntax` battery now closes.

## Open: what each hour of fuzzing compute buys

A third independent review asked what protection each hour of compute and each minute of developer
time actually buys. Every number came from a 4-vCPU machine, in the debug profile unless stated.

The headline is that fuzzing is 327.3 s of a 672 s local gate, which is 49%, and CI runs none of it.
The rest of the gate was the since-deleted `legacy/` at 249.0 s, the shipping crates' fixture suites
at 68.8 s, and `crates/ry`, which is the only thing CI runs, at 22.4 s.

This review disagrees with the input-generation review above, and the disagreement is real. That one
wants the generated arms shrunk across the board. This one measures per-arm cost and wants `syntax`
and `format` grown, at 0.19 to 0.31 ms per input, while `ide` and `semantics` shrink, at 257 to
644 ms per input. Both conclude independently that `format::fuzz_random_bytes` should be deleted.

### The live CI runs 171 of the workspace's tests

The live workflow tests only `crates/ry` (`default-members`). Its staged replacement in
`.github/pending/workflows/ci.yml` runs `just gate`'s recipes over the whole workspace and no
ignored suite, so a human moving it into place closes this (`decisions.md`, "CI runs `just gate`
against GNU R 4.3.3").

### `ide::fuzz_deep` cannot be shortened

`ide::fuzz_deep` runs `iterations().max(5000)` sweeps. That `.max` floor means `FUZZ_ITERS` cannot
lower it, which is the bug. Measured in release on that exact input shape, which is 1 to 12
concatenated seeds averaging 214 bytes over 111 offsets per sweep, one sweep costs 665 ms, so five
thousand sweeps take 55.4 minutes on 4 cores. CI no longer runs ignored suites, so this blocks a
future scheduled deep-fuzz job, not the CI move.

### This is not a fuzzer, it is a fixed 498-program corpus re-derived at 124 s a run

Every generator seeds `SplitMix64` from compile-time constants with no entropy. Two runs of the
semantics generator produce an identical set of 498 distinct programs, while drawing 100,000 times
from the same generator reaches 87,203 distinct programs. The default budget samples 0.57% of its
own generator's reach and re-samples exactly that 0.57% forever.

Split the two jobs it conflates. Keep a small fixed-seed arm as the regression net it actually is.
Give the exploratory arms a `FUZZ_SEED` environment variable defaulting to random, print the seed on
every failure, and run them on a schedule rather than in the blocking gate. Do not randomize the
blocking gate, because a failure surfacing on an unrelated change is a worse trade.

### The syntax arm is the best asset in the test architecture and gets 0.5% of the budget

Distinct observable parser behaviors, counting error templates and node kinds, against iteration
count:

| syntax arm | 100 | 500 | 1500 (default) | 6000 | 20000 | last new behavior |
|---|---|---|---|---|---|---|
| token_soup | 67 | 98 | 109 | 124 | 132 | iteration 19,024 |
| random_bytes | 19 | 36 | 42 | 51 | 62 | iteration 17,012 |
| seed_mutations | 71 | 107 | 124 | 143 | 158 | iteration 18,253 |

All three at 20,000 iterations, which is thirteen times the default, cost 7 s total in debug and
were still finding new parser behavior at iteration 19,024. Cost per input across the battery is
0.19 ms for syntax, 0.31 ms for format, about 257 ms for semantics and 644 ms for ide. That is a
spread of 3,400 times, with the weakest oracle sitting on the most expensive input. Raise the syntax
budget to 20,000, which costs 7 s for 28% more distinct behaviors, and pay for it out of the
completion fix.

### The battery pays a debug tax of 6.1 times for identical assertions

| binary | debug | release | speedup |
|---|---|---|---|
| syntax test_fuzz | 1.58 s | 0.10 s | 15.8x |
| format test_fuzz | 2.31 s | 0.21 s | 11.0x |
| semantics test_fuzz | 123.68 s | 8.62 s | 14.3x |
| ide test_fuzz | 199.77 s | 44.40 s | 4.5x |
| total | 327.34 s | 53.33 s | 6.1x |

Building the four release test binaries costs 143 s warm.

The semantics figure has one root cause, measured directly. `install_shipped_stubs` is 0.4 ms,
because it sets an input, but a fresh database plus stubs plus a render is 118.2 ms against 5.0 ms
without stubs. That is 113.2 ms of stub re-parse per fresh database.
`check_semantics_invariants` builds three fresh databases per input, at 394.8 ms, so 86% is stub
re-parse, and 97.7% in release. A render on a warm shared database is 3.8 ms, which is thirty times
cheaper. An earlier fix reduced the database count from four and the ratio barely moved, because it
was never about database count alone.

Run the fuzz targets in release from `just gate`, and make a shared database the default arm, with
fresh ones only where determinism or incrementality genuinely needs them.

### Coverage-guided fuzzing has no ratchet, so nothing it learns survives the run

`fuzz/` does compile on stable today. `cargo check --all-targets` inside it takes 75 s cold with an
isolated target directory and 1.6 s warm, so the compile-rot risk flagged earlier is real but has
not happened.

`scripts/seed-fuzz-corpus.rs` writes 1,416 files and 5.7 MB into each of three corpora in 2 s, but
it globs `.Rtypes` under `crates/`, where there are none. All 11 `.Rtypes` files, and 33 `.exports`
files, live in the top-level `types/`. It therefore seeds no stub files, none of the 1,967 mined
`programs/*.R.corpus` programs, and it never looks at `corpus/`. Its own doc comment claims it
adds the shipped stubs, and the testing page repeats that. Both are false.

`fuzz/corpus`, `fuzz/artifacts` and `fuzz/Cargo.lock` are gitignored and no job persists them, so
every run restarts from the same seeds and rediscovers the same shallow frontier. `REGRESSIONS`
exists in `format` with 21 entries and in `semantics` with 2, and in neither `syntax` nor `ide`.
Both constants were last modified about 60 commits ago, so the pinning path works, at 0.12 s for
`format` and 0.94 s for `semantics`, and is simply not being fed.

In value order: persist the corpus, fix the seeder to read `types/` and
`crates/syntax/tests/programs/`, and add the 1.6 s `cargo check` of `fuzz/` to the gate.

The gitignored `fuzz/Cargo.lock` is also a trap. A fresh resolution can pick a `salsa-macro-rules`
that does not match the exactly pinned `salsa`, and `semantics` then fails to compile inside `fuzz/`
with "missing hash in implementation" on every interned struct. Commit that lockfile, or seed it
with `cargo update --precise` from the workspace lock.

### The one failure mode fuzzing exists to catch is the one that prints no input

`catch_unwind` appears once in `syntax`, once in `format`, twice in `semantics` and not at all in
`ide`. In `syntax` and `format` the single wrapper is on the program-corpus arm only.

Every `assert!` in `check_parse_invariants` and `check_format_invariants` embeds `{input:?}`, so an
assertion failure replays fine. A genuine panic inside a generative arm prints a backtrace with no
input, and a stack overflow aborts printing nothing. A stack overflow is a live risk the harness
itself acknowledges, through `deep_nesting_is_refused_not_fatal`.

This is recoverable today only because the seed is fixed, and that property disappears the moment
`FUZZ_SEED` lands, so this has to land with it. The semantics fuzz harness's `run_single_file` is
the correct pattern and is already in the tree.

### Two arms are silently dead and one is 99% wasted

`syntax::fuzz_corpus_seeded` costs 0.11 s and `format::fuzz_corpus_seeded` costs 0.16 s, which is
process startup only. Both skip through an `eprintln!` the blocking job never displays.

`format::fuzz_random_bytes` reaches the formatter body 14 times in 1500, which is 0.9%, against
token soup's 90. So 1,396 of its 1,500 iterations only re-test a refusal path `syntax` already
covers on the same generator. Drop it and give the budget to `fuzz_seed_mutations`.

### Radical alternatives, costed

- **The release profile for the battery.** 327 s becomes 53 s, for 143 s of warm build. This is the
  highest value per line in the pipeline.
- **A nightly coverage-guided run with a persisted corpus.** About 10 minutes per target, so about
  30 minutes a night on a free runner. Worth it only with persistence. Without it, that is 30
  minutes a night rediscovering the same frontier. A human has to add it, because it needs workflow
  scope.
- **OSS-Fuzz.** The three targets are already thin wrappers over exported batteries, so it is a good
  fit. Do it only after the replay path, meaning `catch_unwind` plus seed printing, and the pinning
  path exist. Otherwise reports arrive with nowhere to go.
- **`cargo-llvm-cov` over the batteries.** Not attempted. The saturation curves above answer the
  same question in about 7 s. Reach for llvm-cov when deciding which invariants to add, not how many
  iterations to run.

### Who lands what

An agent can land all of these today: completion sampling, the release profile in `just gate`, the
syntax budget raise, `catch_unwind` plus seed printing, `FUZZ_SEED`, the seeder fix, the `fuzz/`
type-check guard, the `fuzz_deep` floor that unblocks the CI move, and the dead-arm cleanup.

A human is required for three things: moving the staged workflows in `.github/pending/` into place,
which needs workflow scope, any scheduled fuzz job and its corpus cache, and an OSS-Fuzz submission.

## Open: robustness harnesses

- **Three `fuzz_iterations` harnesses ignore `FUZZ_SEED`.** `refactor_seed_mutations_keep_code_valid` (`crates/ide/tests/test_fuzz.rs`), `fuzz_the_intern_table_keeps_exactly_the_surviving_strings` (`crates/heap/tests/test_gc.rs`) and the reload fuzz (`crates/loader/tests/test_reload_fuzz.rs`) draw from one fixed generator, so `just fuzz-deep` reruns their same inputs each round. Convert each to `for_each_seed` with `SplitMix64(CONST ^ seed)`.

Gaps in the simulation, fuzzing and scenario design that `contributing/robustness.md` specifies.

- **Drive `oracle::fuzz::program` through `syntax::testing::Entropy`**, so a coverage-guided target can feed it bytes.
- **The oracle's `run_command` timeout kills only the direct child** (`crates/oracle/src/oracle.rs`). A grandchild that inherited stdout or stderr keeps the drain threads blocked in `join`, so a hung subprocess tree still hangs the harness. Spawn the engine in its own process group and kill the group.
- **The simulator does not model idle preemption** (`preempt_idle`/`set_idle` in `server.rs`: a request queued during an idle unit cancels that unit, never a sync or another request). Its interrupted steps deliver only notifications, so schedule requests mid-idle-unit and check the preempted unit is redone, including `pending_lowering` and the parallel warm-up.
- **An in-memory file system for the server.** The simulator's file-system seam is a real directory,
  so read errors, partial writes and permission failures are never injected. The worker reads the
  disk directly (`fs::read_to_string` in `did_close`, watcher handling, metadata), so the seam is a
  trait the worker reads through.
- **Allocation failure is not injectable** in the heap or the analysis.
- **The simulator does not open `.Rtypes` stub buffers**, does not send `workspace/didChangeConfiguration`,
  and never restarts the server mid-session. Each is a cheap action to add to `Session::act`.
- **The console's completeness check disagrees with GNU R** (`syntax::input_is_unfinished`, used by `repl::input_is_complete`). A string left open across lines reads as finished to the parser's check (the console catches it through `open_quote_start`), and `pkg::`, a trailing `\`, a leading `;`, `f(1\n2` and a bare `[` or `[[` read as unfinished where GNU R reports an error. `the_console_holds_back_what_gnu_r_finds_unfinished` (`crates/repl/tests/test_console_input.rs`) skips the `::` and `\` endings; drop the skip once the rule matches R's lexer.
- **Run `just fuzz-deep` on a schedule.** It needs about 30 GB of build space (it builds every test binary, with line tables only) and GNU R with testthat at `R_LIBS_USER` for the scenario-backed suites; a scheduled run passes a fresh base (`just fuzz-deep 480 $(date +%s)`). One partial round from base 2000000 has run: the libr GC-stress suites, format, and heap passed, and the format and ide fuzzers found the two bugs now pinned in their regression lists. The language-server simulation, the C API sequences and the oracle torture have not had a deep window yet.
- **GC torture over the package test suites.** `RY_GC_STRESS=1 oracle packages --runtime ry` against a corpus (`--corpus DIR` with `sources/`, `library/` and `closure.txt`) runs each package's own tests under collection stress. Over the 24 testthat-closure packages, R6, brio and callr passed under stress; the run stopped in cli, and the full 67-package closure has not run. Collector, barrier and rooting bugs it finds go to the heap work with the package, the stress mode and the first bad frame.
- **rprojroot's tests fail on ry and pass on GNU R** (testthat closure, `oracle packages`); not yet triaged.
- **The splice-reparse cross-check is the only `enable_invariants()` check.** Candidates: salsa
  query results recomputed in a fresh database at checkpoints, `LineIndex` round trips at the
  protocol edge, and the heap's `RY_GC_VERIFY` barrier check switched on by the same call.

## Open: test and fuzz architecture

A second independent review of the fuzzing and test architecture, with every claim measured rather
than read off the code. The findings it shares with the two reviews above are written up there. These
are the ones only it found, ranked, each stating the change and why it is worth it.

### `crates/ry` has no property coverage, and the protocol edge panics

The obvious property over `crates/ry/src/position.rs`, that every byte offset converts and
round-trips, finds 11 panics and 2 round-trip failures in twelve lines. There are two distinct
defects.

- `line_column_utf16` and `line_column_chars` slice `text[start..start + byte_column]` and panic for
  an offset inside a character. `line_column_utf16(1)` on `"é😀x\n"` is an example.
- `offset_utf16` clamps to a line length that has already had `\r\n` trimmed, so an offset at a CRLF
  terminator does not round-trip. On `"a\r\nb\r\n"`, offset 2 comes back as 1.

One caveat, stated honestly: every offset this module produces is on a boundary, so the panic was
not shown reachable from today's callers. But "an IDE feature never panics on a stale range" is a
stated soundness invariant with nothing enforcing it, and the CRLF break sits on the Windows default
path.

### The IDE arm asserts almost nothing

Of 13 feature calls per offset, only `hover` is checked, and only for determinism. Eight are
`let _ = ...`, which is never-panic only.

Nothing asserts the invariant that actually matters and that memory already states: every range a
feature returns lies inside the file. `definition`, `references`, `rename`, `type_definition`,
`code_actions`, `inlay_hints` and `document_symbols` all hand ranges straight to the editor. Assert
in-bounds ranges and determinism for every range-returning feature, and pay for it with the offset
sampling the economics review costs out. That is the same wall clock for several times the oracle.

### Seeds are hand-maintained while 1,183 fixture sources sit unused

The batteries seed from 81 hand-written strings and never see the 1,183 fixture cases,
which are the richest R corpus in the repo and the one that grows with every change. Seed the
`syntax` and `format` batteries from it, delete the three `SEEDS` lists, which is about 110 lines,
and keep a hand seed only where it encodes something the fixtures do not. For `semantics`, use
fixture sources as mutation seeds rather than one per input, because 1,183 times 480 ms is 9.5
minutes.

### Two real invariants hold today, are unasserted, and cost about five lines each

- **An `ERROR` node implies a reported error.** Over 20,000 fuzz-shaped inputs there was no
  counterexample. The converse legitimately fails, with `for (x in items)` among six examples, so
  only this direction is assertable. Without it, a silent parse failure lets the formatter mangle a
  file that reports clean.
- **Reporting is monotone.** One, two, four, sixteen and sixty-four copies of one faulty item yield
  one, two, four, sixteen and sixty-four findings. One, four, thirty-two and one hundred
  twenty-eight faulty statements in one item yield two, eight, sixty-four and two hundred
  fifty-six. This is precisely the invariant a per-item or per-file finding cap breaks, which is the
  regression three adoption reviews called a blocker, and nothing guards it.

### A timing-based scaling guard does not discriminate, so read this before retrying

The idea is right and the obvious implementation does not work in the default battery. The attempt
is recorded with numbers so the next one starts further along.

Normalizing by the growing unit is the first trap. Per-declaration cost falls as declarations grow,
because most of the run is fixed work, so that assertion passes against the very bug it targets.

The sound method is a four-corner interaction test. Measure `(I, D)`, `(2I, D)`, `(I, 2D)` and
`(2I, 2D)`, then compare the last against the additive prediction, because a table copied per item
is exactly the interaction term. Measured in debug at `I=200` and `D=300`:

| | per-item copy present | after the fix |
|---|---|---|
| additive prediction | 699.7 ms | 663.2 ms |
| actual at (2I, 2D) | 717.2 ms | 641.5 ms |
| interaction | +17.5 ms | -21.7 ms |

A 39 ms swing on a 700 ms total is about 5%, which is indistinguishable from load noise. The signal
separates only at sizes where the copy dominates, which is around 2,000 items by 2,400 declarations,
where the release measurement showed 71 ms against 27 ms. Four corners at that size cost far more
than a default suite should. An `#[ignore]`d witness was considered and rejected, because the
extended job that would run it does not currently run any of these suites, so it would be machinery
nobody invokes.

What would work is a structural assertion that counts the copies directly, rather than a timing one.
Nothing observable exists for it today.

### No witness measures one file with many top-level items

The `test_perf.rs` witness is corpus-wide, meaning many files. The shape that hid the known quadratic, which is
one file with many annotated items, is measured by nothing.

It is linear today: 100, 400 and 1,600 items cost 0.24 s, 0.65 s and 3.36 s, which is 2.43, 1.62 and
2.10 ms per item. One assertion that milliseconds per item at 1,600 stays within about twice
milliseconds per item at 200 is the only cheap guard against the highest-severity performance class
this project has actually shipped.

### Smaller confirmed items

- `crates/format/tests/test_format_docs.rs` falls back to `text.to_owned()` when a block header is
  not `# name: directive`, which publishes unformatted input as if it were formatter output. No
  block hits it today, across 48 blocks with no fallbacks, so it is latent. Call `panic!` instead,
  in one line.
- `crates/syntax/tests/test_error_messages.rs` draws its own carets with its own line and column
  math, so the golden suite pins a caret the product never prints. There are three newline-table
  implementations in the shipping crates: `ry::position::LineIndex`, `format`'s `line_starts` and
  `line_of`, and this renderer. One `LineIndex` in `syntax`, used by all three, deletes two of them.
- Dead code. `let _ = line_start;` in `crates/syntax/src/testing.rs` and `let _ = before_len;` in
  `crates/syntax/tests/test_fuzz.rs` keep unused locals alive. `floor_char_boundary` and
  `ceil_char_boundary` in that same file, and the same backward scan in
  `crates/semantics/src/testing.rs`, are now in std. `probe/` is an empty untracked directory at the
  repo root.
- The testing page claims each harness pins every input its targets have ever broken. Only `format`
  and `semantics` have a `fuzz_regressions_hold_invariants`. `syntax` and `ide` have none.
- `fuzz/` is its own workspace, so nothing in the default battery type-checks the three libFuzzer
  targets. Renaming a battery function breaks them silently until someone runs cargo-fuzz.

### Suspected, not confirmed

- `ide::completion` does work proportional to the corpus per call and is not memoized, at 15.9 ms
  warm and essentially independent of file size. That is a debug figure. One release measurement is
  warranted before any claim about product latency, because it is a per-keystroke path.
- `ProjectFiles::set_files`, which is the file-set change the server makes on open, close, create
  and delete, is never fuzzed. Every incremental arm mutates `set_text` only. Probing add, remove
  and reorder against fresh databases with cross-file interface cycles found them all equivalent, so
  this is a coverage gap rather than a live bug. It is the mutation that moves cross-file resolution
  winners.
- Lowering, naming and inference have no stage-local invariants. They are covered only transitively
  through `render_semantics`. That is defensible as end-to-end fuzzing, and the doctrine's per-stage
  wording overstates what is asserted. There is no naming invariant saying that every `NameRef`
  resolves or is reported, and no HIR range-containment invariant.

### Judged fine, so do not spend time here

The fixture format and harness are sound: 1,183 ids across 11 suites, none malformed, duplicate-id
rejection works across files, and bless rewrites only the expectation span. The four
empty-expectation cases are all legitimate negative contracts. The `syntax` fuzz suite is the best
thing in the test architecture, with five real invariants plus splice-reparse equivalence against
from-scratch, covering the tree and the errors, over an edit stream, for 0.89 s. Keeping regressions
in `REGRESSIONS` rather than in a committed corpus is right, with `fuzz/corpus` gitignored.

Two more things are deliberately not worth fixing. `render_semantics` and the typing runner's
`render_file` share a scheme loop and render different fact sets on purpose. `render_with_strict`
builds a second database per case, which costs 114 ms across 22 cases.

## Open: reports from the maintainer

### REPL resize is not tracked

The width fix itself is closed and archived in `test-user-reports.md`. What stays open is the
mechanism. A stock terminal session handles `SIGWINCH` by calling `R_SetOptionWidth`, which R does
not export. The console feed is the only other way in, and that is the mechanism already shown to
desynchronize the editor when used between prompts. Worth revisiting if someone finds a third route.

### A variable is reported as unused, and this needs a reproduction

The maintainer's note arrived truncated, reading only that a variable is reported as unused, so the
triggering shape is unknown. Do not guess at it. The `unused` warning has several paths, which are
script liveness, the package top level, parameters, and the interaction with `shadows-namespace`. A
fix aimed at the wrong one is worse than none. Ask for the snippet before working it.

## Open: diagnostic wording is not styled consistently

Six findings shown side by side in the README read as though five people wrote them. Three
`type-mismatch` messages are lowercase sentence fragments. `Unexpected comma after last argument` and
`Use TRUE, not T, for Boolean values` are capitalized, and the second is imperative. ``` `tmp` is
assigned but never used. ``` is the only one carrying a full stop. A README reviewer caught it,
noticing that the page claims a finding says the same thing everywhere while the sample visibly
disagrees with itself.

**The obvious rule does not survive measurement.** Across 62 message literals in `diagnostics.rs`,
36 are lowercase with no period, 14 are lowercase with a period, and 9 are capitalized. So
"lowercase fragment, no period" is the plurality and nothing like a convention. It also cannot be
applied blanket, because the capitalized ones are capitalized for a reason. `R's $ operator` opens
with a proper noun, and three messages are first-person sentences: `I could not resolve ...`,
`I do not know the type ...` and `I cannot construct an infinite type`. Lowercasing those yields
`r's` and `i could not`.

The rule that fits the corpus has two shapes. A message that is a complete sentence is sentence-cased
and ends with a period. A message that is a fragment, which is the expected-and-found family, is
lowercase with no period. Under that rule the real violations are far fewer than "five people wrote
them" suggests, and are mostly sentences missing their period.

This is unblocked. The reporter change that re-captured every rendered sample has landed, so
rewording a message now invalidates only the samples that quote it. Those samples have to be
re-captured in the same change. There is no harness for that yet, so it is a manual pass over the
pages named in the next item.

## Open: findings from the second simulated-user round

Five simulated users each worked on a distinct project of their own writing, learned the tool from
the docs alone with no source access, and tried to reach a clean run. The reports are the
`users/feedback-*.md` files from that round. The findings that were fixed are ledger entries at the
bottom of this file. What follows is what is still open, by theme, with the tester who found it
named where their judgment is the point.

### A clean run still does not mean enough

Every tester reached this conclusion independently, and it is the round's headline. The Shiny
dashboard tester finished with exit code 0 and five real bugs still in the code. The ETL engineer
would not use `typing = true` as a merge gate, not because it is noisy but because "after these, a
green run doesn't yet mean what it needs to mean at 3am". The Quarto analyst would have been "fully
off by end of day, never knowing the default checks were not running".

Two holes of that kind are still open.

- **A package with no manifest still buys the blanket tolerance.** Fifteen manifest-only CRAN
  namespaces now close the common case, but a package a user names, such as `janitor`, still
  switches unresolved detection off project-wide. The remedy is a two-line project stub, and the
  limitations page says so on the user-facing side.
- **A column name inside `DT[...]` is never validated.** `orders[, net := gross_ammount * 1.19]`
  and `by = currrency` are both silently accepted, while the only name that did report was a correct
  one. That is exactly inverted: silent where the typo is real and noisy where the name is right.
  `limitations.md` should say plainly that column names are never validated.

### Two configuration defects that break CI

- **Setting `[check] exclude` disables the built-in `renv/` and `packrat/` skip.** With no key the
  walk finds 1 file. With `exclude = []` it finds 1 file. With `exclude = ["nothing-here/"]`, which
  matches nothing, it walks renv and packrat both. The docs' own example, `exclude = ["scripts/"]`,
  would drag an entire renv library into CI. A user-supplied list must extend the vendored defaults
  rather than replace them.
- **Any file in `stubs/` deactivates the shipped conditional namespaces.** One stub for an unrelated
  package made 19 `data.table::` calls unresolved while bare `fread` still resolved, and it made
  `data.table` unusable as an annotation type name, which silently suppressed a real type error.

### The traits tripwire, tripped a fifth time

A `<T: numeric>` bound admits a `Date`, and the body's arithmetic then fails in R. A helper
annotated `#: <T: numeric> fn(a: T, b: T) -> T` and called as `add_them(d1, d2)` is accepted, while
the direct `d1 + d2` is correctly rejected with ``\`+\` is not defined between \`Date\` and \`Date\```.
A one-line helper therefore launders a real error.

The mechanism is the `declares_arithmetic` relaxation: a class that declares any arithmetic method
satisfies `Numeric` wholesale. That does not follow. `Date` declares `+.Date` for `Date + integer`
and has no `Date + Date`.

A constraint meaning "supports this operator with these operands" replaces both this relaxation and
the operand tie. Do not patch it again. Design it.

One related leak: `sort`, `head` and `rev` on dates report ``found \`T[]\```, which leaks an unbound
type variable.

### Nominal subtyping, which is the same deferred design as S4 and R6 inheritance

`data.table` is rejected where `data.frame` is expected, and the wrong direction is accepted.
`needs_df(data.table(a = 1L))` is legal R and reports `expected data.frame, found data.table`, while
`needs_dt(data.frame(a = 1L))`, the direction that really is wrong, reports nothing. R's class vector
for a data.table is `c("data.table", "data.frame")`.

`TyKind::Named` matches by exact name and nothing in the compatibility relation carries a hierarchy.
The tractable corner is a declared, acyclic nominal-extends-nominal relation in the stub vocabulary.
Do it as that design, not as a data.table special case.

### `Date` does not survive real code

It survives `d + 1L`, `d2 - d1`, `d1 < d2`, `format`, `seq.Date` and a `while` cursor. It is lost to
`Unknown` through `dates[i]`, `dates[mask]`, `for (d in dates)` and `Reduce`. It is wrong through
`min` and `max`. The same bug is therefore caught in one place and missed in another: `cursor + cursor`
after a `while` loop is flagged, and the identical `d + d` inside `for (d in dates)` is not. No doc
describes this boundary.

Two related gaps. `#: @type TradeDate {Date}` disables all Date arithmetic, so `settle - trade` is
rejected, which kills the guide's own recipe for domain types over dates. The working route is a
project `.Rtypes` declaring `-.SettleDate`, which the guide never mentions, and a stub cannot see an
inline `@type` anyway. Separately, `Date[]` is inexpressible, and `Days + Days` yields `double`, so a
units tag dies on arithmetic.

### Strict mode's own advice does not work

The message says to add a type annotation, and no annotation form silences it. `#:`, `@if-unknown`,
`@trust` and `#: Any` all still report, and the last contradicts the reference directly. Only
`# ry: allow(strict)` works.

### Missing type-system coverage, each with its repro

- **A parameter's type is not seeded from its default value.** `f <- function(s = settings) s$hsot`
  is missed while `settings$hsot` and a local alias are both caught. The obvious fix generates false
  positives, so this needs the design rather than a patch. Binding the parameter to its default's
  type makes every caller passing anything else wrong, and reporting the field against the default's
  shape breaks the commonest idiom of all, `function(x = list()) x$name`, where the default is an
  empty accumulator and callers supply the real record. R answers `NULL` there, so an error would be
  plain wrong. The honest model is that such a parameter is the default's type unioned with whatever
  callers pass, which is still open, so a field access on it should be at most `T | NULL`, the same
  rule the accumulator fix established for unions. Recovering the tester's case needs to know the
  argument is never supplied anywhere, which is whole-program information the checker does not have.
  Leave it missed rather than trade it for the class of false positive this round spent its time
  removing.
- **`&` and `|` are unmodelled**, so `TRUE & FALSE` is `Unknown` and every logical mask kills
  downstream checking. The reference documents `&&` and `||` with no counterpart, and the only
  mention of `&` is a parenthetical inside the non-standard-evaluation discussion.
- **A maybe-`NULL` value is only caught by arithmetic.** `toupper`, `nchar` and `substr` on a
  `character | NULL` all pass, despite the guide promising this class of check.
- **Recursion defeats `is.logical()` narrowing.** The identical non-recursive function is clean.
- **`$` on an unannotated parameter constrains nothing**, so the parameter accepts anything.
- **`#: @new` does not stop `structure()` being a strict origin**, so the documented S3 remedy fails
  at exactly the site it exists for.
- **S3 generic and method signature consistency is unchecked.** Both planted violations were missed,
  and `R CMD check` catches both. ry already does the rarer undefined-export check.
- **`vapply(x, f, numeric(1))` errors whenever the callback is not statically `double`.** R accepts
  a wider template, which the tester confirmed with `ry run` on
  `vapply(1:3, function(i) i, numeric(1))`.
- **A function name inside `mutate` and `filter` is swallowed** along with the column names.

### Two lints need a model change rather than a patch

- **`shadows-namespace` fires on a local inside `test_that()` and calls it a top-level binding.**
  Naming is right that the binding lives in the file's frame, because R braces are not a scope and
  the checker cannot know `test_that` makes an environment. What the lint needs is syntactic
  nesting, meaning "written as a direct statement of the file", which the binding model does not
  currently distinguish from "belongs to the file's frame". Adding that must not disturb the unused
  rules, where `TopLevel` means package-visible. Both shadow lints are default-off, so this waits for
  the model change rather than a special case. The wording is part of the bug, because a local two
  levels deep should never be called a top-level binding.
- **`unused-parameter`, `strict` and `shadows-namespace` were all unusable** on a package with an S3
  class and a testthat suite, in the package author's words. Two of the three causes are fixed. The
  shadow lint above is the third.

### Formatter and message defects

- **`m[i, , drop = FALSE]` becomes `m[i,, drop = FALSE]`**, which removes the space that makes the
  empty dimension slot visible, in exactly the code where miscounting slots is the bug. It is
  idempotent, so it is deliberate, and it is undocumented.
- **Any `X[Y]` annotation reports "only one compact annotation fits in a `#:` block, separate the
  annotations with a blank line" when there is only one annotation.**
- **The unknown-type and duplicate-definition messages name only `@type` or `@alias`**, although a
  definition may also be written inline as `type` or `alias`.
- **A bodyless opaque type (`@type Dict<K, V>`) is a stub-format form only.** If project files ever
  accept it, the inline `type Name<K, V>` must accept a missing `= TYPE` in the same change, so the
  two spellings stay one grammar.
- **"Add inferred type annotation" is offered on `x <- @UserId(1L)`**, where the constructor already
  states the type.
- **An unclosed chunk reports the tester's English prose as an unresolved variable** rather than the
  missing fence.

### Documentation gaps the round hit

- The adjacency rules say a plain `#` comment breaks `#:` attachment and never mention `#'`. So
  interleaving `#: @param` with roxygen's `#' @param`, which is the first thing a roxygen user
  tries, fails.
- The guide never shows `...` or `[optional]` in an annotation, and every S3 method needs both.
- `configuration.md` omits `DESCRIPTION` as a project root marker, and a three-line `DESCRIPTION`
  with `Imports:` was what fixed 30 of the ETL engineer's 34 first-run warnings. The path the docs do
  recommend, hand-writing `stubs/*.Rtypes`, cost five files, fixed less, and was actively harmful.
- The guide's showcase uses `list(...)`. Swapping it for `read.csv()` makes the identical mistake
  report nothing, and the page calls that the whole pitch with no caveat at the point of contact.

### What the round praised

Recording this matters, because several of these were the parts the testers expected to be broken.

**Literate handling is the strongest part of the tool.** Line numbers were exact across all 545 lines
of `.qmd` and `.Rmd`, ASCII columns exact, prose ignored, and every chunk-header form parsed,
including `#| fig-cap: "A caption with = signs"` not fooling the `=` lint. `{python}`, `{sql}`,
`{bash}` and `{ojs}` fences and bare fences were all correctly skipped, CRLF and Sweave and no-YAML
all mapped correctly, and suppression comments worked inside chunks.

**Date and time operator modelling is the best static checking of R dates the quant had seen.** Every
date expression R rejects or warns on was caught across 21 probes, with no misses and no false
alarms, including `Sys.time() - Sys.Date()`, which R only warns about and which they have shipped to
production. ``\`-\` is not defined between \`POSIXct\` and \`Date\``` was "the best message in the
tool".

**The data.table bracket really works.** A 162-line transform module using `:=`, `.SD`, `.SDcols`,
`.N`, `dcast` and a non-equi join with `by = .EACHI` produced no complaints about non-standard
evaluation. There were also zero false positives on 50 lines of idiomatic tidyverse, covering bare
column names through the whole verb set, `case_when`, `across(where(is.numeric), ~ .x * 2)`,
tidyselect helpers, joins and `aes()`, which was the Quarto analyst's biggest fear going in.

Also praised: record-field inference from a plain `list()` with a did-you-mean suggestion, which one
tester called worth adopting for alone; named-argument typo suggestions, arity checks and
argument-order errors ranged on the argument; branch-union arithmetic and non-function calls; the
partial-match catch, which R silently accepts and nothing else in their toolchain catches; `NAMESPACE`
being genuinely load-bearing, with import-typo and undefined-export validation at the right line;
`#:` verified invisible to `R CMD check`; the formatter and JSON output as production-ready; exit
codes, JSON Lines, `--min-severity`, config discovery, unknown-key warnings and suppression comments
all matching the docs; syntax-error recovery; and `ry run` with an embedded R letting a tester verify
every claim, which they called underadvertised. Units nominals and domain matrix types both work and
produce good messages. One tester confirmed the `expect_error` decision was right and that the
"testing that something is rejected" subsection worked first try. Performance was never a complaint:
0.15 s on 919 lines and 31.5k lines across 120 files in 2.2 s, both on debug builds.

## Open: what the documentation reviews left

The structural rewrite of the site has landed. The remaining documentation gaps are the ones below.

### The comparison page does not exist

An earlier entry recorded it as done, naming `comparison.md`. No such file has ever been committed,
and nothing in the docs mentions Air, Jarl or lintr.

It is worth writing, and the earlier entry describes the right shape: a capability table plus a
section on where you should use something else, handing formatting to Air and rule breadth to Jarl
and lintr outright, and stating the alpha and one-maintainer position. Verify every claim about
another project from that project's own documentation, and use no benchmark number that was not
measured here, because a second-hand number about a competitor is the fastest way to lose the
argument.

Two measured facts the docs never state and should: 854 files and 166k lines in 3.6 s, and lintr at
38.2 s against 0.47 s on dplyr.

### The headline leads with the two things ry loses at

The landing page says "Editor support, error checking and formatting for R", which leads with the
formatter and the linter. Posit's Air is bundled in Positron, and Jarl ships 71 rules with `--fix`
and an LSP at 140 times lintr's speed. The one thing nobody else has is buried.

Research confirms that nobody has ever shipped a static type checker for R. Vitek's group proved it
viable, finding about 80% of CRAN functions monomorphic or nearly so with a 1.98% contract-failure
rate, then pivoted to a JIT intermediate representation. The one Damas-Milner attempt,
`RTypeInference`, went dormant in 2021. Posit's `ark` README states that it plans "sophisticated
static analysis of R code" and cites rust-analyzer, while Positron ships a Rust type checker for
Python.

The primacy claim is already defensible in its current form, which is that ry is the only one that
infers types rather than the first one, because two unmaintained annotation-only attempts exist and
a hostile reader finds them in one search.

### Self-checking ggplot2 reports 1132 findings and takes 2.4 s

That is the one package a stub ships for. The proposed cause, the shipped stub colliding with the
package's own definitions, does not reproduce minimally: a package named `ggplot2` that defines
`geom_point` and calls it checks clean, so project-wins-over-corpus works.

The real cause is unidentified and needs the actual source, so fetch the corpus. The likely
candidates are ggplot2's own ggproto and R6 layer and its non-standard evaluation, both known gaps,
in which case the number is honest rather than a bug.

### One residual from the numeric-condition fix

A condition whose type is still undetermined is bound to `logical`, so `function(n) while (n) ...`
infers `n: logical` and rejects a numeric caller. Fixing that properly needs a "coercible to a
condition" constraint, which is a fourth constraint kind and therefore the documented tripwire for
designing traits rather than accreting them. `contributing/design/open-questions.md` holds the
reasoning.

## Open: what the adoption reviews left

Three independent black-box adoption reviews simulated real projects from the docs and `--help`
only, with no source access. They were an analysis-script user, a CRAN package author and a
numerical-computing user, and they converged on the same walls. What they found that is now fixed is
in the ledger. What remains is below, ranked by how often a real user hits it.

### The apply family is unblocked but still untyped where its result shape depends on a value

Overload selection with a flexible argument works now, through fact-versus-guess probing that
`decisions.md` records. `sapply`, `mapply`, `Map` and `tapply` stay `Any`, because `simplify = FALSE`
and a vector-returning callback change the result shape without changing any argument type, so no
overload set can discriminate them.

The reachable win is typing only their parameters and leaving the return `Any`. That costs R's
function-name-as-string form, so `sapply(x, "length")` would be rejected, as it already is for
`lapply`.

### The three object systems, re-measured

Two of the four claims this entry used to make were stale, including the one it ranked first.

- **`setGeneric` was already fixed when the entry said otherwise.** It claimed that
  `setGeneric("f", ...)` does not define `f`, so every call to a project's own S4 generic reports
  `unresolved`, and it ranked that the top fix. It does not reproduce. `set_generic_target` binds
  the name, handles the `methods::setGeneric` form and the `name =` argument, and a control probe in
  the same file confirmed the `unresolved` check was live, because `definitely_not_defined` reported
  and `area` did not. Anyone who took the ranking at face value would have spent a cycle fixing a
  non-bug.
- **R6 was mischaracterized.** The entry said R6 has no stub at all, because `R6::R6Class` reports
  an unknown package namespace. R6 does ship an export manifest and is a conditional namespace, so
  it resolves as soon as the project declares it through `Imports: R6` in `DESCRIPTION` or attaches
  it with `library(R6)`. Both were verified clean. The message appears only for `R6::` in a project
  that declares neither, which is the documented rule for any undeclared namespace and is
  deliberate. What is actually missing is typed declarations. The class, its fields and its methods
  are `Unknown`, so `obj$typo()` is silent and completion after `self$` offers every record field in
  the workspace.
- **Still true: an S4 slot typo is silent.** `setClass("A", representation(x = "numeric"))` followed
  by `new("A", y = 1)` reports nothing, where R halts with `invalid name for slot of class "A": y`.
  `x@slot` has no type either, and `setClass`, `setMethod` and `new` are `Any` stubs.
- **Still true: `UseMethod` is not modelled**, so a generic call is `Unknown`, and
  `structure(list(...), class = "dog")` produces a plain record, so the class attribute is data. S3
  operator dispatch is real, because `+.Date`, `Arith.X` and `Ops.X` are built and dispatched, and
  the linter knows `generic.class` names.

All three systems are recognized structurally by the IDE outline, in `classify_symbol_call`, which
is where the type-side work can start. The fix order, cheapest real win first, is the S4 slot-name
check, then R6 class typing, then S4 slot types, then `UseMethod`.

The slot check is bounded, because `setClass` names the slots and `new("Class", ...)` names its
arguments. But `contains =` is the trap. A subclass legitimately takes its parent's slots, verified
against R, where `setClass("C", contains = "P", ...)` followed by `new("C", x = 1, y = 2)` runs. A
check that does not follow the inheritance chain therefore turns correct code into a false positive.
Both the `representation(...)` and the `slots =` form need reading, and a class assembled
dynamically must fall back to silence.

### Matrix shape is untracked

`matrix`, `t`, `solve`, `dim`, `crossprod`, `diag` and `apply` all return `Any`, so a transposed
dimension or a non-conformable product is invisible. Declaring them `-> matrix` is easy. The value
is in dimensions, which needs a shape-carrying matrix type. The data.frame row-type design is the
same shape of problem.

One trap came out of the work that returned the `matrix` nominal: making a constructor return a real
nominal without also declaring that class's operator methods turns every `m + 1` into a false error.

### Everything from a `data.frame` is `Unknown`, and `Unknown` satisfies every annotation

On data-frame-heavy code, annotations therefore look protective and are not. This is the design
consequence that decides the tool's value for analysis users. It needs at minimum a way to see that
a check was skipped, which is what strict mode does once it reports origins.

### An unannotated helper that wraps an operator over a class fails, and the tie is why

`add_layer <- function(plot, layer) plot + layer` infers `<T: numeric> fn(plot: T, layer: T)`, which
ties the two flexible operands to one variable, so `add_layer(base, geom_point())` reports
`expected ggplot, found gg`. Dates have the same shape, in `add_days <- function(d, n) d + n`.

Annotating the helper fixes it and the message is clear, but the tie is an over-commitment. R's `+`
never required its operands to share a type, and a class that declares `+.Class` accepts pairings
the tie forbids.

This is the same third-constraint-kind question recorded above and in
`contributing/design/open-questions.md`. The right fix is a "supports this operator" constraint in
place of `Numeric`, replacing both the tie and the `declares_arithmetic` relaxation. The next stub
corpus addition that returns a real nominal will trip it again.

### A project's own `%op%` stays untyped by design

The result is `Unknown`, which is a strict-mode origin. It may be a non-standard-evaluation wrapper
whose right operand is quoted, as magrittr's `%>%` is, and checking that as an ordinary call would
reject correct code.

Lowering `%op%` to the call it is, following the documented `|>` precedent, would type it and give
goto-definition and references on the operator. Weigh that against the non-standard-evaluation risk
and the `unresolved` finding a bare-script `%>%` would gain.

### Only the stub route to an operator method on a project nominal is blocked

Declaring the method as an annotated R function works, and that is what an author writes anyway for
a real S3 class. The gap is narrower than "operator methods need ergonomics". A `.Rtypes` stub
cannot see a `@type` the R source declares, reporting instead that the declaration does not load
because it does not know the type. So only the stub spelling is unreachable.

Decide whether a stub source should see a project's `@type` declarations at all, or whether the
R-side declaration is simply the answer and the docs should say so.

### The stronger suppression form

A type error inside `expect_error(...)` stays reported, and `# ry: allow(type-mismatch)` is the
answer. `decisions.md` holds the record and the diagnostics page documents it. The open follow-up is
the stronger form, which is a suppression that reports when the expected finding does not appear,
like `@ts-expect-error`. That is a feature for every code, not a special case.

### Smaller items, each with a one-line repro in the reports

- A message leaks an unbound type variable, as `list[T] | T[]`, and expands an alias on only one
  side of an expected-and-found pair.
- Closure re-entry is unmodelled, so the memo idiom
  `if (!is.null(cache)) return(cache); cache <<- v` yields `T | NULL`.
- There is no `--fix`, no stdin input, and no CLI way to ask what type an expression has, which
  makes debugging an inference surprise guesswork for a CLI-only user.

Two items from the reports were checked and are not defects, recorded so they are not re-filed. A
generic parameter rejecting a non-`NULL` default is correct, because `<T> fn(x: T, [fallback]: T)`
defaulting to `0L` would return `0L` from a call the signature promises returns `character`, while
`T | NULL` with a `NULL` default works because `NULL` is a declared member. The `unused`
false-positive on a write followed by `break` does not reproduce, verified across `for`, `while` and
`repeat` and over both used and genuinely dead writes, so drop it unless a concrete shape
resurfaces. Separately, `sum(1, 2, 3,)` formatting to `sum(1, 2, 3, )` is not a defect and stays:
the trailing comma introduces a missing argument, so it parses identically to the `alist(, )` idiom
the space serves, and the `trailing-comma` lint reports the mistake.

## Open: semantics

### A list operation over a record still loses the field types

`rev`, `unique`, `head`, `tail` and `Filter` declare a `list[named: T]` candidate ahead of the plain
list one, so a name survives and a field read is `T | NULL` rather than a missing-field error. But a
fixed-shape input coerces to a name-keyed list on the way in, so the exact field types are gone and
the read stays nullable.

Only a shape-mirroring return, meaning "the same record", fixes it, and the type language has no way
for a stub to say that. `rev` is the case where that claim would be exactly right, because it
reorders and drops nothing, while `head`, `tail` and `Filter` genuinely may drop a name and are
correctly nullable. This is the same family as the data.frame row-type and matrix-shape designs.

### One residual on annotating a lambda

Reporting that an annotation in a call's argument list is silent does not give the lambda parameter
an annotatable position. That stays the one genuine expressiveness argument for inline type syntax,
which `contributing/design/inline-type-syntax.md` records. The message says to lift the function to
its own binding, rather than to move it up a line, which would annotate the wrong thing.

### `Unknown` is both the gradual top and the recursion fixpoint's seed

The fixpoint starts every member at `Unknown`, so a join must keep the other side of an `Unknown`
branch, or recursion never sharpens. That is why `if (c) 1L else untyped()` is `integer` rather than
`integer | Unknown`, and the reference says so. Absorbing `Unknown` at joins, which is the gradual
reading, broke recursion convergence when it was tried. The clean fix is a separate bottom for the
seed, so a join can absorb a genuine `Unknown` while the fixpoint starts from nothing.

### Overload candidates worth adding when the area is touched

`is`, `extends`, `grep(value =)`, and `cor`, where the vector and matrix forms differ and which
therefore needs matrix nominals. `Date` and `POSIXct` arithmetic refuses loudly today, so revisit it
if real code makes that noisy.

## Open: lowering fidelity

Each item below was found by dumping the HIR directly instead of reading it off the far-end type.
Each has a fixture case in `crates/semantics/tests/lowering/` whose comment states the expected
shape, so fixing one turns its case red and forces a deliberate re-bless.

- **A trailing empty argument position is dropped, so `m[1, ]` and `m[1]` lower identically.** The
  case is `indexing__a_trailing_empty_index_position_is_dropped`. R distinguishes them, because
  `` `[`(m, 1, ) `` has arity 3 and `` `[`(m, 1) `` has arity 2, and for a data frame they return
  different things: a row against a column. The cause is in the parser. `argument_list` emits an
  empty `ARGUMENT` only when a comma arrives while an argument is still expected, so a leading or
  interior hole survives, which is why `m[, 1]` is right, and a trailing one vanishes. The fix is
  for the parser to close a pending position when the closer follows a comma. The HIR side already
  models the hole, as `Argument { value: None }`.
- **The R 4.3 extraction placeholder is not desugared.** The case is
  `pipes__an_extraction_placeholder_is_not_desugared`. `x |> _$a` is `x$a` in R, and `_[[i]]`,
  `_[i]` and `_@s` behave likewise. `lower_pipe` accepts only a `CALL_EXPR` right-hand side, so
  these stay an opaque `Binary Pipe` whose field access reads a name `_` that exists nowhere.
  `pipe_shape` needs a second shape for a placeholder at the head of an extraction chain,
  substituting the piped value for the `_` in place.
- **An empty control-flow head slots the body into the condition.** The cases are
  `broken__an_empty_if_condition_slots_the_body_into_the_condition` and its `while` and `for`
  siblings. `if () 1L` lowers to `If(condition: 1L, then: Missing)`, because the `IF_EXPR` arm reads
  its children positionally and the parser emits no placeholder for the missing head. This is
  contained today only because a broken item's type diagnostics are suppressed. The suppression is
  per item rather than per file, verified by a sibling item in the same file still type-checking.
  But an IDE read of the region sees the wrong slot. The fix is to key the slots off the head
  delimiters rather than off child order.
- **A hexadecimal literal keeps a text no consumer can parse.** The case is
  `literals__a_hexadecimal_literal_keeps_a_text_no_consumer_can_parse`. A literal stores source
  text, and every reader parses it with Rust's decimal `parse`, which rejects `0x`. It is
  observable: with `pair: list{a: integer, b: character}`, `pair[[2L]]` resolves `character` while
  `pair[[0x2L]]` falls back to `integer | character`. Both `integer_literal_position` and
  `is_whole_number_double` need a radix-aware parse.

## Open: naming fidelity

Found by rendering `ItemNaming` instead of reading resolution off a downstream type or diagnostic.
The fixed findings are ledger entries. These are what remain, ordered by severity.

- **A top-level definition of a masking verb does not suppress masking, unlike an in-item one.**
  This was found while adding controls for the named-data-argument fix and is pre-existing, verified
  identical before that fix. `with <- function(data, expr) expr` at top level, followed by
  `with(frame, name)` in another item, still masks. Cross-item resolution happens above
  `item_naming`, so the callee read is not in `resolutions` and the shadow is invisible to the walk.
  That is the same item-firewall limitation the naming suite already states. The fix needs the
  file's own top-level binders consulted at the masking check, which `file_binders` can answer.
- **Rebinding `local` makes the rebinding itself read as a dead store.** The `Local` HIR node
  carries no callee expression, so nothing reads a user-defined `local` and a false `unused local`
  fires. The docs sanction treating the syntactic call as the construct. They do not mention that
  the shadowing definition then reports as dead.
- **`library`, `require` and `help` quoting ignores local shadowing, unlike every sibling
  recognizer.** `quote`, `on.exit` and the masking family all guard with
  `!resolutions.contains_key(callee)`. The attach family does not. The docs call this a limitation,
  and the inconsistency lives inside one function.

### Renderer gaps in the naming suite, none of them a product bug

- **A replacement base renders as a read, with `->`, although it is also the write.**
  `assignment_targets` collects only `ExpressionKind::Assign { target }`, and for `x$field <- v` the
  target is the `Field` node rather than the base name.
- **An unresolved replacement base renders two contradictory lines at one span.** Both `u -> b0` and
  `u -> non-local deferred` appear, because the base expression id lands in `resolutions` and in
  `non_locals`. Both underlying facts are right, being an unresolved read plus a slot-creating
  write. The rendering is what is wrong, and this is the shape where the missing write-versus-read
  distinction becomes actively misleading. No case covers it yet.
- **`quote(x <- 1L)` mints a vestigial slot.** `premint_frame_assignments` walks call arguments
  without knowing about quoting, so a `b0 x local` binding appears that nothing writes and nothing
  resolves to. It is harmless, and a premint fix should re-bless the case that pins it.

## Open: what can be wrong while every fuzz arm stays green

The third of three independent fuzzing reviews asked the complementary question to the other two.
Not what inputs the fuzzers feed, and not what they cost, but what can be wrong while every arm
stays green.

The method was injected-bug experiments against a byte copy of `crates/` built as its own workspace,
so the shipping `test_fuzz` targets ran verbatim against mutated code. The baseline and the restored
copy were both green. The oracle corpus was 1,967 mined program-corpus programs plus 1,192 fixture
sources.

Five oracles it proposed are now implemented, and the ledger records them. Three of them caught a
live bug. What follows is what the review left open.

### The parser accepts escape sequences R rejects

The lexer's string scanner skips any escaped character wholesale, with a comment calling escape
validity a semantic concern. No semantic layer checks it, so every malformed escape is silently
accepted. Verified with R as the referee: `"\u{1F600}"` has five hex digits and `\u` takes at most
four, so R refuses it with `invalid \u{xxxx} sequence` while ry reports nothing.

R's rules to implement are `\x` with 1 to 2 hex digits, `\u` with 1 to 4 hex digits either bare or
braced, `\U` with 1 to 8 hex digits either bare or braced, `\0` through `\7` as octal with 1 to 3
digits, the named escapes, and `unrecognized escape in character string` for anything else.

The fixture case `syntax/tests/syntax::quoting__escape_soup` sits in
`crates/syntax/tests/in-tree-acceptance-allowlist.txt` labelled as this gap. Implementing the check
should remove that entry and re-bless the case.

### One surface has no oracle at all

`PackageMetadata` is referenced by no fuzz arm, so the attach tolerance, `imports_every_name` and the
whole NAMESPACE and DESCRIPTION layer are dark to fuzzing. Fixtures cannot reach them either. Only
`crates/ry/tests/test_cli.rs` can.

### Oracles that hold, each measured, none of which found a bug

These are coverage the fuzzers lack. Each is worth adding on its own terms.

| oracle | result | cost |
|---|---|---|
| **Middle edits** through incremental equivalence. The in-tree arm replaces whole text and the cargo-fuzz target truncates and appends, so neither covers a common prefix and suffix together. | 568 checked, none divergent | 9 s |
| **Project-file-set churn.** No arm ever changes `ProjectFiles`, though hosts add and remove files constantly. | 120 removals, none divergent | 2 s |
| **Parallel against sequential** on generated input. `ry check` really does fan out, and `test_parallel.rs` covers three hand-written programs. | 30 runs over 8 files, none divergent | 0.9 s |
| **Two-wave superset.** A `parse_stage_diagnostics` finding that vanishes from `file_diagnostics` is an editor flicker, and this is asserted only for hand-written cases. | 3,159 sources, none lost | 25 s |
| **Leading-comment metamorphic.** | 2,197 compared, none divergent | 36 s |
| **Alpha-rename metamorphic.** 3 of 1,162 hits, all three the transform's own fault: an edit-distance suggestion, a stub-shadowing name, and string-form binders an IDENT-only rename missed. A capture-avoiding transform hit 0 of 994. | usable only with the filters | 20 s |

### One hypothesis killed

The semantics battery is not comparing mostly-empty renderings. Over 250 generated programs exactly
one is empty and the mean is 462 characters. Determinism and incremental equivalence are meaningful
self-consistency checks.

They remain content-blind in the sense the three injected bugs demonstrate. A uniformly wrong answer
is deterministic, incremental and in-bounds.

## Open: a package's own `pkg::name` reads are not validated

The project's own package, taken from `DESCRIPTION`'s `Package` field, is a known namespace whatever
the stubs say. `withr::defer()` inside `withr` therefore reads the project's own definition and has
its type instead of `Unknown`. The own package also wins over a stub namespace of the same name,
matching the rule that a package binding shadows a stub name.

The name itself is still not checked, so `withr::typoed_name()` reports nothing. Validating it
against the project's definitions was implemented, measured and removed, because across the CRAN
corpus every candidate report was a false positive. A package's export set is not the set of names
its sources bind. Five shapes showed up.

- A re-export. `shiny` has `importFrom(htmltools, validateCssUnit)` beside
  `export(validateCssUnit)`, so the name is exported with no definition in the package.
- An S4 generic from `setGeneric("raster", ...)`, which S4 opacity already covers.
- A lazy-loaded dataset under `data/`. `survival::survexp.us` lives in `data/survexp.rda`.
- A binding installed by `.onLoad`, such as `cli`'s `symbol`.
- An S3 generic re-exported from another package, such as `broom`'s `glance`, from `generics`.

Closing this needs the package's real export set, which means reading `NAMESPACE` `export()` and
resolving re-exports, plus a decision about `exportPattern`. That last one is a regex over names, so
it makes the set unknowable and must fall back to silence. It is worth doing, because a typo in a
self-qualified call is otherwise invisible, but it is a namespace-model slice rather than a one-line
check.

## Open: a stdlib wrapper loses all type information

`function(x) abs(x)` infers `fn(x: T) -> Any`, and the same holds for `sum`, `cumsum` and every
other overload set. Wrapping a standard-library numeric function in one of your own throws the types
away. `sum` no longer has an `Any` fallback (it rejects strings), so a `sum` wrapper gives `Unknown`
rather than `Any`.

The cause is the fact-beats-guess rule. The `Any` fallback fits while binding nothing, so it beats
every candidate that would narrow `x`.

The rule is right in general and the fallback is load-bearing. A nominal with an `Arith.` or `+.`
method satisfies the numeric constraint, so forcing `x` numeric would reject a user's S3 class that
legitimately defines `abs.myclass`. Any fix has to keep that working, which is why this is a design
slice rather than a tweak. The candidate shape is that if every non-fallback candidate imposes the
same constraint, imposing it is a fact rather than a guess. That needs a decision record and
adversarial review before it is written.

One thing is worth knowing before that slice starts. The escape hatch it depends on is only now
actually general, because `declares_arithmetic` used to consult the stub library alone while
operator dispatch resolves a method through the global scope. Any measurement of how often
constraining `x` would reject real code, taken before that was fixed, overstates the cost.

## Open: an ignored config key is a warning, and a warning can be missed

A key written at the top level that belongs under a table now names the table, saying that it is
ignoring the config key and where it belongs, instead of saying only that it is unknown. Writing
`typing = true` outside `[check]` used to load clean, check nothing and report no problems. That is
the same failure mode as an unresolvable namespace disabling unresolved-name checking, and the one
this project treats as the worst kind: a clean run indistinguishable from a run that never happened.

The reason this stays filed is that a warning can be missed. A config file is small, hand-written
and rarely revisited, so the cost of refusing outright is low and the cost of proceeding is a
project that silently is not checked.

Consider making an unknown key a hard error when the file is a local `ry.toml`, and keeping the
warning only where forward compatibility actually needs it. Decide it deliberately. The current
forward-compatibility rationale is written down in `config.rs`, at `Config::unknown_keys`, and it is
not obviously wrong.

## Open: a `--jobs` flag, and why the fan-out is not linear

These are two user-requested items, related but separate.

### `--jobs N`, spelled like cargo's

`check` fans out over `std::thread::available_parallelism()` and there is no way to control it. The
only lever today is CPU affinity, because `available_parallelism()` honors affinity and cgroup
quotas, so `taskset -c 0-N` works. That is fine for measuring and undiscoverable for a user.

Add `--jobs N`, with cargo's spelling and its `-j` short form, defaulting to the current behavior,
so the number is explicit and reproducible instead of depending on what the scheduler happens to
expose. It also makes cross-machine measurement possible without fighting `taskset`.

`fmt` is a plain loop over files and does not fan out at all, measured at 646 ms on one core against
644 ms on four. Either wire it to the same flag or say plainly in the docs that it is
single-threaded. What should not stand is a `--jobs` flag that silently governs one subcommand and
not the other.

### Why the speedup is not linear, and what the default should be

Measured with `taskset`, best of two, on a 4-vCPU container:

| | 1 core | 2 cores | 4 cores | speedup |
|---|---|---|---|---|
| ggplot2 | 1,388 ms | 1,255 ms | 1,033 ms | 1.34x |
| data.table | 420 ms | 309 ms | 278 ms | 1.51x |
| `targets` | 998 ms | 778 ms | 599 ms | 1.67x |

Read those against the container's own ceiling. Its 4 vCPUs deliver roughly 1.8 times native compute
at four threads, so `targets` is already near what this machine can give, and these numbers cannot
distinguish real contention from the container. Getting that separation needs a run on real hardware
at 1, 2, 4, 8 and 16 cores. The user has offered to measure, and `--jobs` is what makes that clean.

Three things are worth investigating once there are honest numbers, in order of prior suspicion.

- **Amdahl's law, not contention.** Rendering is deliberately sequential in discovery order, and the
  project-wide interface walk used to be one query on one thread (it is per item now). If the serial fraction
  is about 40%, the observed 1.34 to 1.67 times is simply correct, and the answer is to parallelize
  the walk or accept it rather than to hunt locks. Measure the serial fraction first. It is the
  cheapest thing to rule in.
- **Cycle bookkeeping.** This was the dominant cost, with 54 of 72 sampled stacks in
  `DependencyGraph::block_on` and `targets` getting no speedup at all, and bounding the
  conditional-slot join fixed it. Re-sample before assuming any of it is left.
- **The warm-up phase.** The cold pass warms per-item naming across cores first, precisely because
  computing it inside the interface walk serializes the front half. Check that the fan-out is
  actually balanced there. Files are dealt largest-first, which helps, but one enormous file still
  pins a thread.

Keep `available_parallelism()` as the default. It already honors cgroup quotas and affinity, which
is what a CI container needs, and nothing measured so far suggests over-subscription hurts. Revisit
only if the real-hardware curve turns over at high core counts. That would point at memory bandwidth
or allocator contention, and the fix would be a cap rather than a different formula.

## Open: editor polish

Hover type fences do not highlight in current editor builds, which the user confirmed. The server
tags the fences `roughly-type` and the in-repo VS Code extension ships a grammar for that id, so
reaching users needs a released extension update. Zed renders the fence plain until its extension
registers an equivalent fence language, which needs a tree-sitter grammar. Consider falling back to
tagging fences `r` for Zed if that proves distant.

## Open: structure and performance

- **The post-burst workspace revalidate should have shrunk.** The duplicate-binding and
  duplicate-type walk that was quadratic in file count is gone, and the ledger records it. Every
  file's diagnostics used to depend on every file's ranges through those walks, so any edit
  re-executed all of them. The user measured the revalidate at about 1.1 s at 713k lines. Re-measure
  on the real workspace before closing this.
- **Measure the parallel cold pass on real hardware before optimizing it further.**
  `crates/ry/examples/parallel_probe.rs` is the reproduction tool. The investigation found three
  things. The long-recorded claim that four workers buy only 1.2 times was mostly a measurement
  artifact, because this container's 4 vCPUs deliver only about 1.8 times of lock-free native
  compute at four threads and about 1.2 times at two, so no in-container parallel number is
  meaningful. The one real structural serializer was `interface_sccs` demanding naming for every
  item inside one query, at 25% to 51% of cold wall depending on package shape, which the CLI's
  parallel per-item naming warm before the fan-out fixed, taking mgcv's cold pass from 0.99 s to
  0.80 s even in the throttled container. And same-query blocking is negligible, at 53 blocks per
  5,657 executions, while the interface graph is wide, with ggplot2 at 1,198 items and depth 22, so
  no fixpoint-scheduling work is warranted until a real-hardware measurement says otherwise.
- **CI gates only the product crate.** The whole-workspace workflow is staged in
  `.github/pending/workflows/ci.yml`, and a human must move it into `.github/workflows/`, because
  an automated token lacks workflow scope. Until then the workspace battery runs locally per change.
  Authoritative performance numbers need the CI runner.

## Open: website and docs

- **There is no one-line installer.** The non-Rust route today is downloading a tarball from
  Releases. There is no `curl ... | sh`, Homebrew or winget path, and both uv and Ruff ship one.
- **Every release is marked a pre-release**, so `releases/latest/` resolves to the old `0.1.1` tag
  rather than the newest build, and the installation page names the tag in each URL. Promoting a
  release would let the docs recommend `latest` instead.
- **The README's diagnostics are not checked.** `docs/` samples are tests (`test_docs`), but the
  two diagnostics in `README.md` and `editors/code/README.md` are pasted text that goes stale when
  a message or blame range changes; re-capture them after such a change.
- **Rework the typing reference's presentation.** `reference/type-system.mdx` runs about 3,500 lines.
  It wants tables and short bullets instead of prose subsections, preserving every normative claim.
  This was deliberately deferred out of the docs restructure so the contract got a dedicated pass.

## Open: REPL

The console ships behind `ry run` (ry's runtime) and `ry R` (GNU R), with `ry repl` kept as a hidden alias, and `contributing/design/repl.md` holds the architecture and the
two pty-harness requirements. R is loaded at runtime with no build-time link, so the workspace builds
without R everywhere. The console is reedline inside the `ReadConsole` hook, with lexer
highlighting, conservative completeness backed by R's continuation as the safety net, and SIGINT
interrupt routing. The pty end-to-end suite skips when R is absent and runs green against real R. An
agent container can install R, and `MEMORY.md` has the recipe, so run
`cargo test -p ry-lang --test test_repl_e2e` before a REPL-touching change, anywhere.

Analysis-backed Tab completion ships as the first analysis rung, covering typed signatures for
stdlib names, session bindings, `pkg::` exports and manifest names. The `SessionCompleter` seam keeps
the repl crate syntax-only, and `AnalysisCompleter` in the product crate runs `ide::completion` over
the session-as-script. The headless runner ships too.

Three rungs are open: live-session facts, meaning the R environment listing unioned into completions;
pre-evaluation diagnostics on pending input; and hover on the input line. The graphics-device story
is a fourth, and the design record describes the versioned mirror structs it needs.

**Windows needs a real-machine smoke test.** The embedding is implemented and
`contributing/design/repl.md` has the recipe, covering Rstart callbacks through `R_DefParamsEx`'s
version handshake, sibling-DLL preloading, the RGui-to-LinkDLL switch, and the UserBreak plus
deferred-interrupt pair. It is compile-verified against `x86_64-pc-windows-gnullvm`, and GNU R's `R.dll`
now loads first by full path, because ry's own `R.dll` sits beside `ry.exe` and would otherwise
capture `Rblas.dll`'s import of `R.dll`. No Windows machine with R has ever executed it. The smoke test is
`ry R`, checking the prompt, evaluation, Ctrl-C and vi mode, and `ry R script.R`, checking output
and exit codes 0 and 1. One known caveat to watch is terminal VT input handling in the editor layer.

## Open: what the rename to `ry` left for the user

The rename itself is done. The language is `ry`, the crate is `crates/ry`, published as `ry-lang`
because plain `ry` is taken on crates.io, with both the library and the binary named `ry`. Docs,
editors, scripts, CI and the README carry the new name, and documentation points at ry-lang.org.

Three surfaces keep their former spelling permanently, because each lives where a rename breaks
silently. `roughly.toml` is still read, and both names are checked in one directory before walking
up, so the new name wins a tie. `# roughly: allow(...)` still suppresses, which matters most because
that one lives inside users' source files. Every `RY_*` variable falls back to its `ROUGHLY_*` name.
The VS Code extension reads `ry.*` settings and falls back to `roughly.*`. The REPL history directory
is moved once rather than renamed, so nobody loses their history.

Three things are left for the user, because an agent should not decide them.

- **The GitHub repository name.** Docs, badges and install commands say `felix-andreas/ry`, so they
  are wrong until the repository is renamed. GitHub redirects the old URLs, so this is safe to do
  whenever, and it is currently the one inconsistency in the tree.
- **The VS Code Marketplace identifier.** `package.json` says `felix-andreas.ry`. The Marketplace
  does not redirect an identifier, so publishing under it creates a new listing and starts installs
  and ratings from zero. Revert that one field if keeping the listing matters more.
- **Registering `ry-lang` on crates.io, and `ry-lang.org`.**

## Open: a size bound on a constructed type, and a decision on recursion widening

`TYPE_SIZE_CEILING` caps a type at the point it is recorded, which stops the pathology. The general
question is still open: nothing caps how large one constructed type may get in the first place.

Widening past a bound to `Unknown` is the sound-by-refusal move the loop join already makes for a
variable whose type keeps growing structurally. The alternative is folding to a recursive nominal.
Choosing between them is a semantics design decision rather than an optimization, so it wants a
decision record.

Computing the size cheaply needs care. The graph is small while the tree is enormous, so a
distinct-node count will not see the problem and a naive tree count is itself exponential. It wants
a memoized size where the size of a node is one plus the sum of its children's sizes, which is
linear in the graph and yields the true tree magnitude. `types::type_size` already has that shape.

## Open: a cross-file cycle panics salsa's backdate check

`fuzz_multi_file` (`crates/semantics/tests/test_fuzz.rs`) panics in salsa's
`backdate.rs` ("returned the same value, but the previous execution changed at R2 and the new
execution changed at R1") for `global_scheme`. Minimal repro, with salsa 0.28.0 and 0.28.5 alike:
file one is `g <- rev(g)` then `f <- g$x`, file two is `g <- g + 1L`; empty file two, render both,
restore it, render again. `sort`, `List.reverse` and `as.Array` in place of `rev` panic too,
`unlist` and `as.vector` do not. Every execution of the scheme is `Unknown`, so the result is
deterministic: the panic comes from deep verification re-executing a cycle head
(`maybe_changed_after_cold`) outside its fixpoint, where it reads only backdated cycle members.
The Array fuzz templates reached it first; it predates Arrays. Fix the cycle shape (or report it to
salsa with this repro), then pin the repro in `test_incremental.rs`.

## Open: neither cycle fix has end-to-end coverage

Neither the non-convergence fix nor the missing cycle recovery has a fixture. The failing inputs are
whole CRAN packages, and a synthetic case built from the suspected mechanism reproduced neither. For
the non-convergence, a self-growing definition, three mutual-recursion shapes and an overloaded-call
cycle all converge fine, because a single item pins and settles while the bug needs several members'
pins to interact. What is pinned instead is the structural property the fix rests on, which is
`refusal_is_idempotent` in `semantics.rs`. That is the part testable without reproducing the cycle.
The end-to-end guard rests on the corpus suites.

## Open: DataFrame

- **Top priority: the db-benchmark joins do not fit 1e8 rows in 15 GB.** The factor type outside
  Polars and the code-building reader are in (`decisions.md`, "Factors are ry's own codes plus
  one level table, outside Polars"), and at 1e7 the joins beat data.table on every question. At 1e8 both
  data.table and ry run out of memory (`tools/bench/table/report.py` records it as "out of
  memory"). Measured for ry, with libR's mimalloc settings:
  - Reading `join_x` (5.1 GB, 63.2 million `id6` levels) peaks at 7.35 GB against 3.9 GB live:
    2.9 GB of columns plus about 1.0 GB of level views. `join_big` is the same size, and q5's
    answer is about 4.9 GB, so x, big and one answer already need about 12.7 GB.
  - The transients are the u32 codes a low-cardinality column holds until it narrows (`id4`:
    0.4 GB, then 0.1 GB as u8), the interner's hash tables, `finish`'s keys and rank array beside
    the old views, and the per-slice Polars frames. libR never purges freed memory
    (`PURGE_DELAY=-1`), so they stay resident; `mi_collect(true)` with purging switched on
    briefly freed little at 2e7 (2.04 GB resident against about 0.87 GB live), which points at
    pages held in the rayon threads' heaps.
  - Next: give the reader's large transients an allocator that maps and unmaps whole regions
    (std's `Allocator` on the pinned nightly; hashbrown takes it with its `nightly` feature),
    keep a column's codes narrow while its level count allows, and sum the checksum in
    `tools/bench/table/ry.R` per chunk rather than converting a whole column. Then run
    `report.py DIR --rows 1e8 --joins-only` and the 1e8 group-by, write the tables with
    `--write`, and update `contributing/design/r-runtime.md` section 7, whose 1e8 join prose
    says neither engine fits.
- **`ry migrate data-table` checks a name's uses in the current file only** (`uses_are_safe` in
  `crates/ide/src/migrate.rs`). A top-level `sales <- fread(...)` in one file that another file
  queries with `sales[, z := 1]` is rewritten, which breaks the other file. The safety check
  should see every file of the project that can read the name.
- **A factor whose labels share their first 12 bytes sorts serially** (`Interner::finish` in
  `crates/table/src/factor.rs`). Labels such as `customer_00001234` or ISO timestamps form one
  run of tied prefixes, which is re-sorted on one thread through indirect label lookups. Sorting
  such a run in parallel, or keying on the bytes after the common prefix, would fix it.
- **A Polars `Enum` converted to a factor loses its level order** (`convert` in
  `crates/table/src/table.rs`): it goes through `from_strings`, which sorts the levels by bytes,
  so an Enum read from Parquet with a non-alphabetical order is reordered.
- **A factor column with a missing value costs a second whole-file parse.** The sliced reader
  (`read_sliced` in `crates/table/src/io.rs`) parses a factor column straight into codes, but a
  `Categorical` reads an empty field as `NA` where `fread` reads `""`, so a column with any
  missing value is read again as text, unsliced, which faults the released file back in and
  holds a full string column. Parsing such a column as text within the sliced pass, once a slice
  shows a null, would keep the peak at one slice.
- **A text column is joined from its slices at the end**, which copies its strings once
  (`Builder::Text`); building the final `Utf8View` buffers directly would avoid it.
- **Large allocations page-fault on every query** under mimalloc's default 10 ms purge delay, so
  a kernel's scratch memory at 1e7 rows is faulted in afresh each time. With lazy arena commit and
  the one-pass factor conversion, the whole db-benchmark script (load plus all questions) runs 97 s
  at 6.7 GB peak with the default delay and 91 s at 8.1 GB with the delay set to never. libR sets it
  to 1e9 ms, so pages return only at `gc()` (`crates/libr/src/libr.rs`, see decisions.md "libR keeps freed memory until `gc()`"), because
  refaulting was 15% of the benchmark tests' profile; the cost is resident memory at its peak.
  Keeping scratch small (the grouped median) and a per-query arena remain the targeted fixes.
- **A factor build is bound by Polars' Enum interning.** `string_to_factor`
  (`crates/table/src/convert.rs`) hashes, deduplicates and sorts the levels itself in parallel
  (1.8 s for `join_big.csv`'s 9.95 million distinct `id6` values on 4 cores), then hands the sorted
  levels to Polars' `FrozenCategories::new`, which inserts them one at a time into a concurrent
  table that copies and hashes each key again: 9.6 s for those 9.95 million levels, 3.1 s to build
  and 2.4 s to drop for 6.3 million. The whole factor conversion is 0.9 s for `groupby.csv`, 7.3 s
  for `join_x.csv` and 12.3 s for `join_big.csv`, against data.table's `fread(stringsAsFactors =
  TRUE)` at 5.3 s, 31 s and 37 s (1e7 rows, parse included on both sides). Removing the rest needs
  a factor type outside Polars (codes plus a plain levels array, mapped to an Enum only when a
  Polars plan needs one), which every kernel and the lazy planner must handle (`decisions.md`,
  "Factors stay Polars Enums").
- **API gaps against Polars:** no string or date namespaces (`str.contains`, `dt.year`, so
  `lubridate::floor_date` runs on R vectors in the `trials` module), no `cast`, `n_unique`,
  `top_k` (`Expr.head` after a sort covers the per-group case), `unique`, `rename`, `drop`,
  `unpivot`, or `when/then/otherwise` chains beyond `Expr.if_else`. None of the db-benchmark questions needs them. `LazyFrame.scan_*`
  has no parquet predicate statistics use beyond what Polars' reader does.
- **IDE:** rename of a column (across `col("x")` strings and `$x`) and goto-definition from a
  column string to the verb or constructor argument that created it are not implemented; hover and
  completion are.
- **Checker:** an expression held in a variable (`e <- col("x") * 2`) leaves a verb's columns
  unknown and its operands unchecked, because the refinement reads argument syntax. Typing `Expr`
  with its column type (`Expr<double>`) would close it. The column-error wording says "the frame
  `x`" for every table kind. Operand checks cover arithmetic, logic, comparisons, `Expr.if_else`
  and the numeric aggregations.
- **Checker, file schemas:** only `.csv`/`.tsv` files are sniffed, so `DataFrame.read_parquet` and
  `LazyFrame.scan_parquet` always have unknown columns (a Parquet footer holds the schema and is
  cheap to read on the host). An annotation that disagrees with the sniffed file reports the
  generic `expected ..., found ...` mismatch without naming the file or the differing column.
- **Building a DataFrame copies every column** into the engine, where `data.table()` keeps R's
  vectors: at 1e7 rows this is most of the typed modules' gap in `make_events`, `fetch_prices`
  and `enrol` (`contributing/design/r-runtime.md`, "Real code: the benchmark modules"). Numeric
  columns could share R's buffers once the collector can pin them for a Polars buffer's life.
- **R precedence trap:** `col("a") * 2 |> Expr.alias("b")` pipes only the `2`. A lint that
  recognizes a pipe into an `Expr.` function whose left side is a literal would catch it.

- **Query features:** a `by` that needs R code, `:=` with `by` on the R path, and a join whose `j`
  needs R code (including under `.EACHI`) stop with an error.
- **Native code gets copied columns.** `crates/libr/src/frame_altrep.rs` can expose columns as
  ALTREP, but `frame.rs` still converts a column into an R vector when R code reads it.
- **A filtered `:=` loop is engine-bound** (`tools/bench/table/ry.R`, "update subset", 1e6 rows:
  0.082 s from R, 0.067 s from Rust, 0.11 s for data.table). Profiling puts the R layer at about
  5 µs per query; the rest is the filter's compare and the scatter. The R/Rust gap is the same
  engine code in a different process (libR.so allocates with glibc malloc, the Rust bench with
  mimalloc), which is the next thing to check.
- **A frame satisfies a `list` parameter in the checker** so that `lapply` and `Map` accept it,
  which is unsound for code that indexes the argument with `[` (rows on a frame). Overloads that
  take `frame` directly would let the compat go.
- **R code evaluated while lowering can run twice.** A sub-expression that names no column is
  evaluated eagerly during lowering, and if a sibling then falls back to R, the whole `i` or `j`
  runs again in the data mask. An `i` or `j` that updates the queried frame during lowering
  leaves the IR built against the old schema.
- **data.table masks `setkey`, `setkeyv`, `key` and `copy`** when attached after ry; its versions
  reject a frame, so users call `ry::setkey`.
- **Checker:** `.SDcols` as positions or `patterns()`, `(cols) := ...` through a variable, and
  `c(.N, lapply(.SD, f))` as `j` give a frame with unknown columns.
- **Checker nits:** `x < "b"` on a factor gives the generic comparison message, and `factor == 1` is
  flagged although R allows it.
- **The checker and the runtime disagree about data.table's readers.** The stubs type `fread`,
  `rbindlist`, `melt` and `dcast` as returning frames, because ry's table replaces data.table, but
  under ry's runtime an attached data.table still runs its own versions, which return a
  `data.table` and reject a frame. Routing them to the engine was measured and rejected
  (`decisions.md`, "data.table's helpers run data.table's own C"), so the fix is in the checker:
  type a call that resolves to data.table's export as returning `data.table`, and keep frames for
  `frame_read_csv` and the other `ry` functions.
- **Checker, data.table functions:** `fifelse` on a scalar `test` still types as a vector;
  `melt` with mixed measure types does not warn as data.table does; `dcast` gives a frame with
  unknown columns (the spread columns are data values); a data path resolves against the project
  root only, because `SourceFile` carries no path.
- **Uninitialized vector payloads are read through `&mut [T]`.** `alloc_vector_uninit` (and
  `Rf_allocVector`, which uses it) hands out numeric payloads that callers such as
  `crates/libr/src/frame_altrep.rs` and `crates/interp/src/values.rs` fill through `slice_mut`,
  which forms a reference to uninitialized memory. Fill through raw pointers or `MaybeUninit`.
- **Reader gaps:** integer64 reads as double, dates as character; no `dec = ","`, no `strip.white`
  on character columns, and a ragged line is an error rather than fread's fill.
- **Checker:** a `:=` error makes the updated frame `Unknown` for the rest of the item, as every
  type error does, so later column errors on the same frame are not reported.
- **Checker:** only a literal counts as a single value, so `f[, y := sum(x)]` replaces `y`'s type
  where data.table keeps it, and a frame updated in a nested closure is not refined there.
- **A writable ALTREP pointer copies the column into R's layout.** That is the value-semantics
  contract (Arrow buffers are shared and immutable), so only revisit it if profiling shows it.
- **The 5 GB (1e8 rows) run has not been done.** Its CSVs need about 16 GB of disk, and
  `tools/bench/table/ops.R` needs more than 15 GB of memory, since `rbindlist(list(x, x))` holds
  the 5 GB table and a 10 GB result at once.

## Open: the runtime

What steps 2 to 4 of `contributing/design/r-runtime.md` left for later, most pressing first.

- **Inline annotation checks have known holes** (typing reference, "Inline annotations at run time").
  A promise that escapes its frame unforced (returned inside a closure, or stored) is never checked,
  because the pending check is dropped at exit. A function without parameters whose body is a
  constant or a symbol is not checked. The `ry run` preflight analyzes the
  script alone, so in strict mode `ry run R/b.R` inside a package refuses a name another package
  file defines, which `ry check` accepts.
- **Windows `parallel` is unverified.** GNU R for Windows registers routines of its own there
  (probably `ncpus`, behind `detectCores()`), which `WINDOWS_ROUTINES` in `builtin_libraries.rs`
  lacks, so the namespace may not load. Check against a Windows R home's `parallel` and add them.
- **`Tailcall` does not reuse the caller's frame** (`do_tailcall` in `crates/interp/src/eval.rs`), so
  a deep tail-recursive loop written with it still grows the stack. GNU R 4.6 replaces the frame.
- **R 4.4 and 4.5 are supported by reading GNU R's sources, not by running them.** Install both and
  run the oracle and the interp and ry suites under each (`ORACLE_R`, `R_HOME`). Under 4.6.1 seven
  oracle cases differ, mostly for build reasons (OpenBLAS digits, glibc `mktime` state, a NaN
  payload), plus one that calls the removed `C_pKS2`; `contributing/corpus-and-oracle.md` lists them.
- **ry's console evaluates a chunk only once all of it parses** (`interp::session::run_console`): a
  `-f` script that ends inside an unfinished expression runs none of its complete expressions until
  the user finishes it, where GNU R runs each expression as soon as it is complete.

- **Corpus package tests: what still fails** (`oracle packages`, `.corpus/tests/summary.md`).
  - `R` without `--no-echo` does not echo its input. GNU R prints each line it reads after the
    `> ` or `+ ` prompt (and a last `> ` at the end of input), so `R --silent -e 'expr'` output
    starts with `> expr`; sys's "Jobs can be nested" reads the second line of that output.
  - tcltk is unavailable: `dyn.load` refuses GNU R's own `tcltk.so` (`decisions.md`, "Package-test
    compatibility details"), so code that needs Tcl/Tk gets an error. A tcltk built against ry
    would bring it back.
  - Parse errors name the token GNU R's parser stops on, but ry's parser recovers differently in
    a few places: `x <- function( {` reports the end of the input, where GNU R reports the `{`, and
    `a ;; b` parses where GNU R rejects the second `;`.
  - POSIX (TRE) patterns still run on fancy-regex through a translation (`perl = TRUE` is PCRE2,
    `decisions.md`). A non-constant-size pattern runs on fancy-regex's backtracking VM because of
    `leftmost_longest`; linking TRE itself would give GNU R's exact POSIX semantics.
  - `getParseData` ids are ry's own node numbers, where GNU R's are bison's internal ones, so code
    that hard-codes an id would differ (none in the corpus does).
  - lobstr's `obj_size` counts a closure's environment a few kilobytes higher than GNU R, because
    ry's environments and bytecode have their own layout. Only relative comparisons are portable.
  - Each ALTREP string or list element read through `Sexp::element` pushes a handle that lives
    until the enclosing scope ends, so a builtin walking a 10M-element nanoarrow column pushes 10M
    handles. Materialize once in bulk paths, or root only where the element survives an
    allocation.
  - The TRE matcher now compiles three regexes per pattern, which costs `grepl` in a loop with no
    matcher cache.
  - `getParseData`'s comment parents (`comment_parent` in `crates/interp/src/lower.rs`) scan every
    row per comment, quadratic in a heavily commented file parsed with `keep.source = TRUE`. The
    builder knows each comment's enclosing node while it walks, so record it there.
  - Tier-1 code is freed once its enclosure dies (`compiled_for` in `crates/jit/src/jit.rs`), on
    the reasoning that a running activation keeps its closure, and so its enclosure, alive. An
    activation whose closure's environment is replaced while it runs (`environment(f) <- e`
    inside `f`) would break that. No repro yet; write one before changing the code.
  - Two unserializers: `interp::serialize`'s reader and libr's `R_Unserialize`. The writer is
    already libr's alone (`decisions.md`, "One serializer writer, in libr"); move the reader the
    same way.
  - Gaps a review of the date-time and regex work found, none yet hit by a corpus test:
    - `as.POSIXct(lt)` ignores an explicit `isdst` when the zone has a single offset.
    - POSIX rule strings in `TZ` (`"EST5"`, `"JST-9"`, `"<+03>-3"`) and daylight rules after 2100.
    - `strptime` input: `%%` skipping blanks, no warning for an out-of-range `%z`, and
      `format`/`strptime` do not recycle the format.
    - The fancy-regex backtrack limit is unbounded, so a catastrophic POSIX pattern can hang.
    - `iconv(sub = , "ASCII//TRANSLIT")`.
  - Smaller classes: Bessel's `IJKY.R` (a relative difference of 0.036 between `besselI`
    results), an F value in nlme's `anova`, 12 of data.table's `test.data.table()` tests
    (901.2, 1138.2, 2125.03, 2125.05, 2211.119, 2211.24, 2211.349, 2252.1, 2258.4, 2258.5,
    2291.1, 2300.4), and four tinytest failures in RcppSimdJson.
  - A process the oracle kills at its deadline leaves an empty `.Rout`, because ry buffers
    stdout; reproduce a hang with `ry run` in the background and `gdb -p` to see where it is.
  - utf8's "long text in C locale" tests switch `LC_CTYPE` to C and expect `...` instead of an
    ellipsis and byte-based widths; ry's width and `l10n_info` ignore the C library's locale.
  - glue runs 271 tests on ry against GNU R's 265: ry runs a test GNU R skips, so its failure
    may not be a real one.
  - Matrix's `indexing.R` stops at `identical5(m[L, drop = FALSE], M[L, drop = FALSE], ...)` with
    "logical subscript too long" from `.subscript.2ary`: a method sees a `j`, so the one-index
    path is skipped. The same expressions pass in a fresh script, so state that earlier lines of
    the test set up is involved; bisect the file to find it.
  - nlme's `anova.gls` and Orthodont tests differ by about 1e-7 in fitted parameters, and
    `all.equal` there gets a p-value as its tolerance, so it needs near-identical iterates.
    `nlminb` now runs PORT's drivers (`crates/interp/src/stats_port_minimizer.rs`); rerun these
    tests to see what is left.
  - `nlminb` PORT divergences from GNU R (netlib sources: <https://netlib.org/port/>, the `drmn*`,
    `dg7q*`, `dd7d*`, `dl7*`, `ds7*`, `ds3grd` files; never GNU R's `portsrc.f`). Every routine
    of the six drivers is translated. Bit for bit with GNU R: unbounded runs with finite
    differences, a gradient, or a Hessian; bounded runs with a Hessian; the bounded 4-variable
    quadratic in all three modes; the nlme-like likelihood with and without `scale` and bounds;
    `trace`; `iter.max`/`eval.max`; starts on an active bound; control errors and code 18
    (`stats__nlminb_port_paths`). Still diverging: Rosenbrock from (-1.2, 1) in the box
    [-2, 0.5] x [-2, 2] with finite differences (`DRMNFB`, 21 vs 23 iterations) and with a
    gradient (`DRMNGB`, 19 vs 22), and a run whose objective returns NaN or Inf partway
    (`walled` in `nlminb(c(0, 3), ...)`, 14 vs 24 iterations). Suspects: `DRMNGB`'s bound
    bookkeeping (`DS7BQN`, `DD7DGB`, the `G0` aliasing, `DV7IPR` permutations) and `DRMNG`'s
    restore path after a failed evaluation (`TOOBIG`, statements 190-200). Compare evaluated
    points with `sprintf("%a")` against Rscript to find the first differing step.
    A bounded run (finite differences or gradient) that ends with no free variable reports
    X-convergence (3) where GNU R reports 5 unless the objective is 0 there:
    `nlminb(1, function(x) sum((x-3)^2), lower=0, upper=2)`. `DD7DGB` leaves `NRED` unset in
    that case and ry uses NaN; keeping the previous `nreduc` fixes `lower = upper` starts but
    breaks the zero-objective start pinned in `stats__nlminb_port_paths`. Also minor:
    a gradient that returns a list errors on ry and works in GNU R, and a non-numeric
    objective misses GNU R's "NAs introduced by coercion" warning (`stats_port.rs`, `Objective`).
  - data.table: `S4`, `autoprint`, `knitr`, `other` and `types` pass. `main.Rraw` reports about
    700 failing tests (among them `-xtfrm(col)` on a character column, `scan()` reading
    `000879.17` as an integer, fread's `colClasses` list without names, and a missing "Item 2
    of i is -10" warning), froll misses the warnings of 6000.114 and 6000.115, nafill fails
    3.07, and `programming.Rraw` stops on "invalid type/length (symbol/0) in vector allocation".
  - GNU R's binaries of device packages check `R_GE_version` 16 and ry provides 17, so svglite
    (and magick) must be installed from source on ry, which `oracle install` does. magick's
    configure needs Magick++ (`libmagick++-dev`), which the container lacks.
  - pak fails to install in a parallel `oracle install --jobs 4` (its `install.libs.R` finds no
    `DONE`) and installs alone; the cause is not known.
  - pkgbuild's "warnings can be turned into errors": GNU R's `R CMD build` script on ry does not
    turn a build warning into an error.
  - `eval(quote(next))` inside a loop reports no loop context.
- **The oracle starter corpus does not fully pass yet.** `oracle compare --engine ry` lists the
  differing cases; most remaining ones need stats routines (`builtin_libraries.rs` names each
  unimplemented one when it is called) or printer details (complex numbers, wide matrices).
- **Source references differ in console srcfiles.** In a script or console
  with `keep.source` on, each top-level expression gets its own srcfile whose `lines` are that
  expression's lines (with `fixedNewlines`), while GNU R shares one buffer srcfile, so `lines` and
  the column of a second expression after `;` differ. `.Internal(.traceback(x))`, the helper
  behind `traceback()` printing, is still a stub that returns `NULL`, and the interactive
  `traceback()` output ("at #2") is unchecked against GNU R.
- **Tier-0 speed.** A scalar `for` loop runs about 6x slower than GNU R's bytecode (`s <- s + i * 2`,
  1e6 iterations: 0.27 s against 0.04 s). The cost is per-instruction allocation of scalar results,
  handle-stack pushes, and the guard and frame probes. Next: reuse a register's scalar when it is
  unshared, cache the frame cell of a local variable per activation, and skip guards proven by the
  binding epoch for a whole loop body.
- **Some promise code still runs on the AST evaluator.** A closure argument containing
  `function`, a loop, `break`, `next` or `return`, an argument of a closure call that is itself
  inside promise code (`f(g(x + 1))` compiles `g(x + 1)` but not `x + 1`, because promise code
  does not know its closure), and every promise made outside compiled code (`delayedAssign`, the
  loader's lazy bindings, calls from the AST evaluator) are evaluated by the AST walker.
- **Missing internals.** Several `.Internal`s still raise "not supported" (`prmatrix`,
  `format.Date`, `stem`, ...);
  `grep -n 'not supported' crates/interp/src` lists them.
- **Distributions differ from GNU R in the last digits** in extreme tails and in the noncentral
  families (`decisions.md`, the distributions record in `distributions.rs`'s module comment).
- **`R -f` and `R CMD BATCH` do not echo input.** `ry_run` runs a file like `Rscript`, so
  `R -f` without `--no-echo` and `BATCH` output omit the `> ` echoed code GNU R prints.
- **`exists(name, registry)` on the namespace registry segfaults** (its enclosure is NULL, as in
  GNU R, where the lookup errors "use of NULL environment is defunct" instead).
- **`lazyLoad()` returns visibly** (prints `NULL` at top level); GNU R's result is invisible.
- **Connections gaps:** `fifo()` and non-file URL connections are not implemented. `xzfile()`'s
  negative (extreme) compression levels use the ordinary preset.
- **ALTREP `Extract_subset`** is registered but subsetting does not call it yet.
- **`R_withCallingErrorHandler` runs its handler after unwinding**, not before (`decisions.md`).
- **A task callback's warnings print under the plain "Warning message" header.** GNU R prints them
  under "warning messages from top-level task callback 'name'", with the call shown as `cb(...)`.
- **Indexing a compact sequence materializes it.** `1:n`, `n:1`, `seq_len` and `seq_along` of at
  least 2^20 elements are compact ALTREP objects (`sequence` in `crates/interp/src/arith.rs`;
  doubles past `INT_MAX`), but `x[i]` and `x[[i]]` on one expand the whole vector once, keeping
  the expansion in data2, because subsetting does not call `Elt` or `Extract_subset` (the item
  above). Subsetting `seq_len(2^31)` therefore allocates 16 GB where GNU R reads elements.
- **Approximate matching has two engines.** POSIX patterns with TRE bounds such as `{+}` run on
  `ApproximateRegex` (`decisions.md`, "TRE approximate regular expressions"), but `agrep`,
  `agrepl` and `adist` still use the older edit-cost NFA in `crates/interp/src/approximate.rs`.
  Move them onto `ApproximateRegex`, which already has TRE's cost model. `aregexec` is not
  implemented at all ("there is no .Internal function 'aregexec'").
- **`strptime` drops a leap second.** `strptime("2020-01-01 10:59:60.5", "%Y-%m-%d %H:%M:%OS")`
  gives 11:00:00.5 where GNU R keeps `sec = 60.5`, because the parsed time is stored as epoch
  seconds before the `POSIXlt` fields are built.
- **Deparse differences left by `tests/eval-etc.R`:** line breaking of `if`/`else` inside function
  bodies, `example()` echo without `keep.source` (GNU R shows the original text and comments), and
  the "deparse may be incomplete" warning.
- **R-level finalizers** run through the interpreter; a finalized weak reference stays in the
  weak list with a `NULL` key.
- **Graphics gaps** (`decisions.md`, "One graphics engine for base graphics, grid and device
  packages"). Base graphics, grid, lattice and ragg match GNU R pixel for pixel on the `grid__*`
  oracle cases; what is left:
  - Raster compositing allocates a canvas-sized layer for every mark drawn with an operator other
    than `over` and two per group use, re-renders a mask whenever the clip and mask pair changes,
    and renders a definition again at every use, so a chain of groups that each use the one before
    twice is exponential (`draw_items`, `coverage` in `graphics_render.rs`). Layers sized to the
    mark's bounds and a per-definition cache of rendered pixmaps would fix both.
  - Metrics are Helvetica's AFM tables on every built-in device; GNU R's bitmap devices measure
    with cairo's font, so `strwidth` on `png()` differs slightly.
  - Hershey fonts fall back to ordinary text, and a symbol-face string that is not valid UTF-8
    (`symbol("\256")`) is measured as replacement characters, where GNU R reads the bytes as
    Adobe Symbol codes.
  - A closed X-spline's first segment is sampled more coarsely than GNU R's (same curve, two fewer
    points); open splines match GNU R value for value.
  - `savedSnapshot` is never kept, `NewFrameConfirm` never prompts, graphics events
    (`getGraphicsEvent`) are unsupported, and `Rf_eval_with_gd` is a plain eval.
  - Bitmap pages are written when the device closes rather than per page.
  - The plot gallery (`oracle gallery`) passes on every device, at seeds 1 to 8 on png, pdf and
    svg as well as by default. On agg, no figure has a pixel
    off by more than 64 levels at any seed from 0 to 24, and 140 of the 149 are identical. The
    other nine (`contour_methods`, `contour_volcano`, `hist_density`, `lm_diagnostics`,
    `log_axes`, `ts_decomposition`, `xspline_shapes`, `pa_correlation`,
    `grid_round_rect_curve`) differ only in the antialiasing of 0.01% to 0.3% of their pixels.
    For `ts_decomposition` the cause is the coordinate transform: user coordinates reach the
    device through one affine map in inches (`user_axes`, `Device::to_device`), where GNU R
    composes `win2fig` and `fig2dev`, so an antialiased vertex can round differently in the last
    bit. The others are not yet diagnosed.
  - GNU R's `R_pretty`
    warnings on subnormal axis ranges are missing, and `stem()` only handles the empty case.
  - Matrix is not in the corpus library; its image methods match GNU R pixel for pixel with the archived Matrix 1.6-5 (the current release needs R 4.4).
  - Rendering speed (release build, `ORACLE_RY=target/release/ry oracle gallery --repeats 4`):
    the whole gallery takes 23.7 s on GNU R and 29.1 s on ry. Base graphics (2.8 s against 2.2 s)
    and grid (0.60 s against 0.39 s) are faster on ry, but ggplot2 (12.4 s against 15.8 s),
    lattice (5.8 s against 8.1 s) and PerformanceAnalytics (2.1 s against 2.7 s) are 1.2 to 1.9
    times slower, worst on `lattice_wireframe` and `pa_rolling_performance`. The devices are not
    the cost (`pdf(NULL)` is as slow): the time is in the packages' R code, and callgrind shows a
    flat profile of closure calls, frame setup, argument binding, allocation and builtin dispatch.
    Measured per call against GNU R: a closure call 0.75 µs against 0.35, `pmin` 15.7 against 6.2,
    `vapply` 8.2 against 3.4, `mostattributes<-` 10.6 against 4.0, `diff` 8.1 against 4.8. The JIT
    no longer adds to it: closures from one site share compiled code, bodies that would mostly
    hand back to tier 0 compile only for loops, and tier 0 records type feedback only for warm
    bodies (`decisions.md`), so the gallery takes 6.99 s with the JIT on against 7.10 s off.
    Closing the gap is interpreter call-path work (`decisions.md`, the interpreter's call
    path), not a graphics fix.
- **GNU R's regression tests under ry** (`tests/*.R` of the R 4.3.3 sources, run with a debug
  build: 64 of 66 end with GNU R's exit status, 35 print exactly its output). Two divergences are
  accepted. d-p-q-r-tst-2 checks `pf` to the last bit, and reg-tests-1e stops at a
  `fisher.test` table where GNU R's FEXACT overflows its hash keys and errors but ry computes the
  exact p-value; it also prints `env.profile` bucket counts that depend on GNU R's string hash.
  reg-large needs tens of gigabytes and times out at debug speed, and utf8 and reg-examples1
  come close to the 300 s limit, so confirm them with a release build. The remaining output
  differences are mostly accepted noise (timings, unseeded random numbers, `runmed` and `kmeans`
  debug traces) plus these gaps:
  - A parse error message lacks GNU R's `<text>:1:3: unexpected ...` layout with the source line
    and caret.
  - A finalizer error prints no `Calls:` traceback, and `gc()` errors name `gc(...)` where GNU R
    names `.Internal(gc(...))`.
  - Deparsing a long pairlist breaks lines at different places, and `dput` of an S4 generator
    call prints the class name with its `package` attribute.
  - `memCompress(type = "gzip")` output differs byte for byte at level 9 (zlib-rs picks
    different matches than zlib), though it decompresses to the same data.
  - `serialize` writes a compact integer sequence (`1:n`) expanded, where GNU R writes the
    ALTREP form, so the bytes differ though both unserialize to the same vector.
- **The help server is not implemented** (`tools::startDynamicHelp`). `Rprof` samples only the
  call stack, without memory, GC or line profiling.
- **Native hazards GNU R shares.** A C++ exception thrown from a package's static initializer
  unwinds through Rust during `dlopen`, and unserializing from a `NativeSource` trusts a corrupt
  length enough to allocate before the read fails. Bound the allocation by the bytes remaining
  when the source can tell.
- **Slow numerical cases inherited from GNU R.** `phyper`, `qhyper`, `qnbinom`, `qnbinom_mu`
  and `bessel_i` with huge arguments take tens of seconds, as in GNU R (`phyper` 32 s and
  `qhyper` 74 s there), and `nmmin` with an infinite start loops as GNU R's does. Fix them with
  asymptotic expansions or argument limits when a package hits them.
- **SANN ignores a custom candidate generator** passed in `ex` and always takes the Gaussian step;
  that needs closures from the interpreter.
- **`pgamma` loses up to about 200 ulps for a large shape far above `x`** (shape 15 at `x` below
  0.1, where GNU R loses 14): `exp(-stirlerr - deviance)` amplifies the exponent's rounding by
  its size, and below shape 9 the direct product `x^shape e^-x / Gamma(shape + 1)` avoids it,
  but above, `gammafn` itself is an exponential. A double-double deviance would fix it. For
  small shapes ry is now closer to mpmath than GNU R (mean 4.5 ulps against 2.8 over a grid
  that includes these large-shape cases, and 26 points above 2 ulps against GNU R's 39); the
  remaining `qgamma` differences from GNU R are GNU R's own last-bit errors times `1/shape`.
- **Platforms.** The `rcmd` toolchain assumes gcc and gfortran on Linux. Windows `R CMD INSTALL`
  follows the Rtools conventions but has only been verified cross-hosted (`decisions.md`, "Windows
  `R CMD INSTALL`"): the release smoke test should install data.table from source with Rtools on
  the Windows runner, and `R CMD config` there needs Rtools' `make` on `PATH`.

## Explicitly out of scope for now

- Modelling S3 dispatch, meaning `UseMethod`. It is the prerequisite for an honest `print`,
  `summary` and `plot`.
- Column-level typing for a data.frame, matrix dimensionality, and real S4 typing.
- Traits and typeclasses. The tripwire is the third constraint kind.
- Auto-generating CRAN stubs through R introspection, R-version-keyed corpora, and stubtest
  validation. All three need R.

## Shipped ledger (one line each; rationale in `decisions.md`, contracts in the docs site)

- **Console attach and the data viewer.** `ry run` serves the debugger, a paged data viewer and live values over a Unix socket; VS Code attaches, opens `View()`, and hover/completion read the console. `decisions.md`, "A running console is a debug adapter".
- **R Markdown and Quarto in the editor.** Chunks of a `.Rmd`/`.qmd`/`.Rnw` are one script for diagnostics, hover, completion and goto, and `ry fmt` formats each chunk. `decisions.md`, "Literate documents keep their offsets".
- **Arrays.** `[1, 2, 3]` parses, formats, types as `Array<T>` and runs as a flagged R vector with strict operators and indexing. `decisions.md`, "An Array is a flagged R vector".
- **`ry load` on ry's own runtime.** Lazy bindings are promises over the analysis's syntax trees, both runtimes apply the same plan, and `test_load.rs` runs on both. `decisions.md`, "the loader on ry's runtime".
- **The runtime's object model, collector and `libR` skeleton.** `crates/heap` and `crates/libr` run brio and `rytest` built against ry's headers and GNU R's. `decisions.md` has the record.
- **Inline annotations at run time.** Boundary checks on ry's runtime, erasure for GNU R, and the `ry run`/`ry R` preflight for the annotation warnings and strict mode. `decisions.md` has the record.
- **Inline type annotations, the comment-agreement warning, and strict mode's untyped parameters.** `name: TYPE <- value`, typed parameters, `-> TYPE` and `function<T>`, sharing the `#:` grammar and lowering, with `annotation-mismatch`, `[check] inline-annotations`, and erasure to plain R. `decisions.md` has the record.

- **`declares_arithmetic` is computed from the project as well as the stubs.** It consulted the stub
  library alone, while operator dispatch resolves a method through the global scope, which includes
  the project's own sources. A package defining `+.Money` therefore had its `+` dispatched correctly
  and its class refused by the numeric constraint, so `bump <- function(x) x + 1L; bump(price)`
  reported a numeric-value error on code R runs fine, where R prints 6. The set now comes from stubs
  and the project, through `GlobalEnv::arithmetic_classes`, memoized per corpus and per project,
  with a script's own top level counting the way its `@type` declarations already do, and it is
  carried on the inference table beside `definitions`. A class that declares no arithmetic method is
  still refused, and R halts on that one too, checked both ways. Measured on the corpus, no finding
  changes across data.table, dplyr, ggplot2 and shiny, so this loosening removed nothing
  load-bearing there. The fixtures are the coverage.

- **A config key written outside its table names the table.** Writing `typing = true` outside
  `[check]` loaded clean, checked nothing and reported no problems. The message now says which table
  the key belongs under.

- **A rendered type can be written back, and it means what it came from.** Every user-visible
  rendering of a type goes through `TypeRenderer`, and nothing checked that the string it produced
  was readable by the `#:` grammar or denoted the type it came from. The oracle is the type system's
  own contract: `#: TYPE` asserts the value is compatible with `TYPE`, and the checker just proved
  the value has that type, so re-declaring the rendered scheme above the definition must add no
  finding. Over 3,159 sources it made 1,131 re-declarations and found 41 violations, being 5 grammar
  refusals and 39 type errors, in two classes. A record field name was rendered unquoted, so
  ``list(`max size` = 10L)`` rendered `list{max size: integer}`, which the grammar refuses, and
  ``list(`a,b` = 1L)`` rendered `list{a,b: integer}`, which parses as `list{a}` and produces a bogus
  `type-mismatch` plus a bogus unknown-type error. And `scalar numeric` was rendered although only
  `numeric` and `atomic` are writable constraints. This is the same class the codebase already
  documented one instance of, where a function member of a union must render parenthesized. The rule
  had been applied to unions and never to field names or constraint spellings.

- **A `<T: numeric>` binder keeps its constraint inside a self-recursive body.** The reference is
  explicit that with `<T: numeric> fn(x: T) -> T` the body may use `x` numerically, and a
  self-recursive body reported `expected a numeric value, found T`. Minimal pairs narrowed it: the
  same annotation over a non-recursive body using `x + 1L` or `x > 0L` was clean, and the
  unannotated function was clean, so writing down the checker's own inferred type turned a clean
  file into a failing one. The root cause is the design-review shape this file records elsewhere:
  two places decided whether a type satisfies a numeric constraint and they read different scopes.
  `Checker` carried `rigid_constraints`, so the operand path knew `<T: numeric>` admits arithmetic,
  while `constraint_rejects` in the unification path had no case for `TyKind::Rigid` and fell
  through to false. A self-recursive call is exactly where the two meet, because it instantiates the
  scheme into a fresh constrained variable and unifies it with the body's own rigid `T`.
  `rigid_constraints` moved off `Checker` and onto `InferenceTable`, where admissibility is decided,
  which is the same move `arithmetic_classes` already made for the same reason. A binder is admitted
  when its declared bound implies the required one, expressed as `declared.join(required) == declared`
  so the lattice order is not enumerated a second time.

- **An omitted optional argument takes the default's type.** `function(x = 1) x` inferred
  `<T> fn([x]: T) -> T`, and a call omitting the argument left the parameter a free variable, so
  `lucky()` was `Any` rather than `double`. `Any` is compatible with everything, so the consequence
  was silence, and `nchar(lucky())`, which is `nchar(1)` in R, reported nothing. The same gap made
  the checker reject a scheme it had inferred itself, which is how the round-trip oracle found it.
  `FunctionType.named` is a `Vec<Parameter>` rather than a `Vec<RecordField>` now, carrying
  `default: Option<Ty>`, because reusing the record-field struct for parameters was why the default
  had nowhere to live, and a record field has no default and never will. The default's type had to
  be threaded through every traversal that walks a function type, and skipping substitution,
  `erase_vars` or `resolve` would have been unsound rather than imprecise, because a variable can
  hide in a default un-substituted or un-level-adjusted. `Parameter::types()` is the one iterator
  they all go through, so a future field cannot be half-walked. A call that omits an optional
  argument unifies the parameter with the default's type, skipped when the call forwards `...`,
  where the argument may be arriving through the dots. A default that cannot fit its own parameter
  is the definition's mistake and stays reported there. A declared default is checked against an
  instantiation of the declared type rather than the rigid binder, because a binder is the caller's
  choice and omitting the argument is the one call where the default makes that choice. Controls
  verified that a concrete declared type still refuses a `NULL` default and a wrong-typed one, and
  that `<T: numeric>` still refuses a character default.

- **Formatter preservation compares a token's text, not only its kind.** `significant_kinds`
  compared a `Vec<SyntaxKind>`, so a formatter emitting the wrong bytes for a token, which is
  exactly what a stale or off-by-one `raw()` range produces, preserved kinds perfectly. R is
  case-sensitive, so that is a miscompile. The injected bug uppercased the first letter of IDENT
  tokens of four characters or more. The shipping `format` battery passed all eight arms, including
  both corpus invariants, and a `(kind, text)` oracle caught 1,723 of 2,731 sources. The pristine
  baseline has no violations and costs 0.1 s for all 3,159 sources, which is cheaper than the check
  it replaces. A control mutant dropping the `L` suffix was caught by the old oracle, because
  `1L` to `1` crosses a kind boundary, so the blind spot was precisely within-kind. Worth having
  alongside it is format-implies-semantics agreement, where formatting must not change the
  diagnostic multiset, with a pristine baseline of no divergence over 2,224 sources at 20 to 35 s.

- **The acceptance differential runs against in-tree inputs.** `check_parse_invariants` guards
  against an error cascade and never asserted that a broken file reports anything, while the parser
  carries a lot of dedup and first-wins logic. The injected bug dropped zero-width ranges in
  `push_error`, which is a plausible polish change. All four `test_fuzz` binaries stayed green while
  parse errors went from 750 to 694 and 24 of 489 broken files became silently clean.
  `crates/syntax/tests/test_corpus.rs::corpus_acceptance` is exactly the right differential, and it
  returned silently because it pointed only at the gitignored fetched `corpus/`. Pointed at in-tree
  inputs, the theirs-only-error direction, where ry accepts and tree-sitter rejects, goes from 2 to
  22 on the corpus and from 11 to 815 over 20,000 seed mutations. The ours-only direction is noisy,
  mostly `#:` annotations tree-sitter reads as comments, so only theirs-only is gated, and a
  baseline of 11 in 20,000 is small enough to allowlist. `tree-sitter-r` was already a
  dev-dependency, and the cost is 97 ms for the in-tree corpus and 765 ms for 20,000 mutations. One
  oracle is explicitly marked unproven: "an `ERROR` node implies at least one reported error" holds,
  with no violation over 3,159 sources and 400,000 fuzz inputs, but it did not fire on this bug,
  because the dropped errors came from files with no `ERROR` node. Prefer the differential.

- **An IDE range is checked for what it covers, not only for being in bounds.** The battery asserted
  `range.end() <= text.len()`, so a rename whose edits are all shifted one byte passed and corrupted
  the user's file. The injected bug was an off-by-one at the one place in `occurrences` where
  item-relative ranges are re-anchored to absolute offsets, which is the design's own documented
  single re-anchoring edge. The shipping battery passed both arms. A name-identity oracle, where the
  text at every definition target and rename edit must be the identifier under the cursor, caught
  748 bad edits, and a round-trip oracle, where definition at a reference lands on a range
  `references` reports and back, caught 358. The pristine baseline has no violations over 3,466
  identifier positions and 1,389 definitions, and costs 4 s.

- **`<<-` inside `local()` reaches the enclosing function frame.** The super-assignment search was
  bounded at `current_function_depth()`. With scopes `[TopLevel, Function(f), Local]` that is 1, so
  `0..1` skipped `f`'s own frame at index 1 and the write escaped to the global environment. The
  bound is now `self.scopes.len() - 1`, meaning everything strictly outside the current scope, which
  coincides with the old bound whenever the current scope is the function frame. That is why the
  closure spelling was always right. Checked against R:
  `function() { v <- 1L; local({ v <<- "two" }); v }` returns `"two"` and now types
  `integer | character` like its closure twin, two `local`s deep still reaches the frame, and an
  intervening `local` that binds the same name still catches the write and leaves the outer slot
  alone. Only the super-assignment site changed, because `current_function_depth` still bounds the
  read and capture logic, where a function boundary genuinely is what matters.

- **A named data argument no longer breaks positional masking, and `base::with` masks.** Two false
  `unresolved` findings on code R runs. The positional counter was not advanced past formals already
  claimed by name, so `with(data = frame, column_a)` read `column_a` as the data and evaluated it in
  the caller's frame. Matching now follows R's own rule, where names claim their formal first and
  remaining positionals fill what is left, which also makes the reordered
  `with(column_a, data = frame)` correct. Separately, the `Namespace` arm consulted only
  stub-declared `@masked` verbs, so `base::with` and `base::subset` masked nothing. The base family
  is recognized under `base` as well as bare. Controls confirm the data argument itself is still
  checked, an in-item local `with` still masks nothing, and `somepkg::with` is still its own
  function.

- **`switch` forks from the entry state per alternative and joins.** It was walked as an ordinary
  call, so its branches were sequential writes, and both passes were wrong. Naming reported a false
  `unused` on the first branch's write in
  `switch(key, a = { r <- 1L }, b = { r <- 2L })`, which is live whenever `key == "a"`, and missed
  the `maybe-undefined` on a later read, which R reports as `object 'r' not found` when nothing
  matches. The checker unioned the branch values correctly but inferred them in sequence, so a later
  branch's write won outright and `switch(k, a = { r <- 1L }, { r <- "d" })` typed `r` as plain
  `character` where the `if` spelling joins to `character | integer`. The new shape is `infer_if`'s
  two-arm form generalized to many. Checked against R for each shape: a matched key returns its
  branch, an unmatched one with no default errors, a default catches it, and `switch(k, a = , b = ...)`
  falls through to one branch rather than two. A branch that cannot fall through, such as one ending
  in `stop()`, contributes no state, as a diverging `if` arm does not, and a local binding named
  `switch` makes the call an ordinary one again.

- **A `repeat` loop exits through its `break` points.** `loop_body` reused the converged loop-head
  state as the exit state, so `Unassigned` from the first iteration survived a loop that always
  assigns. `break` now records the reaching-write state where it occurs, and a loop that cannot be
  skipped exits through exactly those points joined with the body's end state. That is precise in
  both directions: `repeat { x <- 1L; break }` reports nothing, and R returns 1, while
  `repeat { if (cond) break; y <- 1L; break }` still reports, and R errors when `cond` is TRUE. A
  loop with no `break` at all keeps the conservative head join, because it leaves by a jump the walk
  does not model. This was found only because the new `maybe-undefined` code made the state visible
  for the first time.

- **A write-only `<<-` no longer reports its initializer unused.**
  `make_flag <- function() { flag <- FALSE; function() flag <<- TRUE }` gave a false
  `unused flag`, but the initializer is what makes `<<-` find a slot at all: remove it and the write
  goes to the global environment instead. The read path already marked a frame's writes used on a
  capture, through `mark_slot_read`, and the `<<-` target path now does the same for the frame it
  resolves into. Fixing this was not optional alongside the `local()` fix, because that fix makes
  the write land on the frame's slot, which is exactly what turned the initializer into an apparent
  dead store. A real dead store still reports, and an outer binding shadowed by an intervening frame
  is still correctly dead.

- **The `maybe-undefined` warning the reference promised exists, as an opt-in.** `maybe_undefined`
  was computed by naming and surfaced nowhere. It is now the `maybe-undefined` code, gated on
  `[check] maybe-undefined = true`. It is off by default on measured evidence: with it on, six real
  packages report 442 findings, at 242 for data.table's 12k lines, 86 for shiny, 45 for MASS, 42 for
  ggplot2, 18 for dplyr and 9 for targets. The dominant shape is a correlated guard the flow cannot
  see. In `data.table/R/print.data.table.R` the flagged `index_dt` is assigned only in one branch,
  but the read is guarded by `show.indices`, which that same branch sets to `FALSE`. That is safe
  code the flow cannot prove. Default-on would have been the "a clean run means nothing" failure the
  adoption reviews already flagged. A top-level variable's unwritten path is exempt, per the
  contract, because at run time it reaches the enclosing environment.

- **An annotation in a call's argument list is reported.** It attached to nothing and said nothing,
  so a deliberately wrong type beside a lambda argument was invisible. `statement_annotations` sees
  an `ARGUMENT` node next, which is not an expression kind, so the block classified as dangling and
  never attached, and the placement walk visits only statement sequences, never an argument list, so
  nothing reported it either. The scope is narrower than the filed item claimed, and getting that
  wrong twice is the lesson. A first implementation reported every annotation the placement walk did
  not reach, which false-positives on four positions that do attach, and it was built, measured and
  reverted. Verified against a no-annotation control, because an uncontrolled probe read `lapply`'s
  own "not a function" error as evidence the annotation had applied. A braceless function body under
  `FUNCTION_DEF`, a braceless `if` branch under `IF_EXPR` and a parenthesised expression under
  `PAREN_EXPR` all attach. Only a call argument, under `ARGUMENT_LIST`, does not, so the check keys
  on `ARGUMENT_LIST` alone. Two fixtures pin the reports and two pin the attaching positions, so a
  future widening has to break them first.

- **A numeric condition is accepted**, because R coerces one, where zero is false and anything else
  is true. That covers `if (length(x))`, `while (n)` and `!length(x)`. A `character`, `complex`,
  `raw` or vector condition stays an error.

- **Named arguments claim their formals before positionals fill what is left.** `match_arguments`
  walked the argument list once in source order, so a positional argument could take a formal that a
  later named argument was going to claim, and `vapply(xs, character(1), FUN = f)` reported a bogus
  "FUN given twice". `argument_targets` now computes the two passes once and shares them with the
  checking loop and the rest-parameter forwarding scan, which previously duplicated the accounting
  and therefore duplicated the bug.

- **`$` on a union subject no longer demands the field on every member.** A field some shapes carry
  and others do not reads as `T | NULL`, because that is what R answers for a name a list lacks. A
  field no shape carries stays an error, because the typo check is the whole value, and a structural
  refusal such as `$` on an atomic vector is still hard from any member. The did-you-mean is drawn
  from every field the union can carry, so the suggestion survives a member with no fields at all.

- **Literate prose is blanked per byte, not per character.** A non-breaking space is whitespace to
  Rust's `char::is_whitespace` and an unexpected character to R's lexer, so prose containing one
  reported a syntax error against a blank line. The same code blanked per character, so any
  non-ASCII prose shifted every byte offset after it, and every downstream range is a byte offset,
  so diagnostics in later chunks were silently misplaced. Each character now blanks to its own
  `len_utf8()` in spaces. The unit test that should have caught it asserted char count instead of
  byte length.

- **A duplicate type name reports in a script too.** The reference said the duplicate-`@type` error
  fires regardless of file, and it fired in package files only. The value-name analogue stays
  deliberately exempt for scripts. Type names are not.

- **A list of functions works, and the cause was not the union.** `function_compatible` demanded an
  exact parameter count, so `mean`, which has one required and two optional parameters, could not
  serve a one-argument callback interface, and the union of two such functions failed member-wise.
  Arity is a range now: a function serves an interface when it accepts every call shape the
  interface promises. Extra optional parameters are therefore fine, while requiring too many, or
  refusing an argument the interface sends, still fail. `lapply(list(mean, sd), function(g) g(1:3))`
  is `list[double]`. `lapply` also keeps its input's names, through `list[named: T]` declared as its
  narrower first candidate, which is what forced the overload tiebreak to become plain first-match.

- **An S3 method declared in R is not reported `unused`.** The unused walk shares the method-name
  knowledge with the lints, and a project's own generics count rather than only the corpus's.
  Dispatch is still not a read, so the exemption is by name shape, meaning `generic.class` for a
  generic that exists, which is the same signal R itself uses to find the method.

- **`%*%`, `%o%` and `%x%` return the `matrix` nominal**, and the class has its arithmetic and
  comparison methods, so a matrix expression types and composes.

- **A `library()` of an unstubbed package no longer disables bare-name resolution project-wide.**
  Four independent testers reported this, and the most damning version was
  `library(totallyMadeUpPackage)`, naming a package that does not exist, buying the same blanket
  tolerance. The tolerance itself is right, because an unstubbed package's export set is unknowable,
  so the fix narrowed it three ways. A manifest is enough, so a namespace with an `.exports` list
  has a knowable export set and never earns the tolerance. The near-miss carve-out now covers locals
  and parameters of the enclosing item as well as top-level project symbols, because a name one edit
  away from a binding of your own is a typo rather than somebody else's export. And a `library()`
  naming the project itself, through `DESCRIPTION`'s `Package` field, buys nothing, because those
  exports are the project's own definitions. `usethis` generates the `tests/testthat.R` that does
  this, so it was switching unresolved detection off in every testthat package.

- **`structure(list(...), class = "x")` keeps its record type.** It yielded `Unknown` against a
  documented promise that this is a plain record whose class attribute is data, so the field typo
  the guide headlines was caught on a bare `list()` and silently dropped once `class =` was added.
  Those are the objects real packages are built from, and 31 of one tester's 34 strict findings
  traced to this one call. `structure()` now returns its first argument's type. The class attribute
  deliberately still does not mint a nominal type, because `@new` remains the only nominal
  introduction and reading `class[1]` would type a `c("grouped_df", "tbl_df", "tbl", "data.frame")`
  value as `grouped_df` and then reject it at a `data.frame` parameter.

- **Optionality comes from the formals.** A formal with a default is optional in R and no annotation
  can change that, so the exported signature takes optionality from the code and an annotation's
  disagreement is reported once at the definition, naming the fix. Callers of correct R are clean.

- **Files directly under `tests/testthat/` share one namespace**, as testthat's own loading does,
  with helpers first and then tests. A shared fixture is therefore neither `unused` at its definition
  nor `unresolved` at its uses, while a real typo in a test still reports with the helper suggested.

- **A project's own S3 generics are discovered by their bodies.** A top-level definition whose read
  set contains `UseMethod` is a generic, across the whole package namespace, so a generic in
  `R/speak.R` covers `speak.dog` in `R/dog.R`. The generic itself is exempt too, because it declares
  the dispatch argument and never touches it. That fixes `unused-parameter` flagging a project's own
  generics and their methods, and the default-on `unused` reporting an S3 method as dead.
  `is_s3_method_name` and `s3_generics` live in `semantics.rs` beside the other project-level
  projections, shared by the lint and the unused walk.

- **A bad `importFrom` is an error**, matching its `export()` sibling, because R refuses to load such
  a package, so it is not survivable advice and must not pass a `--min-severity error` gate. A bad
  `pkg::name` read stays a warning, because a bad import stops loading outright while a bad
  qualified read fails only if that line runs.

- **ggplot2 `+` chains type.** The corpus declared `+.ggplot` and nothing for a component pair, so
  `theme_minimal() + theme(...)`, and a chain whose left operand was lost through `%>%`, fell through
  to the numeric rules and reported arithmetic on a `gg`. R routes all of it through the single
  `+.gg` method, which the corpus now declares. A genuine mistake such as `plot + 1L` is still
  caught, naming both classes.

- **An inline `` `r expr` `` span is code.** The conversion blanks the delimiter and language tag to
  spaces, the expression keeps its bytes and its offset, and the closing backtick becomes the `;`
  that separates two inline expressions on one prose line. A value a report only displays is
  therefore used rather than unused, and a typo inside an inline expression reports at the right line
  and column. A plain Markdown code span, a span naming another language, a `` `rate` `` span that
  is not an `r` tag, and an unclosed span all stay prose.

- **`fmt` on a target with no R in it reports that it formatted no files and exits 0**, as `check`
  already did. A stage with nothing to do must not fail a pipeline, and a pre-commit hook handing the
  formatter a file with no R in it must not fail the commit.

- **`setkey`, `setorder` and `unique(DT, by=)` work.** `setkey` and `setorder` name columns rather
  than values, so they are `@masked` like the other non-standard-evaluation verbs, while the
  `v`-suffixed forms take a character vector and never needed it. `unique`'s fallback candidate is
  variadic, because `unique` is a generic whose methods take arguments base's signature does not
  name, and the typed candidates stay exact so `unique(c(1L, 2L))` is still `integer[]`. Verifying
  that turned up one more defect: every column-name parameter in the data.table stub was declared a
  scalar `character`, so `setkeyv(DT, c("id", "date"))`, which is the whole point of the `v` forms,
  was an error. They are `character[]` now, which accepts one name or several.

- **A read inside `on.exit()` keeps every write of that name in the frame alive**, exactly as a
  closure capture does, because R stores the expression and runs it at return so it observes the last
  value of what it reads. A genuine dead store beside a guard still reports.

- **An overload selection that commits an `Any` return records a strict origin.** `min(Date)` is not
  typed `integer`. It selects the corpus's trailing `Any` candidate, so the check is skipped rather
  than wrong, and the quality bar holds. What that exposed is that `Any` was exempt from strict mode,
  so the feature whose whole purpose is finding gaps missed the commonest one.

- **`x[i, j]` on an unannotated parameter is sound-by-refusal for any index shape**, which is what
  the reference already promised. A subject whose shape was written down still refuses a shape no
  rule covers, so `c(1L, 2L)[1L, 2L]` is an error.

- **`c()` on a classed value keeps the class when a `c.Class` method is declared**, which is R's own
  dispatch rule, so `c(d1, d2)` is a `Date` and `dates + d2` is still refused as `Date + Date`. A
  nominal with no such method is indeterminate rather than an error.

- **The IDE fuzz harness samples completion once per context instead of per offset.** Completion
  was 587.26 ms of the 592.4 ms that nine features spent over 25 offsets, on a warm database, and
  its only assertion was that a label is non-empty. It was also mostly redundant, because 245 swept
  offsets over 10 seeds produced 64 distinct results. Sampling per completion context, which is the
  kind of token the cursor sits in or after, halved the harness and took the whole `ide` binary from
  195 s to 94 s while gaining two range oracles. Two sharper-looking variants were measured and
  rejected: filtering to token boundaries saved 5%, because in a short input nearly every sampled
  offset already is one, and keying on the pair of surrounding kinds saved 29%.

- **One predicate decides whether a file shares a namespace with its siblings.** `cli.rs`'s
  `shares_a_namespace`, the server's `is_package_path` and `stats.rs` each answered that question,
  and they disagreed. The first two counted `R/` and `tests/testthat/` while `stats.rs` counted `R/`
  only, directly under a comment claiming it ordered files exactly as the CLI and server do. On a
  testthat package `analysis-stats` reported 3 diagnostics where `check` reported 0. All three now
  call one predicate, with the sorting key kept separate from the classification, because
  collapsing those two questions into one flag is what caused the drift. A CLI test pins it.

- **The human reporter was quadratic in findings times file length, and is now linear in findings.**
  Neither review saw it, because both measured through `analysis-stats` or `--output json`, which
  never touch that path. On one file of 8,000 reporting items, the whole cold analysis was 452.6 ms
  and `--output json` finished in 550 ms while plain `ry check` took 10,527 ms. Instrumenting the
  reporter put 6,728 ms of `render`'s 6,811 ms in `read_span`, which is 98.8%, because miette's
  `SourceCode for str` locates a span's lines by walking from byte zero: 31 microseconds per span on
  a 1,000-line file and 245 on an 8,000-line one. `read_span` now answers over a bounded window, the
  span's lines plus a margin wider than the requested context, and translates back into whole-file
  coordinates. It delegates to miette inside the window rather than reimplementing `SpanContents`,
  so one implementation decides what a snippet contains, and a per-file `LineStarts` table makes
  locating the window a binary search. Interleaved, 8,000 findings go from 6,483 ms to 530 ms.
  Rendered stderr is byte-identical on data.table, dplyr, ggplot2 and shiny, related-note snippets
  included. A differential test sweeps the window against miette's whole-file walk over every line
  ending, spans mid-line, across a break, at the start and the end, and a file with no trailing
  newline.

- **A quoting form no longer binds its assignments.** `quote`, `substitute`, `bquote` and
  `expression` arguments were binding, so `quote(x <- 1)` published `x` and every quoted call was
  type-checked. Removing that takes `targets` from 16.6 s to 9.9 s and removes 151 findings, all
  false positives: 139 arity and type errors against calls inside `quote({...})`, which R does not
  run there, and 12 unresolved reads of names mentioned in a quotation. The type-system reference
  holds the contract.

- **A conditional slot's join is bounded at eight writers and widens past that to `Unknown`.**
  `conditional_slot_scheme` called `statement_binding_scheme` for every writer of a name, unbounded,
  from a per-item read, so a name written at 238 documents' top levels made every read of it pay for
  all of them. The bound takes `targets` from 7.14 s to 0.74 s interleaved, and to 0.74 s from
  16.6 s together with the quoting fix, with byte-identical finding sets on `targets` and all four
  corpus packages and no regression elsewhere. It is honest on its own terms, because a union of
  dozens of unrelated types is not a fact a check can use and a real conditional slot has a handful
  of writers. Fixtures pin both sides of the bound.

- **Three per-item queries stopped re-deriving the whole file.** `SalsaGlobals::arithmetic_classes`
  collected a `Vec<String>` of every item name and re-scanned it, `item_tree` was `returns(clone)`
  so the whole item list was cloned per item, and `for_item` found an item's index by linear search.
  They are now a memoized per-file query, a borrow and a memoized position index. On the JSON path,
  8,000 items go from 7,045 ms to 600 ms, which turns a quadratic into a linear curve.

- **`item_hir` and `item_naming` return by reference.** Both were `returns(clone)`, and `ItemNaming`
  is `BTreeMap` and `BTreeSet` throughout, so each fetch was a node-by-node allocation walk. A
  counting probe put ggplot2 at 5.4 fetches per item, costing 178 ms and 265 ms of fetch time.
  Interleaved medians improve 8% on ggplot2 and 13% on `targets`. Peak memory is unchanged, which is
  the expected result: the clones were transient, so they cost allocator traffic rather than
  resident set, and peak is dominated by the memos themselves.

- **The project type map and arithmetic-class set are borrowed per file, not cloned per item.**
  `check_item_with_annotation` cloned both and stored them owned on `InferenceTable`. Both are
  per-file facts and pure lookups, so they are memoized per-file tracked queries returning a
  reference. At a fixed 2,000 items with 2,400 declarations, typecheck goes from 71.3 ms to 27.5 ms.

- **A package file got the position-aware lookup a script already had, so a later top-level write is
  no longer lost.** `SalsaGlobals` built its ordered item list for scripts only. A file is sourced
  top-down whichever kind it is, so an immediate read now consults the nearest earlier writer in its
  own file before the project-wide map. The composition constraint splits on the read kind rather
  than the document kind: a deferred read in a package stands aside and falls through to the
  project-wide winner, because a function body runs after the whole package is sourced, so a later
  file's override must win. In a script the closure runs once that file's frame has settled, so it
  still scans the file. `file_binders` indexes it per file, which took an interleaved 20,000 by
  20,000 case from 4,277 ms to 985 ms.

- **A composite type past `TYPE_SIZE_CEILING` records as `Unknown`, which closed a hang.** A record
  whose forty fields all return that record grew through 877, 8823, 104655 and 1046623 nodes. One
  1,563-line file ran past 200 s and now takes 56 ms, and the package holding it went from over five
  minutes to 129 ms. Findings are byte-identical across 1,951 files of real CRAN sources, and
  interleaved it is about 7% faster on 323k lines. Six other fixes were implemented, measured and
  reverted first, and `MEMORY.md` records the walk-with-a-memo rule they taught.

- **Exponential re-inference of arithmetic operands is fixed in `infer_binary`.** One statement with
  248 arithmetic operators re-walked both operand subtrees per level. `mgcv` did not finish in 180 s
  and now takes about 2 s. `MASS` went from 6.5 s to about 0.3 s from the same fix. R6 was suspected
  in both and was the cause in neither.

- **Strict mode reports a read that the attached-package tolerance silenced.** `unresolved_diagnostics`
  buried the tolerance in a `continue`. `classify_non_local_read` now returns `Resolvable`,
  `Tolerated` or `Unresolved`. The ordinary check reports the last and strict reports the middle, so
  strict reports exactly the reads the ordinary check let through and the two cannot drift.

- **Release-artifact versions have one source of truth each.** The workspace `Cargo.toml` version is
  the truth, and the VS Code manifest carries it with any prerelease suffix removed, because that
  manifest needs a plain `major.minor.patch`. Two tests in `crates/ry/tests/test_release_metadata.rs`
  enforce the derivation, and the assertion message names the exact line to write. A stamping script
  was considered and not written, because a script only helps if someone runs it while `cargo test`
  runs on every slice. The Zed manifest versions on its own line, which `decisions.md` records.

- **The formatter is not slower than the type checker.** The original reading compared a parallel
  command against a sequential one, because `ry check .` fans out over `available_parallelism()`
  while `ry fmt` is a plain loop. On an identical file set with both single-threaded, the formatter
  costs 1.01 s against the checker's 2.26 s. `ry fmt` has since been fanned out and sped up (see
  `MEMORY.md`, "Formatter speed").

- **The overload corpus is not inflated by a missing grammar feature.** The constrained binder works
  in a `#:` annotation and in a `.Rtypes` file alike, and shape-mirroring falls out of it: a project
  stub declaring `zzabs : <T: numeric> fn(x: T) -> T` types `zzabs(1L)` as `integer` and
  `zzabs(c(1.5, 2.5))` as `double[]`, and rejects `zzabs("no")`. The extra candidates carry facts a
  binder cannot state. `abs(TRUE)` is an `integer`, so a type-preserving binder would be wrong, and
  a concrete `integer` parameter accepts `logical` by coercion where a numeric-constrained variable
  refuses it. `min`, `max`, `range` and `sort` carry a `character` candidate no numeric binder
  subsumes. Collapsing `abs` from five candidates to four was behavior-identical and was reverted,
  because one line in one function is not worth a corpus-wide edit.

- **The `rofy` crate is deleted.** `crates/repl` covers its whole surface and exceeds it with Tab
  completion and history persisted to a file, and it highlights off ry's own lexer rather than a
  second parser. The canonical test invocation is now `cargo test --workspace --exclude zed_ry`, one
  exclusion rather than two, and the workspace lost `extendr-api`, `extendr-engine` and `libR-sys`,
  which removes the build-time dependency on a local R.

- **A non-converging cycle terminates, because the refusal no longer depends on the round that
  produced it.** `item_check_recover` pinned only `scheme` at the round cap and took `ItemCheck`'s
  six other fields from the freshly recomputed value, so every round returned a different check and
  the recovery's own equality test could never succeed. It iterated to the 200-iteration cap, 184
  rounds past its own, then panicked with "too many cycle iterations" or exhausted memory first,
  whichever the machine reached. Those two symptoms were always one bug. Its sibling recoveries were
  safe only incidentally, because `global_scheme_recover` and `statement_binding_recover` return a
  bare `TypeScheme` so their pin is already a constant, and `item_check` is the only one of the
  three with a composite return. The pin now re-pins what was already returned, which is a fixed
  point by construction, and it cuts both export surfaces rather than one, because
  `top_level_bindings` was leaking moving schemes out of an item already declared non-converging,
  which its own doc comment said it did not. Verified on the real reproduction: rlang's whole
  package directory went from a 213-second death to a clean 9-second run reporting 806 findings, and
  the flattened 163-file variant that had been killed for memory now finishes in 14 seconds.
  `refusal_is_idempotent` pins the property.

- **`ry check` no longer aborts on a cyclic package interface.** `scc_schemes` was the one query in
  the interface-fixpoint chain with no cycle recovery, while `item_check` and `global_scheme` on
  either side of it both had one. Its doc comment asserted that member checks run directly so no
  cycle forms, and the decision record reserved recovery as a backstop for edges the static graph
  cannot see. The backstop was never installed, so such an edge aborted the process.
  `interface_sccs` builds edges only from names appearing in an item's source, so a name the checker
  constructs is invisible to it and the group is not as maximal as the fixpoint assumes. Recovery
  pins the group to `Unknown`, which is the answer the round cap already gives, and refuses on first
  disagreement rather than iterating, because the group runs its own bounded fixpoint internally and
  letting the outer one iterate it too multiplies those rounds into an out-of-memory kill. That was
  measured: the iterating version turned the panic into exit 137, which is worse than the bug.
  Verified on the real reproduction, where rlang's `R/` went from exit 101 to a clean run with 838
  findings.

- **Fifteen CRAN namespaces ship an export manifest.** A manifest-only namespace has no `.Rtypes`
  and every name types `Unknown`, which is enough to keep unresolved detection alive. They cover the
  tidyverse, `knitr`, `rlang`, `glue`, `magrittr`, `scales`, `jsonlite` and `R6`, and
  `library(tidyverse)` activates the nine members it attaches, so dplyr's and ggplot2's typed
  declarations come with it. The generator script refuses to overwrite a manifest recorded against a
  newer R than the running session, because that drops the names the newer version added and turns
  each use into a false `unresolved`.

- **`strict = true` raising every `unresolved` finding to an error is documented.** Two reviews were
  surprised by it. The behavior is right, so the diagnostics table and the strict-mode section say
  so.

- **A reported column counts characters, and the caret lands under the glyph.** `decisions.md` has
  the record, and the `--output json` field documentation changed with it.

- **Shape-preserving stubs preserve the shape.** `Filter` returned `list[T]` for every input, so
  `Filter(f, c(1, 2, 3)) + 1` was a hard type error, where R selects with `[` and keeps the atomic
  type. It declares the atomic, named-list and plain-list forms in that order now. `rev`, `unique`,
  `head` and `tail` gained the named-list candidate too, so a name read off a reordered or sliced
  list is no longer a missing-field error.

- **Declaration order means first match everywhere.** The tiebreak where all-guesses meant the last
  candidate wins is gone, because a general `Any` fallback is already a fact, which was the only
  case it protected. A narrower candidate can therefore be declared first, which is what let
  `lapply` gain `fn(x: list[named: T], ...) -> list[named: U]` without handing a value use of the
  name the narrow contract. The all-fail diagnostic no longer buries the answer behind the count of
  signatures tried: when every candidate fails identically, or one candidate got strictly further
  into the argument list than the rest, that candidate's own finding is reported at its own
  argument's range. Signature help stopped requiring a commitment, so an incomplete call lists the
  set instead of showing nothing, with the committed candidate rendered instantiated for the call
  site.

- **The check fan-out's worker threads run on `ANALYSIS_STACK_SIZE`.** They ran on default 2 MiB
  stacks while cross-file interface resolution recurses per dependency edge, so a chain of about 500
  definitions aborted the whole command. `analysis-stats` survived only because the command thread
  already carried the larger stack. All three scoped spawns in `cli.rs` use it now, verified on a
  5,000-file synthetic tree where a SIGABRT became a clean exit 1 with findings.

- **The duplicate-diagnostics walk is no longer quadratic in file count.**
  `duplicate_binding_diagnostics` and `duplicate_type_diagnostics` rebuilt a project-wide occurrence
  map per file, and made every file's diagnostics depend on every file's ranges, so any edit
  re-executed all of them. Range-free per-file name projections, which are `top_level_binding_names`
  and `type_declaration_names` and stay value-equal under a body edit, now feed memoized project
  duplicate maps filtered to real duplicates. Those maps are near-empty in a healthy project, which
  is the value-equality firewall, and per-file diagnostics fetch ranges only for files actually
  involved in a duplication. Same-tree A/B at about 530k lines across 5,000 files: the semantic
  render went from 12.1 s to 3.7 s and the cold pass from 22.8 s to 14.4 s, with an identical
  diagnostics byte count.

- **Every real R export resolves, through a vendored manifest.** `decisions.md` has the record.

- **The diagnostics phase stopped scanning every project file per non-local read.**
  The `globalVariables` check was work proportional to reads times files, at about 375 million memo
  lookups on a script-heavy workspace, and a script-local cross-statement read paid the
  project-wide guard chain before its own frame-slot resolution. One memoized project-level
  `globalVariables` union set, `project_global_variable_declarations`, replaced it, with the guards
  reordered cheapest first: masked, then frame slots, then package and stub, then super-globals,
  then declared globals, then imports. Reproduced at 305k lines across 2,500 files, diagnostics went
  from 22.7 s to 2.9 s and the cold total from 28.0 s to 8.9 s, with byte-identical output.
  `analysis-stats` permanently splits that phase into semantic render, lints and assembly, which is
  the instrument that found it.

- **`@masked` is formal-aware, and dplyr is a conditional namespace.** A masked verb's leading formals resolve normally and `...` masks the rest, so a zero-formal mask such as `join_by` masks everything. `decisions.md` has the record.
- **The native pipe types as the call it is.** `hir::lower_pipe` desugars `x |> f(y)` to `f(x, y)`, R's own parse-time rewriting, with the `_` placeholder substituted where R allows it. `decisions.md` has the record.
- **data.table is a conditional stub namespace with a result-shape classifier and typed-subject masking.** `decisions.md` has the record.
- **`ry debug analysis-stats [path]` reports staged cold-pass phase timings and memory.** It lives in `crates/ry/src/stats.rs` and assembles the workspace exactly as `check` does, covering stubs, metadata and Collate order.
- **A missing comma between arguments is reported and recovered from, rust-analyzer style.** The parser anchors the message at the empty range right after the previous element, on that element's line, and parses the next element as a proper node so downstream analysis sees the intended list. Junk that cannot start an element keeps the older recovery.
- **`DESCRIPTION` `Collate` order is honored.** `parse_description_collate` reads `Collate`, falling back to `Collate.unix`, and both hosts rank package files by Collate index before the path tiebreak, with unlisted files ordering after listed ones.
- **NAMESPACE and DESCRIPTION metadata feed resolution.** `decisions.md` has the record.
- **Blame ranges are precise through trailing trivia and parentheses.** HIR lowering stores the trivia-trimmed significant range, so an expression range no longer swallows whitespace or a comment the Pratt loop consumed while peeking. Type-error blame drills through `Paren`, while strict origins and missing-formal reads deliberately keep binding-site ranges.
- **`[` on vectors is defined**, across the full matrix of index shape against subject shape, implemented by `subset_result` through a `vector_index_shape` classifier. The typing reference is the contract and seven fixture cases pin every row.
- **A call argument that is the enclosing function's bare `...` skips both arity checks**, because it forwards an unknown number of arguments. `CallArgument::forwards_dots` carries it.
- **Every high-use base, stats and utils stub declares its real optional formals.** The capped-stub premise did not hold, and the remaining single-parameter stubs are genuinely unary in R. A fixture pins both the direct call and the callback-forwarded one.
- **Ten typing fixtures pin the overload-set and function-compatibility rules.** Writing them exposed a reference contradiction on a non-call use of an overloaded name: the implementation resolves to the last and most general candidate with recorded rationale, and the reference wrongly said first. The reference is fixed.
- **Thirteen typing fixtures pin the `[[`, `[` and `$` rules and the guard-narrowing rules.** The per-position IDE differential over them caught one real defect: `$` field completion now offers a non-syntactic name in its backtick-quoted, insertable spelling, through `syntax::is_syntactic_name`.
- **Eight typing fixtures pin the operator rules and the reserved constants.** Writing them caught a real bug, because the typed `NA_*` constants all lowered through the bare-`NA` catch-all and inferred `logical`. `LiteralKind::Na(NaAtom)` carries the atom at lowering now.
- **A whole-project walk reads a small tracked projection of naming, not full `ItemNaming`.** `item_interface_reads` and `item_top_level_names` serve the interface group walk, `conditional_slot_items` and `script_definition`'s statement branch, so a body edit that shifts ranges without changing a name backdates and no project-wide walk re-executes per keystroke. Measured by `examples/keystroke_probe.rs`, walks went from 8 of 8 edits to 0 of 8. A new-item edit still re-runs them, as it must.
- **Two shadow lints ship, both default-off.** `shadows-builtin` covers a top-level binding over a `base` export and `shadows-namespace` covers one over another stub namespace's name. Both read the stub corpus only, because bare resolution is ungated, so there is no NAMESPACE plumbing and no drift between the CLI and the LSP. A dotted S3 name is naturally exempt, because it is not an export.
- **A top-level slot's read-before-write observes the cross-item binding.** It resolves through `GlobalEnv::scheme(name, deferred = false)`, which is the nearest earlier item in a script and the definition winner or conditional slot in a package, mirroring the unused check's cross-item-read rule. The observed type is materialized as the slot's pre-state, re-established after each loop-pass rollback, so the loop join keeps it and a first-iteration type error survives to the reported stable pass. A self-referential-only name keeps the tolerant `Unknown`.
- **Generic-application arity errors report at the applied name.** That covers a generic applied with the wrong number of type arguments, a non-generic given arguments, and a bare generic that must be applied, except under `@new`, whose representation check infers the arguments. A mis-applied name floors to `Unknown` in the relations, so the one arity error never cascades.
- **The golden error suite covers every distinct lexer and parser message template**, at 78 cases organized by area, plus recovery-locality and valid-stays-clean pinning. Writing it exposed and fixed a real cascade class, where a lexer `ERROR_TOKEN` was re-diagnosed by the parser, giving up to four reports for one mistake and now giving one.
- **The formatter docs page is generated by the shipping formatter.** `crates/format/tests/test_format_docs.rs` regenerates it from `crates/format/tests/formatter.template.md`, so every template example formats byte-identically to what the page shows.
- **Converging recursion types precisely, and strict mode attributes the remainder.** `decisions.md` has both records.
- **The corpus grew to 965k lines across 81 packages**, from 507k, after the fetch manifest gained 28 large CRAN packages and the source-extension pattern widened to R's full set of `.R`, `.r`, `.S`, `.s` and `.q`, because mgcv ships `.r` and Hmisc ships `.s`. Everything parses with no acceptance or round-trip divergence. The one new acceptance divergence was a real lexer bug, where `..2dge` mislexed as `..2` plus `dge`, because R has no dot-dot token at lex time so a longer name wins. A golden case pins it.
- **A hover carries a definition summary**, naming whether the binding is a local variable or a package global and where it is defined, a stub's origin namespace and overload count, and a maybe-undefined note. With `debug = true` it also carries lowering, naming and parsing sections.
- **`ry check` runs on the query engine.** `decisions.md` has the record.
- **Interface SCC rounds run per definition, with change-driven skips.** `decisions.md` has the record.
- **A file's check names only what its interface reaches**, so a file opened at server startup settles in about 200 ms. `decisions.md` has the record.
- **`data.frame()` and `matrix()` return their nominals**, so they hover and annotate as `data.frame` and `matrix` instead of `Unknown` and `Any`.
- **Whole-file inference got about four times faster from constant factors.** `decisions.md` has the record of the data model.
- **One inference runs per file per revision, and the typo hint is memoized per symbol.** `decisions.md` has the record.
- **A same-file backward reference is walk-resolved and never interface-imported**, which kills the fake same-file SCC blowup, about a hundredfold on a chain file. A script overlays its declarations on the memoized package type environment, where it used to rebuild from every package module. `decisions.md` has the record.
- **The semantics core.** Multi-member unions, the mutable-slot model with union joins, `<<-`, `->` and the replacement forms, the coercion policy, name-aware signature matching, `Unknown` and `Any` tolerance in `c`, `for`, `$`, `[[` and `[`, and flow-sensitive guard narrowing with divergence-aware joins.
- **The stub surface.** `T[]` constrained generics, overload sets with probe-committed two-round selection and signature-help and hover display, opaque `@type` nominals such as data.frame, factor and matrix, named-into-rest absorption that types `read.csv` and `lm`, and a corpus of about 530 declarations.
- **Trust and user experience.** The config subsystem rebuild, with nearest-ancestor discovery, per-lint severity, config-file diagnostics and a reload refresh. Per-file typing directives `# typing: on`, `off` and `strict`, through a tri-state `TypingMode` with one gate everywhere. Data-masked resolution for non-standard evaluation.
- **Editor features.** Hover quality, rendering `name : TYPE` with overload notes and constraint display. Annotation cursor features through re-lexing, giving hover, goto-definition and completion inside a `#:` comment. An insert-annotation code action that round-trips. Unused fade-outs. The formatter rewrite with `#:` block handling.
- **The engine and its scheduling.** A red-green core with per-symbol interface firewalls, an SCC fixed point, tombstones, eviction and a stacker-grown validation spine. Durability tiers, with open documents LOW and the corpus HIGH, and a sound downgrade that re-mins through cutoff nodes. Memoized completion.
- **The docs site.** Getting started leads with a real bug, installation is its own page, the typing guide and reference are contracts, and the architecture, structure and testing contributor pages are current.
- **`legacy/` is deleted after a full port.** Every case of the previous implementation's suites was rendered on the shipping stack, bucketed by agreement and adjudicated against the typing reference, which fixed 18 real bugs and ported the distinct cases into the fixture suites. Its incrementality contracts became `crates/semantics/tests/test_incremental.rs`, which surfaced and fixed the missing per-name firewalls, its performance instruments became the one `test_perf.rs` witness, and its 1,967 programs stay as inputs in `crates/syntax/tests/programs/`.

## Benchmark code bases (measured)

`just bench` generates the three 300K-line code bases (about 413K lines with tests) and prints this
table; `tools/bench/lsp_latency.py` drives `ry server` over stdio. Release build, 4-core container,
typing on:

| code base | cold check | peak RSS | findings | body edit first / settled | signature edit first / settled |
|---|---:|---:|---|---:|---:|
| finance | 1.36-1.6 s | 406 MiB | 1005 `unused` (genuine), 848 `object-system` | 1 / 44 ms | 2 / 39 ms |
| data science | 1.5-1.65 s | 400 MiB | 2337 `object-system` | 2 / 41 ms | 2 / 39 ms |
| biostatistics | 1.4-1.7 s | 403 MiB | 784 `object-system` | 1 / 41 ms | 1 / 40 ms |

The `object-system` warnings are the intended lint on the generated R6 and S4 classes. The cold check is CPU-bound (about 4.2-5 s of CPU on four cores, about 3 times its wall time). The latest profile of the finance bench (1.4 s, 4.6-4.8 s of CPU): item checking is about a third of samples, parse about 12%, the interface reads about 8% (most of it frame lookups and top-level names), naming about 8%, and no function above about 4% exclusive. The syntax and check passes keep all four cores busy; about 0.1 s between the syntax pass and naming runs at half the cores. The struct-of-arrays HIR lowered from the green tree (`decisions.md`, "HIR as struct-of-arrays, lowered from the green tree") measured, in interleaved runs against the list-owning HIR, 7 to 12% less CPU, 6 to 10% less wall time and 23 to 35 MiB less peak RSS on all three code bases, with identical findings; the edit latencies did not move. Keeping literal text in a per-module buffer instead of interning it took off about another 0.1 s and 0.2 s of CPU; against a main build in the same interleaved runs the median cold check went from 1.74-1.86 s to 1.58-1.60 s, 5.3-5.7 s to 4.8-4.9 s of CPU, and 425-440 MiB to 399-403 MiB. After it, `item_hir` is about 9% of samples and parse about 12%, and parse is now the largest cost of the syntax pass. The machine was busier for these numbers than for the older rows below: in the same interleaved runs the build before hash-consed types measured 1.75-1.9 s, 5.1-5.6 s of CPU and 439-450 MiB. The profile is flat: item checking is about 35% of samples, then HIR lowering (13%), parse (11%), naming (8%), the syntax lints and the annotation queries, and the hash-consing table itself is about 3% exclusive. The syntax pass ran in batches with eviction between them and kept about 78% of the cores busy; it is now one parallel loop like the naming and check passes. Levers left toward 1 s: a cheaper parse (done since: the flat preorder tree replaced rowan, see "Check cache follow-ups"), re-interning names the HIR already holds as `Symbol` (naming's `mint_binding` and `GlobalEnv::scheme` take `&str` and intern again, about 2%), the `GlobalEnv::scheme` and `layers` lookups (about 11% inclusive), and the syntax pass's idle cores. Hashing `Symbol` by its index (ordering stays by spelling, so sorted output is unchanged) and the compact HIR (name sites in one side array, `NonZeroU32` ids, a 16-byte expression kind and an 8-byte argument; `decisions.md`, "HIR as struct-of-arrays") together measured, in interleaved runs against a main build that already kept trees resident, medians of 1.52-1.58 s against 1.61-1.66 s, 5.0-5.3 s against 5.5 s of CPU, and 533-538 MiB against 563-568 MiB, with identical findings. After them the heap peak on finance is 490 MiB under the system allocator: resident green trees 127 MiB, HIR kinds 30 MiB and ranges 15 MiB, the `item_check` memos about 50 MiB, closed top-level schemes 21 MiB (2.3 million allocations, `check.rs` where `top_level_bindings` is built), and the expression-type tables 15 MiB. Tried without gain: a per-check cache of common scalar types, mimalloc's purge and huge-page options. A thread-local front cache for the type table measured no wall-time gain alone, but sampling showed the shard mutexes contended, so it is kept (`decisions.md`, "every host keeps every tree"). Before the green-walk fusion, immortal interning, frame-binder query, one-batch passes and input durability, main measured 2.3-2.5 s, 6.4-7.0 s of CPU and 510-517 MiB on the same machine. Before tree
eviction, compact names and durability the same machine measured 3.4, 3.6, 2.8 s, about 790 MiB, and
47 to 63 ms settled; before the cold-path work, 6.1, 6.7, 6.2 s, about 1 GiB, and 95 to 120 ms.

Before the fixes the same code bases reported 3514, 2421 and 1185 false errors. Remaining debt:
- Peak RSS of `ry check` is 417-419 MiB on the benchmark code bases (`decisions.md`, "`ry check`
  releases its trees once, and memoized records are compact"). The largest heap holders left are
  HIR kinds (30 MB) and ranges (15 MB), salsa's dependency edges (about 30 MB), the naming and
  check memos, the source texts (20 MB) and mimalloc's overhead over glibc (about 45 MiB). A
  typed, strict finance bench peaks at about 712 MiB, because strict origins are recorded there;
  that table is the next memory target for strict projects.
- The server keeps every tree. Bounding its tree memory would need eviction without salsa's
  per-read bookkeeping.
- The cold check's remaining waits are salsa claims on project-wide queries
  (`duplicate_binding_map`, `project_arithmetic_classes`, `project_type_definitions`) computed by
  one thread while the others need them; deriving them before the diagnostics pass, or as
  per-file folds, would let the pass run without them.
- Interning names as `Symbol` added about 7 ms to the settled re-check (not isolated: `Symbol`
  hashes and orders through its string). Durability more than recovered it, but the cost is still
  there to win back.
- Parses outside salsa remain in `server.rs` range formatting (re-parses the document text per request instead of reading `semantics::parse`) and the `ry debug` dumps; route range formatting through the memoized tree.
- The CLI forgets the database instead of dropping it (about 0.9 s of frees on these code bases), so
  `check` cannot reuse a process for a second analysis without leaking the first.
- "Settled" includes the server's idle delay, so it is an upper bound on the semantic wave.
- An `Unknown` argument to an overloaded call now yields `Unknown` when numeric results disagree
  (`sum(x[[i]])`); other disagreements still take the first candidate.
- A top-level zeallot pattern (`c(a, b) %<-% x` outside a function) defines no package global,
  because `top_level_definition` yields one name per item. Other files then report `a` as
  unresolved and the defining file reports it unused. Destructured names also lack declaration
  occurrences in `ide.rs` (goto-definition across files, inlay hints, unused-assignment fixes).
- `Function | Null | Var` callee unions fall through to the not-callable finding.

## Open: the data.table bridge

`crates/interp/src/data_table.rs` declines these to data.table. Each one is a shape to accept once the engine can reproduce data.table's result exactly, checked by a new `datatable__*` oracle case.

- Keyed subjects: accept them by carrying `sorted` through filters and selections the way data.table does, and binary-search `x[.("a")]` key joins.
- Auto-indexing filters (`col == v`, `%in%`, `%chin%`): data.table adds an `index` attribute to the source on the first such filter. Accepting them means building that attribute (`setindexv`) after answering.
- Reductions outside GForce form, and `:=` with `by` over doubles: base R sums in long double there. The engine would need a long double (or compensated, then verified) sum path.
- `median`, `var`, `sd`, `prod` and `mean(na.rm = TRUE)` on doubles in GForce: data.table's own algorithms (long double accumulation, two-pass variance) must be mirrored first.
- Integer sums in grouped `:=` are exact and could be accepted now; they decline because the reduction rule does not split by column type yet.
- Joins without `on =` (key joins) and `x[]`.
- `roll = "nearest"`: at a tie data.table takes the earlier row and Polars' as-of join the later one, so the bridge declines it. Accepting it needs a tie-break toward the earlier key.
- `.EACHI` over a non-equi join, or with a `j` that reads a join column: data.table reports `i`'s bounds there, so these decline.
- The benchmark code bases' data.table-heavy tests have not run under the bridge: their other dependencies (testthat, R6, purrr and more) are not installed in the default environment.

## Open: findings from the records, library and frame round

- **A record prints a classed field as its bare data.** `print((first = as.Date("2024-01-01"),))` shows `(first = 19723,)`, because the record printer deparses each field without its class. Print a classed field through `format()`, as `print.default` does for a list element.
- **A tag payload prints its fields in sorted order.** `:Rect(width: double, height: double)` is shown
  as `:Rect(height: double, width: double)` in `type-mismatch` and `non-exhaustive-match` messages,
  which makes a diagnostic differ from the user's own definition. Show the declaration order.
- **`@Money(list(amount = 1, ...))` is a kind mismatch with no hint.** A `list(...)` never fits a
  record, so blaming the whole call is right, but the message should suggest the native literal
  `(amount = 1, ...)`.
- **A join column missing from both frames is reported twice at one range**, once per frame
  (`sales[ids, on = "teem"]`). One finding naming both frames would be quieter.
- **Strict mode reports an `if (x < 0) stop(...)` guard** as an undetermined type (already on
  `strict-mode.mdx`); a function that ends in `stop()` should type as `Never` there.
- **`ry run` on a debug build is unusable for loops** (a 20M-iteration loop took over three minutes
  against 0.7 s on GNU R), so the JIT could not be checked in this pass; run the docs' runtime claims
  against a release build.

## Open: editor parity

- Completion at two characters is weakest on call heads (httr2 72%, purrr 76%): a package's own functions share long prefixes (`req_`, `resp_`, `oauth_flow_`), so two characters cannot pick among them. Measured and rejected: ranking by words a candidate shares with the enclosing function (helps at half the name, hurts at two characters) and spellings before calls. A signal beyond use counts (the types the call site expects) would be needed.
- An installed package's `exportPattern` is read with the `regex` crate, which accepts most but not every POSIX extended regular expression R allows; a pattern it rejects exports nothing (logged).
- Semantic tokens: `col("name")` strings are not colored as properties (columns read inside a `DataFrame` `[` query are).
- Debugger: columns come from the srcref's tab-expanded column, so they are off on lines with
  tabs.
- The console socket and everything on it (attach, `View()`, live hover and completion) is Unix
  only; Windows needs a named-pipe transport in `interp::console_server` and `ry::console_link`,
  which waits for a Windows runner to test it on.
- The `Browse[1]>` prompt always shows depth 1 (GNU R counts nested browsers), and `where` names
  frames by function, not by call as GNU R does.
- The data viewer filters by one text over all columns and sorts by one column; per-column
  filters (a numeric range, a factor level) and multi-column sorts would extend the `ryTable`
  request the same way.
- Code lenses: "Run test" only finds a `test_that()` whose description is a plain string literal;
  `describe()`/`it()` blocks have no lens.
- Call hierarchy covers project top-level functions only; a local helper or a library function
  is not an item. Calls through `lapply(xs, f)` and `do.call` are not counted as calls. The
  coverage roles in `ry debug analysis-stats` (incoming, outgoing, workspace symbols) are 99% or
  more on R6, purrr, httr2 and finbench.
- Gaps measured by the editor-coverage table of `ry debug analysis-stats` on CRAN R6, purrr and
  httr2 (`pkg::name`, `$` field, argument name, call head, variable):
  - Goto on a `$` field answers 0% of the time. A field of a list built in the project could go to
    its `list(name = ...)` or `x$name <- ...` site; R6 `self$`/`private$` members are most of them.
  - Goto and hover on calls into imported packages with no stub (rlang's `is_null`, `enquos`;
    lifecycle) answer nothing; purrr's call heads answer 74%. More stubs, or reading an installed
    package's R source for navigation, would close it.
  - A named argument that R completes by partial matching (`f(fac = 1)` for `factor`) is not
    linked to its parameter, so a parameter rename leaves it behind.
- Not done from rust-analyzer's list, with the reason:
  - Closing-brace hints (`} # fn_name`): cheap, worth adding once someone asks; long R functions
    are where they help.
  - Document highlight already exists.

## Open: editor latency at 300,000 lines

Measure with `python3 tools/bench/lsp_latency.py --ry target/release/ry --root .bench/finbench
--quiet 2` against the LSP, and with `ry debug analysis-stats` in process (its "editor requests"
table). Generate the code bases with `tools/bench/generate.py --lines 300000`. A `--quiet` under
about 2 s lets the first edit overlap the startup cold check and reports a false one-second spike.
- Keystroke to settled diagnostics on finbench (4 cores, 40 runs) meets the targets (median at most
  30 ms, 95th percentile at most 100 ms): a body edit settles at a 4 ms median and 6 ms p95, a
  signature edit at 4 ms and 5 ms. The slow spots left are the first open below and a cold hover
  (342 ms max, the first hover after startup).
- A new top-level name still re-runs `package_definition_index` (one pass over every file's definitions), the `package_name` projections the edited file reads, and `project_typo_suggestion` for each unresolved name a re-checked file has (a scan of every project name of a similar length). About 4 to 6 ms on dsbench in `examples/keystroke_probe.rs`. Chunked per-file-group indexes would make the fold proportional to one chunk.
- A file opened at startup settles 170 to 240 ms after the open on the three benchmarks (4 cores; `decisions.md`, "a file's check names only what its interface reaches"). What is left is parsing every package file in the warm-up (`warm_project_folds`) and the file's own check, about 0.25 s on one thread after parsing (`crates/ry/examples/cold_open_probe.rs` breaks it down per query). Earlier samples of the old naming warm-up showed about a fifth of helper time spinning on salsa 0.28's per-ingredient claim mutex (`SyncTable::syncs`) and another fifth in mimalloc's page slow path; re-sample the new warm-up before acting on that.

## Open: false errors on real CRAN code

`ry check R` on 25 packages from the benchmark dependency set (purrr, rlang, cli, httr2, zoo, xts, lubridate and others; sources fetched from CRAN, each with `[check] typing = true`) went from 669 errors to 175 after the call-checking, name-resolution and inference fixes (`decisions.md`). What remains, by message shape:
- 38 "expected X, found Y" and 17 "expected a numeric value": mostly `T | NULL` results of functions that really can return NULL, vector `if` conditions, and parameters pinned by an unconditional write before a test (xts `Ops.xts.R:52`).
- 26 "exported but this package defines no such top-level object": dynamic exports (cli `pb_*` through `makeActiveBinding`, rlang `ffi_` natives, lgr's `lgr`).
- 20 "this call leaves out": formals the body does force on some path, worth a second look per site.
- 17 "not a function": parameters named like a function and called (`log`, `serialize`, `format`, `lag`) where the parameter's own type is open.
- 15 "expected a list": `$` on a value that is definitely NULL, and R6's metaprogrammed `generator_funs`.
- 12 "expected a comparable value": `while (n && out)` pins a counter to logical now that comparisons no longer imply numeric (rlang `stack.R:99`).
- 5 matrix indexing, a documented limitation.

## Check cache follow-ups

- A partial `ry check` still parses every file (about 0.3 s of its 0.45 s on 4 cores), because
  `package_definitions` and the other project-wide folds read every file's item tree. Persisting
  each file's share of those folds next to its findings would remove that floor.
- A cold check of the finance bench is about 1.48 s median on 4 cores (4.8 s of CPU, 431 MiB peak
  RSS), so the 1 s target still needs about a third less CPU work. Under callgrind it is 22.0 G
  instructions, after the one-shot check path, shared small green nodes, unaligned mimalloc entry
  points and green walks in the annotation and lint passes (`decisions.md`, "One-shot checks record
  coarse dependencies"). (The red nodes these figures counted are gone with rowan; see the flat-tree
  item below.) What is left is flat: item checking about 7.7 G
  inclusive, parse about 3.1 G (lexing 0.7 G, about 105 instructions per token in the tree builder
  and 300 per node), naming 1.9 G, interface edges and reads about 3 G, the syntax lints 1.7 G, and
  mimalloc about 1.5 G over 19 M allocations. A real third needs fewer allocations in the checker
  (`map_child_types`, `resolve_rec`, the `Ty` maps' rehashing) and a cheaper parse, not one cut.
- The checker's working maps now live in a pooled bump arena and its names are interned
  (`decisions.md`, "The item check's working maps live in a pooled bump arena"), which took the
  finance bench from 19.78 M to 15.82 M mimalloc calls and 23.59 G to 22.68 G instructions,
  with wall time unchanged within noise (median about 1.34 s). The
  1 s target still needs about 35% less CPU, and no single function is above about 1.2% of
  self cost. What is left is structural:
  - Done: rowan is replaced by ry's own flat preorder tree with windowed item trees, one-shot name
    lookups read the project maps once per query, unlisted frame binders are built once per item,
    and overload probes reject by call shape before instantiating (`decisions.md`, "The syntax
    tree is ry's own flat preorder arena" and "Overload probes reject by call shape first"). The
    finance bench is now 22.5 G instructions and 11.1 M `mi_malloc` calls under whole-process
    callgrind (from 23.6 G and 19.8 M), wall median 1.11-1.15 s against 1.35-1.45 s for the previous
    build in interleaved runs, 3.6-3.7 s of CPU and 371 MiB, with byte-identical findings. About 10% of wall time remains to the 1 s target.
  - Done: a cold check holds one parse tree per worker, skips the per-item syntax memos and the
    editor-only check facts, and purges freed memory after 10 ms (`decisions.md`, "A cold check
    holds one parse tree per worker"). Peak RSS is 298-306 MiB on the three benches, under the
    300 MiB target, but with only 1-3 MiB of margin on finance and data science; wall time is
    1.1-1.3 s on a noisy 4-core machine, and instructions moved only 23.4 G to 23.0 G.
  - salsa's per-query edge bookkeeping that remains is per item, behind the firewalls
    `test_incremental.rs` pins; a coarser one-shot `item_check` would be the next cut. It is also
    the next memory cut: salsa edges are about 29 MiB of the heap peak, 8 MiB of it `item_check`.
  - Done: kind-column scans, phase overlap, one-allocation HIR, memchr lexing and a one-pass tree
    finish took finance to 17.6 G instructions and 222-228 MiB in release (`decisions.md`, "The
    one-shot check scans kind columns and overlaps its serial phases"). Left: the parser and
    `TreeBuilder` self cost (about 1.4 G), the lexer's ident scans and per-token work (about
    0.65 G), and salsa fetch overhead spread across queries. Tried without gain: skipping the
    `SalsaGlobals::layer` memo in one-shot mode (+64 M).
  - checker work itself: `infer_ordinary_call` is still the largest inclusive cost; `match_arguments`
    and type instantiation dominate it.
  - small `Vec` growth spread across many sites (`map_child_types` alone reserves about 88k
    times on the slice), which a small-buffer type would absorb.
