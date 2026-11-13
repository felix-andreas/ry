# Memory

The knowledge base of the agents building ry, in three horizons. **Short-term** holds the current focus and loose ends, and an item is deleted once it is resolved. **Mid-term** holds active priorities, open bugs and technical debt. **Long-term** holds durable, non-obvious decisions and traps with their reasons. Every entry is context-free and timeless, and points at code or docs instead of restating them. Measurements and long explanations belong in the companion files, never here.

- `backlog.md`: the prioritized punch-list of open work, with a ledger of shipped work.
- `decisions.md`: settled decisions and why. Every entry carries a `**Status:**` line (built, planned or superseded).
- `test-user-reports.md`: closed findings from simulated-user testing.
- `worklog.md`: one line per cycle, appended by a routine.
- `.agents/orchestrator.md`: the brief for the orchestrating session only. Every other agent ignores it.

The specs live in the docs site under `docs/src/content/docs/`. `reference/type-system.mdx` is the typing contract, `guide.mdx` introduces it, and `development/` covers the internals (`index`, `architecture`, `structure`, `testing`, `robustness`, `corpus-and-oracle`, `releasing`, `authoring-stubs`). Unsettled design work goes to `development/design/`, listed on its index page and kept out of the sidebar, never here.

## Short-term

- **Vector kernels.** Toward 10x on element-wise code, the next steps are fusing `sum`/`mean` consumers into the fused pass and an AVX-512 `exp` (`crates/interp/src/kernels.rs`, `vector_math.rs`; `backlog.md`, "Open: untyped R against GNU R").
- **Typed core.** Next are non-constant defaults and box conversion at the boundary (`backlog.md`, "Open: the typed core", with its parts "Gaps the typed public ports found" and "What the `native` check pins and over-reports"). Every program of the public suite has a typed port that runs natively; `typed_benchmarks_run_natively` covers only the kernels and applications, because the public ports run full-size inputs, so check a public port with `RY_TYPED_LOG=1` on a release build. `RY_TYPED_LOG=1` names every typed closure whose call fell back to the interpreter, and `RY_JIT_LOG=1` names each compiled body, each native function, and the instructions left to tier 0.

## Mid-term

Most important first. Each line names the open item, and the detail is at its pointer into `backlog.md`.

**Runtime correctness and compatibility**

- Failures main already has: "Open: broken on main".
- Open bug: with `RY_JIT=force`, compiled code misses a write that an argument promise makes to its caller's variable, so `crates/oracle/cases/environments__code_argument_references.R` fails in force mode ("Open: the JIT").
- Runtime gaps, among them annotations at run time, graphics ("Graphics gaps"), the corpus packages' own tests ("Corpus package tests: what still fails") and GNU R's regression tests: "Open: the runtime".
- The three benchmark code bases pass every generated test, and their per-test time trails GNU R's. Close that gap, then run `ry check` over the generated code for false reports ("Open: the benchmark packages under ry's runtime").
- The Windows release job fails while building ry's own base R ("Open: the Windows release build").
- The methods package's core moves to Rust, and the next step is a native class table with dense ids ("Open: native methods core").
- Package loader gaps: "Open: the package loader". `RY_FAST_PATHS=0` covers three prologues only ("Open: one switch for every fast path").

**Untyped performance**

- The target is 10x GNU R on one thread, and 1.5x is the floor for every workload. The work is the "Untyped performance roadmap" table in `development/design/r-runtime.md`, one row per idea in build order, so update a row's status with the change that builds it. Raw measurements: "Open: untyped R against GNU R".
- Builtin fast paths (roadmap row (a) of `decisions.md`, "The road to 3x") are in progress: `x[[i]]`, `x[[i]] <- v`, `x[i] <- v` and `.Call` cost more instructions than GNU R's, and closure frame setup dominates `structure()` and `format.default` ("Open: untyped R against GNU R", "Open: performance and memory").
- Closure calls and promises: widen promise elision past effect-free callees only after measuring how often the eager path fires ("Open: untyped R against GNU R").
- JIT: tier 1 runs more instructions than tier 0 alone on `testthat_full`, inlining covers effect-free callees only, `x <- f(x, ...)` is inlined only when `x` is proven plain, and invalidated machine code is never freed ("Open: JIT on against JIT off", "Open: the JIT").
- Collector: some builder peaks sit above GNU R's, and the collector's share of the three benchmark suites is unmeasured ("Open: collector throughput on the benchmark suites").
- Kernels slower than GNU R: "Open: base R kernels against GNU R" (`just bench-base`) and "Open: standard library rows slower than GNU R" (`just bench-stdlib`).

**Typed ry**

- Typed ry grows by steps that `just climb` scores on speed against Rust, friendliness cases and soundness fuzzing (`development/design/language-climb.md`, with open targets under "Open questions"). Taste questions go to the orchestrator in one batch.
- Language gaps: literals, record and tag payloads and empty lists outside elaborated code (`ry load`, `source()`), f-strings, `..rest` patterns, and a literal accumulator through a generic call ("Open: the typed core").
- Typed code that builds vectors runs at untyped speed ("Typed natives, the rest of the end shape", under "Open: the JIT").
- `DataFrame` beats data.table on every db-benchmark question at 1e7 rows; what is open is the 1e8 memory margin, the reference counts some calls leave on a frame, and API gaps ("Open: DataFrame").
- Records, tuples, tags and the standard library: "Open: findings from the records, library and frame round" and "Open: ry's standard library".
- Strict mode: friction from typing the benchmark code bases, the cost of a first strict `ry load`, and the top-level rule's follow-ups ("Open: strict-mode friction from typing finbench", "A first strict `ry load` costs the strict check's wall time", "Open: strict mode's top-level rule"). The typed benchmark code bases check clean in strict mode, so a template change that makes one report is a regression or a new friction item.

