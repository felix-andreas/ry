# Decisions: Builtins and base R

Base R's builtins and native kernels, numerics, regular expressions, connections, the default packages, and package code that runs natively.

## One connection table, in the interpreter

**Status:** built.

- **Previous sources of truth.** There were two connection tables. `libr::connections` held GNU R's
  `struct Rconn` table (128 slots, the devices, custom connections, the C entry points) and an R
  function table (`PRIMITIVES`) that nothing registered once the interpreter was installed.
  `interp::connections` held a second, simpler table (its own connection records and class enum)
  with the sinks, `cat` and every connection builtin the interpreter actually ran. A connection that
  R code opened was therefore invisible to `R_GetConnection`, and a custom connection a package
  created could not be read with `readLines`.
- **What was duplicated.** The table, the slot numbering, the R-level connection object, every
  device kind and the read and write paths existed twice, and the libr copy was dead code apart from
  the C entry points. Warnings were also kept twice: libr's `Runtime.warnings` beside the
  interpreter's deferred list.
- **Target shape.** `interp::connections` owns the `#[repr(C)] Rconn`, the table, the devices, the
  sinks and every connection internal, registered with GNU R 4.3.3's arities. A package's own C
  method is called through the `Hooks::connection_method` hook, which libr implements with the
  `native.c` trampolines under a native scope, and ry's methods that C calls enter through
  `Hooks::enter_from_native`, libr's `enter`, so an R error becomes the `longjmp` C expects. The
  default `vfprintf` is native code, so its address comes in as `Hooks::connection_vfprintf`. libr
  keeps only `R_new_custom_connection`, `R_GetConnection`, `R_ReadConnection`, `R_WriteConnection`
  and `ry_connection_count`. The interpreter's root source traces what the table holds (a text
  output connection's variable and environment), replacing preserve and release. libr's
  `boundary::warn` defers onto the interpreter's warning list, which the test exports read.
- **Impact.** One table means R code and C code see the same connections, which makes the custom
  connections of curl and processx readable from R. The interpreter still builds and tests
  without libR, because native calls go through hooks whose defaults error, and the merged table
  is about 750 lines smaller than the two were.
- **`closeAllConnections` is not an internal.** GNU R defines it as a base closure over
  `getAllConnections` and `close`, so ry registers no internal for it.

## Base R's linear algebra

**Status:** built.

- **Previous shape.** `%*%`, `crossprod` and `tcrossprod` were a plain Rust loop in `arith.rs` with no complex support, and base's `.Fortran` routines (`dqrdc2` and friends) were listed by `getRegisteredRoutines` with null addresses, so `qr()` failed. The routine list was a hard-coded copy of names and arities.
- **Target shape.** `crates/interp/src/linalg.rs` calls the system's BLAS and LAPACK, opened at runtime with `dlopen` (the workspace builds without them; an operation that needs a missing library is an R error), with GNU R's arguments, workspace queries and gfortran's hidden character lengths. On the same libraries the results are GNU R's bit for bit, which `crates/interp/tests/test_linalg.rs` pins.
- **`dqrdc2` is Rust, written from its description** (R's LINPACK variant, which no system library ships), as are the `dqrsl` family and `dtrco`. They still call BLAS level 1 (`dnrm2`, `ddot`, `daxpy`, `dscal`, `dasum`) because those decide the bits: reference `dnrm2` is a scaled algorithm, not a plain square root of a sum.
- **One table of base's native routines**, `native::BASE_ROUTINES`. `getRegisteredRoutines` derives from it, and every `.Call`, `.C` and `.Fortran` (the interpreter's and libr's) asks `native::call_base_routine` first, so `qr()` works with or without libR. A routine is base's only when its `NativeSymbolInfo`'s `dll` is named `base` (base R's loader binds these as `.F_dqrdc2`, `.C_R_addTaskCallback` and so on). GNU R's `R_init_base` calls `R_forceSymbols(dll, TRUE)`, so a lookup by name never reaches base: `.Fortran("dqrdc2", ..., PACKAGE = "base")` is the error `"dqrdc2" not available for .Fortran() for package "base"`, `is.loaded("dqrdc2")` is `FALSE`, and base's `DLLInfo` reports `forceSymbols = TRUE`. ry matches all three.
- **The matrix products are ry's own kernels, not the installed BLAS.** `%*%`, `crossprod` and `tcrossprod` sum each entry's terms in reference `dgemm`/`dgemv`/`dsyrk`/`zgemm` order (columns added as axpys, so the inner loop vectorizes without reordering a sum), which is also the order of GNU R's NaN fallback loops, so one kernel serves both paths and the NaN screen is gone for reals. Previously they called whatever `libblas.so.3` was installed, so the bits followed the host: CI's OpenBLAS (an x86-64-v3 build with FMA kernels) moved `a %*% a` by 1 to 2 ULP. Now the result is GNU R's on its bundled reference BLAS on every machine and needs no library; on a machine whose GNU R uses OpenBLAS, ry's products differ from that GNU R's in the last bit, by design. Complex products use Fortran's plain multiply on finite operands (as `zgemm` does) and C99's otherwise. A faster kernel must keep each entry's sum sequential in the inner index (block over rows and columns only, no FMA); `backlog.md` tracks it.
  - **Which BLAS GNU R uses.** A GNU R built from source links its own reference `libRblas`/`libRlapack` (verified with the R 4.6.2 build: `sessionInfo()$BLAS` is `libRblas.so`), and its `default` and `blas` products equal ry's kernels bit for bit. Debian and Ubuntu `r-base` resolves `libblas.so.3` through the alternatives system, so it can be OpenBLAS; a reported `FALSE` for `default`/`blas` against a plain double loop (reference `dsyrk` is exactly that loop) therefore means an optimized BLAS, which ry does not reproduce by default.
  - **Opt-in host BLAS.** `options(ry.blas = "host")` (or `RY_BLAS=host` when the option is unset; the option wins) sends real and complex products through the BLAS `blas()` found for LAPACK, with GNU R's routine choice from `array.c` (`dgemv` for vector shapes, `dsyrk` for the symmetric `crossprod(x)`/`tcrossprod(x)` plus a mirrored triangle, `dgemm`, `zgemm`) and its NaN screen per `options(matprod=)` (`mayHaveNaNOrInf` pairs for `default`, the plain sum for `default.simd`, none for `blas`; a tripped screen falls to the reference kernel, which is GNU R's fallback loop). `"internal"` ignores it. Default stays `"reference"`, ry's kernels. Verified: all 66 product shapes and modes of a probe equal GNU R on this machine's reference BLAS; no OpenBLAS was installed here, so the last-bit equality with an OpenBLAS GNU R is unverified (the call mapping is the part that was checked). LAPACK-backed functions already call the host libraries. `test_linalg.rs` reruns the pinned products under `"host"` on the reference libraries.
  - **Vector operand shapes follow `do_matprod`.** `tcrossprod(x, y)` with a matrix `x` and a vector `y` takes `y` as a row when `nrow(x) == 1` and as a column otherwise, so `tcrossprod(7x40, vector of 40)` is non-conformable as in GNU R.
  - **`options(matprod = "internal")` is GNU R's long double loop.** Each entry sums the double products in x87 long double and rounds once (`long_double_product` in `linalg.rs`); complex entries sum real and imaginary parts separately. `default` and `default.simd` keep the reference-order kernel, which equals GNU R's NaN/Inf fallback loop, so the screen changes no result. Invalid values are rejected by `validate_option`.
  - **Related long double ports.** `cumprod` keeps the running product in x87 long double like `cumsum` (`long_double::running_products`). Complex `sum`, `prod` and `mean` use per-argument long double parts, as `csum`, `cprod` and the complex `mean` do. One-argument `cor` takes the double `sqrt` of the rounded variance (`cov_complete1`), two-argument `cor` takes `sqrtl` of the long double sum. `fft`/`mvfft` port `fft.c`'s `fft_factor`/`fftmx` (`fft.rs`), so mixed-radix bits match.
- **GNU R's quirks are kept on purpose**: `La_solve`'s condition check norms the already factored copy when `a` was not double; `La_dgecon` estimates with `n` as the leading dimension, which ry refuses for `m < n` instead of reading past the matrix.
- **BLAS and LAPACK buffers sit at GNU R's addresses modulo 16.**
  - **Previous shape.** Every buffer went to the library where ry happened to hold it: R vectors at their heap address (always 0 modulo 16, since `VECTOR_DATA_OFFSET` is 32 and slots are 16-aligned) and scratch space in a `Vec`. On OpenBLAS's SSE2 level-1 kernels (selected for Zen, Sandybridge, Nehalem, Atom, Prescott, Core2, Penryn, Dunnington, Nano, Barcelona, Bobcat) the result moved by one ulp against GNU R on the same library: those kernels peel a first element that is not 16-aligned, which regroups the partial sums of `dasum`, `ddot` and the like. `dtrco`, `La_dtrcon` and the `dqr*` family failed `test_linalg.rs` that way (the release CI's `dtrco` failure). Haswell, SkylakeX and Cooperlake kernels have no peel, which is why it passed locally.
  - **GNU R's rule, measured with `.Internal(inspect())` and read from `memory.c`.** Vector data starts 48 bytes into the node. A vector of 128 data bytes or less comes from a small-vector page whose nodes sit at 8 modulo 16, so its data is at 8; a larger one comes from `malloc`, so its data is at 0. `R_alloc(n, size)` is a raw vector of `n * size + 1` bytes and follows the same rule.
  - **Shape.** `Placed` and `PlacedInput` in `linalg.rs` hand each buffer at the address GNU R's buffer has in the same call, tagged `Allocation::Vector` or `Allocation::Scratch` per GNU R's `Lapack.c`, `array.c` and `.Fortran`. An R vector already at the right address is used in place; otherwise a copy at the right address is used and written back on drop. Only buffers of 128 bytes or less ever move. `La_svd_cmplx`'s real workspace takes GNU R's size, because the size decides the address.
  - **Rejected: moving vector data in the heap.** Putting small vectors at 8 modulo 16 would fix package code too, but `VECTOR_DATA_OFFSET` is baked into the JIT's code generation and the start-up image, and it costs memory. Package code calling BLAS on small vectors stays exposed (`backlog.md`, "BLAS buffer addresses in package code").
  - **Impact.** Results equal GNU R's on every OpenBLAS kernel listed above and on Haswell, SkylakeX and Cooperlake, at the cost of copying buffers of 16 doubles or fewer. The Opteron and Bulldozer-family kernels were not tested.
  - **Pinned by** `test_linalg.rs`, run with `RY_TEST_REFERENCE_LIBRARIES=1`, GNU R on `PATH` and `OPENBLAS_CORETYPE` set to one of the kernels above.

## The default packages' utils, tools, grDevices and graphics routines

**Status:** built.

The routines live in `crates/interp/src/{utils,tools,graphics}.rs`, wired by package in
`builtin_libraries::implementation`. Each behavior was derived from GNU R's documentation and its
observed output (the oracle cases `io__*`, `colours__*`, `graphics__*` and
`utils__sizes_hashtabs_and_tools`), never from its C source.

- **One graphics engine for base graphics, grid and device packages.** GNU R's split is kept:
  graphics systems (base graphics, grid) call the engine (`crates/graphics/src/engine.rs`), and the engine
  calls a device through the C callbacks of a `DevDesc`. `GEDevDesc`, `DevDesc`, `GEContext` and
  `GESystemDesc` are `#[repr(C)]` copies of R_GE_version 17's headers, because grid and ragg read
  and write their fields directly; a built-in device is a `DevDesc` whose callbacks are Rust
  (`crates/graphics/src/render.rs`), so a ragg device and `pdf()` take exactly the same path. Previously base
  graphics drew straight into the built-in devices' shape lists and the GE entry points errored, so
  grid and ragg could not run and two drawing paths would have had to agree.
  - The display list is struct-of-arrays inside the engine's own `EngineDevice` (parallel `ops`
    and `args` vectors of the primitive and its argument pairlist), and `GEDevDesc.displayList`
    stays `R_NilValue`. A pairlist is built only where GNU R exposes one: `recordPlot` snapshots
    and `dev.copy`. Recording a call is two pushes, not a cons chain. The cost: a C device that
    tests `displayList != R_NilValue` before replaying on resize (as GNU R's own window devices
    do) never replays; ragg and the file devices never resize, so none does today.
  - `R_GE_checkVersionOrDie` accepts engine versions 14 to 17 rather than GNU R's exact match,
    so binaries built for any of R 4.1 to 4.6 load. `DevDesc` only grew into its reserved space
    over those versions. `GEDevDesc` gained `lock` before `gesd` in 4.4, and the engine serves
    both layouts by storing each system descriptor in two adjacent slots and mirroring `ask` into
    the last one (`GEDevDesc`'s doc comment).
  - `R_GE_getVersion`, `R_GE_version` in `GraphicsEngine.h` (through `Rversion.h`) and a recorded
    plot's `engineVersion` follow the R release ry presents: 16 through R 4.5, 17 from R 4.6. A
    device binary built for that release and `replayPlot()` on a plot it recorded then see the
    version they expect.
  - `GEregisterSystem` hands the caller its index (through a claim callback) before it calls the
    system on the open devices, because grid's `InitState` reads its own index on that call.
  - `patternFill` defaults to `R_NilValue`, which ragg reads.
  - `fromDeviceHeight` in inches is `value * ipr` without a sign, as in GNU R; a y-down device
    (every bitmap device) otherwise gets negative string heights, which shifted rotated grid text.
  - Base text hands both adjustments to `GEText`, so an unspecified vertical adjustment centres
    the label on its glyphs' extent from the device's metric callback, as GNU R does.
  - Base graphics hands rasters to `GERaster` and X-splines to the engine's `xspline`, as GNU R
    does, so a device package draws them itself. The X-spline is written from Blanc and
    Schlick's paper; its sampling (one point for a segment between two corners, otherwise a step
    of `2 / sqrt(length in 1/1200 inch)`, at most 0.2) reproduces GNU R's open splines exactly.
  - `plot.new` marks the device dirty, because grid starts a page of its own on its first
    drawing unless another system already drew.
  - Text widths are Helvetica's AFM widths plus its ASCII kerning pairs (tables generated from
    GNU R's `afm/Helvetica*.afm`), which makes `strwidth` on pdf match GNU R exactly. Bitmap
    devices in GNU R measure with cairo's font instead, so their widths differ slightly.
- **Built-in devices record a drawing and render it when they close.** Each built-in device's
  callbacks append `graphics::render::Item`s (a path, text or group use, with the clip and mask in
  force) in inches to the current page, and each file format is one writer over the same
  `Drawing`: PDF, SVG and PostScript as vectors, PNG, JPEG, BMP and TIFF rasterized with
  tiny-skia, text outlined with ab_glyph from the system's Helvetica-compatible fonts. Rendering at
  close keeps devices free of file handles and makes a multi-page bitmap device one file per page,
  as GNU R's `%d` names expect. Vector devices create their file at open, so a bad path fails
  there.
  - Patterns, clipping paths, masks and groups are `Definition`s in one append-only arena on the
    `Drawing`, and the reference R receives is the arena index. A mark refers to a definition by
    index, so pages rendered at close still find it after R released the reference (the release
    callbacks do nothing), and nothing is copied per use. A definition's own marks are recorded
    by running its R function with the device's target swapped to a fresh list, so nesting
    (a pattern in a mask in a group) falls out of the same callbacks. Rendering bounds nesting at
    32 levels, which only a malformed drawing reaches.
  - Device versions follow GNU R's devices: pdf, svg and the bitmap devices are version 16 (paths
    and glyphs), and PostScript is version 15, refusing every definition and reporting the
    capabilities unchanged (`NA`), as GNU R's PostScript device does. PostScript also drops
    semi-transparent colours with GNU R's once-per-page warning.
  - The bitmap devices render luminance masks, where cairo ignores them with a warning, and report
    masks as alpha and luminance; this is a deliberate improvement. `svg()` composites like
    `pdf()` (blend modes only; other operators warn and draw as `over`) because SVG has no
    Porter-Duff operators, where GNU R's cairo svg device rasterizes them. Both write isolated
    groups, as cairo does.
  - Glyph callbacks read the font file R names and draw nothing when it cannot be read, matching
    GNU R's pdf device, which is silent there.
  - A raster is one `Mark::Image` (pixels plus a placement mapping the unit square to inches),
    not a quadrilateral per cell. PDF writes an image XObject with an `/SMask` when any pixel is
    translucent and `/Interpolate true` when interpolating. PostScript writes an `image` with the
    alpha dropped and, when interpolating, resamples to the placement's size in points with GNU
    R's sixteenth-step bilinear filter, so the hex data is byte-identical to GNU R's. The bitmap
    devices sample bilinearly (interpolating) or nearest with a pixel-centre coverage mask, as
    cairo does; edge pixels' anti-aliased coverage differs from cairo's by a few levels.
  - `svg()` marks a non-interpolated image `image-rendering:pixelated`, where cairo emits no
    hint and browsers blur the cells; this is a deliberate improvement. An interpolated image is
    upsampled before embedding, as cairo does.
  - A tiling pattern with `extend = "pad"` or `"none"` in `svg()` is a cell large enough to cover
    the page, holding the tile plus its eight edge and corner strips (one point wide) stretched
    outward; the strips overlap the tile, which is drawn last, so renderers show no seams.
  - tiny-skia has no `saturate`, so `draw_items` composites it by hand on premultiplied pixels:
    `dst + src * min(1, (1 - Da) / Sa)`, which matches cairo's pixels exactly.
- **The drawing geometry is GNU R's, checked against its pdf output and ragg pixels.** A line of
  `lwd` 1 is 1/96 inch, a margin line is `mex * csi`, text sits on a margin line by its baseline
  offset by `ylbias`, and symbols are sized from half the character height. Base graphics
  computes positions in inches rather than through GNU R's NDC pipeline, so the last bit of a
  coordinate can differ. The rules below were found by diffing the plot gallery against GNU R:
  - The point size and `cex` reach the device separately (`text_context`), so rounding a size to
    whole points is the device's business, as in GNU R; base graphics rounding it first put
    ragg's text a pixel off at fractional `cex`. An inline `cex` scales text the same way.
  - User coordinates map through the plot region `usr` was set on (GNU R's `win2fig`, the
    `window` field of the parameters), not the current one, so changing `mar` after
    `plot.window` moves nothing until the next `plot.window`.
  - `mtext` and margin titles clip to the device, not the figure; `clip()` keeps its region only
    while `xpd` equals its value when `clip()` ran, as GNU R's `GClip` drops it.
  - Contour labels are a port of GNU R's `contour()` placement (the simple, edge and flattest
    methods, label-box intersection, and its quirks: the cumulative gradient variance, the angle
    computed with `180 / 3.14`), because any other placement lands labels elsewhere.
  - Labels are a `Label` (string lines, or an expression laid out by `plotmath.rs`), and every
    text path (`text`, `mtext`, `title`, `axis`, `strwidth`) goes through `push_label`, so
    plotmath is one code path rather than a deparse fallback per caller.
  - Base graphics draws `pch` 0 to 25 through `graphics::engine::symbol` (GNU R's `GESymbol`),
    which sizes symbols in device units. A second copy of the shapes in `push_symbol` had drawn
    pch 0, 15 and 5 at different half-widths and started pch 23 at another vertex, which moves
    where an antialiased outline closes.
  - A missing `adj` or `at` in `mtext` defaults by `las` and side (`ComputeAdjValue`,
    `ComputeAtValue`), not to `par("adj")`; `tck` below 0.5 is a fraction of the plot region's
    shorter side, above it of its extent across the axis.
- **The editor's plot panel is fed from the engine's display list.** A console with a
  subscribed editor makes its default device (`options(device=)`) a `pdf(NULL)` device with the
  display list enabled, when the subscription arrives so the next command already uses it. A
  default the user set is kept, but the platform's own (`pdf`, `X11`, `quartz`, `windows`) is
  replaced, since an X11 default would open a window or fail without a display. At each prompt, `publish_plot` in
  `console_server.rs` compares the current device's display-list stamp
  (`graphics::engine::display_list_stamp`, drawn from one global counter, so a device that takes
  a closed one's slot never repeats a stamp) with the last one sent; when it changed, it copies
  the display list onto an svg device with `dev.copy` (warnings muffled, and every device it
  opened closed on exit, so a replay that fails leaks neither a device nor a warning) and sends
  the file's text as a `ryPlot` event, which VS Code shows in one "R Plot" panel. A device of its own (httpgd-style) would
  duplicate the engine and the devices; the display list is already the single record of what
  was drawn. File devices keep their display list off, as in GNU R (`GErecordGraphicOperation`
  records nothing while it is off), so `png()` output is never sent.
- **Base graphics is checked against GNU R by a plot gallery.** `crates/oracle/gallery/*.R` holds
  about 150 figures (base graphics including the PerformanceAnalytics charts, grid, lattice,
  ggplot2), and `oracle gallery` renders each on both engines and every device, then compares.
  ragg (`agg`) rasterizes identically on both engines, so it is the sharp check, with a 0.2%
  differing-pixel tolerance; `png` and pdf (rasterized with `pdftoppm`) get 5%, and a pixel
  there differs only when nothing within one pixel in the other image matches it, because GNU R's
  cairo and ry's tiny-skia antialias differently (a dense tangle of thin lines, such as a lattice
  parallel plot, otherwise differs in 18% of its pixels while looking identical). The harness
  auto-prints a figure's visible value, since lattice, ggplot2 and xts draw only when printed. `svg` is rendered and timed but not compared,
  since cairo writes glyphs as paths. The figure sample is drawn from one seed, and
  `test_gallery.rs` runs a bounded sample of it in the default suite; a bug class the gallery
  finds is pinned by a `graphics__*` oracle case, which states the value (a `par`, a metric, a
  return) rather than a pixel.
- **The device table mirrors GNU R's**: slot 0 is the null device, at most 64 devices, `.Devices`
  (a pairlist whose entries carry `filepath` for a device opened on a file) and `.Device` in base
  are rebound from the table after every change, `dev.set(1)` starts the default device but
  reports device 1 as GNU R does, and any routine that needs a device opens
  `getOption("device")` first, which is how a script with no device gets `pdf("Rplots.pdf")`.
- **One copy of `par`.** GNU R keeps default and current parameters so a high-level call's inline
  parameters apply for its duration; ry reads inline parameters from the call instead, so a single
  `Pars` is the source of truth.
- **Regions remember the unit they were set in** (`Margins`, `OuterMargins`, `PlotRegion`,
  `FigureRegion` enums). Base graphics recomputes the others from that one when the device, `cex`,
  `mex` or the layout changes, so `par(mai = ...)` followed by `par(cex = 2)` keeps the inches and
  moves `mar`, and storing all units would let them drift apart.
- **Text metrics are Helvetica's published AFM widths** (regular and bold; the oblique faces share
  them) at the nearest whole point size, with the cap height for `strheight`, and the symbol
  face (`font` 5, which plotmath uses for operators, Greek and delimiters) uses the Symbol AFM.
  That matches the pdf device's numbers exactly; bitmap devices in GNU R measure with cairo fonts
  and can differ. The device's metric callback receives a glyph as its Adobe Symbol code, but a
  string arrives as the Unicode characters plotmath translated it to, so only the string path
  maps back (`symbol_metrics`, `adobe_symbol_code`): mapping both ways confuses raw code 174
  (an arrow) with U+00AE, the translation of code 210.
