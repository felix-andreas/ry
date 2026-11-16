# Decisions: Builtins and base R

Base R's builtins and native kernels, numerics, regular expressions, connections, the default packages, and package code that runs natively.

## One connection table, in the interpreter

**Status:** built.

- `interp::connections` owns the `#[repr(C)] Rconn`, the table, the devices, the sinks and every connection internal (GNU R 4.3.3's arities). Two tables meant R-opened connections were invisible to `R_GetConnection` and package custom connections (curl, processx) unreadable from R.
- A package's C method is called through `Hooks::connection_method` (libr's `native.c` trampolines), ry methods that C calls enter through `Hooks::enter_from_native` so an R error becomes the `longjmp` C expects, and the default `vfprintf` arrives as `Hooks::connection_vfprintf`. libr keeps only `R_new_custom_connection`, `R_GetConnection`, `R_ReadConnection`, `R_WriteConnection` and `ry_connection_count`. Hook defaults error, so the interpreter builds and tests without libR.
- The interpreter's root source traces what the table holds, replacing preserve and release. libr's `boundary::warn` defers onto the interpreter's warning list, the one warning store.
- `closeAllConnections` is a base closure in GNU R, so ry registers no internal for it.

## Base R's linear algebra

**Status:** built.

- `crates/interp/src/linalg.rs` calls the system BLAS and LAPACK through `dlopen` (the workspace builds without them; a missing library is an R error), with GNU R's arguments, workspace queries and gfortran's hidden character lengths (`crates/interp/tests/test_linalg.rs`).
- **`dqrdc2`, the `dqrsl` family and `dtrco` are Rust**, since no system library ships R's LINPACK variant. They call BLAS level 1 because those decide the bits: reference `dnrm2` is scaled, not a plain square root.
- **One table of base's native routines**, `native::BASE_ROUTINES`; `getRegisteredRoutines` derives from it and every `.Call`/`.C`/`.Fortran` asks `native::call_base_routine` first, so `qr()` works without libR. A routine is base's only when its `NativeSymbolInfo`'s `dll` is `base`. GNU R's `R_init_base` calls `R_forceSymbols(dll, TRUE)`, so `.Fortran("dqrdc2", PACKAGE = "base")` errors, `is.loaded("dqrdc2")` is `FALSE` and `forceSymbols = TRUE`; ry matches all three.
- **Matrix products are ry's own kernels.** `%*%`, `crossprod` and `tcrossprod` sum in reference `dgemm`/`dgemv`/`dsyrk`/`zgemm` order (columns as axpys), which is also GNU R's NaN fallback order, so one kernel serves both and the NaN screen is gone. The result is GNU R's on its bundled reference BLAS on every machine; the host BLAS (CI's OpenBLAS with FMA moved bits by 1 to 2 ULP) is not used by default. Complex products use Fortran's plain multiply on finite operands and C99's otherwise. A faster kernel must keep each entry's sum sequential in the inner index (`backlog.md`).
  - A source-built GNU R links its reference `libRblas`; Debian's `r-base` may resolve `libblas.so.3` to OpenBLAS, which ry does not reproduce by default.
  - `options(ry.blas = "host")` (or `RY_BLAS=host`; the option wins) uses the host BLAS with GNU R's routine choice from `array.c` and its `matprod` NaN screen. Last-bit equality with an OpenBLAS GNU R is unverified.
  - `tcrossprod(x, y)` with a vector `y` takes it as a row when `nrow(x) == 1`, as `do_matprod` does.
  - `options(matprod = "internal")` sums in x87 long double (`long_double_product`).
- **Related long double ports.** `cumprod` (`long_double::running_products`), complex `sum`/`prod`/`mean`, two-argument `cor` (`sqrtl`); one-argument `cor` takes the double `sqrt` of the rounded variance. `fft`/`mvfft` port `fft.c`'s `fft_factor`/`fftmx` (`fft.rs`).
- **Quirks kept**: `La_solve` norms the factored copy when `a` was not double; `La_dgecon` uses `n` as leading dimension, which ry refuses for `m < n` rather than read past the matrix.
- **Buffers sit at GNU R's addresses modulo 16.** OpenBLAS's SSE2 level-1 kernels (Zen, Sandybridge, Nehalem and older) peel a first element that is not 16-aligned, which regroups partial sums. GNU R's vector data starts 48 bytes into the node: 128 data bytes or less come from small-vector pages at 8 modulo 16, larger from `malloc` at 0; `R_alloc` follows the same rule. `Placed` and `PlacedInput` in `linalg.rs` hand each buffer at GNU R's address (copying small buffers when needed). Moving vector data in the heap was rejected: `VECTOR_DATA_OFFSET` is baked into the JIT and the image. Package code calling BLAS stays exposed (`backlog.md`, "BLAS buffer addresses in package code"). Test with `RY_TEST_REFERENCE_LIBRARIES=1` and `OPENBLAS_CORETYPE`.

## The default packages' utils, tools, grDevices and graphics routines

**Status:** built.

The routines live in `crates/interp/src/utils.rs`, `tools.rs` and `crates/graphics/src/routines.rs`, wired in `builtin_libraries::implementation`, derived from GNU R's documentation and observed output (oracle cases `io__*`, `colours__*`, `graphics__*`), never its C source.

- **One graphics engine.** Base graphics and grid call the engine (`crates/graphics/src/engine.rs`), which calls devices through a `DevDesc`'s C callbacks. `GEDevDesc`, `DevDesc`, `GEContext` and `GESystemDesc` are `#[repr(C)]` copies of R_GE_version 17 because grid and ragg touch fields directly; built-in devices are `DevDesc`s with Rust callbacks (`crates/graphics/src/render.rs`), so ragg and `pdf()` take one path.
  - The display list is struct-of-arrays in `EngineDevice`, and `GEDevDesc.displayList` stays `R_NilValue`; a pairlist is built only for `recordPlot` and `dev.copy`. Cost: a C device that tests `displayList != R_NilValue` before replaying on resize never replays (none does today).
  - `R_GE_checkVersionOrDie` accepts engine versions 14 to 17. `GEDevDesc` gained `lock` before `gesd` in 4.4, so each system descriptor sits in two adjacent slots with `ask` mirrored. `R_GE_getVersion` and a recorded plot's `engineVersion` follow the presented release (16 through 4.5, 17 from 4.6).
  - `GEregisterSystem` hands back the index before calling the system on open devices, because grid's `InitState` reads it then. `patternFill` defaults to `R_NilValue`, which ragg reads.
  - `fromDeviceHeight` in inches has no sign, as in GNU R; otherwise y-down devices get negative string heights.
  - Base text, rasters and X-splines go to `GEText`, `GERaster` and the engine's `xspline` (from Blanc and Schlick's paper, sampling that matches GNU R's open splines). `plot.new` marks the device dirty, because grid starts its own page unless another system drew.
- **Built-in devices record a drawing and render at close.** Callbacks append `graphics::render::Item`s in inches; PDF, SVG, PostScript, PNG, JPEG, BMP and TIFF are writers over one `Drawing` (tiny-skia, ab_glyph). Rendering at close keeps devices free of file handles and gives bitmap devices one file per page; vector devices create their file at open so a bad path fails there.
  - Patterns, clips, masks and groups are `Definition`s in an append-only arena; R receives the index, so pages rendered at close find them after R released the reference. Nesting is recorded by running the definition's function with the target swapped; rendering bounds it at 32 levels.
  - Device versions follow GNU R: pdf, svg and bitmap are 16; PostScript is 15, refuses definitions, reports capabilities `NA`, and drops semi-transparent colours with GNU R's per-page warning.
  - Deliberate improvements over cairo: bitmap devices render luminance masks; `svg()` composites like `pdf()` instead of rasterizing; `svg()` marks non-interpolated images `image-rendering:pixelated`.
  - A raster is one `Mark::Image`. PostScript resamples with GNU R's sixteenth-step bilinear filter, so its hex data is byte-identical. tiny-skia has no `saturate`, so `draw_items` composites `dst + src * min(1, (1 - Da) / Sa)`, cairo's exact formula. A padded tiling pattern in svg adds edge strips under the tile so renderers show no seams. Glyph callbacks draw nothing for an unreadable font, as GNU R's pdf device.