**Analysis and editor** (ranks behind the reimplementation, and matters as the semantics contract)

- False reports on correct R: "Open: what `ry check` still reports on the GNU R corpus" and "Open: false errors on real CRAN code". Measure on packages the corpus has not mined, because the corpus is fitted, and delete the matching line under Limitations in `reference/type-system.mdx` with each fix.
- A cold check of a 300,000-line code base sits at the edge of its 1 s target with a flat profile ("Check cache follow-ups", "Benchmark code bases (measured)").
- Data-oriented layouts and explicit allocators: the ranked audit is "Open: data-oriented layouts and explicit allocators".
- Adoption findings, each with a minimal repro: "Open: what the adoption reviews left".
- Editor: "Open: editor parity" and "Open: editor latency at 300,000 lines". `ry debug analysis-stats <path>` measures hover, definition, references and rename coverage.
- Robustness: harness gaps in "Open: robustness harnesses", and test debt in "Open: test and fuzz architecture", whose "Judged fine" list is deliberate.

**Releases and operations**

- Releases stay on 0.4.x and focus on untyped R (speed, memory, compatibility) until the typed language settles and ry is significantly faster, and the user says when a release is cut (user decision). Procedure: `development/releasing.md`.
- After each release, append rows to the progress dashboard (`releasing.md`, "The progress dashboard"). Its `cran_popular` dataset is empty ("Open: the CRAN package corpus").
- The Zed extension is not in Zed's registry, and the `VSCE_PAT` fallback for the VS Code Marketplace stops working on December 1, 2026 (`releasing.md`).
- CI runs only on manual dispatch (user decision), and the orchestrator starts it on main when main has new commits (`decisions.md`, "CI runs in the flake's dev shell").
- `.github/pending/test.yml` (the tests that need no GNU R and a 20-second pass of every fuzz target that builds without Polars, on each push to main) and the release workflow's dependency on it wait for the user's `git pull && just accept`. Accepting `test.yml` as staged overrides the user's decision that CI never runs on push, for those tests only; deleting its `push:` block first keeps the decision and still gates releases (`decisions.md`, "A push to main runs the tests that need no GNU R, and a release fails on them").

## Long-term

### Runtime: heap, C API and native boundary

- **Root a handle nothing else roots before allocating** (`heap.root`, or the `keep` list of `Heap::allocate`). Only collection stress shows a miss: run `test_fuzz_native.rs` with a large `FUZZ_ITERS`.
- **Rust calls native code only through a C trampoline in `native.c`, and API callbacks run from C**, so a `longjmp` or C++ exception never crosses a Rust frame. An exported API function runs its body through `boundary::enter`.
- **Mark a C function that native code calls `RY_EXPORT` in `native.c` (data: `RY_EXPORT_DATA`).** rustc hides C symbols in a cdylib, and `build.rs` exports only the marked ones.
- **ry's headers declare and `libR` exports every name in `crates/libr/tests/headers/r_api_names.txt`** (`decisions.md`, "GNU R's public C API is a checked list, not a measured subset"). A libr test package must also build under gcc 14 and load into GNU R 4.6, so its C declares a non-API entry itself, with a parenthesized name (`SEXP (ATTRIB)(SEXP x);`).
- **`LEVELS` reports growability as GNU R's gp bit `0x20`** (`gp::GROWABLE`): packages built before R 4.6, data.table among them, test the bit and otherwise fail with "attempt to resize a non-resizable vector".
- **An ALTREP header holds no length**: read it with `libr::altrep::length`, never `Sexp::length`, which returns garbage.
- **Native code builds calls with `LCONS`, so argument cells are `LANGSXP`**; `libr::eval::normalize_argument_cells` retypes them before the interpreter walks a call.
- **libR's mimalloc needs `local_dynamic_tls` (the default TLS model fails `dlopen`) and the `v2` allocator** (v3 crashes when a second thread allocates after the first exits).
- **GNU R's default-package libraries (`methods.so`, `stats.so`) can never load on ry's heap**, because they inline R's object layout. Their routines are Rust (`builtin_libraries.rs`).
- **On macOS `dyn.load` opens a re-signed copy of a package binary, rebound to ry's libR** (`macho.rs`; `decisions.md`, "macOS packages bind to ry's libR").
- **A fast path that reads vector storage directly must refuse a vector with `flag_bits::ARRAY`**, or it skips the strict checks in `arrays.rs`. `duplicate` drops the flag (re-flag with `arrays::finish`), and `finish_result` copies a shared constant.
- **gp bit 12 is `TYPED_DEFINITION` on a formals list or body and `CHECKED_PROMISE` on a promise.** `annotations::leave` runs at every closure exit, error or not, or a pending annotation check outlives its frame.
- **Create a binding outside a function frame through `Heap::define`** (remove with `remove_binding`, re-parent with `set_enclosure`): they move the binding epoch that bytecode inline caches trust.
- **Environment listing order is derived, not stored** (`heap::env::Frame::listed_cells`): newest first, a closure's formals last, hashed frames in GNU R's bucket order, which depends on removals too.
- **Start-up state the environment can change is re-derived in `interp::session::start_from_image`**, since the image is written once in an emptied environment (`image::LOADING_VARIABLES`, `image::save_state`). Run `just start-image` after editing runtime R sources (`development/architecture.md`, "The start-up image").
- **Start-up and code images are keyed by build**, so builds sharing a base R tree coexist and a build's first start writes its own. `RY_IMAGE_LOG=1` names each skipped image, and `bench.py` fails a timed process that reports one.
- **`read_renviron` (`interp::session`) appends GNU R's user and site libraries to an unset `R_LIBS_USER`/`R_LIBS_SITE`**, skipping any that holds `base`, which would shadow ry's base packages (`reference/cli.md`, "How ry finds R").
- **`Sys.getenv`, `Sys.setenv` and `Sys.unsetenv` read `environ` once per call; never keep a private copy.** Writes go through libc so C code sees them (`decisions.md`, "Environment variable calls read `environ` once").
- **`crates/crash` owns the SIGSEGV/SIGBUS/SIGABRT handler, installed first in `ry`'s `main`.** `image.rs`'s touch recorder and Rust's stack-overflow handler are the only other SIGSEGV users, and a new one must chain as they do.