- **`cairoProps` reports cairo**, so `capabilities()` lists png, jpeg and tiff and the bitmap
  devices take the cairo path, which needs no display.
- **The palette is thread-local state in `graphics.rs`**, packed as GNU R packs colours (red in the
  low byte), because only the colour routines read it and the interpreter is single-threaded.
- **`scan` is implemented in `utils.rs`, not `connections.rs`**, because `scan`, `readtablehead`
  and `countfields` share one tokenizer and only `read.table` drives all three. Connections expose
  their unread input through `connections::with_input`, and `pushBack` splices into it.
- **A hash table keeps its entries in R vectors behind the external pointer** (type, keys, values,
  hashes, count), and a lookup compares a hash that agrees with `identical()` before calling it. The
  collector then sees every key and value without a Rust-side table it would have to trace.
- **`contourLines` reproduces GNU R's line order and direction** by rules found from observation,
  written on `graphics::contour_level`: segment edge order, the saddle rule, the triangle for one
  missing corner, the nudge of values equal to the level, and closing a loop. A saddle whose
  bilinear saddle value is within rounding of the level can still pick the other pairing.

## parallel's fork routines

**Status:** built.

- `crates/interp/src/parallel.rs` and `crates/interp/src/fork.rs` implement all of parallel's `.Call` routines: the L'Ecuyer stream jumps and the `mc_*` fork routines behind `mcfork`, `mclapply` and `mcparallel`. The pipe protocol is GNU R's (a native-endian `R_xlen_t` length, then the bytes; a zero length means the child is exiting), because parallel's R code drives it unchanged.
- **Fork safety.** The runtime starts no helper threads (no GC, JIT or I/O threads), so `fork()` copies a consistent interpreter and no lock can be held by a thread that does not exist in the child. What would be copied wrongly is buffered output, so `mc_fork` first flushes standard output and error and every file connection's write buffer (`connections::flush_pending_writes`). The child closes its siblings' pipes, so a sibling's standard input still reaches its end. `mc_exit` ends the child with `_exit`, so the master's exit handlers and buffers are not run twice.
- **Reaping without SIGCHLD.** GNU R reaps from a SIGCHLD handler, and its children wait for SIGUSR1 before exiting. ry does neither: a child exits at once, and the master calls `waitpid(WNOHANG)` on released children at the start of each routine. A child is released (its pipes closed) only once its pipe reaches its end or it is removed, detached or estranged, so its pid cannot be reused while it still names a tended child. `mc_children` lists tended children only, which `mccollect`'s loop needs in order to end.
- **Cleanup scopes.** `mc_prepare_cleanup` pushes a mark, and each child records the innermost mark at its fork. `mc_cleanup` acts on the children forked since the innermost mark (all children when there is none) and pops the mark only when it signals or detaches, because `mccollect`'s `cleanup(FALSE, FALSE)` runs inside `mclapply`'s scope and must not end it.

## One Rmath, one codec stack, and the numerical routines

**Status:** built.

- **One Rmath.** The interpreter's distributions and libR's Rmath were two independent
  implementations of the same functions, so a precision fix landed in one and not the other.
  `crates/numeric/src/rmath.rs` is now the single copy (libR's version, the one the conformance
  suite holds to GNU R): the interpreter's builtins vectorize it, libR only converts C flags,
  raises the collected warnings and draws variates, and `gamma`, `lgamma`, `beta` and `lbeta` call
  it instead of libm's `tgamma`, which is not exact for whole arguments.
- **One compression stack.** libR decoded bzip2 and xz with `bzip2-rs` and `lzma-rs` while the
  interpreter used `bzip2` and `lzma-rust2`, which also write. libR now calls
  `interp::connections::decompress` (file magic) and `interp::serialize::decompress_record`
  (lazy-load records), so one set of codecs reads every file and both crates drop the others.
- **Time zones come from jiff** reading the system's tz database (`system.rs`, `Zone`). A name the
  database does not know acts as UTC under the abbreviation glibc gives it (the name's leading
  letters when there are three or more, else empty). Two glibc behaviors are copied deliberately:
  an `isdst` that contradicts the zone's rule shifts the reading by an hour, even in a zone with
  no daylight saving time, except for `"UTC"` and `"GMT"`, which GNU R converts itself; and a
  year outside jiff's range moves by whole 400-year cycles, which keeps the calendar and weekdays.
- **A repeated (fall-back) wall time takes the offset in force at the wall time read as UTC**,
  which is what glibc's `mktime` does with `isdst = -1`, so Berlin's 02:30 reads as CET while New
  York's 01:30 reads as EDT. `strptime` keeps a spring-forward gap's wall-clock fields.
- **The locale comes from the C library.** The session calls `setlocale` for the categories GNU R
  sets at startup (numbers stay C), `Sys.setlocale`/`Sys.getlocale` go through it, and `sort()`
  reads `LC_COLLATE` from it (`sort::refresh_collation`) instead of a fixed C.UTF-8 answer, which
  is what withr's `local_locale` tests and C-collation sorts observe.
- **`nlm`, `nlminb` and `polyroot` follow the published algorithms**: Dennis and Schnabel's
  UNCMIN for `nlm`, whose line search and trust-region updates translate GNU R's `uncmin.c`
  (see "licensing"), netlib's PORT reference for `nlminb` (see "`nlminb` runs PORT's own
  drivers"), and
  Jenkins and Traub's three-stage algorithm for `polyroot`, which also fixes the order of the roots
  GNU R returns.
- **One UNCMIN, one QUADPACK, one interval search, all in `interp`.** libR used to carry its own
  copies of the routines behind `optif9`/`fdhess`, `Rdqags`/`Rdqagi` and `findInterval2`, next to
  the interpreter's `nlm`, `integrate` and `findInterval`. The single copies are
  `stats::nlm` (all three methods, analytic derivatives, the `msg` bits, `fdhess`),
  `stats::optimize::{qags, qagi}` (writing into caller-owned `Subintervals`, so the C
  entry points pass `work`/`iwork` straight through) and `interp::arith::find_interval`; libR
  only converts C callbacks and arguments, as `lbfgsb` does. GNU R black box decided the details
  the copies disagreed on: the cubic backtrack groups its coefficient as `3 * t3 * (...)`, and a
  non-finite value (which nlm turns into the largest double) shrinks the step by ten. nlm's call
  protocol is also GNU R's, as observed through call counts: an uncached probe at the start whose
  value is ignored and which alone decides whether `gradient` and `hessian` attributes count
  (wrong length or mode warns and ignores them), a ring of the five latest evaluations, and the
  error "non-finite value supplied by 'nlm'" instead of a call at a non-finite point. Where GNU R
  reads past a later attribute that is too short or missing, ry errors or uses zeros.
  `stats__uncmin_quadpack.R` pins the iterates at full precision and the call protocol.
  Methods 2 (double dogleg) and 3 (More-Hebdon hook) match GNU R bit for bit, which took these
  departures from the textbook, all found by tracing every evaluation in `%a`: the trust-region
  update retries with a doubled radius only for a Newton step whose decrease the model predicted
  to within a tenth; after the switch to central differences an analytic gradient builds the
  Hessian from gradient calls; the hook's mu update uses the derivative at mu = 0 (`phip0`); both
  `phip0` and `phip` square `sqrt(dot)` instead of using the dot product; and the unfactored
  secant update sums its two corrections before adding them to H. The libR case
  `optif9_trust_region` compares at tolerance zero, so it assumes reference-BLAS rounding in the
  GNU R that recorded it (GNU R's `ddot_` and `dnrm2_` come from the system BLAS).
- **`parse_Rd` is a hand-written parser** in `crates/interp/src/rd.rs`, built from the Rd format's
  documentation and GNU R's observed output (`tools__parse_rd`). It tracks the three lexical modes
  (LaTeX-like, R-like and verbatim) per macro, keeps srcrefs, and reproduces GNU R's warnings and
  errors for unterminated strings. `deparseRd` reads the same tree.
- **`iconv` goes through the C library's `iconv`** rather than a Rust crate, because GNU R's
  results (which names it accepts, how `sub` substitutes, transliteration) are glibc's.
- **UTF-8 to UTF-8 copies its valid prefix** (`platform::Iconv::convert`, used by both `Riconv`
  and `iconv()`). glibc decodes and re-encodes at about 30 instructions a byte; validating with
  `str::from_utf8` and copying costs 1.2 a byte on ASCII and 5.7 on dense non-ASCII text.
  `processx` converts every child-output chunk this way, which made it the largest single entry
  in vctrs', rlang's and purrr's test runs. glibc still converts whatever follows the strictly
  valid prefix, because it accepts more than strict UTF-8 (code points past U+10FFFF, five-byte
  forms) and reports a cut-off invalid sequence as `EINVAL`; UTF-8 is stateless, so its answer
  equals one call over the whole input. A full output stops at the last whole character with
  `E2BIG`, as glibc does. The copy applies only when both names spell UTF-8 (in any case,
  with or without the hyphen). `Riconv_open` resolves `""` to the codeset of the current
  `LC_CTYPE` (`platform::native_codeset`) before that test, because GNU R passes `""` straight
  to `iconv_open`, where it names the locale's codeset; under `LC_CTYPE=C` that is ASCII, and
  under a Latin-1 locale it is ISO-8859-1. Pinned by
  `platform::tests::utf8_identity_matches_glibc` and the oracle cases
  `strings__riconv_utf8_identity` and `strings__riconv_native_encoding`.
- **Impact:** each fact has one implementation, so the oracle and GNU R's own tests exercise the
  code that libR and the interpreter both run. No effect on static analysis.

## POSIX regular expressions run on an automaton when they need the longest match

**Status:** built.

- **Previous shape.** TRE patterns (`perl = FALSE`) ran on fancy-regex with `leftmost_longest`, which sends every variable-width pattern to the backtracker. lgr's `(.*z.*){10}` never finished.
- **Target shape.** `Matcher::Automaton` keeps two regex-automata `meta::Regex` beside the fancy one: a leftmost-first search gives the match start, and an anchored `MatchKind::All` search from there gives the longest end, which is POSIX's leftmost-longest rule in linear time. Capture groups still come from fancy-regex, and the callers that do not report groups (`regexpr` and `gregexpr` without `perl`, `sub` and `gsub` without backreferences) never ask for them. Patterns regex-automata cannot compile, such as ones with backreferences, stay on fancy-regex.

## `perl = TRUE` runs on PCRE2, linked statically

**Status:** built.

- **Before.** Both regex dialects were translated into fancy-regex syntax
  (`strings::translate`, with a Perl branch). PCRE features fancy-regex lacks failed to compile:
  rcmdcheck's `(?<q>...)\g{q}`, lubridate's quantified lookahead `(?=\D+)?`, possessive and
  atomic forms, and verbs beyond `(*UCP)`. Error messages were fancy-regex's, not PCRE's.
- **Options weighed.** Extending the translator or fancy-regex would chase PCRE one construct at
  a time and never match its backtracking semantics or messages exactly. Linking PCRE2 gives
  GNU R's own engine for `perl = TRUE`, so behavior and messages are GNU R's by construction.
- **Chosen.** The `pcre2` crate, compiled as `R_pcre2_prepare` compiles (UTF mode, `CASELESS`
  for `ignore.case`, JIT when available), in `strings::compile_perl`. A compile failure warns
  `PCRE pattern compilation error` with PCRE2's message and the rest of the pattern, then errors,
  as GNU R does. `.cargo/config.toml` sets `PCRE2_SYS_STATIC=1`, so the bundled sources are built
  and a release binary needs no system PCRE2.
- **The TRE dialect stays on fancy-regex** behind the translator, which is now Extended-only.
  Moving it to a real TRE build is the same trade and is open in `backlog.md`.

- The `pcre2` crate sets `PCRE2_MATCH_INVALID_UTF` only together with `PCRE2_UCP`, which GNU R does not use. In plain UTF mode the crate's `Regex` therefore validates the whole subject on every match, and a global `gsub` becomes quadratic in the subject's length. `crates/interp/src/pcre.rs` calls the library directly and passes `PCRE2_NO_UTF_CHECK`, which is sound because every subject is a Rust `str`.

## TRE approximate regular expressions

**Status:** built.

- **Previous shape.** A TRE bound with settings (`{+}`, `{~1}`, `{2i+1d<3}`) was read by a
  separate scanner (`approximate_settings`) and either dropped or rejected as unsupported, and
  plain bounds went through `parse_interval`, which missed TRE's quirks. So `grepl("^a{+}$",
  "aaa")` was FALSE instead of TRUE.
- **Target shape.** One reader, `strings::parse_bound`, ports `tre_parse_bound`, so counts,
  settings and errors agree with GNU R by construction. It also reproduces TRE's expansion quirks:
  `{,n}` allows `n + 1` copies (`{,1}` exactly one), and an approximate bound that needs copies
  (`{2+}`, `{1,2#}`) is "Invalid contents of {}".
- A pattern with settings and no backreference compiles to `Matcher::Approximate`
  (`approximate::ApproximateRegex`), a port of `tre_tnfa_run_approx` over a position automaton.
  The translator marks each scope as a named group (`SCOPE_GROUP` plus its index), and the
  automaton turns those groups into parameter changes on transitions. With a backreference TRE
  uses its backtracking matcher, which ignores the settings, so ry does the same.
- Details that only show up against GNU R: the nesting depth is counted in `{` order and more than
  three scopes trip TRE's `value <= 3` assertion, but only when a match reaches the fourth; an
  unset cost is free inside its scope yet counts 1 toward the whole match; ties go to the earlier
  start; minimal repetition is ignored; a later search in `gsub` or `gregexpr` runs on the rest of
  the text with `REG_NOTBOL`.
- `strings__tre_bounds` pins the behavior.

## TRE minimal repetitions over alternatives

**Status:** built.

- GNU R's TRE, when no submatches are asked for (`regexpr`, `gregexpr`, `grepl`), minimizes the
  lazy repetitions over all top-level alternatives: `regexpr("a.*?Z|a.*?Y", "aXbYcXdZ")` has
  length 4. A pattern with a minimal repetition compiles to `Matcher::Automaton` with
  `shortest`, and a plain find takes the earliest end among the matches that start leftmost.
- Submatch queries (`sub`, `regexec`) keep the leftmost-first match of the whole pattern, so
  `sub("(a).*?Z|(a)(.*?)Y", ...)` takes the first alternative, as TRE does. TRE's submatch
  results for these patterns are irregular in general (`regexec` can even skip the leftmost
  match); leftmost-first agrees with it on the shapes the corpus uses.
- `connections__message_sink_and_flush` pins both.

## The message sink and connection flush points

**Status:** built.

- `sink(type = "message")` sets the connection table's `message_slot` (GNU R's `R_ErrorCon`).
  Messages, warnings and writes to `stderr()` go there through `connections::write_message`,
  which flushes after every write as `REvprintf` does. Closing that connection resets it to 2.
- GNU R flushes a file connection after `cat`, after each message-sink write, and when an output
  sink is removed, but not after `writeLines`; ry matches each point, so a second reader of the
  same file sees exactly what GNU R's would. `connections__message_sink_and_flush` pins it.
- Native code printing through `Rprintf` and `REprintf` goes through the same table
  (`libr::boundary::write_console`), so a sink diverts C output too.

## utils' sockets, internal downloads and allocation profiling

**Status:** built.

- `make.socket` and friends keep their `TcpStream`s and `TcpListener`s in one locked map in
  `crates/interp/src/utils.rs`, keyed by the OS handle, because that handle is the `socket`
  number R code sees and passes back.
- `download.file(method = "internal")` and `url(method = "internal")` read only `file://`
  URLs and give GNU R's "defunct" or "not supported" errors for the rest, as R 4.2 and later do.
- `Rprofmem` logs a vector only when its data exceeds 128 bytes, sized as GNU R counts it (data
  rounded up to 8 bytes plus a 48-byte header), because GNU R takes smaller vectors from pages of
  small cells. ry has no such pages, so it writes no `new page:` lines. The heap calls a plain
  `fn(usize) -> bool` reporter (`Heap::report_allocations`), which costs one compare per vector
  while no profile runs. The reporter returns false to end the profile (a failed write), since
  it runs while the heap is mutably borrowed and must not reach the heap itself. The bytecode compiler hides its own vectors with `Heap::swap_report_threshold`.
- `Rprof` raises intervals below 10 ms to 10 ms with GNU R's warning, since GNU R's timer cannot
  sample faster.
- `system(intern = FALSE)` and `system2` return invisibly, as in GNU R.

## One session temporary directory, removed at exit

**Status:** built.

- **Previous shape.** The interpreter's `tempdir()` made `RtmpRy<pid>` and libR's front end
  made a second `Rtmp<pid>` for native code's `R_TempDir`. Neither was ever removed, so every
  run leaked two directories into `/tmp`.
- **Target shape.** `interp::system::session_temp_dir` is the only one: a `mkdtemp`
  `RtmpXXXXXX` under the first writable `R_TMPDIR`, `TMPDIR`, `TMP` or `TEMP`, as in GNU R.
  libR passes it to native code. `session::finish` removes it, but only in the process that
  created it, so a forked `mcparallel` child that ends never deletes its parent's directory.
- **Order at exit.** `session::finish` runs the exit finalizers (the `run_exit_finalizers`
  hook, so `quit()` runs them too) and only then removes the directory, because a finalizer
  may still write there. The embedding exits (`R_CleanUp`, `Rf_endEmbeddedR`) remove it
  through `R_CleanTempDir` after their finalizers.
- **`tempdir(check = TRUE)` recreates the same path**, not a fresh name as GNU R does,
  because native code already copied it as `R_TempDir`. It is created private, and an
  existing directory at that path must belong to the current user.

## Base R kernels: radix sorting, gather subsetting, x87 accumulation and shortest-digit formatting

**Status:** built, except the radix keys computed per pass (`SortKey::get`), which "Base R kernels: position tables, a most significant digit radix order, and a per-call string memo" supersedes.