- **Geometry is GNU R's**, checked against its pdf output and ragg pixels; positions are computed in inches, so a coordinate's last bit can differ.
  - Point size and `cex` reach the device separately (`text_context`), because rounding to whole points is the device's business.
  - User coordinates map through the region `usr` was set on (GNU R's `win2fig`), so changing `mar` after `plot.window` moves nothing until the next `plot.window`.
  - `mtext` clips to the device; `clip()` keeps its region only while `xpd` is unchanged, as `GClip` does. A missing `adj` or `at` in `mtext` defaults by `las` and side (`ComputeAdjValue`, `ComputeAtValue`); `tck` below 0.5 is a fraction of the plot region's shorter side.
  - Contour labels port GNU R's placement, quirks included (cumulative gradient variance, `180 / 3.14`), because any other placement lands elsewhere.
  - Every text path goes through `push_label` with a `Label` (plotmath via `plotmath.rs`); `pch` 0 to 25 go through `graphics::engine::symbol` only, because a second copy drew shapes differently.
- **The editor's plot panel reads the display list.** A console with a subscribed editor makes its default device a `pdf(NULL)` with the display list on, replacing the platform default (X11 would open a window or fail). `publish_plot` in `console_server.rs` compares `graphics::engine::display_list_stamp` (one global counter, so a reused slot never repeats a stamp), replays changes onto svg with `dev.copy` and sends a `ryPlot` event. A separate httpgd-style device would duplicate the engine. File devices keep the display list off, as in GNU R, so `png()` output is never sent.
- **Plot gallery.** `crates/oracle/gallery/*.R` (about 150 figures) renders on both engines; `agg` is the sharp check (0.2% tolerance), `png` and pdf get 5% with a one-pixel neighbourhood because cairo and tiny-skia antialias differently. `svg` is not compared (cairo writes glyphs as paths). `test_gallery.rs` runs a seeded sample; a bug class it finds is pinned by a `graphics__*` case that states a value, not a pixel.
- **Device table mirrors GNU R's**: slot 0 is the null device, at most 64, `.Devices` and `.Device` rebound after every change, `dev.set(1)` starts the default device but reports 1, and a routine needing a device opens `getOption("device")`.
- **One `par`.** ry reads inline parameters from the call instead of keeping GNU R's default and current copies. Regions remember the unit they were set in (`Margins`, `OuterMargins`, `PlotRegion`, `FigureRegion`), so `par(mai = ...)` then `par(cex = 2)` keeps the inches; storing all units would drift.
- **Text metrics are Helvetica's AFM widths and kerning** (Symbol AFM for `font` 5), matching GNU R's pdf device exactly; bitmap devices in GNU R use cairo fonts. Glyphs arrive as Adobe Symbol codes but strings as plotmath's Unicode, so only the string path maps back (`symbol_metrics`, `adobe_symbol_code`); mapping both confuses code 174 with U+00AE.
- `cairoProps` reports cairo, so `capabilities()` lists png, jpeg and tiff. The palette is thread-local in `crates/graphics/src/routines.rs`, packed with red in the low byte.
- **`scan` lives in `utils.rs`**, because `scan`, `readtablehead` and `countfields` share one tokenizer; connections expose unread input through `connections::with_input`.
- **Hash tables keep entries in R vectors** behind the external pointer, so the collector sees every key and value without a Rust table to trace.
- **`contourLines`** reproduces GNU R's order and direction by observed rules in `contour_level` (`crates/graphics/src/routines.rs`); a saddle within rounding of the level can still pick the other pairing.

## parallel's fork routines

**Status:** built.

- `crates/interp/src/parallel.rs` and `fork.rs` implement parallel's `.Call` routines with GNU R's pipe protocol (a native-endian `R_xlen_t` length, then bytes; zero means exiting), because parallel's R code drives it unchanged.
- **Fork safety.** The runtime starts no helper threads, so `fork()` copies a consistent interpreter. `mc_fork` flushes stdout, stderr and file connection buffers first (`connections::flush_pending_writes`). Children close siblings' pipes, and `mc_exit` uses `_exit` so exit handlers do not run twice.
- **Reaping without SIGCHLD.** A child exits at once, and the master calls `waitpid(WNOHANG)` on released children at the start of each routine. A child is released only once its pipe ends or it is removed, detached or estranged, so its pid cannot be reused while tended. `mc_children` lists tended children only, which `mccollect`'s loop needs to end.
- **Cleanup scopes.** `mc_cleanup` acts on children forked since the innermost `mc_prepare_cleanup` mark and pops it only when it signals or detaches, because `mccollect`'s `cleanup(FALSE, FALSE)` runs inside `mclapply`'s scope.

## One Rmath, one codec stack, and the numerical routines

**Status:** built.

- **One Rmath**, `crates/numeric/src/rmath.rs`, shared by the interpreter and libR; `gamma`, `lgamma`, `beta` and `lbeta` call it instead of libm's `tgamma`, which is not exact for whole arguments.
- **One compression stack.** libR calls `interp::connections::decompress` and `interp::serialize::decompress_record`.
- **Time zones come from jiff** (`system.rs`, `Zone`). An unknown name acts as UTC under glibc's abbreviation (its leading letters when three or more). Copied from glibc: an `isdst` contradicting the rule shifts by an hour, except for `"UTC"` and `"GMT"`; years outside jiff's range move by 400-year cycles. A repeated fall-back wall time takes the offset in force at that time read as UTC, as `mktime` with `isdst = -1`; `strptime` keeps a spring-forward gap's fields.
- **The locale comes from the C library.** The session calls `setlocale` as GNU R does at startup (numbers stay C), and `sort()` reads `LC_COLLATE` (`sort::refresh_collation`).
- **`nlm`, `nlminb`, `polyroot`** follow UNCMIN (translating GNU R's `uncmin.c`, see "licensing"), PORT (see "`nlminb` runs PORT's own drivers") and Jenkins-Traub, which also fixes GNU R's root order.
- **One UNCMIN, one QUADPACK, one interval search**: `stats::nlm`, `stats::optimize::{qags, qagi}` (writing into caller-owned `Subintervals`) and `interp::arith::find_interval`; libR only converts arguments. nlm's call protocol is GNU R's: an uncached first probe that alone decides whether `gradient` and `hessian` attributes count, a ring of five evaluations, and "non-finite value supplied by 'nlm'". Bit-exact methods 2 and 3 needed departures from the textbook, found by tracing evaluations: the trust-region retry with a doubled radius only for a Newton step predicted within a tenth; central differences with an analytic gradient build the Hessian from gradient calls; the hook uses `phip0`; `phip0` and `phip` square `sqrt(dot)`; the secant update sums its two corrections before adding. `stats__uncmin_quadpack.R` pins iterates; libR's `optif9_trust_region` assumes reference-BLAS rounding.
- **`parse_Rd`** is a hand-written parser (`crates/interp/src/rd.rs`) tracking the three lexical modes per macro, with GNU R's warnings (`tools__parse_rd`).
- **`iconv` goes through the C library**, because GNU R's results are glibc's. UTF-8 to UTF-8 copies the strictly valid prefix (`platform::Iconv::convert`), which processx output made hot; glibc converts the rest, because it accepts more than strict UTF-8 and reports a cut-off sequence as `EINVAL`. `Riconv_open` resolves `""` to the `LC_CTYPE` codeset (`platform::native_codeset`) first, as GNU R passes `""` to `iconv_open` (`strings__riconv_utf8_identity`, `strings__riconv_native_encoding`).

## POSIX regular expressions run on an automaton when they need the longest match

**Status:** built.

- `Matcher::Automaton` keeps two regex-automata `meta::Regex` beside fancy-regex: a leftmost-first search gives the start, and an anchored `MatchKind::All` search the longest end, POSIX leftmost-longest in linear time (`leftmost_longest` on fancy-regex backtracked forever on lgr's `(.*z.*){10}`). Groups come from fancy-regex only for callers that report them; patterns regex-automata cannot compile, such as backreferences, stay on fancy-regex.

## `perl = TRUE` runs on PCRE2, linked statically

**Status:** built.

- Translating PCRE into fancy-regex would chase constructs one at a time and never match its backtracking or messages, so the `pcre2` crate compiles as `R_pcre2_prepare` does (UTF, `CASELESS`, JIT) in `strings::compile_perl`, with GNU R's warning-then-error on compile failure. `.cargo/config.toml` sets `PCRE2_SYS_STATIC=1`.
- The crate sets `PCRE2_MATCH_INVALID_UTF` only with `PCRE2_UCP`, which GNU R does not use, so its `Regex` validates the whole subject on every match and global `gsub` goes quadratic. `crates/interp/src/pcre.rs` calls the library directly with `PCRE2_NO_UTF_CHECK`, sound because subjects are Rust `str`.
- The TRE dialect stays on fancy-regex behind an Extended-only translator; a real TRE build is open in `backlog.md`.

## TRE approximate regular expressions

**Status:** built.

- `strings::parse_bound` ports `tre_parse_bound`, so counts, settings and errors agree with GNU R, including `{,n}` allowing `n + 1` copies and an approximate bound needing copies (`{2+}`) being "Invalid contents of {}".
- A pattern with settings and no backreference compiles to `Matcher::Approximate` (`approximate::ApproximateRegex`), a port of `tre_tnfa_run_approx`; scopes are named groups (`SCOPE_GROUP`) that become parameter changes on transitions. With a backreference TRE's backtracker ignores the settings, and so does ry.
- Observed details: more than three scopes trip TRE's `value <= 3` assertion only when a match reaches the fourth; an unset cost is free inside its scope yet counts 1 overall; ties go to the earlier start; minimal repetition is ignored; later `gsub`/`gregexpr` searches use `REG_NOTBOL`. `strings__tre_bounds` pins it.

## TRE minimal repetitions over alternatives

**Status:** built.

- Without submatches (`regexpr`, `gregexpr`, `grepl`), TRE minimizes lazy repetitions over all top-level alternatives: `regexpr("a.*?Z|a.*?Y", "aXbYcXdZ")` has length 4. Such patterns compile to `Matcher::Automaton` with `shortest`, taking the earliest end among leftmost matches.
- Submatch queries (`sub`, `regexec`) keep leftmost-first, as TRE does on the corpus's shapes; TRE's submatch results for these are irregular in general. `connections__message_sink_and_flush` pins both.

## The message sink and connection flush points

**Status:** built.

- `sink(type = "message")` sets `message_slot` (GNU R's `R_ErrorCon`); messages, warnings and `stderr()` writes go through `connections::write_message`, which flushes every write as `REvprintf` does. Closing it resets the slot to 2.
- GNU R flushes a file connection after `cat`, each message-sink write and sink removal, but not after `writeLines`; ry matches each, so a second reader sees what GNU R's would (`connections__message_sink_and_flush`).
- `Rprintf` and `REprintf` go through the same table (`libr::boundary::write_console`), so sinks divert C output.

## utils' sockets, internal downloads and allocation profiling

**Status:** built.

- `make.socket` keeps streams in one locked map in `crates/interp/src/utils.rs` keyed by the OS handle, because that is the `socket` number R code passes back.
- `download.file(method = "internal")` and `url(method = "internal")` read only `file://` and otherwise give GNU R 4.2+'s "defunct" or "not supported" errors.
- `Rprofmem` logs only vectors with more than 128 data bytes, sized as GNU R counts (data rounded to 8 plus a 48-byte header), and writes no `new page:` lines. The heap calls a plain `fn(usize) -> bool` reporter (`Heap::report_allocations`) that must not reach the heap, since the heap is mutably borrowed; returning false ends the profile. The compiler hides its own vectors with `Heap::swap_report_threshold`.
- `Rprof` raises intervals below 10 ms with GNU R's warning. `system(intern = FALSE)` and `system2` return invisibly.

## One session temporary directory, removed at exit

**Status:** built.

- `interp::system::session_temp_dir` is the only one: `mkdtemp` `RtmpXXXXXX` under the first writable `R_TMPDIR`, `TMPDIR`, `TMP` or `TEMP`, passed to native code as `R_TempDir`. `session::finish` removes it only in the creating process, so a forked `mcparallel` child never deletes its parent's directory.
- Exit finalizers run first (`run_exit_finalizers`, also on `quit()`), because a finalizer may write there; `R_CleanUp` and `Rf_endEmbeddedR` remove it through `R_CleanTempDir` after theirs.
- `tempdir(check = TRUE)` recreates the same path, unlike GNU R, because native code already copied it; it is created private, and an existing directory there must belong to the user.

## Base R kernels: radix sorting, gather subsetting, x87 accumulation and shortest-digit formatting

**Status:** built, except the radix keys computed per pass (`SortKey::get`), which "Base R kernels: position tables, a most significant digit radix order, and a per-call string memo" supersedes.

- `long_double.rs` is the one home for GNU R's long double accumulation, on x87 where available (`add_all`, `add_differences`, `add_products`, block row sums, `running_sums`). cov and cor round each column mean to double before the products, as GNU R does.
- `format.rs` uses Rust's shortest round-trip digits only for normal values needing at most 15 significant digits; past that GNU R prints exact digits the shortest form lacks.
- **Strings stored at once skip the root handle.** `values::mkchar` pushes a handle that lives until the builtin returns; in per-element loops use `values::set_mkchar` and `set_mkchar_bytes`, which store into a vector the caller keeps rooted. `Heap::mk_char` scans bytes once for NUL, ASCII and the hash (`strings::scan`) and keeps allocation in a cold function.
- **`format` picks digits with GNU R's arithmetic.** `scientific` mirrors `format.c`'s `scientific()`: scale in long double, `nearbyint`, drop trailing zeros, so `roundingwidens` agrees bit for bit. GNU R's `tbl` holds double literals, so `1e23` to `1e27` are the nearest doubles, and scales past 10^27 use glibc's `powl`, not correctly rounded for 65 exponents, so `GLIBC_POWERS_OF_TEN` stores glibc's results. `scientific = TRUE`/`FALSE` set `scipen` to -99/310 as `do_format` does (`fuzz_format_numbers_matches_gnu_r`).
- Pinned by `stats__long_double_moments`, `vectors__radix_sort_and_gather`, `strings__double_to_text`, `arith__cumulative`.

## Base R kernels: position tables, a most significant digit radix order, and a per-call string memo

**Status:** built.

- `sort.rs`'s `PositionTable` (open addressing over 1-based `u32` positions, keys left in their vector) serves `match` and `duplicated`; an integer range of at most about four slots per element uses `DenseRange`.
- `radix_order` sorts by the first key, then each run of ties by the rest, so later keys sort in cache. One key: a fused min/max/sortedness pass, counting sort under about a million buckets (past that its passes miss cache on every row), pair sort under 1024 rows, else MSD radix with 11-bit digits. Keys that fit 32 bits sort as `u32`.
- `values::StringMemo` fronts the heap's string table for one vectorized call and switches off after 65,536 lookups with under one hit in eight. It is per call because the session table misses cache on every probe at 1e7 strings while one call's distinct pieces fit; a faster session table would make it unnecessary.
- `formatC(format = "f")` uses `fixed_fast` below 2^52 scaled magnitude: an `fma` gives the exact error of `value * 10^digits`, so glibc's half-to-even is decided exactly.
- `vectors__radix_paths` pins the paths, including integer `NA` at either end.

## Base kernels pick their data structure by the value range

**Status:** built.

- `as.character()` on integers (`coerce.rs::integers_as_strings`) interns each distinct value once over a small range, which `factor()`, `table()` and `cut()` inherit. Cumulative functions allocate with `values::alloc_uninit`; `strsplit()` interns through `values::StringMemo`; `substr()` recycles by counters. Each falls back to the general path on a wide range, so no input gets slower.

## `as.character` of a long number vector is deferred

**Status:** built.

- As in GNU R, `as.character` of a plain integer or double vector of at least 256 elements returns a deferred string vector (libr's `deferred_string` ALTREP via the `defer_strings` hook) holding the source and `scipen`. Reading one element formats only it; materializing formats all in one batch and drops the source. Below 256 elements the deferred vector's allocations cost more than converting.

## `format()` of numbers streams into the result

**Status:** built.

- `do_format` writes logical, integer and double vectors straight into the result's `CHARSXP`s once the common width is known (`format_numbers` in `crates/interp/src/format.rs`), instead of a `Vec<String>` of every element. Complex numbers and doubles of differing width take the general path.

## The methods package's core runs natively behind hash-guarded prologues

**Status:** built, except the native class table with dense class ids, which the entry names as the next step.

- **Shape.** The core is Rust (`crates/interp/src/methods_core.rs`); the package's R code and metadata objects (classRepresentations, `.__C__` and `.__T__` tables, generics, method tables) stay the single source of truth, read and written directly, so saved RDS objects, `methods:::` callers, Reference Classes and the S4 cache keep working.
- **Mechanism.** `R_initMethodDispatch` calls `install_fast_paths`, which compares each function in `methods_core.R` with `FAST_PATHS` by a stable hash and rewrites the body to `{ prologue; original body }`. Patching in place reaches every copy of the closure, including the per-class `initialize` copies in `.AllMTable`, so every table entry is hashed. A miss on the bundled methods version is a start-up error; on another version the R code stays. R text the native definitions fill in lives in `METHODS_TEMPLATES`.
- **Exactness.** A fast path reads promises only when forcing runs no code (`quiet_value`), bails before any effect wherever R code could observe the work (validity functions, `as()` caching, user `initialize` methods, `.S3Class` slots, non-standard data parts, list entries in the class table), and evaluates calls that may signal in the R function's frame. A signature argument that fails to evaluate is reported as `h(simpleError(msg, call))`. Known difference: frame-local temporaries are not bound when a fast path completes.
- Generics are looked up in `.genericTable`, because the namespace binding may be an unforced promise. `R_set_slot` modifies in place, `.allowPrimitiveMethods(FALSE)` turns primitive dispatch off (selection recurses otherwise), and an argument is `"missing"` only when its own binding is missing.
- **Definitions.** `setGeneric`, `setMethod` and `setClass` are native for the usual shapes and bail before any effect otherwise; results are byte-identical to the R path (compared by `serialize()` on generated S4 packages, Matrix and the methods tests). Adding a direct subclass rebuilds the whole subclass list as `completeSubclasses()` does; appending would differ. Exception to bailing before effects: `setClass` runs `tryNew()` for slot prototypes before its last checks, which fills the same `initialize` cache the R code fills.
- **Switches.** `RY_METHODS_NATIVE=0`, `RY_TRACE_METHODS_BAILS=1`, `RY_PRINT_FAST_PATH_HASHES=1` (for adding a version).
- **The S4 cache is deleted** ("Caching is a sign of a slow design"); per-generic units plus native definitions replace it.
- **A promise stays a root while forced** (`force_promise` in `eval.rs`), as GNU R's `R_PendingPromises`, because a loader S4 placeholder removes its own binding while it runs.
- **`setMethod` on a base S3 generic** builds the implicit generic natively (`implicit_generic`), writing the same objects as `setGeneric(f, where)`, and bails when `.BasicFunsList`, `.__IG__table` or `.__IGM__table` would supply it, when a table already holds the name, or outside a namespace.
- **Next: a native class table, not more ported functions.** The remaining cold cost is flat (environment lookups of `.__C__`/`.__T__`, slot reads, `substitute`-built extensions, allocation), and every ported function still reads and writes the R metadata. The target is dense class ids, a bitset extends relation and id-keyed method tables, materializing `classRepresentation` and `.MTable` views only when R code reads them. Per-generic units came first because not running unused definitions removed more for less.
- **Unions and `cacheMetaData` replicate GNU R's quirks**, because state must `serialize()` identically: `.recacheSubclasses` matches the subclass's own name (always `NA`), so every union-member subclass's extension is overwritten, re-sorted and recached; `cacheMetaData` drops duplicate generic names but not their packages; classes are visited in frame order and generics sorted; `completeExtends` takes `.resolveSuperclasses`' first consistent candidate (`resolved_superclasses`). `.S3Extends` is not ported, so such members bail.
- **The first union fills `initialize`'s dispatch cache itself** (`union_initialize`), as the R code's `new("ClassUnionRepresentation", ...)` would, unless first-unit definitions are pending (they may add a preferred `initialize`). Only the default `initialize` is accepted (`is_default_initialize`, `definition_caches`), since GNU R's `new()` of a definition calls a package's method.
- **`setClass` does `setIs`'s bookkeeping by GNU R's quirks**: only direct superclasses go through `setIs`; ancestors record the class only when `possibleExtends` is false; with a union ancestor, ancestors are recorded first (other unions recache their subclasses' `contains`, dropping `conflicts`), then direct superclasses, the first on the earlier-read definition and later ones on a fresh read (`record_subclasses`, `recache_subclasses`, `recachable`). Cache and namespace binding may differ after `setClassUnion`, so `local_class` takes the cached definition. A slot default whose class has a methods-package `initialize` runs `new()`.
- **`setMethod` builds `.local` natively** (`conform_definition`) with `rematchDefinition`'s result: generic formals, `.local(args)` with args after `...` and from the first `missing` signature position passed by name, and method defaults kept unless a symbol or missing. A method omitting only `...` gets the generic's formals in place. An omitted argument bails only when the signature names it; a different order, or added formals without `...` in the generic, bail.
- `setGeneric` accepts a braced `standardGeneric` body as a `nonstandardGenericFunction`, as `.NonstandardGenericTest` does.
- **Every fallback says why**: `bail!(skip, "reason")` and `report_fallback` under `RY_LOAD_VERBOSE` (`reference/package-loading.md`).
- **`cacheMetaData`** skips classes whose bound definition is the cached one, merges a package's own standard generics natively, and runs GNU R's loop body per remaining item (`Template::CacheClassItem`, `CacheGenericItem`), so effect order matches.
- **A foreign generic with no methods in the namespace is reset natively** (`cache_foreign_generic`), because `setOldClass()` with a prototype makes nearly every package cache `Ops`. It walks the group tree as `.updateMethodsInTable` does and bails before effects on a group slot, a global binding, a namespace method table, or a non-primitive default. Primitive group members take their generic from the generic table, as `getGeneric()` does; lazy-loaded promises are forced by `forced_value` (`s4__cache_metadata_shapes.R`).
- `as()`, `setDataPart()` and inherited dispatch remain (`backlog.md`, "Open: native methods core"). `s4__native_core_queries.R` and `s4__native_definitions.R` pin the covered functions.

## Numeric routines are exact ports of GNU R 4.6.2, and LINPACK lives in the interpreter crate

**Status:** built.

- nmath, smooth.spline, lowess, loess and LINPACK are ported from the 4.6.2 source with its evaluation order, including `dnrm2` and `drotg` from LAPACK 3.10 and R's modified `dsvdc` test. Textbook formulas differed from GNU R in the last bits.
- `linpack.rs` lives in `crates/interp` and `libr` re-exports it, so the interpreter's stats code and the C API share one copy.

## Hot base R closures get hash-guarded native prologues

**Status:** built.

- **Mechanism.** `crates/interp/src/base_fast_paths.rs` puts `.Internal(ryX())` in front of a base closure's body when base loads, as the methods core does: a stable hash of formals and body guards the install (a miss is an error on the bundled R version and a silent skip on any other), the closure is patched in place, and the start-up image carries the patched closures. A prologue either returns from the closure's frame or returns NULL and lets the body run.
- **Patched closures and their reach.** `match.arg` (a single ASCII string), `setdiff` (plain atomic vectors; R 4.6.2's body spends 17 of its 24 µs in two `tryCatch` calls), `format.default` (no marks or `justify`), `structure` (plain attributes, a non-factor character class), `stopifnot` (all-TRUE logicals), `readRDS`, `Reduce` (`accumulate = FALSE` over a plain vector), `deparse`/`.deparseOpts`, `formatC` (plain integer or double, `mode` NULL, formats d f e E g G fg), `pmax` (plain atomics, first without attributes, no fractional recycling), `tryCatch` (one handler), `withRestarts` (`function` literals or strings), and `invokeRestart`/`tryInvokeRestart` (a name only). Small closures with call arguments (`ifelse`, `isTRUE`, `isFALSE`, `unname`, `%in%`, `unlist`) have their own entry, "Small base closures with call arguments run their bodies natively behind prologues".
- **Invariants.** A prologue forces only what the body forces, in the body's order, so a declined call sees the same promises. It covers the common case only and declines everything else, so errors, warnings and messages always come from the R body. A defaulted argument is checked by the cell holding the bound default (`gp::BINDING_MISSING`), not `missing()`: a wrapper passing on its own missing formal makes `missing()` TRUE, but forcing it in the body errors.
- **Nested closures** (`NESTED_FAST_PATHS`). `assignNativeRoutines` is defined inside `loadNamespace`'s body, so the prologue goes into that `function` expression, found by its `name <- function` assignment; `ryAssignNativeRoutines` binds all routines at once instead of growing vectors with `c()`. Hashes of nested functions are taken with nested prologues stripped (`without_nested_prologues`), because the image saves patched bodies.
- **`readRDS(path)`** reads the file whole, inflates gzip in one pass and unserializes the slice; the R body's `gzfile()` connection costs three times as much. It declines for `~` paths, xz, bzip2, headerless streams and any unserialize error. An uncompressed form would need a second file per database or a non-GNU `saveRDS`, so it is not done; `zlib-rs` inflated no faster than `miniz_oxide`.
- **`match.arg` with `choices` missing** evaluates the caller's default only when it is a character constant or `c()` of them, and binds `choices` as the body does, so a declined call does not evaluate it twice.
- **`Reduce`** (`ryReduce`) binds `mis`, `len`, `f`, `init` and `i` in its frame and calls `f` through `eval::call_with_values` with forced promises of the body's own expressions, so `sys.call()` and `substitute()` inside `f` match.
- **`deparse` and `.deparseOpts`** fold `control` into option bits natively; options must be spelled in full, and abbreviations, duplicates, `NA`, `"all"` with `"exact"` or `"hexNumeric"` with `"digits17"` decline (`deparse__fast_path.R`).
- **`formatC` and `pmax`.** The prologue does the body's NA/NaN/Inf substitution and default-digits arithmetic and calls `format::format_c_vector`, the same body as the `formatC` internal, with Rust values. An ASCII `big.mark` with default marks is inserted natively, matching `prettyNum()`'s reversed `([0-9]{k})\B` substitution. While `decimal.mark` holds its unforced default, it reads `OutDec` through `envir::option_value` (`getOption()` has no side effect). The `'` and `I` flags are not pinned because GNU R's output depends on the C locale (`base__format_c_fast_path.R`, `base__format_c_out_dec.R`).
- **`tryCatch`, `withRestarts`, restarts.** The prologues keep every frame the R bodies make (`tryCatchList`, `tryCatchOne`, `doTryCatch`, `withOneRestart`, `doWithOneRestart`); only the bookkeeping between them is native, because a frame GNU R shows cannot be skipped (`sys.calls()`, `conditionCall()`, handler frame variables). Inner closures are made through `bytecode::inner_closure`, which reuses the compiled body stored in the outer function's constants; otherwise each new closure recompiled. `tryCatchList` covers one handler. `.signalSimpleWarning` and `.handleSimpleError` are called from the base environment, as `errors.c` does (`conditions__try_catch_fast_path.R`).
- **Known difference.** The prologue is visible: `body(match.arg)[[2]]` is `.Internal(ryMatchArg())`, so printing, `deparse()` or digesting a patched function differs from GNU R.
- **Known gap.** The `format` internal checks its arguments in `do_format`'s order but does not yet warn "NAs introduced by coercion" for a string `digits` or `width`.
- **Measured.** `validation` went from 0.58 to 0.65 times GNU R to 1.87x; `formatC(42L, width = 6L, flag = "0")` 13.2 to 1.4 µs (GNU R 14), with `big.mark` 76 to 2 µs (GNU R 106); `deparse` 9.1 to 1.4 µs (GNU R 12.8); `tryCatch(1, error = h)` 3.7 µs (GNU R 4.1; 19 µs without `inner_closure`). `match`, `%in%`, `intersect`, `ls` and `pmatch` were each under 1% of the testthat profile and left alone.
- Next candidates: `utils::modifyList` (needs an install hook when utils loads) and `tryCatchList` with several handlers.

## Builtin fast paths take names as CHARSXPs and evaluate nothing twice

**Status:** built, except the `c()` flattener's `LeafName`, which "Builtins with positional direct entries, one implementation per semantic" supersedes.

- Names stay CHARSXPs end to end (`do_attributes` collects pairs on the stack), and every gain is work not done (no interning, no pairlist, no second evaluation), so nothing needs invalidating; no per-builtin cache.
- `forceAndCall` reuses the call's cells after `n` and `FUN` when unchanged (`call_tail`), and passes variables eagerly to an effect-free closure under the same conditions as a compiled call site.
- The apply family's known element reaches a builtin `FUN` directly, and a special through `dispatch_evaluated_subject`, so `[[` methods still dispatch.
- `cached_item` (`records.rs`) reads a classless, non-ALTREP list by position or exact name; out of range falls back to `[[` for its error. The in-place element assignment rejects `""` and `NA`, which match no name.
- Fixed names come from `symbols()` (`type_names` for `typeof`, generics for internal dispatch). `options(c("a", "b"))` reads only `a`, and `options(character())` is an error, as in GNU R (`strings__interned_names.R`).
- **`lapply` and `vapply` share `ElementCalls`** (`envir.rs`), which writes `i` in place but rebinds a fresh `i` whenever its reference count moved (`bound_refcount`), because `FUN` may have captured it. `interp::testing::check_element_access_agreement` fuzzes the direct entries against the AST evaluator.
- Open: the closure call inside `vapply`/`lapply` allocates `i` and a promise per element; `$<-` and `[[<-` with a new name take `complex_assign`.

## One-argument builtins and small base closures skip dispatch when their arguments are values

**Status:** built.

- `BuiltinDef::unary` (`builtins.rs`) is called by `op_call_builtin` for one positional, non-missing argument. It returns `None` for anything not plain (object, ALTREP, attributes, a ry native value), so dispatch and error messages stay on the general path. Results are the shared immutable `TRUE`, `FALSE` and `NA` from `values::scalar_logical`, so modifying one copies it, as in GNU R.
- Native entries (`NATIVE_ENTRIES` in `base_fast_paths.rs`) are keyed by closure identity in `Interp::native_entries` and filtered by `gp::NATIVE_ENTRY`; both are rebuilt on an image start because the registry is not in the image. An entry declines when the closure is traced, debugged or stepped, an argument is named, or an argument is an unforced promise of a call; a promise of a variable or constant is settled with the value it would have produced, so later `substitute` or `missing` agree.
- **Why a field and not new tier-0 ops.** The JIT matches exhaustively on tier-0 ops; a field keeps compiled code unchanged.
- `crates/oracle/cases/builtins__native_entries.R` pins the edges. `missing` is a special and costs about twice GNU R.

## Builtins with positional direct entries, one implementation per semantic

**Status:** built, except the fused element op and `element_access::read_element` under "What is left", which "Tier-0 element reads and stores as fast handlers, not new ops" supersedes.

- **`BuiltinDef::direct`** (`DirectFn = unsafe fn(call: Sexp, &[Sexp]) -> R<Option<Sexp>>`) takes the call (for errors and native context) and the values in formal order. `op_call_builtin` tries it when no argument is tagged or empty, and `replace_variable` when the only tag is the trailing `value`. It declines everything the full call treats differently. The caller sets visibility and records feedback, and skips the entry while a profile or activity trace runs so samples name the builtin.
- **One implementation per semantic.** The full builtin calls the same function after matching arguments. `attribute_access.rs` holds attribute reads and writes by `CHARSXP` (GNU R's `do_attr` order, including its special case for `names` and expanded compact row names on a partial match), the implicit class, and `inherits`. `combine.rs` holds `c()` and `unlist()` as GNU R's two passes (size, then write), so no per-leaf vector or `String` is built. `element_access.rs` holds `[[`.
- `same_text`: interned ASCII strings are equal exactly when they are the same object, so bytes are compared only when either side is not ASCII.
- **`x[i] <- v` in place** (`assign_subset_in_place`) writes an unshared, attribute-free atomic vector with GNU R's recycling and declines NA or zero positions, growth, widening and partial recycles, so the full `[<-` keeps one implementation of those rules. Plain lists accept a `list(...)` value; an atomic value for a list target declines.
- **`.Call` by address** (`crates/libr/src/dot_call.rs`): an untagged, non-null `address` is a native entry point, because builtin-library routines carry a marker tag and base routines a null address. `PACKAGE` or an arity mismatch declines to the general path that owns the errors.
- **`apply_results::VapplyResults`** owns GNU R's `vapply` checks and error text; every loop that calls `FUN` hands results to it (`builtins__vapply_results.R`).
- **Element assignments stay in slot frames.** `ForStep`'s variable gets a slot even if never read. The `SLOTS` constant's fourth element maps constant index to slot (`slot_at`); `index_slots` derives it from code, names and constants in the compiler and the image reader, so images store only names and the map cannot go stale. It covers only the variables `ForStep` and `SubAssign` name, because a map over every constant raised peak RSS by 2 MB. Parentheses over a side-effect-free subscript stay side-effect free.
- **No per-call allocations.** `subset::resolve` returns a `SmallVec`; negative subscripts build a `SmallVec<[u64; 4]>` bitset; `match_builtin_args` uses a bitset and borrowed names; primitive formals are a `Vec` indexed by `builtin_index` (`subset__negative_index_bitset.R`).
- **What is left.** At tier 0, `x[[i]]` and `x[[i]] <- v` cost about twice GNU R, all of it dispatch around the builtin. Oracle cases `builtins__attribute_access.R`, `builtins__combine.R`, `builtins__element_access.R`, `builtins__assign_subset_vector.R` and `builtins__dot_call_native.R` pin the edges.

## Builtin call machinery and subassignment toward 10x

**Status:** built.

- `direct_subset` reads an in-range scalar position of an attribute-free atomic vector and otherwise calls the same `subset()` as the full builtin. `.Call`'s direct entry calls `call_native` inside `foreign_context` with no argument list.
- `-x` on an attribute-free scalar goes through `BuiltinDef::unary`, because the compiler does not fold `-1L`.
- **Appending a name** (`append_named` in `subset.rs`) grows the vector in place when it is unshared, not held by a register and not bound through a promise, and copies with room to grow otherwise, so growth is amortized like GNU R's growable vectors.
- `scatter_slab` copies contiguous runs when the first dimension is taken whole. `round` on doubles runs `fround_slice` and copies attributes only when present.
- `Reduce(accumulate = TRUE)` runs natively; `out[[i]] <- init` with NULL `init` deletes the slot, so it evaluates the assignment then. The final `unlist` is evaluated in R when an element is an object.
- **Why no new tier-0 ops.** The `direct` and `unary` fields reach the same code from tier 0 and the JIT fallbacks without changing the op set.
- **Measuring RSS.** A cold binary inflates peak RSS through page-cache fault-around on its text, so compare builds only after reading both files once.
- Left: `structure()`, `format.default` and `Reduce`'s callee calls are dominated by closure frame setup. Oracle cases `builtins__round_vector_fast_path.R`, `assignment__append_name_in_place.R`, `subsetting__matrix_slab_assign.R`, `builtins__dot_call_direct.R`, `subsetting__direct_single_bracket.R` and `apply__reduce_accumulate.R`.

## One kernel layer for R's vector builtins: slice kernels, a reuse rule, and a fused superinstruction

**Status:** built, except the helper `first_nan`: the kernels pick the NaN payload inline.

- **Slice kernels** (`crates/interp/src/kernels.rs`) take `Reals` or `Integers` (slice or scalar) and write into `&mut [T]`. They touch no R object, allocate nothing and raise nothing; integer overflow is a returned flag. `kernels::binary` serves plain operands only and returns `None` otherwise, so dispatch and messages stay on the builtin. Scalar operands are answered without the element loop, because most package calls are on scalars.
- **No AVX2 build.** A second AVX2 build of every kernel cost 0.6 MB of resident code for no gain on memory-bound loops; a narrow AVX2 `real_operation` was 15 to 25% slower end to end and faulted in 64 KB text windows on every workload. Only `vector_math`'s exp and log run AVX2.
- **NaN payloads.** SSE returns the first operand's NaN payload and LLVM may commute `+` and `*`. GNU R's loops: for two doubles, `x + y` and `x * y` take `y`'s payload when a scalar `x` meets a longer `y` and when both recycle, `x`'s otherwise; with an integer operand always `x`'s; `-`, `/`, `%/%` take `x`'s. `kernels::payload_from_right` encodes this by swapping operands. Beside a non-NaN scalar, `+` and `*` use the plain operator (only the vector element can be NaN). Probed in `kernels__elementwise.R`.
- **Reuse rule.** A result goes into an operand's buffer only when the compiler marked its register as a temporary only this instruction reads (operand 6 of `Op::Arithmetic`) and at run time the count is zero, it is mutable, not ALTREP, and has the result's type and length. The count alone is not enough: registers and the JIT's unwritten slots hold uncounted references. This is GNU R's `R_allocOrReuseVector` restricted to what the compiler proves.
- **Aliased destinations.** `kernels::binary`/`unary` take an optional destination that may be an operand (`x <- x * y`); the slice kernels take operands as `Option`, where `None` means the output buffer, so no `unsafe` aliasing. `kernels::fits` requires length above one, because the shared scalar constants can be held by a single binding with a count of one. `Call.reusable` offers `x` for `x <- f(x, ...)` when the binding holds the only reference and `f` is marked `reusing_first_argument`; an `Ops` member dispatches first so a method sees untouched operands.
- **AST temporaries** (`eval::temporary_argument`): a `reusing_first_argument` builtin called from AST code reuses an argument that is a two-argument call to a base operator builtin with count 0. The operator must be the base builtin (a closure bound to `*` may return a held value), and unary `+` is excluded because it returns its operand. Reference-counted registers were rejected: a count update on every register write, unneeded while every reuse source is a known fresh temporary.
- **Reductions.** `sum` on integers, `any`/`all`, `min`/`max`/`range` are SIMD kernels; `real_extremes` lets the first zero decide a zero bound's sign, because GNU R keeps the first of equal values. `prod` and double `sum` accumulate in x87 long double (`long_double.rs`), which no SIMD lane reproduces.
- **Fusion.** `Op::Fused dst program target` precedes the ordinary code of an element-wise tree (layout in `bytecode::fusion`). The compiler emits the ordinary code unchanged and matches the plan against it, emitting `Op::Unfused` when they differ. At run time it reads leaves in the ordinary code's order, checks operator guards, and declines (falls through) on scalars only, vectors shorter than `kernels::SHORTEST_FUSED`, active or missing bindings, attributes, objects, ALTREP, non-scalar recycling, integer overflow, or a produced NaN; after 16 declines in a row it rewrites itself to `Op::Unfused` and drops its program. Otherwise it runs 512-element chunks in per-thread scratch and records the replaced instructions' type feedback.
- **Why a declining superinstruction.** Nothing observable happens before it decides, so fused and unfused evaluation are indistinguishable, and the JIT keeps compiling the ordinary code (baseline lowers `Op::Fused` to `runtime::jit_fused`).
- **Call leaves.** A call such as `mean(x)` in `sum(x - mean(x) > 0)` is evaluated ahead by `Compiler::fused_call`: guards for every operator head before the last call leaf come first, because the guards' cold fallback evaluates the whole AST and would run the call twice; R looks operators up before evaluating arguments anyway. Variables read before a call are hoisted with it (`Compiler::hoisted`) so a call that rebinds them leaves earlier reads with the old value. A call that `may_modify_in_place` is never a leaf, because registers do not count as references.
- **Fused assignment** (`fusion::ASSIGNED`) writes `y <- tree` into `y`'s old buffer when `bytecode::reusable_target` allows (not active or locked, count one, `eval::reuse_permitted`); when `y` is also a leaf, each chunk's root is copied over only after every node read it. Integer passes never reuse, because an overflow declines halfway. Do not verify reuse with `.Internal(inspect(y))`: the call references `y` and defeats it (`kernels__vector_math.R`).
- **Rejected: lazy fused vectors as ALTREP** (Riposte, pqR): every reader, C code through `DATAPTR` included, would materialize them, and when a deferred warning surfaces would become observable.
- **SIMD `exp` and `log`** (`crates/numeric/src/vector_math.rs`) are four-lane ports of glibc's `__exp_fma` and `__log_fma`, which GNU R calls through ifunc on AVX2+FMA, so the bits match; any other SIMD polynomial would not. The first call probes against the host glibc and disables the kernels on a mismatch. Lookups use 128-bit loads, not `vgatherqpd`, which is slow under the Gather Data Sampling mitigation.
- **`R_POW`'s bits.** GNU R computes `x^2` as `x * x` and `R_pow` returns `x + y` for NaN, so both quiet a signalling NaN (`NA^2` is `7ff8...07a2`). Division by a normal power of two multiplies by the exact reciprocal (`kernels::exact_reciprocal`).
- **Complex arithmetic.** `complex_binary` serves complex beside plain double (or, from R 4.4.0, logical or integer); `arith::complex_arithmetic` calls the same `kernels::complex_arithmetic`. `z^2` computes `R_cpow_n`'s `1 * (z * z)`.
- **Complex bits.** GNU R uses GCC's libgcc `__muldc3`/`__divdc3` and glibc `cpow`; Rust links compiler-rt's, whose recovery differs, so `arith::complex_multiply`, `infinite_product` and `complex_divide` port libgcc's, and `cpow` is called through FFI. Which NaN wins decides `NA` against `NaN`, so steps use first-operand-wins `arith::add`/`subtract`/`multiply` in GCC's operand order (read from `libgcc_s.so.1` disassembly). The ordered formula runs only in out-of-line `nan_product`, after deciding the recovery, because vectorized mandelbrot hits the recovery on about 40% of products.
- **Complex coercion.** An integer or logical NA becomes `NA + NAi` before R 4.4.0 and `NA + 0i` from it (`r_at_least(4, 4)`). `abs`/`Mod` keep `dim` and names; other `Math` functions and `clog` map NA in either part to `NA_complex_` and warn as `cmath1` does; `atan` reproduces `z_atan`'s full complex product.

## Package code that dominates a workload runs natively: rlang's trace_back

**Status:** built.

- **Decision.** Package functions get hash-guarded prologues like base's (`package_fast_paths.rs`). `registerS3methods()` runs for every namespace load (`library()`, `requireNamespace()`, `pkg::`, `ry load`, image or lazy-load) after the code is in place and before anything can call it, so its first statement `.Internal(ryPackageFastPaths(package, env))` installs them without changing `loadNamespace()`. A hash miss silently keeps the R body.
- **Policy for package-specific native code.**
  - *Generic first.* Measure the R body on ry against GNU R in the same sitting and fix every slower primitive; native code covers only what remains.
  - *Eligibility.* The package is in the AGENTS.md benchmark set, or a real workload profile puts the function above about 2%.
  - *An exact guard per release.* `FAST_PATHS` names the release and hash; `crates/ry/tests/scenarios/native-package-functions.scenario` fails when the installed release differs, so an update is not a silent slowdown.
  - *Fallback.* `decline()` whenever the R body could observe something the port does not model.
  - *Evidence.* A seeded differential harness against the R body (`fuzz_native_trace_back_matches_its_r_body` in `crates/oracle/tests/test_differential.rs`), oracle cases against GNU R, and a news-page line.
- **Why trace_back.** It is about 80% of `expect_error()` on GNU R too; no primitive under it was slower than GNU R, so the gain had to be native. `ryTraceBack` is a pure function of `interp().frames` and reproduces rlang 1.3.0's columns (`call_zap_inline`, `call_trace_context`, trimming, attribute order). It declines on `rlang:::visible_bottom`, `rlang:::error_frame`, `knitr.in.progress` without `top`, an unforced promise in call position, an object as function, or a malformed `function` call (`conditions__rlang_trace_back.R`, `conditions__rlang_trace_back_generated.R`).
- **Interned strings carry a validity fact.** `flag_bits::VALID_UTF8` is set in `Heap::intern_new`, and `char_str` borrows such a string unchecked unless it is marked Latin-1 or bytes. The header flag byte is unreachable from `SETLEVELS` and interned bytes never change, so unlike the gp `ASCII` bit it is a fact.
- **Names stay symbols.** `::`, `getNamespaceValue`, `options()` and the fast paths take symbols and `CHARSXP`s; fixed names come from `Symbols`, generated by one macro so a name is listed once (`envir__namespace_lookup.R`).

## withr's environment and option helpers run natively; defer() stays in R

**Status:** built.

- **Decision.** withr 3.0.3's `as_envvars`, `get_envvar`, `set_envvar`, `get_options`, `set_options`, `reset_options`, `list_combine` and `reset_gettext_cache` get prologues under the package policy above. `defer()` and the `local_*()`/`with_*()` functions keep their R bodies, so restores register through the ordinary exit-handler path.
- **Generic first.** The bodies' primitives got base fast paths first (`ryIntersect`, the `names` branch of `ryStructure`, `rySysGetenv`, `rySysSetenv`, the `Sys.unsetenv` native entry), which help every caller.
- **Exactness.** Helpers decline whenever the body could raise a condition: `set_envvar` with `action` other than `"replace"` or a non-string, non-NA value; `as_envvars` on missing, empty or `NA` names or extra attributes; `set_options`/`reset_options` unless every option sets quietly (`envir::option_sets_quietly`), because an option error's call is the R body's `do.call()`-built call.
- **reset_gettext_cache.** ry's `bindtextdomain()` keeps no cache, so the native helper calls the `tempfile()` and `bindtextdomain()` kernels (keeping the `tempfile()` sequence) but skips creating and removing the directory. That is unobservable: names mix the clock with a counter and the directory never outlives the call. It declines when the session temp dir is missing.
- **GNU R behavior.** `Sys.setenv`/`Sys.unsetenv` return `FALSE` for an empty name or one containing `=`, and write `NA` as `"NA"`. `Heap::install_char(NA_STRING)` installs the symbol `NA`, as `installTrChar` does. A list with an `NA` name prints `$<NA>`.
- **Evidence.** `fuzz_native_helpers_match_their_r_bodies` (`test_differential.rs`), oracle cases `packages__withr_env_and_options.R`, `base__intersect_native.R`, `base__structure_names_native.R`, `system__sys_env_native.R`, and `crates/ry/tests/scenarios/native-withr-helpers.scenario`.
- **JIT and RSS.** With these helpers, `isTRUE`'s body becomes the one function the JIT compiles in the testthat workload, and the code generator's first use costs about 4 MB; making that cheap is JIT work, not a reason to keep the helpers in R.

## Environment variable calls read `environ` once

**Status:** built.

- `scan_environment` (`crates/interp/src/system.rs`) walks `environ` once for all names of a call. A one-name call compares each entry's first then second byte, exactly glibc's loop (building a two-byte key per entry cost twice glibc). Several names use a 256-bit first-byte set, keys in a plain slice zeroed when found, and stop once all are found; a `SmallVec` deref or shrinking list in the loop costs more than glibc's whole lookup.
- `Sys.setenv` skips a name whose value is unchanged (unless repeated earlier in the call), and `Sys.unsetenv` skips an absent name, because withr restores every variable on exit. Writes go through libc, so C code sees every change.
- **Rejected.** A private copy or index of the environment would miss C code calling `setenv()`, and it would be a cache.
- **Evidence.** `fuzz_environment_variables_agree_with_the_c_library` (`crates/interp/tests/test_fuzz.rs`) and `system__sys_env_round_trips.R`.
- **Measuring RSS.** Stage both builds' files at the same time and warm them: a freshly written code image sits whole in the page cache, so fault-around maps more of it and shows a false gap of several MB.

## Conditions: message(), warning(), signalCondition() and getOption() run natively when nothing could observe the difference

**Status:** built.

- **message()** (`ryMessage`) writes directly when no handler for `simpleMessage`, `message` or `condition` is established (`conditions::handled`, which allocates nothing), `domain` is default, every `...` argument is a plain atomic vector, and no `conditionMessage` method exists; the R body would then signal to nobody and write `conditionMessage()` to `stderr()`. Latin-1 or bytes text declines.
- **signalCondition()** (`rySignalCondition`) binds `msg` and `call` in its frame first, because handlers read them through `sys.frames()`. It reads `message` and `call` as `$` does; a name only a longer field starts with declines, because `$` would partially match. It declines when a `conditionMessage`, `conditionCall` or `$` method exists for a class before `"condition"`. Handler lookup re-reads the class per handler, because a calling handler may change it.
- **warning()** (`ryWarning`) builds the message for `NULL`, logical, integer, double and character arguments without attributes (where `.makeMessage()` runs nothing observable and ry's `gettext()` is the identity) and signals through `conditions::signal_warning` from warning's own frame. Condition objects, classed arguments, a non-default `domain`, a non-logical `call.` or `immediate.`, and Latin-1 or bytes text decline. `noBreaks.` is ignored, as ry's `.Internal(warning())` ignores it.
- **getOption()** compares the name's CHARSXP by pointer while walking `.Options` (`envir::option_value`). A symbol-keyed table would be a second copy of `.Options`, which R and C code (`GetOption`) read and write directly; the fix is a redesign of `.Options` (`backlog.md`, "Options as a symbol-indexed array").
- **Switch.** `RY_FAST_PATHS=0` disables these paths and the warning-call shortcut, so a case can be diffed against ry's own R bodies (`development/corpus-and-oracle.md`).
- **Evidence.** `conditions__message_and_restart_frames.R`, `conditions__signal_condition_dispatch.R`, `conditions__warning_native.R`.

## Builtin and assignment hotspots of the public suites

**Status:** built.

- **`<<-` to a symbol is `Op::SuperAssign`**: it assigns in the first enclosing environment that binds the name, creating it in the global environment, erroring on a locked binding.
- **Defensive copy narrowed.** The copy in complex assignment guards an index expression that rebinds the target (`x[{x <- 0; 1}] <- v`), so it is taken only when an index syntactically assigns to the target or a part of it (`eval::assigns_to`); an index that assigns another variable still compiles to `Replace`.
- **Array slabs in place.** `x[i, j, ] <- v` on an unshared numeric or logical array writes through an odometer (`subset.rs`). `[<-` is a special, so `replace_variable` accepts a special with a direct entry for a non-object target and allows empty subscripts only for it.
- **Matrix `[[i, j]]`** (`direct_subset2`, `scalar_position`) reads two scalar in-range subscripts and declines the rest. A negative matrix `[[` subscript is an error in GNU R 4.3.3, while 4.6.2 applies the extent-two rule per extent (PR#19116, `m[[-1, 1]]` of a two-row matrix reads row 2); `get1index` follows the emulated release (`r_at_least_release(4, 6, 2)`).
- **`dim<-` and `array()`** validate as 4.6.2's `dim2total`: empty `dim`, `NA`, negative extents, then a double product beyond the longest vector, and `dim<-` prints the mismatch with both lengths (`dims_total`). Older emulated releases keep their own messages, and their `array()` no checks.
- **`matprod` lives in interpreter state**, as GNU R's `R_Matprod`: `Interp::product_options` (`linalg::ProductOptions`) is written only by `set_option` and reset by `clear_options`. The option list stays the source of truth; the state is its parsed form, written where the option changes.
- `attr<-` installs the name once (`install_charsxp`) and compares by identity. Complex arithmetic: see "Complex results match GNU R bit for bit, NaN payloads included".
- **Evidence.** `assignment__superassign_compiled.R`, `assignment__array_subassign_compiled.R`, `builtins__grow_copies.R`, `builtins__matrix_element.R`, `assignment__assigning_index_compiled.R`, `arith__complex_vectors.R`, `attributes__many_attributes.R`.

## One-pass attribute lookup and the blocked matrix product

**Status:** built.

- `attribute_access::attribute` finds a full and a partial match in one walk; only a tag strictly longer than the wanted name can partially match, so vectors used as maps (shootout's knucleotide stores thousands of attributes) cost one pointer compare per tag.
- `%*%` computes `dgemm("N", "N")` itself in the reference BLAS order, with four result columns sharing each pass over `x`. Do not reorder the sum (blocking over `inner`, pairwise or FMA kernels): results would differ from GNU R in the last bits.
- **Evidence.** `attributes__partial_lookup.R`, `numerics__matrix_product_blocks.R`.

## Distribution functions and the RNG run as vector kernels and batched samplers

**Status:** built.

- **Kernels.** `distributions!` (`crates/interp/src/distributions.rs`) generates one `Kernel` type per row, so `math::<K>` monomorphizes with the scalar body inlined. NaN operands go to a cold path that keeps GNU R's NA-over-NaN rule. `qnorm` overrides `fill_first` (`qnorm_central_fill`, `qnorm_tails_fill` in `rmath.rs`).
- **Samplers.** `random { ... }` rows generate a `Sampler` with `prepare` once per parameter change and `draw`. `Poisson` and `Binomial` hold GNU R's static caches as fields, so the sequence is the one GNU R's caches give.
- **Batched uniforms.** `with_deviates!` splits the RNG into `Draws<Twister>` or `Draws<&mut State>` once per call, so state stays in locals and the kind is matched once (`twist_words_avx2`, `temper_block_avx2`, `inversion_fill_avx2` in `random.rs`).
- **Bit-exactness.** Every SIMD lane performs the scalar operations in the same order, and Rust never contracts `a * b + c` into an FMA, so AVX multiversioning cannot change a result. Unsigned-to-double goes through `(word ^ 2^31) as i32 as f64 + 2^31`, which is exact. `log` and `exp` vectorize only through `vector_math.rs` (`pnorm_standard_fill`, `qnorm_tails_fill`), with scalar libm loops where those kernels are off. The inversion combine runs on lanes only for generators whose uniforms lie in (0, 1), because vector truncation and Rust's saturating cast disagree on overflow, which a user-supplied generator can reach.
- **`sample` without replacement** (`do_sample2`) keeps a bitset over the population when `population / 128 <= size` (then at most twice the hash set's memory) and an `FxHashSet` otherwise; draw order and redraws are unchanged (`random__integer_index_draws.R`).
- **`.Random.seed` in place.** `put_rng_state` overwrites the vector only when it is unshared, plain, of the right length, in an unlocked, unwatched, ordinary binding, so a copy the user saved keeps its values (`random__batched_streams.R`).
- **Evidence.** Seeded tests in `random.rs` and `distributions.rs`; oracle cases `distributions__vector_paths.R`, `random__batched_streams.R`, `random__prepared_samplers.R`.

## unlink() follows GNU R's R_unlink, with directory descriptors

**Status:** built.

- `crates/interp/src/file_removal.rs` is GNU R's `R_unlink` on directory descriptors (`openat`, `readdir` with `d_type`, `unlinkat`), so only `force` or an unknown entry type costs a `stat`, children are removed before parents, and nothing is allocated per entry.
- **GNU R's quirks kept.** `lstat`, so a link to a directory is removed as a link; `force` uses `chmod`, which follows links; the directory test is `st_mode & S_IFDIR`, so sockets and block devices are walked and fail; `NA` counts as a failure, `"~"` is skipped, `expand` globs with `GLOB_NOCHECK`; a path of 4096 bytes or more is "path too long".
- **Rejected: `rmdir` on a background thread.** ext4's `rmdir` waits for the journal, but most corpus `unlink()` calls remove single files, so the corpus did not move, and the thread cost RSS plus a join before every fork and at exit.
- **Coverage.** `interp` `test_file_removal` compares against a port of `R_unlink`; oracle cases `io__unlink`, `io__unlink_fork`.

## Parse data as dense columns

**Status:** built.

- `parse(keep.source = TRUE)` builds parse data by replaying bison's reductions over ry's syntax tree (`crates/interp/src/parse_data.rs`). An item records only `reduced_by`; one backwards pass derives parents. Ids are dense, so placing rows at their id (`row_of_id`) orders them without a sort. Comment parents come from one sweep with an open-rule stack.
- `SourceMap` keeps a bitset of ASCII, tab-free lines; on such a line the column is the byte offset. `grammar_action_failure` scans the tree's kind column through `NodeRef` and returns early when the kinds are absent.
- **Correctness.** Ids and parents match GNU R exactly (`parse__parse_data_full`, `fuzz_parse_data_matches_gnu_r`). A trailing `#:` comment continued on the next line is one `ANNOTATION` node inside the statement, so `annotation_newline_significant` decides from its position whether the newline is significant.
- Next step if needed: emit parse data from the parser's own events instead of replaying the tree.

## Stats distribution closures enter their routine directly

**Status:** built.

- **Recognition by shape, not hash.** `recognize_routine_closure` (`base_fast_paths.rs`) accepts a stats-namespace closure whose body is `.Call(C_<routine>, ...)` with formals in order or `1/<formal>`, optionally inside `{}` or `if (missing(f)) <call> else ...`. Defaults used must be constants, and the routine and arity must be a row of `distributions::DIRECT_ROUTINES`. Stats bindings are lazy-load promises, so recognition runs in `do_lazy_load_fetch`, and an image start re-recognizes forced closures (`register_routine_entries`). A user's own `dnorm` is never in the stats namespace. Not recognized: the gamma family (`rate`/`scale` check), `dsignrank` (`on.exit`), nbinom (`mu`).
- **Decline instead of signal.** The entry runs the kernel in `Conditions::Decline` mode: any warning or error, a non-numeric or ALTREP argument, or a supplied `missing()` formal returns `None` and the closure runs, so conditions report GNU R's call. A draw first takes `random::checkpoint` (`.Random.seed` a plain integer binding equal to the live state, no user generator), and a declined draw calls `random::rollback`, so the R body redraws the same deviates.
- **Wrappers.** A promise of a variable bound to another unforced promise is read through, up to eight deep, when every promise settles without running R code; a self-referential chain (`function(n = n)`) declines so the R body reports GNU R's error. This holds for every native entry.
- `Interp::native_entries` is a `Vec<(Sexp, Entry)>` sorted by address and binary-searched; `Entry::Routine` carries an inline `RoutinePlan`.
- **Evidence.** `stats__direct_routine_calls.R`.

## Base `.Internal` wrappers inlined; string fast paths not kept

**Status:** built, except the rule that a replaced base binding always takes effect in compiled code, which "Compiled code binds GNU R's inlined base functions at compile time" supersedes.

- **Shape.** Tier 0 inlines the safe base internals (`INLINED_BASE_INTERNALS` in `crates/interp/src/bytecode.rs`, GNU R's `cmp.R` list) as `Op::Guard` plus `Op::Internal` when the head is base's closure by identity, its defaults are constants, and its body is exactly `.Internal(f(...))` over formals and literals. No wrapper closure runs, so `eval::call_inlined_internal` blames an error on the original call. The JIT treats `Op::Internal` as opaque, because `match` can run R code through `mtfrm`.
- **Past GNU R's list.** The `vector` family, `paste`, `paste0`, `sprintf`, `startsWith`, `endsWith`, `make.unique`, `.row_names_info` and `isNamespace` are inlined too. A wrapper with `...` takes R's matching (exact tags, positions before `...`, rest into `...`); a tag partially matching a formal before `...`, or `...` at the call site, refuses the inline, and `sprintf` refuses tagged arguments because `Op::Internal` passes no tags.
- **Base namespace and the wrapper's call.** `Op::Internal` runs the internal in `base_namespace`, so `paste` dispatches `as.character` from there and a caller's local method is not found, as in GNU R. `call_inlined_internal` sets `assignment_call` to the original call, so a warning reports `sprintf(...)`, not the enclosing call.
- **Left out on purpose.** Frame readers (`parent.frame`, `sys.call`, `sys.function`, `sys.parent`, `sys.nframe`, `sys.frame`, `environment(NULL)`, `eval`, `new.env`, `get`, `get0`, `exists`) read the wrapper's own context and would need a per-internal frame offset. `options(...)` passes tags. `lengths` and `as.vector` dispatch internally. `lockEnvironment` returns invisibly, which `Op::Internal` does not keep. `file.path` has a non-constant default.
- **Not kept: string compare, `nchar` and JIT string ops.** A pointer-compare `==`, a stored-length `nchar` and JIT `Str` types made the loop ops several times cheaper, but no program moved: a loop's string work is mostly the element read, already fast. Revisit only when a profile shows string compares or `nchar` on top.
- **Evidence.** `calls__inlined_base_internals`, `calls__inlined_base_internals_c_api`.

## The serialize reader checks what a stream claims before it stores it

**Status:** built.

- **Why.** The heap takes an object's kind from the position that holds it, so a stream with another kind there left objects the collector misread, and the damage outlived the read. Made-up lengths, out-of-range reference indexes and deep nesting were found by the libFuzzer target `interp_unserialize`.
- **Shape** (`crates/interp/src/serialize.rs`). The reader refuses a stream at the first claim it cannot back: a length must fit the rest of the input (`Reader::expect_elements`), a reference must name a slot (`reference_slot`), nesting runs through `on_large_stack`, and an item read for a kind-implying position is checked (`Reader::character`, `environment`, `attributes`, `tag`, `named_cells`, `fill_frame`). `named_symbol` guards symbol-name reads, because the unbound marker is a symbol without one.
- **Runtime state stays out.** A promise keeps only the two bits GNU R writes (ry's other bits name a live frame slot), a promise without a value needs an environment, and a `wrap_*` ALTREP must wrap a vector of its type. A byte-code reference slot is filled after its cell is read, unlike GNU R, so a cell cannot contain itself. A closure without an environment gets the base environment, as in GNU R.
- **Divergence.** A cell whose tag is not a symbol (only C code writes one) is refused here, where GNU R accepts it.
- **Evidence.** `crates/interp/tests/test_fuzz.rs` (`unserialize_refuses_an_object_of_the_wrong_kind`, `unserialize_keeps_a_stream_out_of_the_runtime_state`). `crates/rdb` and `crates/libr` have their own readers, which this does not cover.

## sprintf formats numbers with the native kernels

**Status:** built.

- `%f`, `%e`, `%g` and integer conversions write through `format.rs`'s `push_fixed`, `push_scientific` and `push_general`, so ry has one native number formatter. Formats parse once into a flat `SmallVec` of pieces; `n$` and `*` are `Amount::Argument` indexes.
- **Rounding is printf's, not `format`'s.** glibc rounds the exact binary value, so `sprintf("%.2f", 0.125)` is `0.12` where `format(0.125, nsmall = 2)` is `0.125`. The sprintf kernels use `fixed_fast` (exact through `fma`) or Rust's exact formatting, never `format`'s long-double power table.
- **glibc behavior matched.** A negative `.*` precision echoes the unparsed spec (`%.0-1f`); `%#g` carrying to the next power of ten prints `1.e+02`; `%#o` adds 0 only when the digits do not start with one. Type errors print the spec with `n$` removed and `*` replaced.
- **Contract.** `base__sprintf_conversions.R` and `fuzz_sprintf_matches_gnu_r` (`crates/oracle/src/fuzz.rs`).

## Hash tables for `unique()` and `match()` stop growing with the elements at 256 KB

**Status:** built.

- `PositionTable` (`crates/interp/src/sort.rs`) starts at two slots per element, capped at 65,536 slots, and doubles when half full past the cap, so few distinct values touch little memory. Starting small and always doubling was rejected: rehashing made `match()` on 5,000 distinct strings cost about 100 more instructions per element and slowed `merge()`.
- Strings hash by interned object, which is exact only for ASCII (equal text in latin1 and UTF-8 is two objects), so `TableKey::exact` is checked per distinct key and miss, and the first inexact one goes to the general path.
- Duplicate flags are a bitset; `anyDuplicated()` stops at the first word with a duplicate. `split()` counts per level and scatters through per-level cursors.
- `gather_kind` keeps the filtered position walk: mask walks and branchless compaction cost more where the selection branch predicts well.
- **Pinned by** `subset__logical_index_gather`, `base__split_factor_kinds`, `base__unique_match_kinds`.

## Positional builtin calls read their arguments from the registers

**Status:** built, except the values-plus-parallel-tags closure call shape.

- **Shape.** Whether a site is positional is a fact of the call expression, so the compiler writes `POSITIONAL_CALL_BUILTIN` into the opcode word, and `op_of` reports it as `CallBuiltin`, so the JIT, step table and images see no new opcode; the verifier accepts it only where `positional_call` holds. The handler writes up to eight `Arg`s into a stack array (`eval::with_positional_args`, shared with `jit::runtime::jit_call`), tries dispatch only for objects or S4 values (or matrix products, whose arity error dispatch raises), and calls `eval::call_builtin_positional`. The generic handler forwards positional words, because the optimizing tier routes every `CallBuiltin` to it.
- **Why compile time, not run-time quickening.** The image's code is mapped privately, so writing quickened opcodes at run time copies each touched page into anonymous memory in every process.
- `heap::activity::HOOKS` holds the activity-summary and profile-tick bits, so the common call tests one byte.
- **GC liveness.** The stack array copies registers that stay rooted and unchanged until the result is written, so it needs no root set.
- **Planned.** Builtins and closures agree on positional values plus parallel tags as the call-boundary shape; the closure protocol adopts it first. Pinned by `builtins__positional_and_tagged_calls`.

## A warning reaches `suppressWarnings` and `tryCatch` without running R

**Status:** built.

- **Muffled.** `muffled_by_suppress_warnings` reads the topmost handler for `simpleWarning`, `warning` or `condition`, and the warning returns at once when it is a calling handler with formals `w` and body exactly `if (inherits(w, classes)) tryInvokeRestart("muffleWarning")`, whose environment is a frame directly inside the base namespace binding none of `if`, `inherits`, `tryInvokeRestart`, and whose `classes` names one of the three classes. Identity with the literal in `base::suppressWarnings` does not work: the image does not keep a body and its compiled code as one object.
- **Why unobservable.** The handler's only effect is the unwind, and the muffled path runs no handler that could see frames or the condition. A `classes` promise is read without forcing only when its code is a string literal. Lower handlers never see the warning in either route, `last.warning` is untouched, and `options(warn = 2)` converts only at top level. Follows `RY_FAST_PATHS`.
- **Exiting handlers.** For a topmost `tryCatch(warning =)` or `condition =`, `warning_call` builds `simpleWarning` natively and jumps, because the handler runs after the unwind anyway. With no handler the default runs natively unless `warn = 2`, whose converted error's handlers see R frames. Other calling handlers take the R route.
- **Limit.** A rebound `base::inherits`, `base::tryInvokeRestart` or `base::simpleWarning` is not detected.
- Pinned by `conditions__suppress_warnings_muffle.R` and `conditions__warning_signal_routes.R`.

## `%%` and `%/%` on doubles port GNU R's kernels, with a 64-bit path for whole numbers

**Status:** built.

- Both kernels (`real_modulo`, `real_integer_divide` in `crates/interp/src/arith.rs`) port GNU R as it runs with an x87 `long double`. Earlier approximations differed in 311 of 2,209 grid pairs (`1 / (-6 %% 3)`, `1e-300 %/% -1`, NA payloads).
- Whole operands below 2^53 divide as `i64` (`whole_division`), exact and giving GNU R's `+0.0`. Other pairs follow GNU R's test order: zero divisor, NaN operands (`extended_nan` reproduces the x87 NaN choice, so `NA` survives against `NaN`), infinite dividend, the divisor-above-2^63 shortcut, then the correction through `numeric::LongDouble`.
- The scalar path, vector kernels, tier 0, the JIT helpers and typed ry's `F64` remainder and quotient (`crates/typed/src/backend.rs`, `values.rs`) all call these two functions.
- **Contract.** `builtins__modulo_bit_patterns.R` prints bit patterns over a grid and checks scalar and vector agreement.

## `validObject` runs validity methods without its slot loop, and `new()` copies the prototype shallowly

**Status:** built.

- **`validObject`.** When every slot passes the native check, `ryS4Valid` binds what the R body has bound after its slot loop (`Class`, `classDef`, `where`, `errors`, `slotTypes`, `slotNames`, `attrNames`, `idx` and the loop's last `i`, `classi`, `classDefi`, `namei`, `sloti`, `ok`) and returns 2; the prologue in `crates/interp/src/methods_core.R` runs GNU R's statements after the loop verbatim, so validity methods see the same frame, `sys.call()` and errors. The slot loop wraps each check in `try()`, which is what it avoids. It declines on `complete = TRUE`, unmodelled slots, a data part as the last slot, or an unresolvable package. Running the tail through `eval::eval` was rejected: AST evaluation is slower than compiled R.
- **`new()`** (`do_new`, `C_new_object`) copies the prototype with `shallow_duplicate`. This is sound because interpreted code modifies in place only with the only reference, and the prototype holds one. `R_do_new_object` in libr stays deep, because package C code may write into a `NEW_OBJECT` slot without checking.
- **Accepted risk.** C code that writes into a prototype-valued slot of an argument without checking `MAYBE_SHARED` changes the class prototype on ry; such code already breaks on GNU R when the slot came from a shared value.
- **Left open.** `standardGeneric` turns the generic's slot frame into an environment per dispatch (`dispatch::do_standard_generic`, `methods::exec_method`), and `new`, `initialize`, `validObject` get environments because their prologues pass `environment()`.
- **Contract.** `s4__validity_fast_path.R`, `s4__new_copies_prototype.R`.

## Small base closures with call arguments run their bodies natively behind prologues

**Status:** built.

- Native entries run only when every argument is a value, so `isTRUE(!is.na(x))` ran the R body. Prologues (`base_fast_paths.rs`) force arguments in the frame in the body's order, compute natively and decline otherwise. A closure may have both: `register_native_entries` hashes the body behind the prologue (`prologue_original`).
  - `ifelse` (`ryIfelse`) takes only the quick return: an atomic length-one test without attributes; only the chosen branch is forced and must be a plain length-one vector or a function.
  - `unname` declines unless `force` is default, and for objects and one-dimensional arrays.
  - `%in%` forces `x` then `table` in its own frame with no `match` frame, because GNU R's byte compiler inlines the body's `match()` as its `.Internal`.
  - `unlist` forces `recursive`, `x`, `use.names` in the body's order and declines for an object `x`, a list of factors, or a flag that is not plain `TRUE`/`FALSE`.
- **Rejected: an entry that evaluates the argument expression itself.** It would have to prove the evaluation unobservable (head lookups, handlers, `sys.calls()`), and the `isTRUE` prologue already costs what a call of `function(x) x` costs.
- **Not done.** `unique` needs the S3 lookup repeated natively to prove it reaches `unique.default`. `lapply`/`vapply` call `match.fun(FUN)`, which forces `FUN` in its own frame, and the `.Internal` evaluates `FUN(X[[i]], ...)` in the closure's frame, so the saving would be the `match.fun` call alone.
- **GNU R behavior.** `names(x) <- NULL` on a call or pairlist drops its tags. The `unlist` internal errors on a bad `use.names` as `asLogical2()` does, returns an expression vector unchanged, and recurses into expressions for the answer type (`answer_type`, shared with `c(..., recursive = TRUE)`).
- **Contract.** `builtins__ifelse_scalar_fast_path.R`, `builtins__scalar_closure_prologues.R`, `builtins__in_and_unlist_prologues.R`.