### Runtime: interpreter, bytecode and JIT

- **Read a closure's body through `bytecode::closure_body`, never `function.cdr()`**: a body that ran twice is replaced by its compiled `BCODESXP` (`decisions.md`, "Closures over one body expression share its compiled code").
- **A tier-0 fast path makes no calls, and the generic handlers and `jump` stay `#[inline(never)]`** (`bytecode.rs`). Any call makes the handler save registers on every instruction. Use `Heap::try_alloc_scalar`, `become` the generic handler, and check with `objdump -d` that the handler starts without `push`.
- **A quickened opcode is visible only to tier 0's dispatch**: every other reader of code words goes through `op_of`, never a comparison with `op_word`.
- **Every JIT helper that can allocate releases its handles** (`scoped`, `crates/jit/src/runtime.rs`). A compiled loop has no backward jump where tier 0 would release them, so a miss keeps every boxed scalar alive until the call returns.
- **Compiled code writes variables back before anything that can read the environment and forgets them after anything that can write it**, warnings included. Run the `ry-jit` oracle engine (`RY_JIT=force`) and `test_tiers.rs` after changing either tier.
- **A JIT activation's pending unwind is a collector root through `Interp::held_unwinds`**, because a deopt exit allocates before tier 0 raises it (`run`, `crates/jit/src/jit.rs`; oracle case `conditions__exiting_handler_from_compiled_code`).
- **A native port of a package function sits behind a hash-guarded prologue and declines whatever its R body could observe that the port does not model.** A new one is a zero-argument `.Internal` marked `BuiltinDef::prologue`, with an oracle case against the R body (`decisions.md`, "Package code that dominates a workload runs natively: rlang's trace_back").
- **A methods-package fast path is exact or invisible** (`methods_core.rs`): it reads arguments only through `quiet_value` and bails before any effect R code could observe. `RY_PRINT_FAST_PATH_HASHES=1` prints the `FAST_PATHS` hashes after a GNU R upgrade.
- **The methods package's R code relies on native quirks** (`methods.rs`): `R_set_slot` modifies in place, `.allowPrimitiveMethods(FALSE)` must turn primitive dispatch off or selection recurses, and an argument is `"missing"` only when its own binding is missing.
- **S4 methods are rewritten at install time (a `.local` wrapper in the lazy-load database)**, so a fix to what `setMethod` runs needs the package reinstalled (`oracle install <pkg>`). `length<-` returns the object itself when the length is unchanged, because `conformMethod` compares with `identical`.
- **`missing()` counts a defaulted formal as missing only in its own function**, as GNU R's `R_isMissing` does (`eval::is_missing`); methods' `registerNames` depends on it.
- **`ry run` and the console parse the whole input but lower each top-level expression just before it runs**, because `options(keep.source = TRUE)` affects the expressions after it (`interp::session`).
- **A parse error inside a `#:` comment is not an R syntax error** (`SyntaxError::in_annotation`, code `annotation`). The runtime's parser and the console's completeness test skip it, or a malformed comment stops a script GNU R would run.
- **`sys.call()` and `sys.calls()` copy only a frame's call cells** (`envir::call_with_srcref`): `do.call` puts values into the call, and data.table's `cedta()` runs `sys.calls()` on every `[`. `test_call_stack.rs` counts allocations.
- **A text output connection's target vector is shared with its binding**, so `push_text_line` copies when `maybe_shared` and `textConnectionValue` returns a copy (`connections.rs`).
- **`perl = TRUE` patterns go through `crates/interp/src/pcre.rs`, never the `pcre2` crate's `Regex`**, which revalidates the subject on every match and makes a global `gsub` quadratic.
- **Polars computes `NaN` where R says `NA`** (`cor` over a constant group), so each IR lowering that can produce `NaN` decides which R means (`crates/table/tests/test_table.rs`).
- **Code translated from GNU R lives only in a GPL crate, under a provenance header that `just notices` reads.** A UPL crate never depends on `interp`, and `crates/libr/include` (LGPL) must not copy R's GPL-only `Rinterface.h` or `Rembedded.h` (`reference/licensing.md`).
- **A behavior GNU R changed between releases is gated on the emulated release** (`interp::system::r_at_least`, or `r_at_least_release` for a patch release), because ry runs whichever base R its home holds. NEWS omits most message changes, so find the release in the r-source tags (`raw.githubusercontent.com/wch/r-source/tags/R-4-5-0/src/main/...`).
- **The base-closure fast paths install only on GNU R 4.6.2's code**, because each prologue is guarded by a hash of the R body it replaces. A home that holds another release's base R leaves most of them out, so a container with GNU R 4.3.3 as its reference never runs them against GNU R: check them with a GNU R 4.6.2 as `ORACLE_R` and a library that release installed as `R_LIBS`.