- **Previous shape:** sort and order compared elements one pair at a time, subsetting went through a per-element generic path, `rowSums`, `rowMeans`, `cumsum` and the moments accumulated in a software long double model, and doubles turned into text through the general formatter, which searched for the digit count by trial formatting.
- **Target shape:** `sort.rs` sorts integers, doubles and strings with radix and counting sorts (strings by their interned order), `subset.rs` gathers plain vectors in one typed loop, and `long_double.rs` is the one home for GNU R's long double accumulation, run on x87 instructions where the target has them: `add_all`, `add_differences` and `add_products` for sums and moments, block row sums for `rowSums` and `rowMeans`, and `running_sums` for `cumsum`. cov and cor round each column mean to double before the products, as GNU R does. `format.rs` builds a double's text straight from Rust's shortest round-trip digits when the value is normal and needs at most 15 significant digits; past 15 digits, or when zero padding would pass 15 integer digits, GNU R prints exact digits that the shortest form does not have, so those values take the general path.
- **Strings stored at once skip the root handle.** `values::mkchar` pushes a root handle that lives until the builtin returns, so a kernel that builds a long string result held one handle per element (8 MB at 1e6 rows, 16 MB while the handle vector doubled). `values::set_mkchar` and `set_mkchar_bytes` intern and store into a vector the caller keeps rooted, with no handle; use them in any per-element loop. `Heap::mk_char` reads a string's bytes once, a word at a time, for the NUL test, the ASCII test and the intern hash together (`strings::scan`), and compares a candidate of 8 to 16 bytes with two word loads instead of a `memcmp` call; allocating a new string is a separate cold function, so the hit path keeps its registers. Re-interning an 8-byte string that is already interned costs about 126 instructions, down from about 167, and the time per hit (about 30 ns with 200,000 strings) is dominated by the two cache misses on the table and the candidate. `order` computes each radix key per pass from the column rather than storing every key (`SortKey::get`), so two keys at 1e6 rows hold one `u64` per row of scratch instead of all of them, and the result is written straight into an uninitialized integer vector: the benchmark's peak went from 206 MB to 171 MB at the same speed.
- **`format` picks its digits with GNU R's own arithmetic.** `scientific` in `format.rs` mirrors `scientific()` in GNU R's `format.c`: it scales the value by a power of ten in long double (`long_double.rs`), rounds with `nearbyint` (ties to even) and drops trailing zeros, so the significant-digit count and `roundingwidens` (a value just under a power of ten that rounds up to it) agree with GNU R bit for bit; shortest round-trip digits disagree near 15 digits. Two details are not in the C source's spirit but are in its bits: GNU R's `tbl` is declared `long double` but filled with unsuffixed double literals, so `1e23` to `1e27` are the nearest doubles, not the exact powers, and scales past 10^27 call glibc's `powl`, which is not correctly rounded for 65 of the exponents in -360..340, so `GLIBC_POWERS_OF_TEN` stores glibc's results instead of computing exact powers. `scientific = TRUE` and `FALSE` set `scipen` to -99 and 310, as `do_format` does. `fuzz_format_numbers_matches_gnu_r` checks random vectors and arguments against live GNU R.
- **Impact:** results match GNU R to the last digit (the `stats__long_double_moments`, `vectors__radix_sort_and_gather`, `strings__double_to_text` and `arith__cumulative` oracle cases pin them), and the sorts, subsetting, `rowSums`, `cumsum` and `as.character(double)` run faster than GNU R. No heap layout changed: every kernel reads and writes existing vectors.

## Base R kernels: position tables, a most significant digit radix order, and a per-call string memo

**Status:** built.