### Numerics

- **Numeric ports match GNU R's last bit, so port the routine, not the formula**: keep the 4.6.2 evaluation order (`x**3` is `(x*x)*x`, sums left to right) and find a difference with a grid sweep before reading code. A residual few-ulp gap usually sits in `crates/numeric/src/linpack.rs`.
- **Translate a C `do { ... } while (a > b)` that can meet NaN as `if !(a > b) { break }`**, never `if a <= b { break }`, which spins forever on NaN (`rmath.rs`).
- **An OpenBLAS result can depend on its buffer's address modulo 16**, so every BLAS and LAPACK buffer in `linalg.rs` goes through `Placed`. Reproduce with `OPENBLAS_CORETYPE=Zen` (`decisions.md`, "Decision record: base R's linear algebra").
- **BLAS and LAPACK are found by soname, then through GNU R's `etc/Makeconf`, then as ry's reference copies** (`library_candidates`, `linalg.rs`), which `open_reference_blas` opens `RTLD_LOCAL` before the first package `dlopen` so a package's `NEEDED libRblas.so` resolves without interposing.
- **Last-digit output follows the C math library and FMA contraction.** libr test packages build with `NO_CONTRACTION`, and oracle cases that print such digits carry `# oracle: library-dependent` (`decisions.md`, "The oracle suite names the GNU R build it was recorded with").

### Analysis performance

- **A per-item query never scans a per-file or per-project list**: compute once per file in a memoized query and probe per item (`item_spans` is the one source of item identity). Vary both items and names when measuring.
- **A per-item read that joins over a project-wide list needs a cap, not just memoization** (`CONDITIONAL_SLOT_JOIN_CAP`, widening to `Unknown`).
- **Never call `Checker::infer` twice on one expression.** It has no memo, so a double walk costs 2^depth on operator chains and repeats findings, which the deduplicating fixtures hide.
- **Hot tracked queries returning maps or vectors use `returns(ref)`**, and `GlobalEnv` facts are borrowed `&'db` values; `returns(clone)` deep-clones per call.
- **A type is a hash-consed DAG: walk it with a memo, never as a tree** (`substitute_rigid`).
- **A composite type past `TYPE_SIZE_CEILING` records as `Unknown`** (`Checker::record`), or a record whose fields return that record grows tenfold per level. `types::type_size` counts paths, not nodes.
- **A query that reads a syntax tree keeps a small memoized result and is warmed in `diagnostics::derive_from_syntax`**, or it walks other files' trees on the checking threads (`decisions.md`, "Syntax trees are evicted, and only small syntax facts are memoized").
- **Every host runs the same queries; never add a lookup that only the command or only the server takes.** A host chooses the capacity of its `TreeStore` and `Db::editor_facts`, nothing else. Syntax trees live in that store, so no query may memoize a file's tree or an `ItemSyntax` window (the detached subtree of an annotation is the one syntax a memo keeps), and a command's pass that derives from a file's tree holds the file (`TreeStore::hold`) (`decisions.md`, "Decision record: every host resolves names through one path, and trees live in the host's store").
- **A check reaches other items through `item_links`, the per-name queries and `file_interface_groups`.** A new per-(item, name) memo for names nothing binds costs about 100,000 memos on a 300,000-line project, so filter by `package_name(..).bound` first.
- **Server inputs are `Durability::MEDIUM`, except open documents and `ProjectFiles` (`LOW`), and every setter names its durability.** One `LOW` input that most memos read turns each keystroke into a project-wide verification (`decisions.md`, "Decision record: editor responsiveness at 300,000 lines").
- **Salsa clears a handle's cancellation flag whenever a top-level query returns**, so background work that must yield polls `CancelHandle::has_queued_jobs` between units (`Worker::in_parallel`).
- **Editor budget: keystroke-to-diagnostics p50 at most 30 ms and p95 at most 100 ms at 300K lines** (`test_perf.rs`, `just perf`).

### Measuring

- **Alternate the two binaries run by run**, measure analysis with `--output json` (the human reporter is quadratic in findings), and compare corpus outputs as unique sorted records. An in-container timing is a two-core number.
- **An A/B build of another commit needs its own target directory**, or it links the other commit's crates. Where the disk has no room for one, touch every file under `crates/` (never `patches/`, which rebuilds Polars) in a tree before building it. Code images belong to the build that installed the library (`refuse_foreign_images`, `bench.py`).
- **Compare peak `RssFile` only from the same page-cache state**: evict both builds with `tools/bench/evict.py` first (`decisions.md`, "Peak RSS is judged as anonymous, image and code size, not as one number").
- **Measure loaders with the same `keep.source`** (`tools/loader-bench`): `ry run` keeps source references and `Rscript` does not.
- **testthat calls `gc()` after every test file with a teardown**, so suite time and process wall time give different ratios against GNU R.
- **The application workloads are ours, so never call them real-world.** The bar for the typed suite's geometric mean is 2x Rust's time (user direction); check a local build with `just bench typed --ry PATH --any-build`.
- **Never quote the numeric kernels' geometric mean as general speed.** It comes from tight loops and vector code. The call-heavy application workloads (`tools/bench/applications/results.json`) are the measure of ordinary R code.
- **A package with `Config/testthat/parallel` runs its tests in worker processes** (`rlang`, `purrr`), so the main process only waits in `processx` and its profile is empty. Profile with `ry test --profile-summary`, which runs the suite in one process, or set `RY_PROFILE_SUMMARY_DIR` and read the workers' summaries.
- **A fat-LTO link of `ry` beside callgrind runs can exhaust a 16 GB machine.** The tier tests take minutes under `--release` for the same reason, so run them with `cargo test-release` (the `iterate` profile).
- **A full `just bench-migrate` run over the three domains takes over 50 minutes on four cores.** Pass `--domain` while iterating, and rebuild the release `ry` first, because a stale binary misses stub types such as `condition`.

### Checker semantics

- **Typing is off unless `ry.toml` has `[check]` `typing = true` or the file starts with `# typing: on`**, so a throwaway project passes every type rule silently. To read an inferred type, annotate the binding with a type it cannot have and read the `found` half.
- **`GlobalEnv` has no default method bodies**: the SCC fixpoint's view is a second implementor, and a default silently gives it a wrong answer. Test with a recursive and a non-recursive fixture.
- **An immediate read resolves to the nearest earlier writer in the file; a deferred read (a closure) sees the project-wide winner in a package and the settled file frame in a script** (`frame_binder`).
- **A top-level loop variable stays a `ForVariable` binding** (`Naming::loop_bound_slots`). As `BindingKind::TopLevel` it becomes a global item, which breaks the unused check, goto-definition and rename.
- **A read recorded by name instead of by expression id is invisible to the slot model**, so `%op%` reads go through `mark_slot_read` (`naming.rs`). Implicit reads for `unused` stay in naming's slot reads (`mark_formals_read`), never in `captured_slots`, which the checker types with.
- **`"x" <- 1` binds `x` through the name-target arm, not `replacement_base`**, which reads the base first.
- **A called name is looked up as a function** (`Checker::function_lookup`), and editor features follow `ItemCheck.callee_targets`, not slot resolution (`decisions.md`, "Called names are looked up as functions").
- **Dispatch and the numeric constraint read the same class scope** (stubs plus project), so a package's own `+.Money` makes `Money` arithmetic.
- **Test files are `DocumentKind::Test`, helper and setup files `TestHelper`**: only helpers fill the shared test namespace, and no test binding reaches the package (`decisions.md`, "testthat files mirror testthat's environments").
- **HIR lists are `Span`s into the module's side arrays and every HIR kind is `Copy`** (`hir.rs`). Literal text is a `Text` span, never a `Symbol`, because the interner never frees. Lowering visits arguments before the callee, and per-expression maps rely on that `ExprId` order.
- **One structural map rewrites types: `types::map_child_types`.** New `TyKind`, `FunctionType` or `Parameter` members go there only, because a second hand-written walk misses one.
- **Every type the checker prints must parse back** (`test_roundtrip.rs`), so a new constraint or `TyKind` includes its grammar side.
- **Every mismatch report goes through `Checker::type_mismatch`**, which narrows to the failing field or position. A finding underlines the smallest expression its message is about (`name_range`).
- **Every finding inside an item is reported; only the item's export is cut to `Unknown` when it fails.**
- **Do not `table.resolve` a type you display as written** (it expands aliases), and use one `TypeRenderer` across everything that shares variable names.
- **A `#:` annotation declares parameter types, never the parameter list.** The export is always the formals, and `check_declared_function` reconciles, so a call is never blamed for an annotation's mistake.
- **A function has one signature, resolved in `syntax::annotation` for checker and runtime alike**, and both spellings lower into one `Annotation`, so no checker path knows the spelling. A path that lowers a function from less than its file keeps the statement and the `#:` lines above it (`decisions.md`, "A function has one signature, resolved in `syntax::annotation`").
- **An inline type definition pairs with a `#:` definition directly above it**, so read definitions through `file_type_definitions`, never by walking the tree for one spelling.
- **Stubs are declaration-only `.Rtypes` files in `types/`, a set-once input that project `stubs/*.Rtypes` files override**; an overload set ends with an `Any` fallback (`stubs.rs`).
- **Export manifests (`types/*.exports`) come from a live session** (`Rscript scripts/export-manifests.R`) on an R at least as new as the recorded one, or real exports become false `unresolved` findings.

### Parser and formatter