- **Previous shape:** `match`, `duplicated` and `unique` hashed elements into an `FxHashMap` keyed by value, `order` with several keys sorted rows with a comparison sort over all keys, every radix key was a `u64` in 16-byte `(key, row)` pairs, a dense key always took a counting sort however wide its range, and string kernels interned each piece through the heap's session-wide string table.
- **Target shape:** `sort.rs` keeps one `PositionTable` (open addressing over 1-based `u32` positions, keys left in their vector, slot from the hash's high bits) for `match` and `duplicated`, and an integer range at most about four slots per element uses a direct-address array instead (`DenseRange`). `radix_order` sorts by the first key, then sorts each run of ties by the rest, so a later key is only sorted inside a run that sits in cache. One key sorts by a fused min, max and sortedness pass, then a counting sort when the range has fewer than about a million buckets, a pair sort under 1024 rows, and otherwise an MSD radix sort with 11-bit digits that ping-pongs between two buffers. Keys that fit 32 bits (integers, factor codes, string ranks) sort as `u32` with 8-byte pairs; only doubles need `u64`. `values::StringMemo` fronts the heap's string table for one vectorized call (`substr`, `formatC`), and switches itself off after a window of 65,536 lookups with fewer than one hit in eight. `formatC(format = "f")` formats a value whose scaled magnitude is below 2^52 with `fixed_fast` in `format.rs`: an `fma` gives the exact error of `value * 10^digits`, so the half-to-even rounding glibc applies is decided exactly, and only larger values take Rust's arbitrary-precision formatter (about 1,100 instructions per value). That took the kernel from 6.4 s to 2.7 s at 1e7 rows (GNU R 5.6 s); a unit test cross-checks it against the exact formatter.
- **Why the memo is per call:** at 1e7 strings the heap's table misses cache on every probe, while the distinct pieces one call produces (a code inside ids, a rounded number) fit in cache. `substr` at 1e7 fell from 2.3 s to 1.0 s (GNU R 1.8 s). A faster session-wide table would make the memo unnecessary; that is the heap's to decide.
- **Why the counting sort stops at a million buckets:** past that, its count and scatter passes miss cache on every row, and on a 1e7 permutation the radix sort's sequential scatters win (0.34 s against 0.43 s).
- **Impact:** the `vectors__radix_paths` oracle case pins the radix and match paths, including integer `NA` at either end in either direction. No heap layout changed.

## Base kernels pick their data structure by the value range

**Status:** built.

- **Previous shape.** `as.character()` on integers formatted and interned every element, the cumulative functions zeroed their result before filling it, `strsplit()` probed the session-wide string table for every piece, and `substr()` recycled `start` and `stop` with a division per element. Each was 1.2x to 2.7x GNU R's time.
- **Shape.** `as.character()` on integers (`coerce.rs::integers_as_strings`) interns each distinct value once when the values span a small range, which `factor()`, `table()` and `cut()` inherit. `cumsum` and friends allocate with `values::alloc_uninit`, and real `cummax`/`cummin` run a branch-light loop until the first NaN. `strsplit()` interns ASCII pieces through the call's `values::StringMemo`, and `substr()` recycles by counters. Each falls back to the general path when the range is wide, so no input is slower than before.
- **Impact** (`tools/bench/internal/base`, ry time over GNU R's): at 1e6 rows `table` 0.28 (one factor) and 0.36 (two), `cut` 0.77; at 1e7 rows `cumsum` 0.90, `cummax` 1.00, `strsplit` 0.90, `substr` 0.79.

## `as.character` of a long number vector is deferred

**Status:** built.

- **Shape.** As in GNU R, `as.character` of an integer or double vector of at least 256 elements without attributes returns a deferred string vector (libr's `deferred_string` ALTREP class through the `defer_strings` hook), holding the source and the `scipen` in force at the call. Reading one element formats only that element; materializing with no element read formats the whole vector in one batch, as the eager path would, and then drops the source. Subsetting reads elements without materializing.
- **Why it fits.** ALTREP is already a first-class part of the object model, so no new object kind was needed. Below 256 elements the deferred vector's own allocations cost more than converting.

## `format()` of numbers streams into the result

**Status:** built.

- `do_format` formats logical, integer and double vectors element by element straight into the result's `CHARSXP`s once the common width is known (`format_numbers` in `crates/interp/src/format.rs`), instead of building a `Vec<String>` of every element first, which held about 40 MB of Rust strings at 1e6 elements. Complex numbers and doubles whose items differ in width fall back to the general path.

## The methods package's core runs natively behind hash-guarded prologues

**Status:** built, except the native class table with dense class ids, which the entry names as the next step.

- **Previous shape.** Every S4 operation ran GNU R's methods R code on ry's interpreter, with only
  the C routines (`methods.rs`) native. A cold first `new()` on the generated S4 package
  (2,892 definitions) spent about 7.7 s running definitions, and `new()` itself cost about 180 µs
  per call against GNU R's 130 µs.
- **Target shape.** The methods package's core is Rust (`crates/interp/src/methods_core.rs`), and
  the package's R code and metadata objects stay as they are: classRepresentation objects, the
  `.__C__` and `.__T__` tables, generic objects, `MethodDefinition`, `.SigLength` and the method
  tables. Those objects remain the single source of truth, and the core reads and writes them
  directly, so there is no native mirror to drift and saved RDS objects, `methods:::` callers,
  Reference Classes and the S4 cache keep working.
- **Mechanism.** `R_initMethodDispatch` (methods' `.onLoad`) calls `install_fast_paths`, which, for
  each function named in `methods_core.R`, compares a stable hash of the formals and body with
  `FAST_PATHS` and on a match rewrites the body to `{ prologue; original body }`. The prologue calls
  an `.Internal` that either does the whole job and returns, or returns without observable effect.
  A hash mismatch (another GNU R version) leaves the function as it is, so the core can never
  apply to code it was not written against. Patching the closure in place, instead of replacing the
  binding, reaches every copy that holds the same closure. The default `initialize` method has
  several separate copies, including one per class that inherited dispatch cached in `.AllMTable`,
  so every entry of the generic's tables is hashed and the matching ones are patched. A preloaded
  base image carries the patched closures.
- **Exactness rules.** A fast path reads promises only when forcing them runs no code
  (`quiet_value`), bails before any effect wherever R code could observe the work (validity
  functions, `as()` caching coerce methods, user `initialize` methods, `.S3Class` slots, a data
  part not set through the standard replacement method already cached in `coerce<-`'s table, list
  entries in the class table), and evaluates the calls that may signal in the R
  function's frame, so condition calls and `sys.calls()` match GNU R. Known difference: when a fast
  path completes, the R function's frame-local temporaries are not bound; only code that inspects
  that frame from outside (a debugger, `sys.frames()` from a validity method the fast path never
  calls) could see it.
- **Versions.** `FAST_PATHS` lists the hashes each function may have, one per `methods` version the
  fast path was checked against; the bundled GNU R 4.6.2 is the primary target. A miss on a
  methods package whose version is the bundled one is an error at start-up, because it means the
  base changed under the core; a miss on any other version keeps the R code. The R expressions the
  native definitions fill in (coerce and replace methods) are R text in `METHODS_TEMPLATES` in
  `methods_core.rs`, parsed once at install time.
- **Definitions.** `setGeneric`, `setMethod` and `setClass` are native for the shapes a package
  usually writes: local supers only, no `setClassUnion` member classes passed as a
  classRepresentation, a `validity` that is `NULL` or a function of `object`. Anything else bails
  to the R code before any effect. The native results are byte-identical to the R path (classes,
  generics, method tables and `serialize()` output, compared on the generated S4 packages, the
  Matrix sources and the methods package's own tests). Adding a direct subclass rebuilds the
  superclass's whole subclass list as `completeSubclasses()` does, regenerating every indirect
  extension (whose `by` and `to` carry the package attribute differently from the first time) and
  repeating a class reached through two direct subclasses; appending would differ. One benign
  exception to bailing before any effect: `setClass` evaluates `tryNew()` for slot prototypes
  before its last checks, which fills the same `initialize` dispatch cache the R code fills.
- **Switches.** `RY_METHODS_NATIVE=0` turns the definition fast paths off (the differential uses
  it), `RY_TRACE_METHODS_BAILS=1` prints why each call bailed, and `RY_PRINT_FAST_PATH_HASHES=1`
  prints the hashes of the installed functions, for adding a version.
- **The S4 cache is deleted** (AGENTS.md, "Caching is a sign of a slow design"). Per-generic
  units plus the native definitions bring a cold first `new()` on `target/s4heavy` to 80 to 87 ms
  (one run in five about 103 ms from machine noise), against 0.36 to 0.40 s with a cache miss
  before.
- **A promise stays a root while it is forced** (`force_promise` in `interp/src/eval.rs`).
  Before, only the bindings that held a promise kept it alive. A loader S4 placeholder removes
  its own binding while it runs, so a collection mid-force freed the promise and its environment
  (debug smallvec precondition panics, "object not found"). The promise is pushed on the handle
  stack for the duration of the force, as GNU R's `R_PendingPromises` keeps it, which costs one
  handle push per force and makes self-replacing promises safe for any caller.
- **`setMethod` creates the implicit generic of a base S3 generic natively** (`implicit_generic`
  in `methods_core.rs`). `setMethod("as.list", ...)` in a package namespace on a base closure
  whose body calls `UseMethod` runs `setGeneric(f, where)`, which builds the generic with
  `makeGeneric`, a `derivedDefaultMethod` of the base function, `.MTable`/`.AllMTable` holding
  `ANY`, the entries in `.genericTable` and `methods:::.implicitTable`, and `.__T__f:base` in
  the namespace, without the message R prints for a non-S3 function. The native path writes the
  same objects (the differential compares `serialize()` output) and bails when `.BasicFunsList`,
  `.__IG__table` or `.__IGM__table` would supply the generic, when either table already holds
  the name, or outside a namespace. It removed 25 ms (`as.list`, `print`, `toString`) of the
  cold run.
- **The rest of the gap goes to a native class table, not more ported functions.** With
  `setMethod` on primitives, inherited single-argument dispatch and most
  `setClass` and `setGeneric` shapes native, a cold run of the 2,892 definitions takes about 145 ms and
  only about 24 calls still bail, yet that is about 670M instructions against the roughly 250M a
  sub-100 ms cold `new()` allows. The profile is flat: environment lookups of `.__C__` and
  `.__T__` bindings, slot reads of class representations, `substitute`-built extension objects and
  allocation, spread over every definition. Porting more functions one by one cannot remove that,
  because every ported function still reads and writes the R-level metadata objects. The next
  step is a native class table with dense class ids, a bitset extends relation and method tables
  keyed by id, which materializes the R objects (`classRepresentation`, `.MTable` environments)
  only when R code reads them.
- **Per-generic units came before the class table, because they removed more for less.** Running
  every definition cost about 670M instructions, and a large part of that was `setGeneric` and
  `setMethod` for package generics that nothing calls at start-up, so not running them beat running
  them faster. What remains is unit 0 (about 416M instructions on `target/s4heavy`, with the
  implicit generics native): `setClass` 29% (`try_new`,
  class-definition lookups, `combine_extensions`, `direct_extension`), lazy-load unserialization
  of `methods` objects 12%, the collector 10%, symbol interning 8%, then dispatch,
  `checkAtAssignment` and `setClassUnion`. The class table is still the shape for that `setClass`
  share. The R-level metadata (`.classTable`, `.genericTable`, `classRepresentation` objects)
  becomes views produced from it on demand, so R code that reads them sees GNU R's objects
  while native definitions and dispatch never build them.
- **Unions and `cacheMetaData` replicate GNU R's quirks, not its intent.** The native
  `setClassUnion` and `cacheMetaData` must leave state that `serialize()`s identically, so they
  copy behavior that looks like a bug: `.recacheSubclasses` tests `match(what, names(subDef@contains))`
  with the subclass's own name, which is always `NA`, so every subclass of a union member gets its
  union extension overwritten in place, re-sorted by distance and recached; `cacheMetaData` drops
  duplicate generic names but not their packages, so the packages misalign after the first
  duplicate; classes are visited in unsorted frame order and generics in sorted order; and
  `completeExtends` resolves a class reached twice with `.resolveSuperclasses`' first consistent
  candidate (`resolved_superclasses` in `crates/interp/src/methods_core.rs`). `.S3Extends` is not
  ported, so a union member whose indirect extensions it would rewrite bails.
- **The first union of a session fills `initialize`'s dispatch cache itself.** The R code's
  `new("ClassUnionRepresentation", ...)` dispatches `initialize` and caches the inherited default
  method (target `ClassUnionRepresentation`, defined `ANY`) in the generic's all-methods table, so
  a native union must leave that entry too. Before the first union no entry exists, so the native
  path caches the inherited method the way dispatch would (`union_initialize` in
  `crates/interp/src/methods_core.rs`), unless first-unit definitions are still pending: those may
  add an `initialize` method that dispatch would prefer, and that case still runs the R code. The
  native path accepts only the default `initialize` (`is_default_initialize`), for the union and
  for `classRepresentation`, `SClassExtension` and `classPrototypeDef` (`definition_caches`): a
  package may define `initialize` for any of them, and GNU R's `new()` of the definition calls it,
  so a cached package method sends `setClass` and `setClassUnion` to the R code.
- **`setClass` does `setIs`'s subclass bookkeeping natively, by GNU R's quirks.** Only the
  direct superclasses go through `setIs`; `completeSubclasses` then records the new class in each
  ancestor only when `possibleExtends(new, ancestor)` is false, so a union that already lists the
  new class or any direct superclass among its subclasses is skipped (a later subclass of a union
  member is not added to the union). A class whose ancestor is a union member follows GNU R's
  order exactly: ancestors are recorded first (a union already related to the class is skipped,
  any other union re-caches its subclasses' `contains` right after, resorting them by distance and
  dropping their `conflicts` attribute as R's sort does), and only then the direct superclasses,
  where the first one is recorded on the definition read before those steps and later ones on a
  fresh read (`record_subclasses`, `recache_subclasses`). Recaching bails when a union member is
  not cached yet or has a non-numeric distance (`recachable`). The class cache and the namespace binding of a superclass
  may legitimately differ after `setClassUnion` (GNU R orders the extensions differently in each),
  and `getClassDef` reads the cache, so `local_class` takes the cached definition and only
  requires a binding to exist. `prototype()` objects reduce to the list `do.call("prototype",
  list)` would wrap (`prototype_def_list`). A slot default whose class has a methods-package
  `initialize` (`environment`) runs `new()`, which cannot fail, unlike a package's method, where
  `tryNew` would fall back to the prototype.
- **`setMethod` builds the `.local` wrapper natively** (`conform_definition`), with the result
  `rematchDefinition` gives: the generic's formals, `.local <- definition; .local(args)`, args
  after `...` and from the first `missing` signature position passed by name, and the method's
  defaults kept unless they are a symbol or missing (`.copyMethodDefaults` then `matchDefaults`).
  A method that only omits the generic's `...` gets the generic's formals in place, with its own
  body and environment, as `conformMethod` and `rematchDefinition` leave it. An omitted argument
  bails only when the signature names it (R then fills `missing` into the signature), and a renamed
  argument becomes a `.local` wrapper. A different order, or added formals with no `...` in the
  generic, bail.
- **`setGeneric` accepts a braced body.** `function(x) { standardGeneric("f") }` gives a
  `nonstandardGenericFunction`, as GNU R's `.NonstandardGenericTest` does; any other body bails.
- **Every fallback says why.** `bail!(skip, "reason")` records the reason and line, and
  `report_fallback` prints them under `RY_LOAD_VERBOSE` for the four definition functions, so a
  slow load names the definitions to fix without a profiler (`reference/package-loading.md`).
- **`cacheMetaData` does the cheap cases and delegates the rest per item.** A class whose bound
  definition is already the cached one (and, for a union, whose members' cached extensions match)
  needs nothing; a package's own standard generic gets its tables merged natively. Anything else
  runs GNU R's own loop body for that one item (`Template::CacheClassItem` and
  `CacheGenericItem`), so the order of effects matches and a partial port is safe. On
  `target/s4plain` this took `cacheMetaData` from 104 to 16 ms.
- **A foreign generic with no methods in the namespace is reset natively.** `setOldClass()` with
  a prototype leaves `.requireCachedGenerics` asking for `Ops`, so nearly every package (rlang
  among them) caches a base group generic it defines no methods for. For such a generic,
  `cache_foreign_generic` walks the group tree as `.updateMethodsInTable` does, resets each
  `.AllMTable` to its direct methods and re-sets the primitives' dispatch tables. It bails before
  any effect on a group slot, a global binding, a method table in the namespace or a default that
  is not a primitive. Group members that are primitives (`&`, `+`) take their generic from the
  generic table, as `getGeneric()` does. Lazy-loaded namespaces bind `.packageName`,
  `.requireCachedGenerics` and class definitions to promises, which `forced_value` forces as
  `get()` does. This took `cacheMetaData` of rlang from 19% to 2% of a warm load
  (`s4__cache_metadata_shapes.R` pins the shapes).
- **Order.** Object creation and class queries came first because they are the run-time hot path
  (`new()` 13.7 µs, `validObject` 3 µs, `is()` 2.3 µs against GNU R's 130, 73 and 11.8 µs). Then
  the definition functions, then `setClassUnion` and `cacheMetaData`; `as()`, `setDataPart()` and
  inherited dispatch remain (`backlog.md`, "Open: native methods core").
- **Impact.** Cold first `new()` on the bench package fell from 8.6 s to 6.3 s with the native
  queries, and native definitions bring the generated `target/s4plain` package (2,892 definitions)
  from about 2.2 s of S4 work to about 0.9 s. `crates/oracle/cases/s4__native_core_queries.R` and
  `s4__native_definitions.R` pin the covered functions, including their error messages and calls,
  against GNU R.

- A fast path evaluates a call that may signal in the R function's frame, so a handler sees the same call stack as under the R body. A signature argument that fails to evaluate is reported against `h(simpleError(msg, call))`, as the R dispatch code does.
- A fast path looks a generic up in `.genericTable`, because the generic's namespace binding may be an unforced promise.
- `R_set_slot` modifies its argument in place, `.allowPrimitiveMethods(FALSE)` turns primitive dispatch off (method selection recurses otherwise), and dispatch treats an argument as `"missing"` only when its own binding is missing, not when a caller passed a missing argument on.

## Numeric routines are exact ports of GNU R 4.6.2, and LINPACK lives in the interpreter crate

**Status:** built.

- **Previous state.** nmath, smooth.spline, lowess and loess used textbook formulas or earlier algorithms and differed from GNU R in the last bits (1468 of 12210 distribution grid points, tens of ulps in loess). LINPACK's `dnrm2`, `drotg` and `dsvdc` followed older reference versions and sat in `libr`.
- **Decision.** Each routine is ported from the 4.6.2 source with its evaluation order, including `dnrm2` and `drotg` from LAPACK 3.10 and R's modified `dsvdc` test. `linpack.rs` moved to `crates/interp`, which `libr` re-exports, so the interpreter's stats code and the C API share one copy.
- **Impact.** The grid sweep has 0 mismatches of 12210, a random sweep 0 of 2400, and the loess, smooth.spline and lowess matrices are identical to GNU R. Consumers of the shared routines (such as `lm` through `dqrdc2`) move toward GNU R as well.

## Hot base R closures get hash-guarded native prologues

**Status:** built.

- **Previous shape.** `match.arg`, `stopifnot`, `setdiff`, `structure` and `format.default` ran their R bodies. Validation code calls them at every boundary, and each body spends its time in nested closure calls: `match.arg` in `sys.function`, `formals` and `eval`; `setdiff` in R 4.6.2's `.set_ops_need_as_vector`, which runs two `tryCatch` calls (17 µs of `setdiff`'s 24 µs); `format.default` in `match.arg` and `prettyNum`. The bench-real `validation` workload ran at 0.58 to 0.65 times GNU R.
- **Target shape.** `crates/interp/src/base_fast_paths.rs` puts `.Internal(ryX())` in front of each body when base loads, with the methods core's mechanism: a stable hash of formals and body guards the install (a miss is an error on the bundled R version and a silent skip on any other), the closure is patched in place, and the start-up image carries the patched closures. A prologue either finishes the call by returning from the closure's frame or returns NULL and lets the body run. It forces only what the body forces, in the body's order, so a declined call sees the same promises. Each fast path covers the common case only (a single ASCII string for `match.arg`, plain atomic vectors for `setdiff`, no marks or `justify` for `format.default`, plain attributes and a non-factor character class for `structure`, all-TRUE logicals for `stopifnot`) and declines everything else, so errors, warnings and messages always come from the R body.
- **Nested closures get prologues too** (`NESTED_FAST_PATHS`). `loadNamespace()` defines `assignNativeRoutines` in its own body, so there is no binding to patch: the prologue goes into the body of the `function(...)` expression inside `loadNamespace`'s body, found by its `name <- function` assignment and guarded by the same hash, and every closure made from it carries it. `ryAssignNativeRoutines` binds all registered routines at once instead of growing two vectors with `c()` per routine, which cost rlang's load about a tenth (12% to 2.5% of a warm load). It declines on the warning cases and on `symbolNames`. `namespaces__native_routines.R` pins the bindings and the `nativeRoutines` namespace info.
- **`readRDS(path)` is a native entry, with no uncompressed copy.** The R body reads through a `gzfile()` connection that feeds the unserializer a few bytes at a time, three times the cost of unserializing the same bytes from memory. The entry reads the file whole, inflates gzip in one pass and unserializes the slice (rlang's `nsInfo.rds` 82 to 48 µs, its 15 KB `.rdx` 220 to 190 µs). What remains is inflation, about half of an `.rdx` read; an uncompressed form would remove it, but it means a second file per database or a non-GNU `saveRDS` at install, so it is not done. The `zlib-rs` backend for flate2 inflated no faster than `miniz_oxide`. The entry declines for `~` paths, unreadable files, xz and bzip2, streams without an `X`, `B` or `A` header (the slice reader also takes `save()` files) and any unserialize error, so the R body reports those (`io__read_rds_formats.R`).
- **`match.arg` with `choices` missing** evaluates the caller's default only when it is a character constant or `c()` of character constants, and binds `choices` in the frame as the body does, so a declined call does not evaluate it twice.
- **Bugs fixed on the way.** The `format` internal checks `trim`, `digits` (1 to 22), `nsmall` (0 to 20), `width`, `justify`, `na.encode`, `scientific` (length 1, logical or numeric) and `decimal.mark` (one string) in the order of GNU R's `do_format`, and reports them against the calling closure. It does not yet warn "NAs introduced by coercion" for a string `digits` or `width`. `attributes<-` and `attr<-` reject a non-character class, including an empty one, and a `"factor"` class on a non-integer object (GNU R's `classgets`); a numeric class used to be stored and crashed `print`.
- **Defaulted, not missing.** A fast path that leaves an argument unforced checks that the cell holds the default the call bound (`gp::BINDING_MISSING`), not `missing()`: a wrapper that passes on its own missing formal makes `missing()` TRUE, but the R body forcing it raises an error.
- **Known difference.** The prologue is visible: `body(match.arg)[[2]]` is `.Internal(ryMatchArg())`, and printing, `deparse()` or digesting the patched functions differs from GNU R.
- **`Reduce`** (`ryReduce`) covers `accumulate = FALSE` over a plain vector with a closure or builtin `f`, both directions, `init` given or not. It binds `mis`, `len`, `f`, `init` and `i` in Reduce's frame as the body does, and calls `f` through `eval::call_with_values`: forced promises of the body's own expressions `init` and `x[[i]]`, so `sys.call()` and `substitute()` inside `f` match. `right` and `accumulate` are forced in the body's order, which depends on whether `init` is missing.
- **`deparse` and `.deparseOpts`** (`ryDeparse`, `ryDeparseOpts`). testthat deparses a call for every expectation label, and each R call spent most of its time on its defaults rather than on deparsing: `backtick` runs `mode()` and `%in%`, and `.deparseOpts` runs `pmatch`, `unique` and `sum` over its option table. The prologue forces `expr`, `width.cutoff`, `backtick` (computed from the type of `expr` when defaulted), `control` and `nlines` in the body's order, folds `control` into the option bits and calls the `deparse` internal in deparse's frame, so a width warning carries the R body's call. Options must be spelled out in full: an abbreviation, a duplicate, `NA`, a non-string, `"all"` with `"exact"`, or `"hexNumeric"` with `"digits17"` declines, and the R body does the partial matching or reports the error (`deparse__fast_path.R`). Measured: 9.1 to 1.4 µs per call (GNU R 12.8 µs), and the testthat workload (`RY_BENCH_REPEAT=1`, callgrind) 1.687G to 1.554G instructions. `match`, `%in%`, `stopifnot`, `match.arg`, `intersect`, `ls` and `pmatch` were each under 1% of the same profile and were left alone.
- **`formatC` and `pmax`** (`ryFormatC`, `ryPmax`). `formatC` was 77% of the report workload and 67% of strings (inclusive), almost all of it in the R body's argument handling and, with `big.mark`, in `prettyNum()`'s regex surgery. The prologue covers a plain integer or double `x` with `mode` NULL, a covered `format` (d, f, e, E, g, G, fg) and a flag of `0+- #'I`. It forces the arguments in the body's order, does the body's NA, NaN and Inf substitution and default-digits arithmetic, and calls the `formatC` internal. An ASCII `big.mark` other than `"."` with the default small mark, decimal mark, `zero.print` and `drop0trailing` is inserted natively, which matches `prettyNum()`'s reversed `([0-9]{k})\B` substitution. Anything else declines: attributes, `mode`, `format = "s"`, digits that the body would reduce with a warning, a value `format = "d"` would turn into NA, `preserve.width = "common"` and other marks. `pmax` takes plain atomic arguments whose first carries no attributes and declines on fractional recycling. Writing the oracle case (`base__format_c_fast_path.R`) found older bugs in the shared kernels, which the R bodies call too: an `fg` zero printed with its flag and width, zero padding went in front of a `+` or space sign, logical and complex `x` were formatted instead of rejected, and `pmax` kept the first NA instead of the last NA or NaN, typed an empty result by its first argument, failed on `NULL`, accepted an NA `na.rm` and never warned on fractional recycling. The `'` and `I` flags are left out of the case because GNU R's output for them depends on the C library's locale. Measured per call against GNU R: `formatC(42L, width = 6L, flag = "0")` 13.2 to 1.4 µs (GNU R 14 µs), with `big.mark = ","` 76 to 2 µs (GNU R 106 µs), `pmax(3L, 2L)` 4.2 to 0.6 µs (GNU R 4.6 µs). The report workload (`RY_BENCH_REPEAT=1`, callgrind) went from 5.30G to 1.61G instructions and strings from 5.32G to 2.12G.
- **`formatC`'s prologue calls the internal's body with Rust values.** It used to build `mode`, `width`, `digits` and `format` as R scalars on every call (interning `"double"` and `"g"` each time) and send them through `kernel()`, whose builtin parsed them back; `format::format_c_vector` is now the body both the `formatC` internal and the prologue call. While `decimal.mark` still holds its own unforced default promise, the prologue reads the `OutDec` option with `envir::option_value`, as `getOption()` does, instead of forcing the promise into a `getOption()` call: base's closure fixes the promise's code, `getOption()` has no side effect, and the prologue returns before the body could force it. `string_scalar` borrows the interned text instead of copying it. On report (release, `RY_BENCH_REPEAT=5`) this took 2.98G to 2.63G instructions (-11.7%), about 2,900 fewer per scalar `formatC()` call. `base__format_c_out_dec.R` pins `options(OutDec = ",")` with and without a big mark.
- **Measurement.** `validation` (bench-real harness, 3 processes): GNU R 4.3.3 0.660 s, ry 0.353 s with the JIT on and 0.373 s with it off (1.87x and 1.77x; it was 0.58 to 0.65x). Callgrind, JIT off, validation(1000) plus a warm-up run of 300, start-up included: 2.71G instructions before, 1.32G after (GNU R 3.16G). Per call: `format(2.5, nsmall = 3)` 4.7 µs (GNU R 13.9 µs, ry before 18 µs).
- **`tryCatch`, `withRestarts` and restart invocation** (`ryTryCatch`, `ryTryCatchList`, `ryDoTryCatch`, `ryWithRestarts`, `ryWithOneRestart`, `ryDoWithOneRestart`, `ryTryInvokeRestart`, `ryInvokeRestart`). The prologues run the R bodies' statements natively but keep every frame the R bodies make: `tryCatch` still calls `tryCatchList`, `tryCatchOne` and `doTryCatch`, and `withRestarts` still calls `withOneRestart` and `doWithOneRestart`. Only the bookkeeping between those calls is native (forcing the handler dots, building `handlers`, `classes` and `parentenv`, building restart objects without `makeRestart`'s `do.call`), so `sys.calls()`, `sys.function()`, `sys.parents()`, `parent.frame()`, `conditionCall()` and the frame variables a handler can inspect match GNU R. A frame that GNU R shows cannot be skipped, which is why the native code stops short of folding the nested calls together. Each inner prologue defines the inner closures (`tryCatchList`, `tryCatchOne`, and so on) in its frame as the body does, through `bytecode::inner_closure`, which gives every closure made from one `function` expression the compiled body stored in the outer function's constants, as `op_closure` does. Without it each new closure compiled its body again at its second call, and `tryCatch(1, error = h)` took 19 µs instead of 3.7. `tryCatchList` covers one handler; two or more decline to the R body. `withRestarts` declines unless each spec is a `function` literal or a string, and `invokeRestart`/`tryInvokeRestart` take a name only (a restart object, `"abort"`, or a missing name with `invokeRestart` decline). Hashes of nested functions are taken with the nested prologues stripped (`without_nested_prologues`), because the start-up image saves the patched bodies. `.signalSimpleWarning` and `.handleSimpleError` are called from the base environment, as GNU R does (`errors.c`), so `sys.parents()` gives their frame its own number. `conditions__try_catch_fast_path.R` pins frames inside the expression and the handler, `conditionCall`, handler order with `withCallingHandlers`, `finally` and `on.exit` on a normal exit, an error, `return()`, `break` and a restart, and the muffle restarts under `suppressWarnings` and `suppressMessages`. Measured per call against GNU R 4.6.2: `tryCatch(1, error = h)` 3.7 µs (GNU R 4.1, before 4.5), `tryCatch(stop('x'), error = h)` 12.3 (12.3), `withRestarts(1, r = h)` 8.8 (7.5), `suppressWarnings(as.integer('x'))` 11.2 (19.3, before 19.5), `suppressWarnings(warning('w'))` 11.6 (26.1, before 21.9); two handlers stay at 8.2 (6.3). Callgrind (`RY_BENCH_REPEAT=1`): `conditions` 5.68G to 4.95G instructions, `team_scores` 6.56G to 5.92G.
- **Next candidates** from the same profile: `utils::modifyList` (needs an install hook when utils loads), `sprintf`, `vapply`, and `tryCatchList` with several handlers.

## Builtin fast paths take names as CHARSXPs and evaluate nothing twice

**Status:** built, except the `c()` flattener's `LeafName`, which "Builtins with positional direct entries, one implementation per semantic" supersedes.

- **Previous shape.** `attributes()` built its names as Rust `String`s and interned each again through `mkChar`; `c()` read every element through `vector_element` and `coerce_vector`, and its flattener built every leaf name as a `String`. `forceAndCall` built an argument vector and a fresh pairlist per call. `lapply` and `vapply` with a builtin `FUN` (`vapply(rows, `[[`, 0, "fee")`) read the element natively for a closure only, and for a builtin evaluated `X[[i]]` again through the AST evaluator. The compiled `[[` read (`Op::Item`) served records only, and the in-place element assignment took numeric constant subscripts only, so `x[["fee"]] <- v` always went through `complex_assign`'s replacement calls.
- **Chosen shape.**
  - Names stay CHARSXPs end to end: `do_attributes` collects (print name, value) pairs on the stack; the `c()` flattener keeps a leaf name as `LeafName::Char` (the element's own CHARSXP) and builds a `String` only when it pastes a prefix (`subset.rs`).
  - `c()` widens integer and logical runs into double and integer with a direct loop (`int_to_real`, NA to NA).
  - `forceAndCall` reuses the call's cells after `n` and `FUN` when the special received them unchanged (`call_tail`), and passes variables eagerly to an effect-free closure when no argument is a call (`no_calls`), with the same conditions as a compiled call site.
  - The apply family's known element reaches a builtin `FUN` directly, and a special that evaluates its arguments through `dispatch_evaluated_subject`, so `[[` methods still dispatch on the element.
  - `cached_item` (`records.rs`) reads a classless list that is neither ALTREP nor an Array by position or by exact name; an out-of-range position falls back to `[[` for its error.
  - A one-string constant is a side-effect-free subscript at the top level of an element assignment; the in-place assignment rejects `""` and `NA`, which match no name.
- **Why not a per-builtin cache.** Every gain comes from not doing work (no interning, no pairlist, no second evaluation), so nothing has to be invalidated.
- **Impact.** Instructions per call (JIT off, callgrind, GNU R 4.3.3 in parentheses): `attributes` on an xts object 4,183 to 1,786 (2,692); `c(1:3, 4.5)` 4,262 to 2,131 (2,731); `x[[2L]]` 1,150 to 698 (709); `x[["net"]] <- 3` 4,910 to 987 (2,011); `forceAndCall(2, f, x, y)` 5,520 to 4,232 (5,198); `Reduce(f, x)` over 100 elements 748.7k to 423.1k (688.8k). `crates/oracle/cases/builtins__fast_paths.R` pins the edge cases.
- **Names the runtime asks for by text are interned once.** `typeof` answers from `symbols().type_names`, one CHARSXP per `SEXPTYPE` code filled at start; internal dispatch on `$`, `@`, `$<-`, `@<-` and `seq` passes the generic's symbol from `symbols()` instead of installing it per call; `lapply`/`vapply` bind `X`, `FUN` and `i` through the same table; and a replacement call shares one tail pairlist between its getter and setter calls. `options(c("a", "b"))` reads only `a`, as GNU R does, and `options(character())` is an "invalid argument" error. Pinned by `crates/oracle/cases/strings__interned_names.R`.
- **`lapply` and `vapply` share `ElementCalls`** (`crates/interp/src/envir.rs`), which writes `i` in place and reads `X[[i]]` directly. It rebinds a fresh `i` whenever the bound integer's reference count moved (`bound_refcount`), because `FUN` may have captured it: a promise or closure that reads `i` later must see its own value. `interp::testing::check_element_access_agreement` fuzzes the direct entries against the AST evaluator, which takes the full builtins.
- **Not yet done.** The closure call inside `vapply` and `lapply` still allocates `i` and a promise per element; `$<-` and `[[<-` with a new name still take `complex_assign`; `[<-` with a scalar costs 813 instructions against GNU R's 497, almost all of it tier-0 guards and frame lookups.

## One-argument builtins and small base closures skip dispatch when their arguments are values

**Status:** built.

- **Previous shape.** A compiled call to `!`, `length`, `is.null`, `is.na`, `is.character`, `is.logical` or `is.function` built an argument vector, went through `call_builtin`'s dispatch and `run_builtin`'s scope and visibility handling, and `!` allocated a fresh logical. A call to `identical`, `isTRUE`, `nrow`, `match`, `%in%` or `%||%` built a closure frame and promises and ran the R body, which calls `.Internal` or `length` again.
- **Chosen shape.**
  - `BuiltinDef::unary` (`builtins.rs`) is an optional `fn(Sexp) -> R<Option<Sexp>>`. `op_call_builtin` (`bytecode.rs`) calls it with the operand register when the call has exactly one positional, non-missing argument. It returns `None` for anything that is not plain (an object, ALTREP, attributes other than none, a ry native value), so S3 dispatch, names and the general error messages stay on the general path. Results are the shared immutable `TRUE`, `FALSE` and `NA` from `values::scalar_logical`, so modifying one copies it, as in GNU R.
  - Native entries (`base_fast_paths.rs`, `NATIVE_ENTRIES`) are keyed by the closure's identity in `Interp::native_entries` and filtered by `gp::NATIVE_ENTRY`; both are rebuilt on an image start because the registry is not in the image. `apply_closure` asks the entry before it builds a frame. The entry declines (and the R body runs) when the closure is traced, debugged or stepped, when an argument is named, or when an argument is an unforced promise of a call; a promise of a variable or a constant is settled with the value it would have produced, so a later `substitute` or `missing` sees the same promise state.
  - `match` and `%in%` scan in place for one element of the table's type (strings by pointer when the element is ASCII or `NA`) and otherwise call the `match` kernel directly.
- **Why a field and not new tier-0 ops.** The JIT matches exhaustively on tier-0 ops, so a new op would change `crates/jit`. A field on the builtin keeps the compiled code unchanged and gives the baseline tier one function to call.
- **Impact.** Per call (JIT off, callgrind, GNU R 4.3.3 in parentheses): `!x` 944 to 563 (750), `is.na(x)` 1,128 to 641 (1,490), `identical` 8,545 to 1,818 (3,149), `isTRUE` 6,087 to 1,114 (5,594), `match` 6,869 to 1,905 (2,963), `%in%` 11,327 to 1,745 (8,226). `crates/oracle/cases/builtins__native_entries.R` pins the edges.
- **Not yet done.** `missing` is a special and still costs about twice GNU R's instructions.

## Builtins with positional direct entries, one implementation per semantic

**Status:** built, except the fused element op and `element_access::read_element` under "What is left", which "Tier-0 element reads and stores as fast handlers, not new ops" supersedes.

- **Previous shape.** A compiled call of a builtin with more than one argument always built `eval::Args`, ran `call_builtin`'s checks (arity, a name check that compares strings when any argument is tagged, the replacement copy), opened a handle scope, built a `Call` context and set visibility, even when the builtin's work was a pointer read (`attr(x, "dim")`, `x[[i]]`). Only one-argument builtins had a way around it (`BuiltinDef::unary`). Replacement calls from `Op::Replace` (`attr(x, "a") <- v`) carry the tag `value`, so they always took the checked path with its string compares, and `do_attr` and `do_set_attr` matched their formals by name and interned the attribute name per call. Several semantics had two implementations that disagreed: `attr()`'s partial matching ignored GNU R's special case for `names` and returned compact row names unexpanded on a partial match, and `inherits` was answered by `do_inherits` (byte compares on a `Vec<&[u8]>`) and by `values::inherits` (a string compare per class).
- **Chosen shape.**
  - `BuiltinDef::direct: Option<DirectFn>`, with `DirectFn = unsafe fn(call: Sexp, &[Sexp]) -> R<Option<Sexp>>`, takes the call (for error messages and native context) and the argument values in the primitive's formal order. `op_call_builtin` tries it when no argument is tagged or empty, and `replace_variable` tries it for a replacement call whose only tag is the trailing `value`. `None` declines to the full call; a direct entry declines everything the full call treats differently (an object that would dispatch, ALTREP, a ry native value, an argument whose error message the full call words). The caller sets visibility, records feedback and releases the entry's handles; it skips the entry while a profile or activity trace is being taken, so samples still name the builtin. `unary` remains the one-operand form that reads its register without a slice.
  - Each fast path is the implementation, not a copy beside it: the full builtin calls the same function after matching its arguments. `attribute_access.rs` holds attribute reads and writes by `CHARSXP` (a full match is a pointer compare of the print name against the interned `which`, and only a miss scans prefixes, in GNU R's `do_attr` order), the implicit and explicit class of a value as a fixed-size list of `CHARSXP`s, and `inherits` over it. `combine.rs` holds `c()` and `unlist()` as GNU R's two passes: one sizes the answer (type, length, whether names exist), the other writes elements and names straight into the result, so no per-leaf vector or `String` is built. `element_access.rs` holds `[[` on plain vectors and lists (`[[<-` keeps compiled code's `assign_element_in_place`). `direct_c` copies attribute-free atomic arguments in one sizing pass and one copy pass, without the naming walk, because no name can arise from them.
  - String compares use `same_text`: interned ASCII strings are equal exactly when they are the same object, so bytes are compared only when either side is not ASCII.
- **Why a field and not new tier-0 ops.** Track A rewrites frames and the call convention, and the JIT matches exhaustively on ops; a field changes neither, and the baseline tier gets one function to call.
- **Why no cache.** Every gain is work not done (no `Args`, no formal matching, no interning, no leaf vectors), so nothing can go stale.
- **Measured impact** (callgrind, instructions per call net of the loop, tier 0): `attr(x, "a")` 1006 to 590 (GNU R 1814), `attr(x, "a") <- v` 3174 to 992 (2520), `names<-` 1668 to 1191 (1520), `inherits` 1266 to 795 (1469), `class` 552 to 376, `dim` 638 to 427, `c(a, 4.5)` 1782 to 863 (1877), `c(a, a, a)` 2315 to 1351 (2050), `c(list, list)` 3425 to 3032 (2700), `x[[i]]` on a double vector 1155 to 925 (223). Whole programs (callgrind, `RY_BENCH_REPEAT=2`, JIT on and off alike): the attribute, `c` and `[[` paths cut testthat_full by about 0.5%; `x[i] <- v` in place, `.Call` by address and `vapply` results cut its test phase (the run minus `library(testthat)`) by 1.77% and functional by 1.4%, with load and peak RSS unchanged. Load is 4% of testthat_full's instructions and tests are 94%.
- **`x[i] <- v` in place.** Compiled `x[i] <- v` on an unshared, attribute-free atomic vector with an integer, real or logical index writes into the bound vector (`assign_subset_in_place` in `element_access.rs`, called from `op_sub_assign`), with GNU R's recycling. It declines everything that changes the result's shape or warns (NA or zero positions, growth, widening coercion, a partial recycle), so the full `[<-` keeps one implementation of those rules. 5273 to 2146 instructions per call (GNU R 1580).
- **`.Call` by address.** A native routine is recognized from its `NativeSymbolInfo`'s `address` pointer alone (`crates/libr/src/dot_call.rs`): an untagged, non-null pointer is a native entry point, because builtin-library routines carry a marker tag and base routines a null address. `call_registered` reads the `address` and `numParameters` slots by position (names checked by pointer against interned symbols, scanned only for a list built another way), declines a `PACKAGE` argument or an arity mismatch to the general path that owns the errors, and passes the arguments from a `SmallVec<[Sexp; 8]>`. `retire_escaped` returns at once when the call handed out no data pointer. The old `list_element` and `native_symbol_address` helpers are gone; the general path uses the same slot readers. `.Call(r, x)` through `rlang:::ffi_length`: 4133 to 2916 instructions per call (GNU R 830); what remains is the builtin call machinery and the boundary's scope handling.
- **`vapply` results in one module.** `apply_results::VapplyResults` (`new(FUN.VALUE, n, USE.NAMES)`, `push(index, value)`, `finish(X)`) owns GNU R's checks and their error text, writes each result into the preallocated answer with widening done inline, and sets names, dims and dimnames once. Any loop that calls `FUN` per element, interpreted or compiled, hands its results to it; the loop owns calling `FUN`, this module owns what happens to each result. It fixed four divergences from GNU R (complex widening, list strictness, `USE.NAMES = NA`, array `FUN.VALUE`), pinned in `builtins__vapply_results.R`.
- **Element assignments stay in slot frames.** Three gaps sent warmed loops of element assignments down slow paths:
  - A `for` variable the body never reads had no slot, because `slot_table` numbered only symbols a `GetVar`, `SetVar` or `GetFun` names, so the first `ForStep` gave the frame an environment and every `x[[i]] <- v` after it ran through `frame_cell`. `ForStep`'s variable is numbered too.
  - `ForStep` and `SubAssign` found their variable's slot by comparing it against every slot name on each step. The `SLOTS` constant gains a fourth element, an integer map from constant index to slot (`slot_at`), so the lookup is one load (about 65 instructions saved per step and per assignment at tier 0). `index_slots` derives it from the code, the names and the constants, in the compiler and in the image reader, so images store only the names and the map cannot go stale. It covers only the variables those two ops name and is absent from code without them: a map over every constant of every function raised the testthat workload's peak RSS by 2 MB.
  - A parenthesized subscript (`x[[(k %% 8L) + 1L]] <- v`) was not `side_effect_free_index`, so the assignment compiled to `complex_assign`; parentheses over a side-effect-free operand qualify.
  - `x[i] <- list(...)` on an unshared plain list writes the value's elements in place (`assign_subset_in_place`), counting each as stored through `set_element`; an atomic value for a list target still declines, since it would need boxing.
- **No per-call allocations in subscript and argument matching.** `subset::resolve` returns positions in a `SmallVec` with eight inline slots, and negative subscripts build their kept mask as a `SmallVec<[u64; 4]>` bitset (inline up to 256 elements), so `x[-1L]` on a short vector makes no Rust allocation (2,455 to 2,149 instructions per loop iteration, GNU R 2,372). `match_builtin_args` marks used arguments in a bitset and borrows argument names instead of copying them. The primitive formals `check_argument_names` reads come from a `Vec` indexed by builtin index (`builtin_index`), not a map keyed by name. `crates/oracle/cases/subset__negative_index_bitset.R` pins the masks past one word and argument-name matching.
- **What is left.** At tier 0, `x[[i]]` and `x[[i]] <- v` cost about twice GNU R per iteration, and the remainder is dispatch around the builtin, not `[[`: for a read, `GetFun`, the primitive and object branches, `CallBuiltin` and the `Jump` cost about 300 instructions around a 130-instruction read; for an assignment, two `Guard`s cost about 100. Closing it takes a fused element op, which belongs to the tier-0 op set; `element_access::read_element` is the element read such an op calls. Under the JIT both already run far below GNU R. `crates/oracle/cases/builtins__attribute_access.R`, `builtins__combine.R`, `builtins__element_access.R`, `builtins__assign_subset_vector.R`, `builtins__dot_call_native.R` and `builtins__vapply_results.R` pin the edges against GNU R.

## Builtin call machinery and subassignment toward 10x

**Status:** built.

- **Previous shape.** `DirectFn` took only the argument values, so a builtin whose errors name the call (`[`, `.Call`, `round`) could not have a direct entry and paid for `Args`, the dispatch probe and a handle scope on every call. `x$name <- v` and `x[["name"]] <- v` that append a name always copied the vector (`[[<-`'s full path), even when it was unshared. `m[, j] <- v` wrote element by element through an index walk. `round(x)` on a vector went through `round_like`'s generic loop and copied attributes unconditionally. `Reduce(accumulate = TRUE)` ran the R body.
- **Chosen shape.**
  - `DirectFn = unsafe fn(call: Sexp, &[Sexp]) -> R<Option<Sexp>>`. The entry gets the call for error messages and native context, so `[`, `.Call` and `round` take the direct path. `direct_subset` reads an in-range numeric scalar position out of an attribute-free atomic vector and otherwise calls the same `subset()` the full builtin uses. `.Call`'s direct entry resolves the routine and calls `call_native` inside `foreign_context`, with no argument list.
  - `-x` on an attribute-free integer or double scalar negates through `BuiltinDef::unary`, so `x[-1L]` does not pay for a general call per iteration (the compiler does not fold `-1L`).
  - Appending a name (`append_named` in `subset.rs`) grows the bound vector in place when it is unshared, not held by a register and not bound through a promise, and copies with room to grow otherwise. The element assignment rebinds the variable when the result is a new vector, so growth is amortized like GNU R's `growable` vectors.
  - `scatter_slab` copies contiguous runs with `copy_from_slice` when the first dimension is taken whole, and walks the other dimensions with a counter.
  - `round` on a double vector runs `fround_slice` into the result and copies attributes only when the operand has them.
  - `Reduce` with `accumulate` runs natively: `out[[i]] <- init` writes the slot directly while `out` is unshared and `init` is not NULL, and evaluates the assignment otherwise (NULL deletes the slot). The final `unlist` reads lengths directly unless an element is an object, in which case the R tail is evaluated.
  - `c()` reuses the tag's `CHARSXP` as the element name when an argument contributes one unnamed element, instead of building a new string.
- **Why no new tier-0 ops.** The `direct` and `unary` fields reach the same code from tier 0 and the JIT's fallbacks without changing the op set Track A owns.
- **Measured impact** (callgrind, instructions per iteration, JIT on, against the main it is based on; GNU R in parentheses): `round(x)` on doubles 63 to 29 per element (76); scalar `round` 1785 to 1129 (2526); `x$name <- v` appending 7739 to 4292 (4387); `x[["name"]] <- v` appending 7937 to 4289 (6112); `m[, j] <- v` 34 to 8 per element (20); `.Call(f, 1)` with its loop 2229 to 2021 (964); `x[-1L]` 3179 to 2485 (2372); `x[lgl]` 1467 to 1132 (1948); `l[chr]` on a list 2295 to 1959 (3775); `Reduce(f, 1:20, accumulate = TRUE)` 208.4k to 93.4k per call (190.9k); `c(a = 1, b = k)` 4282 to 3698 (4149).
- **Whole programs** (callgrind instructions, JIT on, `RY_BENCH_REPEAT=2`): records -7.9%, simulation -7.9%, tables -5.4%, functional -2.5%, testthat_full -0.2%, the other six under 0.5% either way. bench-real wall time, paired rounds: 1.03x over main with the JIT on (95% interval 0.96 to 1.13), too noisy on this machine to resolve a change this size. Peak RSS with the binaries warm in the page cache: functional -14 MB, objects -4 MB, the rest within 0.5 MB. A cold binary inflates RSS through page-cache fault-around on its text, so compare builds only after reading both files once.
- **What is left, and whose it is.** `structure()`, `format.default` and `Reduce`'s callee calls are dominated by closure frame setup and argument matching (Track A's frames and call convention). `.Call`'s remaining gap to GNU R is the compiled loop and the JIT's call sequence. `vapply` and `lapply` over three elements beat GNU R (2677 against 2920, 2625 against 2718), and what remains is calling `FUN`. Oracle cases `builtins__round_vector_fast_path.R`, `assignment__append_name_in_place.R`, `subsetting__matrix_slab_assign.R`, `builtins__dot_call_direct.R`, `subsetting__direct_single_bracket.R` and `apply__reduce_accumulate.R` pin the edges.

## One kernel layer for R's vector builtins: slice kernels, a reuse rule, and a fused superinstruction

**Status:** built, except the helper `first_nan`: the kernels pick the NaN payload inline.

- **Previous shape.** Each vector builtin carried its own element loop (`real_kernel`, `compare_kernel`, the `Summary` loops in `arith.rs`), always allocated a fresh result, and ran one full pass per operator, so `a * b + c` on 30,000 doubles allocated and filled two 240 KB vectors. The loops vectorized only as far as SSE2 and the compiler's choice allowed, and nothing let compiled code call a kernel without building a builtin call.
- **Kernel interface** (`crates/interp/src/kernels.rs`). Two layers.
  - Slice kernels take operands as `Reals` (doubles, integers read as doubles, or a scalar) or `Integers` (a slice or a scalar) and write into a `&mut [T]` output. They touch no R object, allocate nothing and raise nothing: an integer kernel reports overflow as a returned flag and the caller warns. The loops vectorize for the baseline instruction set; a second AVX2 build of every kernel was tried and dropped, because it cost 0.6 MB of resident code for no measurable gain on memory-bound loops. `+` and `*` beside a scalar that is not NaN use the plain operator: only the vector element can be NaN, and the hardware returns its payload in either operand order, so the per-element NaN select (which made the swapped-operand loops GNU R's payload rule needs a third slower) is not needed. `kernels::unary`, comparisons in `kernels::binary` and unary minus answer a scalar operand directly without the element loop: most calls in package code are on scalars, where the call into a kernel costs more than the work. This is the interface compiled code calls later.
  - The value entry `kernels::binary` serves plain operands only (double, integer or logical, no attributes, not ALTREP, not an Array) and returns `None` for anything else, which takes the builtin's general path with its dispatch, attributes and messages.
- **NaN payloads.** On x86-64 an SSE operation on two NaNs returns the first operand's payload, quieted, and LLVM may swap the operands of `+` and `*`, so the kernels pick the payload explicitly (`first_nan`). GNU R's own compiled loops swap them in some variants: for two double operands (logicals are coerced to double first), `x + y` and `x * y` take `y`'s payload where `x` is a scalar beside a longer `y` and where both recycle, and `x`'s otherwise; with an integer operand they always take `x`'s. `kernels::payload_from_right` encodes this and the callers swap the operands, which changes nothing else. `-`, `/` and `%/%` (which divides first) take `x`'s. Probed against GNU R 4.3.3 in `crates/oracle/cases/kernels__elementwise.R`.
- **Reuse rule.** A result is written into an operand's buffer when both hold: the bytecode compiler marked the operand's register as a temporary that only this instruction reads and that an arithmetic or fused instruction wrote (operand 6 of `Op::Arithmetic`, a bit per side), and at run time the operand's count is zero, it is mutable and not ALTREP, and it has the result's type and length. The count alone is not enough because registers and the JIT's unwritten slot values hold uncounted references, so a zero count does not prove that nothing reads the value later. A builtin called from the AST evaluator never reuses. This is GNU R's `R_allocOrReuseVector` rule restricted to what the compiler can prove.
- **Destinations that alias an operand.** `kernels::binary(call, operator, left, right, destination)` and `kernels::unary(call, function, operand, destination)` take an optional destination, which may be one of the operands, so `x <- x * y` and `x <- sqrt(x)` write into `x`'s own buffer. The slice kernels take each operand as an `Option`, where `None` means the operand lives in the output buffer, so an aliased operand is read and written through one `&mut` slice with no copy and no `unsafe` aliasing; recycling stays exact because only an operand of the result's full length can be the destination. `kernels::fits` admits a destination only when its length is above one, it is plain and it has the result's type and length. The length rule exists because the shared scalar constants (`values::scalar_logical`'s `TRUE`, `FALSE` and `NA`) can be held by a single binding with a count of one, so a one-element destination is never provably private. Two sources offer a destination: `Op::Arithmetic`'s reuse bits (`bytecode::arithmetic_destination`, a temporary only this instruction reads) and `Call.reusable`, which the evaluator sets for `x <- f(x, ...)` when `x`'s binding holds the only reference and `f` is a builtin marked `reusing_first_argument` (the arithmetic, comparison and logic operators, `!`, and the one-argument math functions). For an `Ops` group member the reuse path dispatches first, so an object's method sees its operands untouched.
- **The evaluator offers fresh operator temporaries too** (`eval::temporary_argument`). A builtin marked `reusing_first_argument` called from AST code gets as its destination an argument whose expression is a two-argument call to a base operator builtin and whose value no counted reference holds (count 0 after `evaluate_args` uncounts the argument list). This is the AST twin of the compiler's reuse bits, so `x * 2 + 1` at top level or in an uncompiled body peaks at one result, as in GNU R (153 MB over the inputs at 2e7 doubles, against 305 before). The operator must be the base builtin because a closure bound to `*` may return a value something uncounted holds, and unary `+` is excluded because it returns its operand, which may be a vector embedded in a constructed call. Reference-counted registers were the alternative; they are unnecessary while every reuse source is a statically known fresh temporary, and they would cost a count update on every register write.
- **Reductions are kernels too.** `sum` on integers (`sum_integers`, an `i64` total that is NA on the first NA unless `na.rm`), `any`/`all` (`any_all`, where a nonzero integer is `TRUE`), and `min`/`max`/`range` (`integer_extremes`, `real_extremes`, which make one pass and report NA and NaN separately) run as SIMD slice kernels; `range` computes both bounds in that one pass. GNU R's `min` and `max` keep the first of equal values, which matters only for `0` and `-0`, so `real_extremes` runs eight lane-wise extremes and then lets the first zero decide a zero bound's sign. `prod` keeps its scalar loop because GNU R accumulates it in x87 long double, which no SIMD lane reproduces bit for bit; the double `sum` uses the long double kernels in `long_double.rs` for the same reason.
- **Call operands are fused by evaluating them ahead, with the operator guards checked first.** A call the pass cannot compute (`mean(x)` in `sum(x - mean(x) > 0)`) becomes a leaf when no operator of the tree has produced a result before it in evaluation order. The ordinary code after the pass reaches each operator through a guard whose cold fallback evaluates the whole call's AST, which would run the call a second time, so `Compiler::fused_call` emits, in order: guards for every operator head before the last call leaf, sharing one cold fallback that evaluates the whole expression; the variable reads and calls before that point, into registers (leaf kind 2 in the fused program); `Op::Fused`; and the ordinary code, in which those operands compile to `Op::Move` from their registers (`Compiler::hoisted`, consumed occurrence by occurrence, so a later read of the same variable reads afresh) and the pre-checked heads have no guard (`Compiler::preguarded`; `general_call` loads the checked function as a constant instead of `GetFun`). That matches R, which looks an operator up before evaluating its arguments. Variables read before a call are hoisted with it so that a call that rebinds them (`gx <<- ...`) leaves the earlier reads seeing the old value. A call that `may_modify_in_place` is never a leaf, because registers do not count as references.
- **Fusion representation.** `Op::Fused dst program target` is an optional superinstruction that the compiler emits in front of the ordinary code of an element-wise tree: arithmetic, comparisons, `&`, `|`, `!`, unary minus and one-argument math functions, optionally under `sum`, `any` or `all`, whose leaves are all constants and variables. The program constant (layout in `bytecode::fusion`) lists the operators' guards, the leaves with their lookup cache slots, and the nodes in post-order, each with the position of the ordinary instruction it replaces. The compiler emits the ordinary code exactly as without fusion and matches the plan's steps against the instructions it emitted, leaving the program `NULL` when they differ. At run time the instruction reads the leaves in the ordinary code's order, forcing a frame argument's promise as `GetVar` would (with the values read before it referenced and rooted, as registers are), then checks each operator through its guard's cache. It declines, falling through to the ordinary code, when every leaf is a scalar or the vectors are shorter than 64 elements (`kernels::SHORTEST_FUSED`: below that the ordinary instructions are cheaper, because fusion saves too little allocation to pay for reading the program), on an active binding, a missing or unbound variable, an attribute, an object, ALTREP, recycling other than a scalar against vectors of one length, integer overflow, or a NaN a math function produced; after 16 declines in a row it rewrites its opcode to `Op::Unfused` (a no-op in tier 0, no code in baseline code) and drops its program from the constant pool. A site that never fuses then costs one dispatch and holds no heap, and the compiler emits `Op::Unfused` directly when the plan does not match the ordinary code. Otherwise it evaluates scalar nodes once, then the vector nodes in 512-element chunks in per-thread scratch buffers, writes the root straight into the result, records the type feedback the replaced instructions would have recorded, and jumps past the ordinary code.
- **Why a superinstruction that may decline.** Fused and unfused evaluation are then indistinguishable: nothing observable happens before the instruction decides, and the ordinary code it falls through to is what the compiler emitted before. The JIT keeps compiling that ordinary code (it treats `Op::Fused` as a no-op), so its unboxed scalar arithmetic is unaffected, and a later JIT can lower the program itself.
- **Rejected: lazy fused vectors as ALTREP objects** (Riposte, pqR). Every reader of a vector, C code through `DATAPTR` included, would have to materialize it, and the point where a deferred warning surfaced would become observable.
- **Fused assignment writes into the old buffer.** For `y <- tree`, the program's `REDUCTION` word is `fusion::ASSIGNED`, and the pass reads `y`'s symbol and lookup slot from the `SetVar` at its own jump target rather than storing them (two extra words in every program grew the start image by two 64 KB pages). At run time `bytecode::reusable_target` offers `y`'s vector when its binding is neither active nor locked, its count is one and `eval::reuse_permitted` holds; `kernels::fused` then takes it when it is a double vector of the result's length. When `y` is also a leaf (`x <- exp(-x^2/2) * 0.5 + x`), `kernels::chunks` evaluates each chunk's root into scratch and copies it over `y` only after every node has read that chunk. Integer passes never reuse: an overflow declines halfway, and the ordinary code it falls to must read the old value. Pinned by `crates/oracle/cases/kernels__vector_math.R` (captured old values, `get` through the frame, a locked binding, overflow). Do not verify reuse with `.Internal(inspect(y))`: the call takes a reference to `y` and defeats it.
- **Baseline code runs the pass too.** The JIT lowers `Op::Fused` to `Action::Fused`, a call to `runtime::jit_fused` that reads the registers the program names (`bytecode::fused_operand_registers`) and returns `FUSED` (jump past the ordinary code) or `FUSED_INVALIDATED`; type feedback comes from `bytecode::fused_result_site`.
- **SIMD `exp` and `log` are ports of glibc's FMA variants** (`crates/numeric/src/vector_math.rs`). GNU R on an AVX2+FMA machine calls glibc's `__exp_fma` and `__log_fma` through ifunc, so a four-lane port of those exact algorithms (same tables, same polynomial, same special-case paths) reproduces its bits; a different SIMD exp would not. The first call runs a probe against the host glibc on edge values and switches the kernels off on a mismatch or without AVX2+FMA. Lookups use four 128-bit loads and unpacks, not `vgatherqpd`: gathers are microcoded slowly under the Gather Data Sampling mitigation (1.02 against 0.84 ns per element for exp). glibc's scalar exp costs 3.9 ns per element and log 2.9; the kernels cost 0.84 and 1.1. `kernels::vector_lanes` wraps them with R's NA handling and the produced-NaN warning.
- **No AVX2 build of the real arithmetic loop either.** A narrow `#[target_feature(enable = "avx2")]` copy of `real_operation` and of unary minus measured faster on L1-resident fused chunks in isolation, but end to end on 10,000 doubles it was 15 to 25% slower than the baseline loop (`y <- x * 2 + 1`, `x * z - z / 3`, `exp(-x^2/2) * k`, a four-operator tree; three runs each), and its 85 KB sat on every double arithmetic call, so every workload faulted in its 64 KB windows of text. Only `vector_math`'s exp and log run AVX2, where the gain is 4x.
- **`R_POW`'s bits.** GNU R computes `x^2` as `x * x` for every `x`, and `R_pow` returns `x + y` for a NaN operand, so both quiet a signalling NaN: `NA^2` has bits `7ff8...07a2`, not NA's `7ff0...07a2`. `arith::power` and the fused square follow that. Division by a normal power of two is multiplication by its exact reciprocal (`kernels::exact_reciprocal`), which is bit-identical and avoids `vdivpd` (0.6 ns per element).
- **Complex arithmetic is a slice kernel too.** `kernels::binary` sends `+`, `-`, `*`, `/` and `^` with a complex operand to `complex_binary`, which serves a complex operand without attributes and a plain double (or, from R 4.4.0, logical or integer) beside it, under the same reuse rule and recycling runs. The arithmetic builtin's general path (`arith::complex_arithmetic`) calls the same `kernels::complex_arithmetic`, so there is one loop per operator. `z^2` with the single exponent 2 (complex, double or integer) is its own loop computing `R_cpow_n`'s `1 * (z * z)`, bit for bit the general path.
- **Complex results match GNU R bit for bit, NaN payloads included.** GNU R compiles C99 complex arithmetic with GCC: `*` inline with libgcc's `__muldc3` where both parts came out NaN, `/` as a call to libgcc's `__divdc3`, `^` as `mycpow` (`R_cpow_n` for an integer exponent up to 65536, else glibc's `cpow`). Rust's runtime links compiler-rt's `__muldc3` and `__divdc3`, whose scaling and recovery differ, so `arith::complex_multiply`, `infinite_product` and `complex_divide` are ports of libgcc's, and `cpow` is called through FFI. Which NaN wins decides whether a part prints `NA` or `NaN`: x86 SSE returns the first operand's payload, so each step uses `arith::add`, `subtract` and `multiply` (first-operand-wins, because LLVM may otherwise commute or rewrite the operation) in the operand order of GCC's compiled code, read from the disassembly of `libgcc_s.so.1`. A NaN ratio inside `__divdc3` fails its subnormal test, so NaN operands take the alternate formula. A 625-case probe over NA, NaN, 0, 1 and 2 in each part matches GNU R exactly. Only a NaN part depends on that order, so `complex_multiply` runs the plain formula and leaves the ordered one to an out-of-line `nan_product`, which first decides the `__muldc3` recovery (NaN-ness does not depend on order) and recomputes in order only when an operand is NaN and no infinity is recovered. In vectorized mandelbrot every escaped point takes that recovery on each iteration (about 40% of the products), so its cost decides the program: running the ordered formula before the recovery made `mandelbrot_noout` slower than a plain-formula loop.
- **Complex coercion follows the emulated version.** A double, NA included, becomes `x + 0i` in every R version; an integer or logical NA becomes `NA + NAi` before R 4.4.0 and `NA + 0i` from it (`r_at_least(4, 4)`), so before 4.4 an integer operand declines the kernel and `arith` coerces it first.
- **Complex math keeps attributes and NA.** `abs`/`Mod` of a complex vector keep `dim` and names, as GNU R's `do_cmathfuns` does. The other `Math` functions and one-argument `log` (`clog`) map an NA in either part to `NA_complex_`, keep attributes and warn `NaNs produced in function "<name>"`, as `cmath1` does; `log(z, base)` is `clog(z) / clog(base)` through the `__divdc3` port. `atan` reproduces `z_atan`'s `rr + ri*I`, a full complex product, so a NaN imaginary part makes the real part NaN too.

## Package code that dominates a workload runs natively: rlang's trace_back

**Status:** built.

- **Measured.** On the testthat workload (`tools/bench/applications/testthat.R`), stubbing `testthat:::can_entrace` to `FALSE` removed 23% of the instructions. The cost is rlang's R code, not ry's condition machinery: `rlang::trace_back()` is about 80% of `expect_error()` on GNU R as well. Every `expect_error()`, `expect_warning()` and `cnd_entrace()` builds a trace of the whole frame stack.
- **Decision.** Package functions get hash-guarded prologues like base's (`package_fast_paths.rs`). `registerS3methods()` runs for every namespace load, so its first statement, `.Internal(ryPackageFastPaths(package, env))`, installs the prologues of that package through `base_fast_paths::install_prologue`. A prologue applies only when the closure's formals and body hash to the version it was ported from (rlang 1.3.0 for `trace_back`), and a changed package silently keeps its R body.
- **Policy for package-specific native code.** These rules settle when a package function may get native code, so the next case needs no new debate:
  - *Generic first.* Measure the function's R body on ry against GNU R in the same sitting and fix every primitive under it that is slower than GNU R; native code covers only what remains.
  - *Eligibility.* The package is in the AGENTS.md benchmark dependency set, or a profile of a real workload puts the function above about 2% of the time.
  - *An exact guard per release.* `FAST_PATHS` names the release and the hash of its formals and body. A miss falls back silently at run time, but `crates/ry/tests/scenarios/native-package-functions.scenario` fails when the installed release is not the listed one, so an update shows up in the test suite instead of as a silent slowdown.
  - *Fallback.* The native function declines (`decline()`, which returns `NULL` from the prologue) whenever the R body could observe something the port does not model, and the R body runs.
  - *Evidence.* A seeded differential harness compares the native result with the R body's by `identical()` in the default test suite (`fuzz_native_trace_back_matches_its_r_body` in `crates/oracle/tests/test_differential.rs`, `FUZZ_SEED` replays), oracle cases compare it with GNU R, and the news page names each package-specific fast path.
  - *Review rule.* A reviewer accepts a new entry only with the same-sitting generic measurement, the profile share, the hash test entry, the differential harness, and the news line.
- **Generic first, measured.** Under trace_back's R body at a depth of 50 frames, ry's primitives run at 0.5 to 1.2 times GNU R's cost: `sys.calls`, `sys.frames` and `sys.parents` are faster, `call_trace_context` (the bulk, about 1.5 ms of the trace) is 0.7 times, `trace_slice` (`vec_slice` and `[<-.data.frame`) 1.07 to 1.16 times. `intersect()` is 2.6 times only because R 4.6's body does more work than 4.3's; its primitives are faster on ry. No primitive was clearly slower, so the whole gain is the native layer. The R body takes 1.19, 3.20 and 8.96 ms at depths 10, 50 and 200 on ry, against 0.89, 2.31 and 7.74 ms on GNU R; the native trace takes 0.015, 0.045 and 0.16 ms.
- **Why `registerS3methods()` is the hook.** Every namespace load runs it: `library()`, `requireNamespace()`, `pkg::` and the imports that `ry load` resolves, from a code image or from lazy-load files alike. It runs after the namespace's code is in place and before anything outside can call it, and it needs no change to `loadNamespace()`. The scenario above pins the script path and the `ry load` path.
- **trace_back is a pure function of the frame stack.** `ryTraceBack` reads `interp().frames` (calls with srcrefs, `sys.parents()` numbering, the functions) and reproduces rlang's columns exactly: `call_zap_inline` (inlined values become `call_type_sum()` results, evaluated in rlang's namespace), `call_trace_context` (namespace and scope), trimming by `top` and `rlang_trace_top_env`, `bottom`, and the attribute order. It declines whenever the R body could observe something the port does not model: the options `rlang:::visible_bottom` and `rlang:::error_frame`, `knitr.in.progress` without a `top`, an unforced promise in call position, a call whose function is an object, or a malformed `function` call.
- **Evidence.** `crates/oracle/cases/conditions__rlang_trace_back.R` (hand-written stack shapes) and `conditions__rlang_trace_back_generated.R` (150 seeded random stacks of closures, operators, `::`, `do.call`, `lapply`, `tryCatch`, calling handlers, `eval`, `local`, S3 dispatch and trimming) compare the native trace with GNU R and with trace_back's own R body on the same frame.
- **Interned strings carry a validity fact.** `flag_bits::VALID_UTF8` is set in `Heap::intern_new` when the bytes are valid UTF-8, and `char_str` borrows such a string unchecked unless it is marked Latin-1 or bytes. The header flag byte is unreachable from `SETLEVELS`, and interned bytes never change, so unlike the gp `ASCII` bit it is a fact.
- **Names stay symbols.** `pkg::name`, `getNamespaceValue`, `options()` and the base and package fast paths used to turn symbols into `String`s and intern them again on every call (`Heap::install` was 1.6% of the workload). They take symbols and `CHARSXP`s throughout, and the fixed names they read come from `Symbols`, which one macro now generates (struct and installer) so a name is listed once. `crates/oracle/cases/envir__namespace_lookup.R` pins `::`, `:::`, their errors and multi-option `options()` against GNU R.
- **Environment variables.** See "Environment variable calls read `environ` once" below.
- **Impact.** testthat workload under callgrind, `RY_BENCH_REPEAT=2`, main and this change in one sitting: 6,787M to 4,690M instructions with the JIT on (−30.9%), 6,781M to 4,686M with it off. The load phase (`library(testthat)` alone) goes from 423M to 408M and the test phase from 6,364M to 4,282M (−32.7%). Measured step by step on an earlier main, `trace_back` is about 2,076M, the validity bit about 45M and symbol names about 99M. Warm wall time (median of three medians of five runs) goes from 0.78 s to 0.60 s, and peak RSS from 77.6 MB to 74.1 MB.

## withr's environment and option helpers run natively; defer() stays in R

**Status:** built.

- **Measured.** After `trace_back`, the testthat workload's next package-level costs were withr's helpers behind `local_envvar()`, `local_options()` and `local_language()` (`as_envvars`, `get_envvar`, `set_envvar`, `get_options`, `set_options`, `reset_options`, `list_combine`) and `reset_gettext_cache()`, which testthat calls for every test through `local_reproducible_output()`. `reset_gettext_cache()` creates and deletes a temporary directory to make `bindtextdomain()` drop its cache, which is a system call pair per test.
- **Generic first.** The R bodies spend their time in `intersect()`, `structure(names =)`, `Sys.getenv()`, `Sys.setenv()` and `Sys.unsetenv()`, so those got base fast paths first (`base_fast_paths.rs`: `ryIntersect`, the `names` branch of `ryStructure`, `rySysGetenv`, `rySysSetenv`, the `Sys.unsetenv` native entry). They help every caller, not only withr.
- **Decision.** The eight helpers get hash-guarded prologues for withr 3.0.3 (`package_fast_paths.rs`), under the policy of "Package code that dominates a workload runs natively". `defer()` and the `local_*()`/`with_*()` functions keep their R bodies, so the restore registers through the ordinary exit-handler path and stays one mechanism; only the helpers that compute and apply the change are native.
- **Exactness.** Each helper declines whenever its R body could raise a condition or see something the port does not model: `set_envvar` with an `action` other than `"replace"` or a value that is not a string scalar or `NA`; `as_envvars` on missing, empty or `NA` names or extra attributes; `set_options`/`reset_options` unless every option sets without a condition (`envir::option_sets_quietly`), because an option error's call is the `do.call()`-built call the R body makes.
- **reset_gettext_cache.** ry's `bindtextdomain()` keeps no state and translates nothing, so the cache the R body resets does not exist. The native helper still calls the `tempfile()` and `bindtextdomain()` kernels, so its result and the `tempfile()` sequence match the R body, but skips creating and removing the directory. The skip is unobservable: `tempfile()` names mix the clock with a counter, and the directory never outlives the call. It declines when the session temporary directory is missing, where the R body would fail.
- **Kernel fixes the port surfaced.** `Sys.setenv`/`Sys.unsetenv` return `FALSE` for an empty name or one containing `=` instead of panicking in `std::env`, and write `NA` as `"NA"`, as GNU R does. `Heap::install_char(NA_STRING)` installs the symbol `NA`, as GNU's `installTrChar` does, instead of a distinct symbol. A list with an `NA` name prints `$<NA>`.
- **Evidence.** `fuzz_native_helpers_match_their_r_bodies` (`crates/oracle/tests/test_differential.rs`, `FUZZ_SEED` replays) compares every native helper and base fast path above with its own R body by `identical()` on the value, visibility, condition and the environment and option state, including `NA`, empty, unset and non-ASCII values. `crates/oracle/cases/packages__withr_env_and_options.R`, `base__intersect_native.R`, `base__structure_names_native.R` and `system__sys_env_native.R` compare with GNU R, and `crates/ry/tests/scenarios/native-withr-helpers.scenario` fails when the installed withr is not 3.0.3.
- **Impact.** testthat workload, main and this change in one sitting, each with its own base R and package code images: under callgrind (`RY_BENCH_REPEAT=2`) 4,705M to 3,313M instructions with the JIT on (−29.6%) and 4,700M to 3,299M with it off (−29.8%). With withr's prologues stripped back to their R bodies the change runs 4,411M, so the generic base fast paths take 6.2% and the native helpers 23.3% of main's count. Warm wall time (median of three medians of five runs) goes from 0.51 s to 0.24 s with the JIT on and from 0.56 s to 0.23 s with it off; the wall gain exceeds the instruction gain because `reset_gettext_cache()`'s directory system calls are kernel time callgrind does not count.
- **Peak RSS and the JIT.** With the JIT off peak RSS falls from 86.5 MB to 83.8 MB. With it on it rises from 86.7 MB to 87.9 MB, because the faster call mix makes `isTRUE`'s body the one function the JIT compiles in this workload (main compiles none), and the first compile initializes the code generator, about 4 MB. No helper retains memory: 80,000 calls of each keep RSS flat. Making the code generator's first use cheap is JIT work (backlog), not a reason to keep the helpers in R.

## Environment variable calls read `environ` once

**Status:** built.

- **Before.** `Sys.getenv`, `Sys.setenv` and `Sys.unsetenv` called libc once per name through `std::env`, which takes a lock, allocates a C string, and scans all of `environ` each time. withr's `local_envvar()` restores every variable on exit, so most `setenv` calls wrote the value already there and most `unsetenv` calls removed a name that was absent. In a 172-variable environment where 64 names start with `CL`, glibc's two-byte prefix check fails often enough to cost a `strncmp` per entry.
- **Shape.** `scan_environment` (`crates/interp/src/system.rs`) walks `environ` once for all names of a call. Each name is keyed by its first two bytes as glibc does (a one-byte name uses `=` as the second byte). A call with one name, which is most of them, compares each entry's first byte, then its second, before anything else, exactly glibc's loop (`find_variable`); building a two-byte key per entry first cost twice glibc's instructions per entry. A call with several names keeps a 256-bit set of their first bytes, so most entries are rejected after one load, and only then compares keys. The keys sit in a plain slice, a found name's key becomes 0 (no entry's key is, because an entry's first byte is not NUL), and the walk stops once every name is found. A `SmallVec` deref or a shrinking list in the per-entry loop costs more than glibc's whole lookup. `Sys.getenv` fills every element from that one walk; `Sys.setenv` skips a name whose current value equals the new one, unless the name repeats an earlier element of the same call; `Sys.unsetenv` skips an absent name. Writes still go through libc, so C code sees every change, and nothing is copied or cached.
- **Exactness.** Empty names, names with a NUL byte, `NA` (the text `"NA"`), values containing `=`, empty against unset, repeated names, and non-UTF-8 values (marked native) behave as before. `fuzz_environment_variables_agree_with_the_c_library` (`crates/interp/tests/test_fuzz.rs`) checks every `Sys.getenv` result against `std::env::var_os` after random sets and unsets from both R and Rust, and `crates/oracle/cases/system__sys_env_round_trips.R` compares set, unset and save-and-restore round trips with GNU R.
- **Impact.** On the testthat workload under callgrind (`RY_BENCH_REPEAT=2`), measured against main in the same sitting, total instructions go from 3,274M to 3,228M with the JIT on (−1.4%) and from 3,260M to 3,213M with it off. The three calls together cost 126M before and 82M after. The savings come from skipping redundant writes and the `strncmp` they caused. Rejecting entries on their first byte then took the testthat workload from 3,210M to 3,177M instructions with the JIT on (−1.0%) and from 3,195M to 3,163M with it off: `Sys.getenv` fell from 44M to 24M inclusive, below the 36M it cost through libc, and a one-name `Sys.getenv` call in a loop from about 3,900 to 2,100 instructions. The test phase (warm median, `RY_BENCH_REPEAT=5`) and the load phase (`library(testthat)` alone) do not change beyond noise. Peak RSS is unchanged (86.6 MB with the JIT on for both) once both builds' staged files are written at the same time and warmed. Staging them hours apart showed a false gap of several MB: a freshly written code image sits whole in the page cache, so fault-around maps more of it.
- **Rejected.** A private copy or index of the environment would make lookups cheaper, but C code that calls `setenv()` would then go unseen, and it would be a cache by this project's rules.

## Conditions: message(), warning(), signalCondition() and getOption() run natively when nothing could observe the difference

**Status:** built.

- **Decision.** `message()` and `signalCondition()` get hash-guarded prologues (`ryMessage`, `rySignalCondition` in `base_fast_paths.rs`) and `getOption()` a native entry. Each declines to its R body whenever the body could do something the port does not model, so the R body stays the definition.
- **message().** It runs natively only when no handler for `simpleMessage`, `message` or `condition` is established (`conditions::handled`, which walks the handler stack without allocating), `domain` is the default, every `...` argument is an evaluated plain atomic vector without attributes, and no `conditionMessage` method for `simpleMessage` or `message` is defined. Under those conditions the R body would build a condition, signal it to nobody, and write `conditionMessage()` to `stderr()`, so writing the text directly is unobservable. Text in a Latin-1 or bytes encoding declines.
- **signalCondition().** The R body binds `msg` and `call` in its frame before signalling, and a handler can read them through `sys.frames()`, so the native path defines both in the same frame and then signals through `conditions::signal_condition`. It reads the `message` and `call` fields as `$` does: an exact name wins, an absent field is `NULL` (testthat's expectation conditions have no `call`), and a name that only a longer field starts with declines, because `$` would partially match it. It also declines when a `conditionMessage`, `conditionCall` or `$` method exists for a class before `"condition"`, because those methods run before any handler. Handler lookup re-reads the class attribute for each handler, because a calling handler may change it.
- **warning().** A prologue (`ryWarning`) builds the message for plain arguments and signals it through `conditions::signal_warning`, the same code `.Internal(warning())` runs, from warning's own frame, so the condition's call and `sys.calls()` match the R body. For `NULL`, logical, integer, double and character arguments without attributes or a class, `.makeMessage()` runs no code a handler or method could observe: `lapply()` and `as.character()` dispatch on nothing, and ry's `gettext()` is the identity. A condition object, an unevaluated or classed argument, a non-default `domain`, a `call.` or `immediate.` that is not a single non-NA logical, and Latin-1 or bytes text decline. `noBreaks.` is ignored as ry's `.Internal(warning())` ignores it. Signalling still reaches calling handlers, `tryCatch()` and every `warn` level through the shared path, so handlers see the same condition.
- **getOption().** `envir::option_value` walks `.Options` and compares the name's CHARSXP by pointer, and by bytes only for a non-ASCII name. A dense table keyed by the option's symbol would be a second copy of `.Options`, which R code and C code (`GetOption`) read and write directly; the O(1) shape is a redesign of `.Options` itself (`backlog.md`, "Options as a symbol-indexed array").
- **Switch.** `RY_FAST_PATHS=0` disables these paths and the warning-call shortcut, so a case can be diffed against ry's own R bodies when the local GNU R is not the bundled release (`development/corpus-and-oracle.md`).
- **Evidence.** `crates/oracle/cases/conditions__message_and_restart_frames.R` and `conditions__signal_condition_dispatch.R` and `conditions__warning_native.R` compare frame variables, user methods, numeric formatting in warning text, partial and missing fields, handler order, a class change inside a handler, non-condition arguments and each `warn` level with GNU R.
- **Impact** (main 7bc273d against 7c2114b in one sitting, bench profile, testthat workload under callgrind): 2,902.6M to 2,867.3M instructions with the JIT on (-1.22%), 2,900.5M to 2,865.4M with it off (-1.21%); peak RSS 79.6 MB to 77.6 MB. Per call, `RY_FAST_PATHS=0` against the default: unhandled `message()` 31.7 µs to 0.95 µs, unhandled `warning()` 22.2 µs to 6.0 µs, `signalCondition()` with no handler 5.15 µs to 2.70 µs, `getOption()` 0.75 µs to 0.20 µs. The `warning()` prologue, measured against its own declined path (`domain = NA`) under callgrind: an unhandled `warning("value ", i, " is large")` from 43.8k to 13.2k instructions per call, one under a calling handler that muffles it from 201.3k to 171.5k, where signalling and the restart dominate what is left.

## Builtin and assignment hotspots of the public suites

**Status:** built.

- **Measured.** Profiling every program of the vendored public suites (`tools/bench/public/{awfy,rcp,shootout}`) against GNU R in one sitting put the slow ones' time in assignment forms and small kernels, not in one builtin: `<<-` went through a builtin call, `x[i, j, ] <- v` copied the whole array, `m[[i, j]]` took the general subset path, `%*%` walked the options list twice per call, and growing a vector copied it element by element.
- **`<<-` to a symbol is a tier-0 op.** `Op::SuperAssign` assigns in the first enclosing environment that binds the name (creating it in the global environment, erroring on a locked binding), so a compiled `<<-` costs a lookup and a store instead of a builtin call with its `Args`.
- **The defensive copy in complex assignment is narrowed.** The copy guards against an index expression that rebinds the target while the assignment runs (`x[{x <- 0; 1}] <- v`). It is taken only when an index expression syntactically contains an assignment to the target variable or a part of it (`assigns_to` in `eval.rs`), not for every index with a call in it, so `x[i + 1] <- v` writes in place.
- **Array slabs are written in place.** `x[i, j, ] <- v` and `x[, j] <- v` on an unshared numeric or logical array write through an odometer over the subscripts (`subset.rs`), and `[<-` has a direct entry for that case, so `Replace` handles them instead of the general complex assignment.
- **Growth copies in bulk.** `c()`, `length<-` and the binds copy the existing prefix with one slice copy, and scalars come from a fast heap path (`alloc_scalar`).
- **Matrix `[[i, j]]` has a direct entry.** `direct_subset2` reads one element of a matrix for two scalar in-range subscripts (`scalar_position`, `element_access.rs`) and declines everything else, including NA, zero, negative and non-scalar subscripts, to the full builtin. A negative matrix `[[` subscript is an error in GNU R 4.3.3 ("attempt to select less than one element in integerOneIndex", "invalid negative subscript in get1index <real>"); GNU R 4.6.2 applies the vector extent-two rule to each matrix extent (PR#19116, so `m[[-1, 1]]` of a two-row matrix reads row 2), and `get1index` follows whichever release the runtime emulates (`r_at_least_release(4, 6, 2)`).
- **`dim<-` and `array()` validate dims as 4.6.2's `dim2total` does.** On 4.6.2 and later both check, in order, an empty `dim` ("'dim' cannot be of length 0"), `NA`, negative extents, and a double product beyond the longest vector ("too many elements specified"), then `dim<-` prints the mismatch with both lengths whatever their size (`dims_total`, `subset.rs`). An older emulated release keeps its own messages ("length-0 dimension vector is invalid", the product only when it fits an `int`), and its `array()` keeps no checks.
- **`matprod` lives in interpreter state.** GNU R keeps `options(matprod =)` in the C global `R_Matprod`, updated when the option is set. ry does the same: `Interp::product_options` (`linalg::ProductOptions`) is written only by `set_option` and reset by `clear_options`, and `%*%` reads it instead of looking up `matprod` and `ry.blas` per call. The option list stays the single source of truth for what `getOption()` returns; the state is the parsed form, written in the one place the option changes. A one-row `%*%` is computed as dot products.
- **An index that assigns still compiles.** `x[k <- i] <- v` and `x[j <- if (c) a else b] <- v` compile as long as they assign some other variable (`eval::assigns_to`), so the assignment stays on `Replace` instead of falling back to the AST path, which rescans the target's attributes on every call.
- **`Replace` takes a special's direct entry.** `[<-` is a special, so a slab write with an empty subscript (`a[i, 2, ] <- v`) used to miss the compiled path. `replace_variable` accepts a special with a direct entry for a non-object target, allows empty subscripts only for it, and falls back to the general path when the entry declines.
- **`attr<-` interns the name.** The attribute name is installed as a symbol once (`install_charsxp`) and compared by identity, instead of comparing text against every attribute.
- **Complex arithmetic follows C99.** It runs in the vector kernel layer, through ports of libgcc's `__muldc3` and `__divdc3` and libc `cpow`, so special values such as `(Inf+1i) / 0` and `complex(real = -Inf, imaginary = NA) / 2` match bit for bit; see "Complex results match GNU R bit for bit, NaN payloads included".
- **Measured impact** (wall, ry JIT on against GNU R on the same machine): `m[[3L, 7L]]` 2M reads 0.233 s to 0.120 s (GNU R 0.100 s); 200k 5x5 `%*%` 0.070 s to 0.042 s (0.040 s). Per-program numbers are in the pull request that landed this.
- **Left for other owners.** Closure call overhead (`rev.default`, `rep_int`, `as.vector`, `get` are closures), `:` and `rep` (arith), and unboxed scalar loops in the JIT dominate the remaining slow programs (fannkuchredux, nbody, spectralnorm_alt_3).
- **Evidence.** `crates/oracle/cases/assignment__superassign_compiled.R`, `assignment__array_subassign_compiled.R`, `builtins__grow_copies.R`, `builtins__matrix_element.R`, `assignment__assigning_index_compiled.R`, `arith__complex_vectors.R` and `attributes__many_attributes.R` match GNU R with the JIT on and off.

## One-pass attribute lookup and the blocked matrix product

**Status:** built.

- **Attribute lookup.** `attribute_access::attribute` finds a full and a partial match in one walk of the attribute pairlist. Only a tag strictly longer than the wanted name can be a partial match, so a vector used as a map from fixed-width keys (shootout's knucleotide programs store thousands of attributes on one vector) costs one pointer compare per tag instead of a second walk with a `memcmp` per tag. knucleotide_brute_2 went from 0.69 s to 0.48 s (GNU R about 0.95 s).
- **Matrix product.** `%*%` computes `dgemm("N", "N")` itself, adding `x[, inner] * y[inner, col]` into each result column in ascending `inner`, which is the reference BLAS order and keeps results bit-identical to GNU R. Four result columns share each pass over `x`, so a large `x` streams from memory a quarter as often without changing any entry's summation order. Do not reorder the sum (blocking over `inner`, pairwise or FMA kernels): results would differ from GNU R in the last bits. The 1000 by 1000 product in spectralnorm_math went from 0.29 s to 0.17 s. The remaining time there is `eigen` in the reference LAPACK, which GNU R uses too.
- **Evidence.** Oracle cases `attributes__partial_lookup.R` and `numerics__matrix_product_blocks.R` (every remainder of four result columns, and non-finite entries).

## Distribution functions and the RNG run as vector kernels and batched samplers

**Status:** built.

- **Previous shape.** Every `d*`/`p*`/`q*` call went through one generic loop that recycled operands with `%`, called the scalar function through a function pointer per element, and coerced the flag arguments per call through the general coercion path. Every `r*` call drew one deviate at a time through `unif_rand`, which matched on the RNG kind per uniform, and samplers such as `rpois` and `rbinom` recomputed their setup (GNU R's static caches) per element. `.Random.seed` was rebuilt as a fresh vector after every call.
- **Kernels.** `distributions!` (`crates/interp/src/distributions.rs`) generates one `Kernel` type per row, so `math::<K>` monomorphizes the loop with the scalar body inlined. Recycling uses one wrapping counter per operand. NaN operands go to a cold path that keeps GNU R's NA-over-NaN rule. A kernel may override `fill_first` for the case where only the first operand varies; `qnorm` does, running the central region of Wichura's AS 241 across AVX lanes and the tails in a batch (`qnorm_central_fill`, `qnorm_tails_fill` in `rmath.rs`).
- **Samplers.** `random { ... }` rows generate a `Sampler` whose `prepare` does the per-parameter setup once and whose `draw` takes a `Deviates`. Scalar parameters prepare once per call; recycled ones prepare again only when a parameter's bits change. `Poisson` and `Binomial` hold GNU R's static caches (the table for `mu < 10`, the BTPE constants) as fields, so the sequence is the one GNU R's caches give.
- **Batched uniforms.** `with_deviates!` splits the session RNG into `Draws<Twister>` or `Draws<&mut State>` once per call, so the generator's state stays in locals across the batch and the kind is matched once. The twister regenerates eight words per AVX2 instruction and tempers four at a time (`twist_words_avx2`, `temper_block_avx2` in `random.rs`), and inversion normals combine their two uniforms four at a time (`inversion_fill_avx2`).
- **Bit-exactness.** Every SIMD lane performs the scalar algorithm's operations in the same order. Rust never contracts `a * b + c` into an FMA, so `#[target_feature(enable = "avx")]` multiversioning cannot change a result. The integer kernels are exact by construction, and the unsigned-to-double conversion goes through `(word ^ 2^31) as i32 as f64 + 2^31`, which is exact. `log` and `exp` vectorize only through `vector_math.rs`, the exact ports of glibc's `__exp_fma` and `__log_fma` shared with the fused element-wise kernels, because any other SIMD polynomial would differ from glibc in the last bit. `pnorm`'s `fill_first` (`pnorm_standard_fill`) gathers each 256-element chunk's two exponent arguments and runs them through `vector_math::exp`, and `qnorm_tails_fill` runs its logarithms through `vector_math::log`; both keep a scalar libm loop for machines where the kernels are off. The inversion combine runs on lanes only for generators whose uniforms lie in (0, 1), because the vector truncation and Rust's saturating cast disagree on overflow, which a user-supplied generator can reach.
- **`sample` without replacement tracks drawn values in a bitset when the population is dense.** `do_sample2` (a small sample of a large population, redrawing duplicates) keeps a bitset over the population when `population / 128 <= size`, and an `FxHashSet` keyed by drawn value otherwise. At that ratio the bitset is at most twice the hash set's memory and needs no hashing or probing; below it the bitset would dominate the call. The draw order and redraw count are unchanged, so the stream is GNU R's (pinned by `random__integer_index_draws.R`, which covers both sides of the threshold).
- **`.Random.seed` in place.** `put_rng_state` overwrites the existing integer vector when it is an unshared, attribute-free, non-ALTREP vector of the right length in an unlocked, unwatched, ordinary binding, so a copy the user saved keeps its values (pinned by `random__batched_streams.R`).
- **Measured** (callgrind, `set.seed(1); for (i in 1:3) x <- rnorm(1e6)` minus start-up): 164 instructions per normal on the previous shape, 67 after. Wall clock, same rounds against the previous build: `simulation` 1.26x faster (1.15 to 1.43, 9 rounds), its native bucket 0.53 s to 0.30 s; per element at length 1e6 against GNU R, `rnorm` 3.2x, `runif` 5.7x, `qnorm` 3.0x, `pnorm` 1.7x, `rpois` and `sample` about 1.1x.
- **Evidence.** Seeded tests in `random.rs` (`batched_draws_match_single_draws`, `simd_kernels_match_their_portable_versions`) and `distributions.rs` (`qnorm_lanes_match_the_scalar_function`, `pnorm_batches_match_the_scalar_function`, `recycling_matches_modular_indexing`, `prepared_samplers_match_fresh_draws`); oracle cases `distributions__vector_paths.R`, `random__batched_streams.R` and `random__prepared_samplers.R`.

## unlink() follows GNU R's R_unlink, with directory descriptors

**Status:** built.

- **Previous shape.** `do_unlink` called `is_dir` (which follows links) and `std::fs::remove_dir_all`, ignored `force` and `expand`, counted `NA` as success, and removed an empty directory without `recursive`.
- **Shape.** `crates/interp/src/file_removal.rs`. The walk is GNU R's `R_unlink` on directory descriptors: `openat`, `readdir` with `d_type`, and `unlinkat` relative to the parent, so only `force` or an unknown entry type costs a `stat`, and each subdirectory is removed as soon as it is empty, children before parents. The path length is tracked as a number, so the walk allocates nothing per entry.
- **GNU R's quirks kept.** `lstat`, so a link to a directory is removed as a link; `force` uses `chmod`, which follows a link; the directory test is the bit test `st_mode & S_IFDIR`, so sockets and block devices are walked as directories and fail; `NA` counts as a failure, `"~"` is skipped, `expand` globs with `GLOB_NOCHECK`; a path of 4096 bytes or more inside the walk is the error "path too long".
- **Rejected: handing `rmdir` to a background thread.** `rmdir` on ext4 waits for the journal (about 0.1 ms idle, 2 to 4 ms under writes), and renaming the emptied skeleton away for a helper thread to remove cut a testthat-shaped tree from 2.0 to 3.7 ms to 0.27 to 0.46 ms under write load. The corpus did not move (base over new 1.013x, 95% interval 0.973 to 1.048), because most of its `unlink()` calls remove single files (`cli`'s snapshot tests remove testthat's `verify_exec_*` files), and the thread cost 0.28 MiB of peak RSS plus a join before every fork and at exit.
- **Effect.** Single-threaded, a testthat-shaped tree (two files, two nested directories) takes 0.29 to 0.39 ms (median 0.35) against 0.37 to 0.49 ms on the previous shape and 0.31 to 0.42 ms on GNU R; under fsynced background writes all three take 2.3 to 3.3 ms, because the `rmdir` wait dominates. The change is for correctness, not speed.
- **Coverage.** `interp` `test_file_removal` compares generated trees removed by a port of `R_unlink` and by `file_removal::unlink`; the `io__unlink` and `io__unlink_fork` oracle cases.

## Parse data as dense columns

**Status:** built.

- **What was slow.** `parse(keep.source = TRUE)` builds GNU R's parse data by replaying bison's reductions over ry's syntax tree (`crates/interp/src/parse_data.rs`), and it ran at 2.3 times GNU R's time on rlang's 73 test files (46 against 20 ms per pass). testthat's `source_file` parses every test file this way, so rlang, lubridate and cli paid it in every run. Callgrind (953M instructions per pass) put it in four places: `position` scanned each line's characters for every row, every replayed item owned a `Vec` of children that the parent pass cloned, the rows were sorted by id, and `grammar_action_failure` walked the whole tree with owned nodes and a `HashSet` per parameter list.
- **Target shape.**
  - An item records only `reduced_by`, the item that reduced it; children are a `SmallVec` passed to `reduce` and dropped there. One backwards pass derives each row's parent and the id handed down through invisible items.
  - Ids are dense, so placing each visible item at its id (`row_of_id`) orders the rows without a sort.
  - Comment parents come from one sweep: rules sorted by start, comments by position, an open-rule stack, and a binary search over the top-level ids for comments outside every rule. The per-comment scan of every row is gone.
  - `SourceMap` keeps a bitset of lines that are ASCII and tab free. `position_near` checks the previous row's line before a binary search, and on a plain line the column is the byte offset.
  - The token and text columns are allocated once and filled in place; each grammar name is interned once per table and its `CHARSXP` reused.
  - `grammar_action_failure` and the escape and placeholder checks scan the tree's kind column through `NodeRef` and return early when the kinds they look for are absent.
- **Correctness.** Ids and parents match GNU R exactly, not only the shape of the table: `parse__parse_data_full` covers the grammar's corners, and `fuzz_parse_data_matches_gnu_r` (`crates/oracle/tests/test_differential.rs`) parses `oracle::fuzz` programs with injected comments, blank lines and tabs. The fuzzer found that a trailing `#:` comment continued on the next line is one `ANNOTATION` node inside the statement, so its newline was replayed as insignificant; `annotation_newline_significant` decides it from the annotation's position.
- **Measured** (rlang's 73 test files, `parse(text =)` per pass): `keep.source = TRUE` 46 to 34 ms (GNU R 20), without parse data 15 to 14 ms (GNU R 9), callgrind 953M to 584M instructions. What is left is spread over the tree walk, the replay itself and string interning; a further step would emit parse data from the parser's own events rather than replay the tree.

## Stats distribution closures enter their routine directly

**Status:** built.

- **Previous shape.** `dnorm(x)` built a frame for the stats closure, forced its promises, evaluated `.Call(C_dnorm, x, mean, sd, log)` through the builtin call machinery, and resolved the routine there: about 0.9 µs per call against GNU R's 1.0 µs, so below length 10 the call, not the kernel, was the cost. Native entries lived in an unsorted `SmallVec` scanned linearly.
- **Recognition by shape, not hash.** `recognize_routine_closure` (`base_fast_paths.rs`) accepts a closure of the stats namespace whose body is `.Call(C_<routine>, ...)` with formals in order or `1/<formal>` (the exponential family's `rate`), optionally inside `{}` or `if (missing(f)) <call> else ...`, which applies only while `f` is not supplied (t, chisq, beta and F with `ncp`). Every default the plan may use must be a constant, and the routine and arity must be a row of `distributions::DIRECT_ROUTINES`. Stats bindings are lazy-load promises, so recognition runs in `do_lazy_load_fetch`; an image start re-recognizes closures already forced (`register_routine_entries`). A user's own `dnorm` is never in the stats namespace, so it is never recognized. Not recognized: the gamma family (its `rate`/`scale` consistency check), `dsignrank` (`on.exit`) and nbinom (`mu`).
- **Decline instead of signal.** A direct entry matches arguments itself (exact tags, then positions) and runs the kernel in `Conditions::Decline` mode: any warning (NaN produced, NAs produced, Rmath's own), any error, a non-numeric or ALTREP argument (coercion warns, ALTREP may run code), or a supplied `missing()` formal returns `None`, and the closure runs as before, so every condition reports GNU R's call and `sys.call()`. A random draw first takes `random::checkpoint`, which requires `.Random.seed` to be a plain integer binding equal to the live state and no user-supplied generator, and a declined draw calls `random::rollback`, so the R body redraws the same deviates.
- **Wrappers.** An argument that is a promise of a variable bound to another unforced promise (a wrapper's formal passed on, `function(n) rnorm(n)`) is read through, up to eight promises deep, when every promise in the chain settles without running R code; a chain that reaches itself (`function(n = n)`) declines so the R body reports GNU R's error. This holds for every native entry, by value and with promises built: `function(n) dnorm(n)` 799 to 435 ns per call.
- **Registry.** `Interp::native_entries` is a `Vec<(Sexp, Entry)>` sorted by address and searched by binary search; `Entry::Routine` carries the argument plan (`RoutinePlan`, at most six sources, inline).
- **Measured** (ns per call inside a closure, 1e6 calls; ry before, ry after, GNU R): `dnorm(x)` 932, 462, 1044; `pnorm(x)` 1017, 460, 1108; `qnorm(0.3)` 889, 582, 914; `rnorm(1)` 1286, 677, 2820; `runif(1)` 1234, 536, 2372; `rexp(1)` 1762, 548, 2042; `rpois(10, 3)` 1857, 963, 2271.
- **Evidence.** Oracle case `stats__direct_routine_calls.R` (named and positional arguments, warnings and errors with `sys.call()` in handlers, declined draws with NA parameters, masking, `1/rate` with integer and invalid rates, `ncp` present and absent, `.Random.seed` after every draw).

## Base `.Internal` wrappers inlined; string fast paths not kept

**Status:** built, except the rule that a replaced base binding always takes effect in compiled code, which "Compiled code binds GNU R's inlined base functions at compile time" supersedes.

- **Shape.** Tier 0 inlines GNU R's "safe base internals" (the first twenty of `INLINED_BASE_INTERNALS` in `crates/interp/src/bytecode.rs`, the list in GNU R's `cmp.R`) as `Op::Internal`. The call qualifies when its head is base's closure by identity, its formals have only constant defaults, and its body is exactly `.Internal(f(...))` over formals and literals with `f` an evaluating internal. It then compiles to an `Op::Guard` on the head, the internal's arguments evaluated in the caller, and the internal called directly. No wrapper closure runs, so `eval::call_inlined_internal` blames an error that names no call on the original call, and errors read `nchar(x, ...)` as in GNU R. The JIT treats `Op::Internal` as an opaque step, because `match` can run R code through `mtfrm`.
- **The guard is the existing one.** `Op::Guard` resolves the head through the site's lookup-cache slot and compares it with the closure seen at compile time, as every guarded base-builtin inline does; nothing new is cached. A global or attached definition, a local one, and one an enclosure gains after compilation each move the lookup, and a replaced base binding (`assignInNamespace`, `unlockBinding` plus `assign`) fails the comparison. So ry always reaches the replacement, where GNU R's compiled code keeps the original internal and only its interpreter reaches it. That divergence is the contract, not a bug to fix: GNU R's interpreter is the reference, and copying its compiled code would make the answer depend on the tier, so the oracle compares that section with GNU R's JIT off. `calls__inlined_base_internals` covers every R route, and `calls__inlined_base_internals_c_api` covers bindings that C code makes through `Rf_defineVar`, `Rf_setVar` and `R_MakeActiveBinding`.
- **What it buys.** Paired callgrind Ir against main: testthat_full -4.7%, records -2.0%, validation -1.7%, strings -0.5%, report -0.3%, almost all from `match` and `identical` (about 1.9k Ir per call saved).
- **Extended past GNU R's list (`INLINED_BASE_INTERNALS`).** A census of every base closure whose body is only `.Internal(f(...))`, counted by replacing each with a counting copy, found that the calls the safe list missed were mostly string builders in the string-heavy applications (report: 60k each of `paste`, `paste0`, `sprintf`; validation: 81k `paste`; records: 75k `character`) and frame readers in testthat (`parent.frame`, `sys.call`, `sys.function`, `environment`, about 17k of its 31k). The frame-free ones are inlined: the `vector` family (`character`, `logical`, `integer`, `numeric`, `double`, `raw`, which pass a literal mode), `paste`, `paste0`, `sprintf`, `startsWith`, `endsWith`, `make.unique`, `.row_names_info`, `isNamespace`. A wrapper with `...` takes the arguments R's matching gives it: exact tags first, positions into the formals before `...`, the rest into `...` in call order; a tag that partially matches a formal before `...` refuses the inline, as does `...` at the call site. The body passes them on as `...` (`sprintf`, refused when they carry tags, since `Op::Internal` passes none) or as `list(...)`, which base's `list` builds with their tags through `Op::CallBuiltin`. Measured per call in a loop: `paste` 8.6k to 5.4k Ir, `sprintf` 10.4k to 5.0k, `character` 4.8k to 2.5k. Paired Ir against main: report -28.5%, validation -8.4%, strings -4.1%, records -2.6%, testthat_full -0.8%.
- **The internal runs in base's namespace and reports the wrapper's call.** `Op::Internal` hands the internal `base_namespace` as its environment, not the caller's frame: the closure's frame would enclose base's namespace, so `paste` and `sprintf` dispatch `as.character` (and `match` `mtfrm`) from there, and a caller's local `as.character` or local method is not found, as in GNU R. `eval::call_inlined_internal` sets `assignment_call` to the original call (with the unbound marker as the replacement call, which no call equals) while the internal runs, so a warning reports `sprintf("%s", i, "extra")` rather than the enclosing function's call.
- **Left out on purpose.** The frame readers (`parent.frame`, `sys.call`, `sys.function`, `sys.parent`, `sys.nframe`, `sys.frame`, `environment(NULL)`, `eval`, `new.env`, `get`, `get0`, `exists`) read the context the wrapper's own call creates, so inlining them needs a per-internal frame offset, not this shape; together they are about 1.1% of testthat_full at 1.5k Ir per call. `options(...)` passes tags to the internal, which `Op::Internal` does not carry. `lengths` and `as.vector` (`as.name`) dispatch internally on objects. `lockEnvironment` returns invisibly, which `Op::Internal` does not keep. `file.path` has a non-constant default.
- **Not kept: string compare, `nchar` and JIT string ops.** A pointer-compare `==`/`!=` on cached `CHARSXP`s with equal encoding marks, an `nchar` that reads the stored length, and JIT `Str`/`StrVec` types cut a loop iteration's compare from about 2,400 Ir to 935 in tier 0 and 235 in the JIT, and `nchar` from 5,200 to 1,210 and 229 (an integer element read costs 791 and 91). Programs did not see it: the branch that carried them plus the inline above came to the totals listed, and the string ops' share was at most strings' 0.5%. A loop's string work is mostly the element read, and the const `STRING` element paths in `element_access.rs` had already made that fast, so the compare and `nchar` left little to take. Revisit only with a workload whose profile shows string compares or `nchar` on top.

## The serialize reader checks what a stream claims before it stores it

**Status:** built.

- **Previous shape.** `Reader` in `crates/interp/src/serialize.rs` trusted the stream, as GNU R's reader does. It allocated a vector of the length the stream gave, indexed the byte-code reference table with the stream's index, recursed once per nesting level, and stored whatever item came next as a symbol's name, a string's element, an attribute list, an environment or a binding's name.
- **What was weak.** The heap takes an object's kind from the position that holds it, so a stream with another kind there left objects the collector and the accessors misread, and the damage outlived the read: one stream corrupted a collection during a later one. A made-up length asked for gigabytes, a reference index read and wrote outside the table, and deep nesting overflowed the stack. The libFuzzer target `interp_unserialize` found each of these in its first minutes of running.
- **Target shape.** The reader refuses the stream with an R error at the first claim it cannot back. A length must fit in the rest of the input (`Reader::expect_elements`, for a read from memory), a reference must name a slot of its table (`reference_slot`), and each level of nesting runs through `on_large_stack`. An item read for a position that implies a kind is checked: `Reader::character` (a symbol's name, a string's element), `environment`, `attributes` (`NULL`, or a pairlist that ends in `NULL` and whose cells are named by symbols), `tag` (a cell's tag is `NULL` or a symbol, because calls and argument lists are read by tag without a check), `named_cells` for a closure's formals and `fill_frame` for an environment's bindings. `named_symbol` guards every read of a symbol's name, because the marker for an unbound value is a symbol without one. Three rules keep a stream out of runtime state: a promise keeps only the two bits GNU R writes (ry's other promise bits name a slot of a live frame), a promise without a value needs an environment, and a `wrap_*` ALTREP object must wrap a vector of its type, since the stream's attributes are set on the result and `NULL` or an environment is shared by the session. A byte-code reference table slot is filled after its cell is read, not before as in GNU R, so a cell cannot contain itself and every list a stream builds ends. A closure without an environment gets the base environment, as in GNU R. `crates/rdb` and `crates/libr` have their own readers of the format, which this record does not cover.
- **Impact.** `unserialize()` and `readRDS()` on a damaged or hostile file fail with an error. GNU R reads a cell whose tag is not a symbol and fails later or never, so such a stream, which only C code can write, is refused here where GNU R accepts it. `crates/interp/tests/test_fuzz.rs` pins a stream for each check: `unserialize_refuses_an_object_of_the_wrong_kind` asserts that each is refused and that every stream GNU R wrote for libR's conformance cases reads (all but the two that need base R), under collection stress, where the pair of streams the fuzzer found aborts the test without the checks. `unserialize_keeps_a_stream_out_of_the_runtime_state` pins the promise bits and the reference table. Each check is a comparison per item, which a lazy-load fetch also pays.

## sprintf formats numbers with the native kernels

**Status:** built.

- **Previous shape.** `do_sprintf` handed every numeric conversion to a `format!` call built from the specification, parsed each format once per row into a `Vec`, and kept a separate `Vec` of argument kinds. On the report workload `sprintf` was 15.8% of the instructions, about 4,340 per scalar call.
- **Target shape.** `%f`, `%e`, `%g` and the integer conversions write through `format.rs`'s `push_fixed`, `push_scientific` and `push_general`, the same kernels `format` and `paste` use, so ry has one native number formatter. All formats parse once into one flat `SmallVec` of pieces with per-format end offsets, an `n$` or `*` reference is an `Amount::Argument` index, and the `%d` check on a double ("whole and in range, or NA") runs once per argument. A scalar call costs about 2,180 instructions, of which 607 are `mk_char` (the heap's).
- **Rounding is printf's, not `format`'s.** GNU R hands each conversion to glibc's printf, which rounds the exact binary value (half to even on a true tie), so `sprintf("%.2f", 0.125)` is `0.12` where `format(0.125, nsmall = 2)` is `0.125`. The sprintf kernels therefore use `fixed_fast` (exact through `fma`) or Rust's exact formatting, and never the long-double power table that `format` emulates.
- **glibc behavior matched on purpose.** A negative `.*` precision is echoed as the unparsed specification (`%.*f` with -1 prints `%.0-1f`). `%#g` whose rounding carries to the next power of ten prints `1.e+02`. `%#o` adds a leading 0 only when the digits do not start with one. Type errors print the specification with `n$` removed and `*` replaced by its value, as GNU R does.
- **Contract.** `crates/oracle/cases/base__sprintf_conversions.R` covers each path, and `fuzz_sprintf_matches_gnu_r` (`crates/oracle/src/fuzz.rs`, `sprintf_program`) compares random format strings and argument types against GNU R, with a bounded pass in `cargo test -p oracle`.

## Hash tables for `unique()` and `match()` stop growing with the elements at 256 KB

**Status:** built.

- **Previous shape.** `PositionTable` in `crates/interp/src/sort.rs` reserved two `u32` slots per element up front, so `unique()` of a million strings with four distinct values zeroed and probed an 8 MB table. A character vector reached the table only after a scan of every element's ASCII flag, duplicate flags were a `Vec<bool>`, and `anyDuplicated()` flagged every element before reading the first. `split()` copied element by element through `copy_element`.
- **Target shape.** The table starts with two slots per element, capped at 65,536 slots (256 KB), and doubles when half full past the cap, so a long vector with few distinct values zeroes 256 KB once and touches only the cache lines its keys hash to. Starting at 32 slots and always doubling was tried and dropped: rehashing and re-zeroing through each doubling made `match()`, `duplicated()` and `anyDuplicated()` on 5,000 distinct strings cost about 100 more instructions per element than the presized table, and slowed `merge()` by about 10% through `cbind()`'s check of `make.unique` row names. Strings hash by their interned object, which is exact only for ASCII text (equal text in latin1 and UTF-8 is two objects), so `TableKey::exact` is checked on each distinct key and each miss, and the first inexact one sends the call to the general path. Duplicate flags are a bitset built one register word at a time, and `anyDuplicated()` stops after the first word that holds a duplicate. `split()` counts per level, allocates each piece once and scatters typed slices through per-level cursors held on the stack.
- **Why not a faster logical gather.** A 64-flag mask walk and a branchless compaction both cost more instructions than the filtered position walk on the sales workload, whose selection branch predicts well, so `gather_kind` keeps it.
- **Impact.** Per call on a million elements, `split()` of doubles drops from about 121M to 69M instructions and `match()` of strings from 51M to 37M. On the application suite in plain release builds, the sales workload's warm iteration drops from 299.5M to 237.2M instructions (-20.8%) and its cold run by 11.8%; the other fourteen workloads move by less than 0.2%. The table is a local, freed when the call returns, so peak anonymous RSS does not rise: sales goes from 96.1 MB to 89.1 MB and tables, testthat and validation stay within run-to-run noise (medians of five runs).
- **Pinned by** the oracle cases `subset__logical_index_gather`, `base__split_factor_kinds` and `base__unique_match_kinds` (NA keys, latin1 against UTF-8 text, `incomparables=`, `fromLast=`, unused factor levels, multi-word early stops).

## Positional builtin calls read their arguments from the registers

**Status:** built, except the values-plus-parallel-tags closure call shape.

- **Previous shape.** Every `CallBuiltin` walked the call's pairlist to pair each register with its tag, collected the pairs into `eval::Args`, and went through `bytecode::call_builtin` (which tried S3/S4 dispatch first) and `eval::call_builtin_reusing`, which checked an activity flag, a profile-tick flag, the arity, the names and the replacement target on every call.
- **Target shape.** Whether a site is positional (every argument cell untagged, none empty, and as many cells as registers) is a fact of the call expression, not of the builtin bound at run time, so the compiler decides it: `emit` writes the handler index `POSITIONAL_CALL_BUILTIN` into the opcode word, and `op_of` reports it as `CallBuiltin`, so the JIT, its step table and code images see no new opcode. The verifier accepts that word only where `positional_call` holds for the call constant. Its handler writes up to eight `Arg`s with nil tags straight into a stack array (`eval::with_positional_args`, which `jit::runtime::jit_call` shares), tries dispatch only when an argument is an object or an S4 value (or the builtin is a matrix product, whose argument-count error dispatch raises), and calls `eval::call_builtin_positional`, which skips the names check. The generic handler hands a positional word to the positional handler, because the optimizing tier's step table routes every `CallBuiltin` word to it.
- **Why compile time and not run-time quickening.** The start-up image's code is mapped privately, so writing a quickened opcode at run time copies each touched page into anonymous memory in every process (+130 to +230 kB RssAnon on the benchmark workloads). Deciding at compile time writes no code page at run time and costs nothing per call, which a side array of quickened state would not. A rule that needs the bound builtin would have to quicken when the image is built instead.
- **One hooks byte.** `heap::activity::HOOKS` holds the activity-summary and profile-tick bits, so the common call tests one byte, which replaces the separate flags rather than adding to them.
- **One argument shape across the call boundary.** Builtins and closures agree on positional values plus parallel tags (empty when the site has none) as the shape a call boundary takes. The builtin interface `Call { args: &[Arg] }` is unchanged for now; the closure protocol adopts the values-plus-tags shape first.
- **GC liveness.** The stack array only copies registers `first..first + count`, which stay rooted and unchanged until the instruction writes its result, so it needs no root set of its own.
- **Impact.** Warm instructions per iteration fall 1.25% on testthat_full, 5.2% on bond_book_full and 1.3% on validation_full, with no measurable compile cost and no growth in anonymous memory. Pinned by the oracle case `builtins__positional_and_tagged_calls`.

## A warning reaches `suppressWarnings` and `tryCatch` without running R

**Status:** built.

- **Previous shape.** Once any handler was established, `conditions::warning_call` evaluated `.signalSimpleWarning(msg, call)` in R: `withRestarts` built the `muffleWarning` restart, `withOneRestart` and `doWithOneRestart` added three frames, the condition object was allocated, and `suppressWarnings`'s handler ran `inherits` and `tryInvokeRestart`, which unwound to the restart. That came to about 72,000 instructions per muffled warning, about 12% of `team_scores` (`suppressWarnings(as.integer(x))` over 14,000 strings).
- **Chosen shape.** Before that hook, `muffled_by_suppress_warnings` reads the topmost handler for `simpleWarning`, `warning` or `condition`. The warning returns without running anything when that handler is a calling handler with formals `w` and body exactly `if (inherits(w, classes)) tryInvokeRestart("muffleWarning")`, whose environment is a frame directly inside the base namespace that binds none of `if`, `inherits` and `tryInvokeRestart`, and whose `classes` is a character vector naming one of the three classes. Those conditions make the handler behave exactly as the stock one, whoever built it, so a user handler written the same way in the global environment, or in a frame that rebinds `inherits`, takes the R route. Recognising the handler as the function literal inside `base::suppressWarnings` does not work: the start-up image does not keep a closure's body and its compiled code sharing one object, so that identity test failed on every warning in a release build.
- **Why it is unobservable.** That handler has no effect but the unwind, so skipping it is invisible unless something sees the frames or the condition: only handlers that run do, and the muffled path runs none. A `classes` promise is read without forcing unless its code is a string literal, so an expression with side effects (`classes = f()`) takes the R route and is forced there, once, as in GNU R. Handlers below the muffling one never see the warning in either route, and `last.warning` and `warnings()` are untouched because the warning never reaches the top level. `options(warn = 2)` converts only at top level, after the handlers, so it does not apply either. The fast path follows `RY_FAST_PATHS`.
- **Exiting handlers.** When the topmost handler for the warning is an exiting one (`tryCatch(warning =)`, `tryCatch(condition =)`), `warning_call` builds `simpleWarning(message, call)` natively and jumps to its `tryCatch`, as `signal_condition` would. The handler runs after the unwind, so the frames and the `muffleWarning` restart of `.signalSimpleWarning` are gone before it sees the condition. With no handler, the default handler runs natively unless `options(warn = 2)` holds, because the converted error's calling handlers and traceback see the R frames. Every other case (a calling handler that is not `suppressWarnings`'s) takes the R route.
- **Limit.** A rebound `base::inherits`, `base::tryInvokeRestart` or `base::simpleWarning` is not detected; the R route would call the rebound function.
- **Impact.** A muffled warning costs about 68,600 fewer instructions (plain release, warm callgrind against main): `team_scores`, whose 60,000 rows include about 3,500 muffled warnings, drops 10.2% in instructions and from 0.383 s to 0.354 s median wall. `conditions` drops 0.3% and `testthat` 0.1%. 20,000 muffled warnings plus 20,000 caught by `tryCatch(warning =)` take 0.34 s instead of 0.49 s. The `withCallingHandlers` setup on every `suppressWarnings` call remains and is the larger share of its cost. Pinned by `crates/oracle/cases/conditions__suppress_warnings_muffle.R` and `conditions__warning_signal_routes.R`.

## `%%` and `%/%` on doubles port GNU R's kernels, with a 64-bit path for whole numbers

- **Previous shape.** `real_modulo` and `real_integer_divide` (`crates/interp/src/arith.rs`) approximated GNU R's `myfmod` and `myfloor`: a software `fmod` for every pair, a "divisor too large" shortcut at 2^52, no branch for a quotient below one in `%/%`, and NaN operands passed through as the double arithmetic left them.
- **What was wrong.** On a grid of 47 by 47 special and ordinary values (2,209 pairs, each computed by the scalar path and the vector kernel), 311 pairs differed from GNU R 4.6.2 in at least one bit of `%%` or `%/%`. Examples: `1 / (-6 %% 3)` gave `-Inf` (GNU R `Inf`), `1e-300 %/% -1` gave `0` (GNU R `-1`), `1e-300 %% -2^53` gave `-2^53` (GNU R `0`), and `NA_real_ %% 1` returned a signalling NaN. The software `fmod` was also the cost of every whole-number remainder, which is the common case in date and index arithmetic.
- **Target shape.** Both kernels are ports of GNU R as it runs with an x87 `long double`. Two whole-number operands below 2^53 divide as `i64` (`whole_division`), which is exact and gives the `+0.0` GNU R gives for a zero remainder. Every other pair follows GNU R's order of tests: a zero divisor, NaN operands (`extended_nan` reproduces the x87 choice between two NaNs, so `NA` survives against `NaN` on either side), an infinite dividend, the shortcut for a divisor above 2^63, then the correction in extended precision through `numeric::LongDouble`. The scalar path, the vector kernels, tier 0 and the JIT's runtime helpers all call these two functions. Typed ry's `F64` remainder and quotient call them as well (`crates/typed/src/backend.rs`, `values.rs`); only its integer remainder is its own.
- **Contract.** `crates/oracle/cases/builtins__modulo_bit_patterns.R` prints the bit pattern of both results over a 37 by 37 grid and checks that the scalar and vector kernels agree.
- **Measured** (hot suite, callgrind instructions per call, `iterate` build): `PerformanceAnalytics::table.AnnualizedReturns` 41.63M to 39.82M (-4.4%), `SharpeRatio.annualized` 26.83M to 25.63M (-4.5%), `Return.calculate` 5.91M to 5.82M (-1.5%), `rmgarch::dccspec` 2.00M to 1.97M (-1.3%). On the weighted totals: finbench -1.3%, dsbench -0.1%, biobench -0.1%. The code bases were not run for this change, because the predicted change is far below what one wall-time run resolves.

## `validObject` runs validity methods without its slot loop, and `new()` copies the prototype shallowly

**Status:** built.

- **Previous shape.** `validObject`'s native check (`ryS4Valid`) answered only when no validity method had to run. A class with a validity method ran the whole R body, whose slot loop wraps every slot check in `try()`: for a six-slot class, six `try`/`tryCatch`/`withCallingHandlers` frames per object, each a closure that outlived its frame. `new()` made a deep copy of the class prototype for every object, so a prototype slot holding a million doubles cost about 1.7 ms per `new()`.
- **Target shape for `validObject`.** When every slot passes the native check, `ryS4Valid` binds in `validObject`'s frame what the R body has bound once its slot loop finishes (`Class`, `classDef`, `where`, `errors`, `slotTypes`, `slotNames`, `attrNames`, `idx`, and the loop's last `i`, `classi`, `classDefi`, `namei`, `sloti`, `ok`) and returns 2. The prologue in `crates/interp/src/methods_core.R` then runs GNU R's statements after the loop, copied verbatim, so superclass and own validity methods run from the same frame, with the same `sys.call()`, `parent.frame()` and error, and `test = TRUE` works. The check declines (the R body runs) on `complete = TRUE`, a slot it does not model, a data part as the last slot, or a class whose package namespace it cannot resolve. Running the tail through `eval::eval` was rejected: the statements then ran on the AST evaluator, slower than compiled R.
- **Target shape for `new()`.** `new()` (`do_new`) and the `C_new_object` routine copy the prototype with `shallow_duplicate`: the object and its attribute list are new, and slot values and list elements are the prototype's. This is sound because interpreted code changes a value in place only when it holds the only reference, and the prototype holds one, so a change to a slot of the new object copies that value first. `R_do_new_object` in libr stays deep, because package C code may write into a slot it got from `NEW_OBJECT` without checking.
- **Contract.** `crates/oracle/cases/s4__validity_fast_path.R` pins validity results, messages, condition calls and what a validity method sees of its frame. `s4__new_copies_prototype.R` pins that changing a slot (by `@<-`, `slot<-`, `attr<-`, `[[<-`, a nested S4 slot, a list or vector data part, or an `initialize` method around `callNextMethod()`) leaves the prototype and other objects unchanged, and that an environment slot is shared.
- **Measured** (release build, shared machine). A 2,000-iteration `new()` loop on a class whose prototype holds `runif(1e6)` took 3.4 to 3.9 s and takes 0.009 to 0.014 s (GNU R 4.6.2: 9.7 to 10.0 s); a small two-slot class is unchanged at about 5 µs per `new()` (GNU R about 50 µs). On a 21,000-object kernel with a six-slot class and a validity method, the `ryTryCatch` calls fell from 126,090 to none, frames a closure outlived from 189,198 to 63,192, and ry's time from about 2.6 s to about 1.6 s (GNU R about 3.8 s).
- **Left open.** `standardGeneric` still turns the generic's slot frame into an environment for every dispatch (`dispatch::do_standard_generic` reads `call.env()`, and `methods::exec_method` copies the generic's binding cells into the method's environment). `new`, `initialize` and `validObject` each get an environment because their prologues pass `environment()`.
- **Accepted risk.** GNU R's `new()` copies the prototype deeply, so C code that writes into a prototype-valued slot of an argument without checking `MAYBE_SHARED` happens to work there. On ry that write changes the class prototype. Such code already breaks on GNU R when the slot was set from a shared value, and the alternative costs a copy of every prototype slot per object.