- **Recovery stops at the statement boundary when the next line assigns** (`starts_new_statement`). `=` does not count, because inside a list it is a named argument.
- **One mistake, one finding.** A lexer `ERROR_TOKEN` suppresses its statement's boundary report, and the `#:` grammar reports once per region (`error_unclosed`). Blame the incomplete construct, not where the search stopped.
- **An unterminated quote ends at its first line break**, so typing a quote does not take the rest of the file out of analysis.
- **`#:` block-form rules count lines over all of the block's tokens**, since a stitched line's marker may sit in a nested node. A `<T>` binder list prefixes the next type; a block containing an `ERROR` node carries no typing payload.
- **A ry rule on a construct that parsed in full reports through `Parser::error_rule`**, so the runtime keeps ry's message and a console reports instead of waiting for input. `error_at` there blames whatever token follows.
- **Inline type grammar defers its trivia** (`Parser::deferred_trivia`): a new `ann_*` path emits through `bump`, `start` or `checkpoint`, never the builder directly.
- **An annotated binding is recognized only at a statement start** (`Parser::statement`), because R reads `name:TYPE <- v` as `` `:<-` ``. An inline union continues onto a line starting with `|`.
- **The parser's acceptance gate is zero unadjudicated divergences against tree-sitter, with R as the referee**, judged on the erased text (`erase_inline_annotations`). A new error path adds its golden case (`crates/syntax/tests/errors`).
- **Formatter placement is token-level** (`last_token_before`, walking back from the offset; a subtree scan is quadratic on deep files), because trailing comments attach inside expression nodes. It refuses only on R-grammar errors. `ry fmt` on 309,000 lines takes about 0.5 s on four cores; what remains is cursor traversal (a reference-count bump per `SyntaxNode`) and allocation. Sweep a corpus with `format_directory` (`RY_FORMAT_DIR`, `test_fuzz.rs`).

### IDE, server and console

- **Cursor position model: the smallest end-inclusive expression wins and a `NameRef` wins a tie.** `semantics::item_name_range` is the one answer to which token declares an item, and `:=` is never a definition.
- **A quick fix is a field on the diagnostic (`fix`), never re-derived in `ide`.** Editor gaps are in `backlog.md`, "Open: editor parity".
- **Completion ranking by expected type only demotes, and only on declared types** read across every overload (`decisions.md`, "Completion pushes down names whose declared type cannot fit, and never promotes").
- **`list` and `c` have stubs only for signature help and their value type**; the checker types their calls first. data.table's `.()` reads as `list()` when `.` is unbound inside a `[` argument (`is_query_list`).
- **Syntax-only highlighting lives in `syntax::highlight`, shared by server and console.** An argument name is `Parameter` only with a direct `EQ` child, because `f(x)` passes `x` as a value.
- **miette's header column counts bytes, so `read_span` re-counts characters** (`crates/ry/src/cli.rs`); a multi-line range clamps to the first line, and sources are borrowed (`NamedText`).
- **The console's crossterm is patched (`patches/crossterm`) to drain the terminal before returning an event**, or the rest of a paste waits on reedline's two-second cursor query. The `use-dev-tty` reader is no substitute.
- **R gets console input one line per chunk** (`take_chunk`), because R discards the rest of its buffer on an error. A read below top level is answered from the console, never from `pending` (`crates/ry/tests/test_repl_e2e.rs`).
- **The console preflight checks each command as its own file against earlier commands**, and buffers plain R unanalyzed until the session holds ry syntax or strict mode (`decisions.md`, "Decision record: inline annotations at run time").
- **The console reads typed-ahead bytes only up to the first line break** (`take_typed_line`), leaving the rest for fd-0 readers. Ctrl-C needs an R `interrupt` handler plus a chained SIGINT handler, since GNU R swaps its own in during `Sys.sleep` and `system()`.
- **Console socket requests run on the interpreter thread only** (`interp::console_server`): readers queue, `poll_interrupt` drains, and anything that evaluates R is refused as busy while a command runs. `browser()` drives the same `debugger::Adapter`.
- **The REPL never feeds R input through the console to set state**: start-up work goes through `RApi::eval` before `run_Rmainloop()` (`crates/repl/src/libr.rs`; `development/design/repl.md`).
- **The CLI and the language server must compute `check_cache::key` identically, and whatever a name-keyed cross-file map reads must stay inside `check_cache::surface`** (`crates/ry/src/check_cache.rs`). Otherwise `.ry_cache/check.json` hands back stale findings. The seeded simulation in `cli.rs` catches a read that escapes the surface.

### Package loader

- **A load plan is backend-neutral.** `runtime.R` is shared; `gnur.R` with `r.rs` and `ry.R` with `crates/libr/src/loader_backend.rs` are the backends' hooks. Never put R-specific logic in `loader::diff`.
- **On ry's runtime a lazy binding is a promise over a per-file source handle** (`FileSource`), and its first force reparses only the definition, so srcrefs are real (`decisions.md`, "Lazy definitions keep source references").
- **Every load of a strict package is checked, on both runtimes** (`Plan::strict`). A refused plan is never committed.
- **Every reload rule keeps `test_reload_fuzz.rs` green** (a reload against a fresh load in a model). Hand-reasoned rules missed swapped writers and double wrapping.
- **A body edit must stop at `file_binds`**: equal binds backdate `package_bindings`, and `test_incremental.rs` counts the executions (`decisions.md`, "A reload re-derives one file and rewrites only the bindings it changed").
- **A cold load analyzes the package on a thread while base R starts** (`loader_backend::prepare` in `libr`), and strict mode reaches the runtime as an atomic, so starting R never waits. S4 definitions are held back until first use (`decisions.md`, "A load holds back S4 definitions and caches its index across processes").
- **R traps on the GNU R backend:** `environment<-` strips bytecode; `keep.source.pkgs` must not shadow `keep.source`; `.Internal(...)` evaluates its arguments, so quote a language argument; `ls()` sorts, so hot paths use `names(env)`; the attached environment keeps names the namespace dropped unless each sync removes them.

### Testing

- **The IDE fixtures print the token a range covers**, not a byte offset, so a stub addition never forces a re-bless. If one fails after a corpus change, the range is genuinely wrong.
- **An oracle is trusted only once an injected bug turns it red.** Determinism, idempotence and round-trip checks pass on a uniformly wrong answer.
- **Give a differential oracle inputs that always exist**: a test pointed only at a gitignored corpus never runs, and the runner hides its skip note.
- **A foreign renderer's expectations are never bulk-blessed**; adjudicate each case against the typing reference.
- **A gate must fail on its own exit status**: `cmd | grep …; echo OK` always passes, and a background job's output file can be read mid-write.
- **An `interp` test runs its body inside `interp::testing::with_interpreter`**, which takes the lock on the process-global interpreter. A symbol's name is `symbol.car().bytes()`, not `Sexp::bytes()` on the symbol.
- **The oracle stops a case at its first top-level error**, so compare behavior under `options(error = ...)` against `Rscript` directly.
- **A background job starts with SIGINT ignored, and GNU R keeps an ignored SIGINT**, so a harness running interrupt cases resets it in each child, as the oracle's `run_command` does.
- **The corpus tests skip without `.corpus/library`** (`tools/corpus/install.R`). Re-record `corpus_calls/cases.rds` under `LC_ALL=C.UTF-8`, or string routines record C-locale results.
- **A session that sources a user profile never loads the start-up image**, so a test about start-up points `HOME` and the working directory away from any `.Rprofile` (`test_start_image.rs`).
- **A simulator failure is a server bug until shown otherwise, but check the client model first**: a reopened document continues its version, and the first-wave-is-a-subset check holds only when no config or disk change landed since the sync.
- **A REPL pty test expects text the echoed command cannot contain** (`Error: boom`, not `boom`), or it matches the echo before the command runs.
- **The fuzz batteries' cost is re-parsing the stub corpus per fresh database**, not the iteration count.
- **GNU R's answer to `NaN op NA` is not portable.** `real_binary` is the same source in 4.3.3 and 4.6, but the C compiler may commute `+` and `*`, so on the AST interpreter (a closure's first, uncompiled call) the 4.6.2 build gives NA for `NaN + NA_integer_` where compiled code and the 4.3.3 build give NaN. `NA_real_` does not avoid it. An oracle case about NaN and NA order compiles its closures with `compiler::cmpfun` before the first call (`crates/oracle/cases/builtins__nan_na_order.R`).
- **A forced oracle against a GNU R other than the one in `crates/oracle/cases/gnu-r.txt` checks less.** ry then runs that release's base R, so the run cannot catch a release-gated gap, and a library that ry installed says it was built under 4.6.2, so every `library()` warns under an older GNU R.
- **Run `oracle compare`, `oracle record` and `oracle packages` with `R_LIBS` naming only `.corpus/library`, and with an absolute `ORACLE_RY`.** GNU R's side inherits `R_LIBS`, so `.corpus/ry-library` ahead of it makes GNU R load ry-built shared objects and fail with "undefined symbol: PREXPR".
- **Tell a prologue difference from a kernel difference by stripping the prologue** (`body(f) <- body(f)[[3]]`) and running ry again. Writing a prologue's oracle case is often how a kernel bug surfaces.
- **Re-run a filed finding before ranking work off it**, and add a control in the same file (an obviously undefined name), so that "no finding" cannot mean "the check is off". Treat a "landed" claim in an old note the same way: the verified state is `backlog.md` plus the fixtures.
- **Every generated benchmark test must pass under GNU R at every grid point.** The clustering template runs k-means with `nstart = 25L`, since fewer starts land in local optima at some grid points, and the logistic AUC bound is 0.7 because the grid's minimum is 0.78 (`tools/bench/tooling/templates/`).
- **`just docs-test` needs base R for the `runtime=ry` blocks.** A plain `cargo build` embeds no base R, so those blocks fail with "this ry has no base R" unless `R_HOME` names a GNU R installation (`R_HOME=/usr/lib/R just docs-test`) or `just base-r` has run.

### Build, CI and platforms

- **The toolchain is a dated nightly in `rust-toolchain.toml` (user directive), which `flake.nix` reads too.** Its rustfmt applies `rustfmt.toml`'s unstable import options; a stable rustfmt ignores them and rewrites every `use` tree.
- **The Zed extension is the one stable-Rust crate** (`editors/zed/rust-toolchain.toml`): the nightly's `wasm32-wasip2` std imports `exit-with-code`, which Zed's host lacks. In the nix shell, build it through the rustup proxy.
- **Exclude exactly `zed_ry` from workspace builds**: cargo only warns on a misspelled `--exclude` and builds the crate anyway.
- **The interpreter is linked into `libR.so`, which `cargo build -p ry-lang` does not rebuild**, so a runtime fix seems to have no effect. Use `just ry`; `just doctor` names stale binaries. `cargo test` puts its fresh `libR.so` under `target/<profile>/build/libr/<hash>/out/` (`library_path`, `harness.rs`).
- **A new dev-dependency feature goes into `crates/features`**, or building and testing compile two dependency trees (`decisions.md`, "Workspace-wide feature unification").
- **A full disk shows up as a test-build failure** (ENOSPC, or a Bus error at link). Run `just prune` first; a cloud container's quota is about 40 GB whatever `df` says.
- **CI is `nix develop .#ci -c just ci`.** Reproduce a failure in that shell, and change CI in the `ci` recipe or `flake.nix` (`decisions.md`, "CI runs in the flake's dev shell").
- **A crash is never a known failure.** The oracle counts a case whose engine was killed by a signal as new under every engine, even when `known_failures.toml` lists it, and the file has no intermittent mark (`decisions.md`, "A crash is never a known failure").
- **Agent containers are provisioned by `tools/dev-setup.sh`, and `flake.nix` inputs stay `git+https`** (`decisions.md`, "Cloud sessions are provisioned by tools/dev-setup.sh, not the nix shell", "Flake inputs are git URLs").
- **Nightly releases are GitHub pre-releases with `--latest=false`, and the Zed extension reads full releases only** (`pre_release: false`, `editors/zed/src/lib.rs`). Zed matches the flag exactly, so the two move together.
- **BOLT's `-use-gnu-stack` overwrites libR's `PT_GNU_STACK` header, and glibc 2.41+ refuses a library without it.** `tools/layout/optimize.sh` restores it (`gnu_stack.py`), and `just package` checks.
- **Type-check macOS and Windows from Linux before touching platform code** (`cargo clippy --target …` with a clang wrapper as `CC_<target>`; nothing links), because the release job is otherwise the first build there.
- **Cross-build the macOS runtime with clang and ld64.lld, not zig**, whose Mach-O linker ignores `-exported_symbols_list` and so hides export bugs.
- **`R.dll` and `ry.exe` are built for gnullvm, linked with lld, from the same tree** (`decisions.md`, "Windows runtime: `R.dll` and `ry.exe` for windows-gnullvm, linked with lld"). A windows-gnu cross-link needs a `synchronization` import library, made by the flake's `preBuild`; a crane deps-only build cannot catch its breakage.
- **A path that reaches R must not carry Windows' `\\?\` prefix**, which `fs::canonicalize` adds (`loader::find_package_root` strips it). Embedded R sources keep LF endings (`.gitattributes`), because GNU R's parser rejects `\r`.

### Docs and conventions

- **The landing page and `guide.mdx` share the four migration step names**, and the tagline outside the user-owned hero is "A type checker and a fast runtime for R, written in Rust." (`decisions.md`, "Decision record: positioning").
- **Docs content:** explain a thing once where it is introduced, and never use a capability before the page introduces it. A new user topic joins a guide step as an H3. Fixed terms: finding, diagnostic code, annotation, check (the command), code analysis (the capability), the language server, project.
- **A contradiction between two sections of one spec is invisible to every gate**, so read a page end to end when you touch it.
- **Docs output blocks are tests** (`development/testing.md`, "Docs examples"): a `text ry check typing` fence pairs with the code before it. Keep marker words free of `/`, quotes and braces, which Expressive Code reads as highlight markers.
- **Docs are served as Markdown too** (`/<page>.md`, `/llms.txt`), built from raw MDX by `docs/src/lib/markdown.ts`. Build with `bun --bun run astro build`, because the container's Node is older than Astro requires.
- **`reference/formatting-rules.md` is generated** from `crates/format/tests/formatter.template.md`, so a site-wide link rewrite must include the template.
- **Landing-page and README samples are captured from the running tools, never hand-edited** (`test_docs`, `just readme --check`). Nothing checks the VS Code README's two plain-text diagnostics, so re-capture them when a message changes.
- **Scratch output goes to absolute paths.** `cd dir && job &` backgrounds the whole compound, so later commands run in the repository; read `git status` before `git add -A`.
- **Seed the gitignored `fuzz/` lockfile from the workspace lock** (`just fuzz-all` does), or a fresh resolution picks a `salsa-macro-rules` that does not match the pinned `salsa`.
- **UI:** no `border-l` accent rail on rounded elements.
- **A new em-dash in a diagnostic or a comment is a regression.** Three test strings hold one on purpose as multi-byte test data (`crates/syntax/src/literate.rs`, `crates/ry/tests/test_cli.rs`, `crates/ry/src/server/simulation.rs`), and `worklog.md` keeps its original wording.
- **Validate a docs link against the ids in the built HTML** (`docs/dist/**/index.html` after `cd docs && bun run build`), never against a hand-rolled slug. Two headings that differ only in punctuation slug to the same id, and the second silently becomes `-1`.
- **Before rewriting a docs page, run an adversarial reader subagent with the owner's objections as the rubric:** too abstract, information the reader does not need, a term used before it is introduced, and copy about the tool instead of what the reader gets. A page that reads like a telegram is as much a regression as dense prose.
- **Docs vocabulary for data shapes:** an R list is "fixed-shape" or "any length", so "record" and "tuple" name only ry's own types. A tagged union in an example goes one variant per line with a leading `|` (user preference), which `ry fmt` keeps.
- **`reference/std/` is generated from `types/std.Rtypes` by `just docs-std` and never edited by hand.** The examples in `std.Rtypes` run in the docs test, so regenerate an example's output by running it.
- **The former spellings of the tool fall back** (`roughly.toml`, `# roughly: allow`, `ROUGHLY_*`, the editor extension's `roughly.*`), and `roughly.toml` must stay.
- **Format only the hunks you wrote, never the whole workspace.** `cargo fmt --all` rewrites files that other tracks are editing, because work lands without waiting for the format check, and the stray hunks turn into merge conflicts for them.
