# Backlog

The standing goal, which is a user mandate, is to empty this list and keep the project at
rust-analyzer quality. Every open item sits in one of the sections below. An item is deleted once it ships: the rationale
goes to `decisions/<area>.md`, the contract to the docs, and a closed user-testing finding to
`test-user-reports.md`.

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


## Open: the evaluator's edit loop

`decisions/interp.md`, "The interpreter is an engine crate between a numeric crate and four leaf
crates", has the shape and the measurements behind this list.

- **Put the evaluator core in a crate above base's builtins.** Cargo recompiles every crate
  above an edited one, so an edit to `eval()` rebuilds the engine's 126,000 lines (76 s of the
  82 s loop on `iterate`) and then every leaf, `typed`, `jit` and `libr`. The evaluator core
  (`eval`, `bytecode`, dispatch, closure application) is about 30,000 of those lines. Move it
  into a crate that sits above the rest of the engine: the lower crate keeps values, coercion,
  the unwinding types, environments, builtin registration and the builtins, and calls `eval`
  and closure application through `fn` pointers (the apply family, `do.call`, dispatch). Tier 0
  sits above and keeps calling builtin kernels directly, so no hot kernel call goes through a
  pointer. The top crate re-exports the lower one so `interp::` paths keep working, and the
  leaves depend on the lower crate, which takes them out of the loop. Measure `eval` reached
  through a pointer from `lapply` and `do.call` on release before landing. This is also the
  natural place to start passing the interpreter state as an argument instead of reading the
  global.
- **The regex engines and compression libraries stay engine dependencies.** They cost a cold
  build only, since a dependency compiles once. A leaf for base's string builtins would shorten
  the cold build of `jit` and `typed`, and nothing else.
- **`libr` has 24 test binaries, each linked against the whole runtime**, and no fast linker is
  configured. Both show in `cargo test-release`, not in the edit loop measured above.

## Open: the analysis core after the type store

`decisions/syntax-semantics.md` has the shape and the measurements behind this list, in "Types
and symbols are four-byte ids into append-only columns", "An item's syntax has a stamp, so an edit
lowers only the items it changed" and "The analysis core's per-item and per-file tables after the
type store".

- **`item_links` resolves every name an item's naming lists**, where a check that resolved a
  name on first use would skip the names a body never reaches. On the finance
  bench `item_links` with what it calls is 4.6% of a check's instructions, and its memos hold
  10.5 MB at the heap peak, of which 3.9 MB is the dependency edges salsa records and 2.4 MB
  the memo boxes. A smaller value does not move either: fewer memos or fewer edges would.
- **A tracked read on the server's worker outside `Worker::cancellable` can stop the worker.**
  A notification queued behind the running job flips the cancellation token, and the next
  query fetch unwinds. One such read was found by an intermittent test failure and fixed.
  Nothing structural prevents the next one: audit the worker's reads in `server.rs`, or make
  the database handle the worker holds outside `cancellable` unable to run a query.
- **An edit to one `#:` block re-runs `item_annotations` for every item of the file**, because
  each reads the per-file `file_item_annotations` map, which changes as a whole. Each comes out
  equal and backdates. A per-item stamp for the block, like the item's own, would cut it
  before lowering.
- **Edits at both ends of a file between two reads re-stamp every item between them**, so they
  lower again and backdate on `item_hir` equality. The store compares a prefix and a suffix
  with the previous tree and nothing else.
- **The type store and the symbol store never free an entry**, and the type store panics at
  2^24 types. A server session measured so far saturates far below that. Revisit if a workload
  shows growth.

## Open: one switch for every fast path

- `RY_FAST_PATHS=0` is checked inside three prologues only (`fast_paths_enabled` in `crates/interp/src/base_fast_paths.rs`), so every other base and package prologue still runs and a fast-path bug cannot be bisected with it. Install time is the wrong place for the check, because a start-up image already holds installed prologues; check it once where a prologue internal is dispatched. Found while tracing withr's `get_options` prologue, which passed every name in one character vector to `options()` (only the first is read) and so broke `local_options` with an unset first option.

## Open: tier-0 liveness maps (low priority, correctness only)

- A tier-0 register a frame has stopped using keeps its object alive until the frame returns, so a finalizer can run later than in GNU R for a variable that is dead before its scope ends. `Op::EndStatement` already clears each statement's temporaries, and clearing the register window on every grow keeps finalizer timing equal to GNU R's on the pinned cases (`decisions/jit-ssa.md`, "Tier-0 call entry: raw pool reads, an out-of-line tier schedule, and a cleared register window").
- Per-safepoint liveness maps (stack maps) would close the rest, and they are the only exact way to remove the per-call register fill, which costs about 1% of `validation_full` Ir (104M in `grow_registers`). Do this together with any GC or JIT stack-map work, because the JIT will want the same maps.
- Skipping the fill without them is unsound as R semantics. The collector marks the whole register file up to its length, so a word that a returned call left in the next call's window keeps its object alive until the window writes that word, and a finalizer GNU R runs in that window's `gc()` runs late (`gc__register_window_frees_returned_temporaries` fails on such a build).
- Tier-1 deopt does not need them. Each deopt block is generated from the plan's liveness bitset at compile time, and compiled code keeps every boxed value in tier 0's register window or the frame, both of which are roots.

## Open: the kernels of a package test suite's profile

`just bench-ci` runs fourteen kernels from a user's profiles of a large package's test suite on ry 0.4.5 (`tools/bench/internal/ci/kernels.R`; `development/testing.md` describes them). In the profile tier 0 held 32% of the time, builtins 32%, native code 17% and closure call setup 10%, and tier 1 ran 0.1%. Speed-up over GNU R per kernel: `k1` 2.6x, `k2` 11x, `k3` 3.2x, `k4` 2.8x, `k5` 2.7x, `k6` 1.7x, `k7` 1.8x, `k8` 9.5x, `k9` 1.7x, `k10` 2.4x, `k11` 2.1x, `k12` 10x, `k14` 1.7x, and `k13` takes 4 µs per object against GNU R's 4.8 ms.

- `k2` and `k8` run their base closures behind prologues (`ryAsDateCharacter`, `ryPmin`, `ryPmax`); what is left is the closure calls and S3 dispatch around them (`zoo::as.Date`, `UseMethod`, `as.Date.default`) and `[` on the matrix columns.
- `k4` (an argument checker over `list(...)` with `delayedAssign`), `k6` (an xts merge with a fill function) and `k7` (a data.table grown by `rbind`) have no work planned. Of `k6`'s 1.13M instructions per merge, `do_merge_xts` in xts's own C code runs 48%, and the rest is spread over `match.call`, deparsing the eight arguments for column names and `na.locf`, none above 2%. `k4` and `k5` are tier-0 closure calls: see "Open: JIT on against JIT off".
- Replacement calls turned 23.0M slot frames into environments in the profile, and `standardGeneric` 24.8M.
- `parse(file, keep.source = TRUE)` (`k9`) is faster than GNU R. On 40 generated files of twelve functions each, GNU R takes 450 µs per file, 243 µs without parse data and 168 µs without source references, and ry 263, 140 and 100 µs. Three things made the difference: the parse data replay (`parse_data.rs`) and the lowering (`lower.rs`) walk `NodeRef`/`TokenRef` cursors with no reference counting and no per-node `Vec`; a parse looks up a function's signature only when its tree holds type syntax; and the grammar-action checks are gated on the tree's kinds, not on `_`, `function` or `|>` in the text. The parse data is still a replay over ry's tree after parsing; having the parser record it would remove the lexeme pass, but the replay costs about 120 µs per file and nothing asks for less.
- `parse()` accepts or refuses some text differently from GNU R, in the parser and lowering shared by every entry point. A left-nested chain about 20,000 operators deep (`1+1+...`, `x[[1]][[1]]...`, `f(1)(1)...`) overflows the stack in the lowering's recursion, where GNU R parses it; running `lower` under `eval::on_large_stack` would fix it. In `parse(text =)` a bare `\r` ends a line, so `"x <- 1\r2"` gives two expressions (GNU R: `unexpected invalid token`) and a comment holding one swallows less than GNU R's. `f(a <- b = c)` parses, where GNU R reports `unexpected '='`. `function(a = _) 1` is an error, where GNU R takes `_` as a default. `parse(text, n = 2, keep.source = TRUE)` builds parse data for every expression in the text, not the first two.
- GNU R's srcref byte fields count one byte too many for each multibyte character of a string literal in a file parsed as UTF-8 (`s <- "café"` ends at byte 13, not 12); ry counts real bytes. Parse data agrees. `parse__parse_data_files.R` leaves the srcrefs of its non-ASCII file out until ry matches.
- `list.files` differs from GNU R in two argument edge cases: `pattern = NA_character_` lists everything where GNU R reports `invalid 'pattern' argument`, and a directory written with a trailing slash joins with one slash under `full.names = TRUE` where GNU R keeps two (`"sub//inner"`). `pushBack` on a raw connection is accepted where GNU R refuses it.
- Loading packages held about 7% of a 6 s test run in a second profile: the loader's plan (90 ms in one call), running and forcing its entries, `lazyLoadDBfetch` (104 ms over 3,889 calls), `dyn.load` (26 ms over 35 calls) and `unz` (19 ms over 438 calls). Each measured part is faster than GNU R already. A lazy-load fetch (`k14`) takes about 0.6 times GNU R's time: inflating the record is about 30% of it (see "Measured and not taken: libdeflate for lazy-load records") and the reader about 22%. The reader skips a byte-code record's code vector and its numeric constants (`Reader::discard_item` in `serialize.rs`); the calls among the constants are still built and dropped, because reading them fills the reference tables that later items index. Reading a zip entry through `unz` takes 40 to 50 µs against GNU R's 350. The loader's plan is linear in the package's files: on generated packages of 300 to 2,400 files of about 130 lines, about 0.4 ms per file without the index cache and 0.1 ms with it, on four loaded cores. A fresh CI container has no index cache, so its first plan is the uncached one. Where that time goes inside the analysis is not measured; a release build keeps its symbol table, so `perf` on it can attribute the time.
- A named empty argument differs from GNU R in three places. `log(x = , 8)` and `log(base = , 8)` fail with "argument 1 is empty" (argument evaluation refuses the empty argument before `log` matches) where GNU R gives `log(8)`. `rep(times = , 1:2)`, `rep(x = , 1:2, 2)` and `rep(each = , 1:2, 2)` return a value where GNU R fails with "argument is missing, with no default". And `NextMethod` does not do GNU R's `patchArgsByActuals`, which passes the empty named argument on as a promise of its formal and the positional argument that filled it as an empty one: with `gen.k <- function(x, y, ...) NextMethod()`, `gen(y = , obj, 2)` gives the default method a `...length()` of 1 in GNU R and 0 here, and with formals `(x, y, z)` `gen(y = , obj, 2, 3)` is "unused argument" in GNU R and binds all three here. A closure and `round(digits = , 2.567)` follow GNU R (`calls__empty_named_argument`).
- An "unused argument" message differs from GNU R's for three shapes: an empty argument prints `(w = )` where GNU R prints `(alist(w = ))`, a non-syntactic name loses its backticks, and code of several lines is joined by spaces where GNU R keeps the newlines.
- Builtin unit costs the same profile ranked: `sys.calls` 6.4 µs per call, `grepl` 5 µs, the deparse behind `all.names` and `traceback`, and the reset of the gettext cache that withr asks for (9 µs). No kernel covers them.
- data.table's own code held 17% of that run's self time. It is a normal package, so only the runtime under it can get faster, and at the sizes a test suite uses it already is: on 200-row tables fourteen operations (`rbindlist`, a filter, `setkey`, `copy`, `forderv`, `unique`, `setDT` and others) run 1.1x to 1.7x faster than on GNU R, and testthat expectations over tables 1.3x to 10x faster. What is below GNU R is on tables of 300,000 to a million rows with few strings alive: `rbindlist` of character or integer columns (0.7x), a row subset of a list column (0.5x), `melt` of character columns (0.7x), `fread` (0.8x). Two costs are left per element there. `SET_STRING_ELT` takes about 48 instructions a call, comparable with GNU R's. And each result column is a young large vector that is still alive when the next column's allocation starts a minor collection, so it is marked (about 28 instructions per element) and its cards are scanned (11). No kernel covers these sizes.
- Time a single large allocation with `system.time(for (i in 1:k) f(), gcFirst = FALSE)`. `system.time()` runs `gc()` first, and `gc()` returns the freed mappings of vectors of 1 MB and more to the system (`MappedPool`, `crates/heap/src/large.rs`), so every timed call pays the page faults again and ry looks two to four times slower than it is in a loop.

- Calls that native code evaluates repeatedly run compiled from their third evaluation (`bytecode::run_call`; `decisions/jit-ssa.md`, "A call native code evaluates repeatedly compiles at its third evaluation"), which `k15` measures. Three gaps are open. A call that passes `...` compiles to `Op::CallGeneric` and gains nothing, so purrr's mapped call (`map_int(x, length)`) costs 4,095 instructions per element on ry against 2,937 on GNU R; closing it is work on the generic call path. The tree walker spends about 150 instructions per evaluation in `normalize_argument_cells` for a call that has not compiled, and on every evaluation of a call that never compiles (a loop or `return` in data.table's `j`). And `eval()`'s own language case runs on the tree walker: routing `do_eval` through `run_call` is unmeasured.

## Open: JIT on against JIT off

With the JIT on, thirteen of the fifteen `just bench applications` programs run 0.01% to 0.55% more instructions than with it off, and `orders` and `bond_book` run 37% and 23% fewer (callgrind). The remainder is the entry and back-edge counters, one recording window per hot body and the compiles themselves. Tier 1 compiles a body only when its native work covers what compiled code costs (`decisions/jit-ssa.md`, "A body compiles only when its native work covers what compiled code costs"), so most application bodies stay in tier 0: their instructions are builtin calls on boxed values, which compiled code can only step through tier 0. What is left, in order of expected gain:

- The benchmark code bases gain nothing from tier 1. On every tenth test file of each code base JIT on runs slightly more instructions than JIT off: 0.28% more on finbench (64,500M instructions), 0.36% on biobench (38,100M) and 0.22% on dsbench (36,200M). On the dsbench sample (50 files) cache misses and branch mispredictions agree within 0.7% (callgrind with cache and branch simulation), and the wall times agree within the noise of six alternating runs. One body compiles there (`tryCatch`), about 200 are planned and refused: 129 run too rarely to repay a compile, 34 have no instruction that could run natively, 26 do too little natively. Their work is calls of builtins and of small closures tier 1 cannot inline yet, so the levers are the ones below (wider frames, native forms for builtins on boxed values) and the call path itself. Measure with `RY_JIT_LOG=1` on a sample of test files and group the refusal lines.
- The hot bodies of a package's test suite gain nothing from tier 1 either (`just bench-ci`, kernels `k3`, `k4` and `k5`: scalar calls of `ifelse`, `isTRUE` and `unname`, an argument checker over `list(...)`, `%in%` and `which` on a small vector). Tier 1 plans every hot body and compiles none, so JIT on runs 0.1% to 0.7% more instructions than JIT off (callgrind at `SCALE=0.1`: 999.6M against 993.4M, 618.2M against 614.0M, 602.0M against 601.2M). `RY_JIT_LOG=1` gives the refusals. The bodies do too little or nothing natively, because their instructions are `class`, `[`, `==`, `is.na`, `isS4` and `seq_along` on boxed values and calls of closures. The callees cannot be inlined: `ifelse` is too long, `is`, `which` and `is.data.frame` have no type feedback, the checker collects `...` and calls `delayedAssign`, and any callee with a builtin call is refused. The loops around them would step every instruction. Until "Wider frames" and native forms for builtins on boxed values exist, such code gets faster only through tier 0, native entries and prologues (`base_fast_paths.rs`).
- Compare JIT on against JIT off by instruction counts, not wall time. Two alternating wall-time runs of a whole code base showed JIT on 2% slower on all three, and six alternating runs of a 50-file sample on a quiet machine showed no difference; the instruction counts differ by 0.2%.
- **Why the code bases gain nothing (finbench, measured).** Three causes, each enough alone. (1) testthat runs every block under `withCallingHandlers`, so `jit_enter_frame` declines every inlined frame (`calling_handler_established()`), tier 0's eager binding is off and pure steps write back. (2) The hot bodies are thin wrappers (`expect_equal` and its helpers, argument checkers) whose compiled entry and steps cost more than tier 0: a loop calling two builtins runs 2,395 instructions per iteration compiled against 1,627 in tier 0, of which about 400 are entry (`jit::run` 230, `enter` 67, `entry_event` 50, register growth 36, the handler check 18) and 80 more per builtin call. (3) `just bench hot` warms with one batch, so a body called 20 times per batch (`expect_equal`) never reaches the threshold and the suite measures tier 0. Admitting every body (zero `ENTRY_COST` and `STEP_COST`) ran 4.9% more instructions on finbench, so the refusals are right until compiled code gets cheaper. The full suite (`oracle bench --ry-only`, 9,592 tests) shows the same: ry/R 1.99x JIT on against 2.00x off on finbench, 1.87x against 1.86x on dsbench, 1.85x against 1.90x on biobench.
- **Next for the code bases, before the list below, cheapest first.**
  - *Inlined frames under a calling handler.* Drop the `calling_handler_established()` decline in `jit_enter_frame` and rely on deferred signalling: a warning whose signalling runs R code is abandoned (`step_unsignalled`, `Signalling::Abandoned`) and the frames are rebuilt before the handler runs. Run a root `PureUnlessObserved` step `Deferred` too, so it exits on a warning instead of writing back at every step. Needs oracle cases with `withCallingHandlers` around inlined frames that warn, signal and `invokeRestart`.
  - *Lean entry.* Code pointer in the code header, the register window pre-sized, the handler check folded into the state word, an unboxed scalar return: about 400 instructions per entry. Then recalibrate `ENTRY_COST` and `STEP_COST` on the application programs and `just bench hot`, and retry zero-cost admission as the upper bound.
  - *A cheaper builtin call from compiled code.* `jit_call_builtin` costs about 125 instructions against tier 0's 85: call the direct entry from codegen when the builtin is known at compile time.
  - *Inlining through `...`* (see "Wider frames" below). Read tier 0's spread plan for the site rather than a second representation: a `CallPlans` row flagged `PLAN_SPREAD` (`bytecode.rs`), whose id is in the call instruction's feedback word, holds the callee-slot sources and the spread words (`SPREAD_AT`, `SPREAD_SLOT`, `SPREAD_COUNT`, the tags). A plan with `PLAN_PREPARED` and misses below `PLAN_MISSES` means the site has one shape. The plan can be replaced or freed, so compiled code copies the count, tags and sources at compile time and guards on the `...` chain (exactly that many `Dots` cells with those tags, none empty) and on the callee, exiting on a mismatch, as `spread_holds` does (decision "A call site that passes `...` keeps a plan for what `...` held", `interp.md`). It needs a public accessor in `interp` for `sources`, `spread` and the `SPREAD_*` constants.
  - *Inlining a tier-0-only callee* (see "Wider frames" below): materialize the callee's frame, run it in tier 0, and resume compiled code in the caller.
  - *A hot suite that reaches the code bases' tier state*: warm until the tier state settles, or measure the JIT on the code-base suites directly, since the hot suite cannot show a gain on bodies it never compiles.
- **Next for tier 1, in order: frames in shared code, the inline back-edge count, wider frames.** Each item below has its design. None is built, except the planner half of the first, which is on the branch `perf/tier1-shared-frames`.
- *Frames in shared code.* A body shared by several enclosures inlines no callee it finds in a short-lived enclosure. `spectralnorm_alt_3` defines `eval_A` and its callers inside `execute`: the first `execute(250)` of a process runs in 0.11 s, and every later one in 0.56 to 0.70 s, the tier-0 time. The second `execute` enters the compiled loops from a new enclosure, which invalidates them, and the shared recompile drops the speculation on `eval_A` (`Planner::portable`, `analysis.rs`), so the call is a step and the loop is refused (`STEPPING_LOOP`). Local helpers and function factories have this shape. Repro: `source` the program and call `execute()` five times with `RY_JIT_LOG=1`; done means every call runs near 0.1 s. The design guards the callee's code instead of the closure:
  - *Planner (written, on the branch, never compiled).* In a shared plan a `GetFun` whose lookup is not portable becomes `Action::GetFunFree { dst, symbol }`, and its register gets the hint type `Ty::Callee(example)`: any boxed value that is probably a closure with the example's formals and code. A call through such a register becomes `Action::Frame` with `loaded: Some(Loaded { register, levels })`, where `levels` counts the enclosures from the caller's enclosure to the callee's environment (`Planner::levels_to`, at most `MAX_CALLEE_LEVELS`). The callee is planned as a shared plan of its code (`plan_callee(function, loaded)`), and `Planner::adopt` turns each function lookup the callee roots at its own environment into a rooted speculation of the caller. That is sound because the callee's environment is an enclosure of the caller's, so `valid` and `prove` (`runtime.rs`) re-resolve it for whichever closure runs.
  - *`GetFunFree` in `codegen.rs`.* Generalize `probe_free` into a walk that starts at the activation's enclosure. For a level that is a function frame, replace the identity comparison of the environment by a test of its flags, `gp & (FUNCTION_FRAME | PENDING_FRAME) == FUNCTION_FRAME`, and keep the structural checks (filter bit, length, slot symbol, formals hint). Keep the identity comparison only for long-lived environments and for a level whose bindings live on the symbols. The root body must first test that its own frame has no environment, as `get_fun_fast` does. The value found must be a closure: an active binding or an unforced promise leaves the fast path. The slow path is a new helper `jit_get_fun(activation, symbol)` that does the same lookup without running code and returns null where tier 0 must run the instruction (`hand_off`). Store the result in the `dst` register, which also roots the function during the frameless call. `refine` and `check` read `Ty::Callee` as any boxed value, and `known_object` treats `GetFunFree` like `GetFun`.
  - *A loaded `Action::Frame`.* `jit_enter_frame` takes the function as a third argument: a constant for a callee the plan knows, the register for a loaded one. Its fast path adds `function != child.machine.function` to the "needs a frame start" test, and for a loaded frame it also checks at every start that `function.tag()` is still the `levels`-th enclosure of the parent's enclosure, because `environment(f) <-` can change the chain. `prepare` checks kind, formals (`car`) and code (`cdr`) against the plan, counts a hand-off on a mismatch (`count_handoff`) and returns null. `runtime::Inlined` drops `function` and keeps the formals, the code and `levels`; `enter_inlined` and `eval::enter` take the function from the child machine. `probe_free` must take its example enclosure from a field that exists only while compiling, so shared code never embeds the address of a short-lived environment.
  - *Rooting in `jit.rs`.* For a loaded frame keep only the callee's formals, code and slot table alive, never the example closure or its environment. Skip the callee's speculations whose environment is not long-lived, because the caller adopted them.
  - *A live slot frame as the callee's environment.* A closure made in a slot frame has an environment flagged `PENDING_FRAME` for as long as that frame runs on its slots, and `plan_callee` refuses such a callee ("may not outlive"). `eval_A` in `spectralnorm_alt_3` is in that state during the whole loop, which is why the benchmark takes about 1.1 s instead of 0.11 s. Lifting the owner frame before planning (`eval::lift_frame` on the enclosure from `plan_callee`, dropping the refusal for slot frames) was tried and is not enough: with `RY_JIT_LOG=1` the first compile of `eval_A_times_u`'s loops reports `0 inlined, 1 frames`, the code is dropped, and the recompile is refused because the loops would hand too many instructions to tier 0. The steps left are `GetFun eval_A` (not speculated from the enclosure), the `eval_A` call (`CallBuiltin`/`CallGeneric`), `SubAssignMatrix(Int, Int, Boxed)`, and `matrix`; `eval_A`'s own body `eval_A_cache[[i, j]]` is refused alone because `intrinsic_for` handles a one-index `[[` only. So the fix needs three parts: the callee's free variables read from the owner frame's slots (`eval::slot_binding`, `Heap::slot_owner`) or a lift that keeps the speculation valid; `speculate` resolving a function through a slot frame's environment; and a two-index `[[` intrinsic on a matrix. Check `spectralnorm_alt_3` with `RY_JIT_LOG=1` after any change to when a slot frame gets its environment.
  - *Tests.* An oracle case and a `test_tiers` case each for: a factory called twice (two enclosures, one body, a sibling callee); the callee rebound or shadowed between and during entries; `environment(f) <-`; another closure with the same code; the callee replaced by a builtin or by a closure with other code. Extend the cross-tier fuzz generator with shared bodies.
- *The inline back-edge count.* A tier-0 loop pays for an installed tier at every backward jump. `jump_fast` hands each backward jump to the out-of-line `jump` while a tier is installed, which counts the back edge there (`crates/interp/src/bytecode.rs`). `mandelbrot_ascii` (public suite) compiles nothing, because its loops are refused for good, and runs about 10% slower with the JIT on than off (0.65 against 0.59 s), and `jump` is the largest single item of the JIT-on excess on the dsbench sample (13.5M of about 80M instructions). That the jump explains the whole 10% is unverified. Design: `jump_fast` runs `back_edge_due` itself when a tier is installed (one increment and two comparisons on the profile words, no call) and returns `None` only when the count is due or an interrupt is requested. A body refused for good already parks its count at `i32::MIN` (`back_edge`), so it never leaves the fast path again. Done means `mandelbrot_ascii` costs the same instructions with the JIT on and off (callgrind), and the JIT-on excess on a code-base sample drops by the `jump` share.
- *Wider frames.* An inlined frame is refused for a callee without type feedback, one that takes or is passed `...`, and one with an instruction only tier 0 runs; on the code bases these are the hot small closures (`plan_callee` in `analysis.rs` lists every refusal, and `RY_JIT_LOG=1` prints them per call site). By expected gain:
  - *No type feedback.* Tier 0 records only near a body's thresholds and never in a small closure it runs without an activation, so a hot one-line callee can have none. Plan such a callee from its caller's argument types instead of refusing: every instruction without feedback gets a guarded generic form whose failure hands off, and the first hand-off storm retires the code through the existing `count_handoff` path, after which the callee has run in tier 0 with recording on. The alternative, opening a recording window for the callee when the caller's plan refuses it and retrying the caller, needs a tier-0 change and a second compile.
  - *`...`.* Two cases. A callee that only forwards `...` to a call the plan steps can bind it as tier 0 does: the frame's `...` slot holds the caller's argument references (`eval::direct_sources` already plans named and omitted arguments). A caller that passes its own `...` needs the length and names at compile time, which a shared plan does not have, so guard the `...` length at the frame start and hand off on a mismatch.
  - *Tier-0-only instructions in the callee.* A callee whose plan contains a step is refused when the step needs the frame. Let the step build the frame through the frame state (`enter_inlined` does this for a deoptimization) and continue in tier 0 to the callee's return, then resume compiled code in the caller through the resume record a stepped call already leaves. The cost model must charge such a frame as a call that is not removed.
- After those, native forms for the hot builtins on boxed values ("Cheaper steps" below).
- Take a speedup ceiling from plain callgrind self cost, never from a run with `RY_PROFILE_SUMMARY`. The summary hooks call `top_env` from `profile_summary::enter_closure`, about 1.1% of `validation_full`, which rescaling the shares by the total does not remove.
- Inline paths for scalar `+ - * /` (needs an inline allocation of a length-one vector), `Guard`
  (it cost 227M instructions on `functional`), and the trivial predicates through
  `BuiltinDef::unary`.
- Tier-1 handoffs: compiled code hands arithmetic it did not specialize back to tier 0 through
  `jit_step` (158,500 times in one `functional` body). Loads probe the frame inline
  (`decisions/jit-ssa.md`, "Tier 1 probes the frame inline for a variable load").
- Cheaper steps. An instruction compiled code hands to tier 0 costs about 300 machine
  instructions more than tier 0 running it (`STEP_COST`, `crates/jit/src/jit.rs`), so a loop
  that mostly steps is refused (`STEPPING_LOOP`): the `vasicek` kernel's loop (`stats::rnorm`,
  `rates[t, ]`, six vector operators), and the loops of `team_scores`, `records`, `report`
  and `objects`. Two levers: native forms for the hot builtins on boxed values, and a step
  that does not synchronise the whole frame before and after the instruction.
- A refused plan is wasted work. Planning a body that ends as `NOTHING_NATIVE` or `SMALL`
  after the plan costs 0.5M to 1.4M instructions, and a body can ask up to three times.
  Decide more of those from `analysis::shape` before planning.
- A loop whose compiled code deoptimized is compiled again from the same feedback on
  `team_scores` (the loop is refused today, so the path is idle; check it when cheaper steps
  let the loop compile).
- Native recursion is bounded by the depth budget only, not by the native stack
  (`decisions/jit-ssa.md`, "A native recursive function's depth budget shrinks with the interpreter
  depth").
- The JIT's first compile costs about 4 MB of peak RSS (code generator start-up). On
  `testthat_full` one compile of `isTRUE`'s body (3 KB of code) makes JIT on 4 MB above JIT off;
  a compile that saves microseconds should not cost megabytes (`decisions/builtins.md`, "withr's environment and option helpers run natively; defer() stays in R", peak RSS). The cost is clean, file-backed
  libR text, not heap: on the `conditions` application one tier-1 compile of `fetch()` pages in
  about 3.3 MB, which resident-page symbolization puts entirely in `cranelift_*`, `regalloc2`
  and `jit::codegen`. Anonymous memory and the GC peak do not rise, and with `RY_JIT=off` the
  two builds' peaks are equal (47.85 against 47.80 MB). So a speed-up elsewhere that pushes a
  hot body over the compile threshold shows up as a few MB of peak RSS. Levers: a lighter first
  tier that does not start Cranelift, or code layout that keeps the code generator's pages
  together (function ordering).
- Calls from tier-1 code while calling handlers are established, instead
  of falling back to tier 0, pinned in `crates/jit/tests/test_tiers.rs` with a body hot under
  `withCallingHandlers`.
- Inlined closure calls without a frame are built (`decisions/jit-ssa.md`, "Tier 1 leaves compiled code
  through frame states", "An inlined call starts with one leaf helper call"). They pay where a
  compiled loop calls a small closure (`spectralnorm_alt_3`, `orders`), and nowhere else yet,
  because a call is only inlined into a caller tier 1 compiles. In order of expected gain:
  - A lean entry from the dispatch loop into compiled code. An entry costs about 570 machine
    instructions more than starting the body in tier 0 (`count_entry`, `entry_event`,
    `jit::enter`, `compiled_for`, `jit::run`, the register file, the boxed result), which is why
    a loop-free body under five native instructions is refused (`decisions/jit-ssa.md`, "A body
    compiles only when its native work covers what compiled code costs"). The end
    shape jumps from the call instruction to a code pointer in the code header. Recalibrate
    `ENTRY_COST` (`crates/jit/src/jit.rs`) when it lands.
  - Evaluate more arguments before an inlined call. An argument is computed eagerly only when
    `lower_expression` can produce it as an unboxed scalar without a failure; anything else stays
    a promise, and the frame is refused when the callee reads it ("an argument the callee reads
    that may have an effect"). A pure expression that can fail (`x[[i]]`, `a / b` on vectors) or
    yields a boxed value can still run before the call, with an exit at the call instruction
    when it fails. An argument that mentions one of the callee's formals under a condition stays
    a promise too (`analysis.rs`, `Lowering.conditional`); tracking which formals are already
    forced at the call would allow it.
  - Compile the callers. Most application and code-base bodies are refused as `SMALL`,
    `STEPPING_LOOP` or "would run nothing natively": their instructions are builtin calls on
    boxed values, vector operators and calls of closures with `...`. Each needs a native form
    in tier 1 before removing the calls between them can pay.
  - Callees tier 1 cannot inline yet: one with `...`, a loop, a default that is code and is
    read, a recursive call, a call that needs its own frame, and a callee without type feedback
    (tier 0 runs the smallest closures without an activation and records nothing for them;
    plan those from the argument types at the call).
  - A direct call from compiled code when an inlined frame cannot start. `jit_enter_frame`
    declines while a calling handler is established, the call instruction then deoptimizes the
    whole root activation, and a root where that is frequent is recompiled without frames
    (`NO_FRAMES`). A root that mostly runs under `withCallingHandlers` or `suppressWarnings`,
    which includes every testthat expectation, therefore calls for real. Calling the callee
    through the direct-call path instead keeps the root compiled with frames and makes
    `NO_FRAMES` unnecessary.
  - Start an inlined frame without the one remaining helper call (`jit_enter_frame`, a leaf of
    about 50 instructions), then delete `Action::Inline` and `effects::effect_free_lookups`,
    which exist only because the frame path costs that call.
  - R-level value nodes and passes in `crates/ssa`. The IR carries frame states, but its
    instructions are still Cranelift's, and inlining is decided by the planner in
    `analysis.rs`. Value numbering across an inlined call, guard removal and scalar
    replacement need typed R-level nodes above the lowering.
- `conditions` leaves compiled code 4438 times at one call site, once per call: the compiled
  `tryCatch` body calls `doTryCatch`, the R error raised under it unwinds through the compiled
  frame, and the exit (`reason::PENDING`) rebuilds the frame for tier 0 to handle it.

- **Tier 1 costs more wall time than its instruction count suggests**, because it adds mispredicted branches that callgrind's instruction count does not show. Closing the gap needs cheaper tier-1 compiles, not a change of the entry threshold (`decisions/jit-ssa.md`, "Tier 1 keeps the 3000-entry threshold"). With the inline frame probe, `functional` runs 1.8% fewer instructions with the JIT on than off, and `testthat_full` runs 1.2% more, mostly in tier 0's own functions.

## Open: console input in ry's runtime

- Ctrl-C during the script `ry run` queues before its console is not reported to the preflight, because the script goes to R as one unit: the console session keeps the bindings of the script's unrun rest.
- `semantics::session::binder` answers a deferred read of a name without a `SessionBinder` untracked. `prepare` creates binders for the naming sets the checker reads deferred; a checker path asking about another expression (`callee_fallbacks` in `check.rs` is the candidate) would read a stale answer. A `debug_assert!` that a deferred read finds its binder would pin it.
- Console `scan()` prompts with the running item count (`1: `, `3: `); GNU R's counts records for a
  list `what`. `select.list(graphics = TRUE)` and `.External2(C_selectlist)` still raise the
  non-interactive error.

- Lines pasted in one chunk together with the command that reads them (`x <- readline()` followed by its answer) go to the console as commands, because a read below top level is answered from the console and never from `pending`.

## Open: the typed core

- **Box conversion at the boundary.** A box result reaches R as a lazy record that converts only when R reads into it, and goes back to typed code as the same box (`decisions/typed-stdlib.md`, "A typed record or tag that R holds stays a box until R reads into it"). A record R built itself still converts deeply into boxes on every call, and a record R has filled keeps no link to its box. A `List<Str>` argument from R converts on every call (see the next entry). Boxes are also never scalar-replaced: a record built and read in one function is still allocated. `Str.to_i64` and `Str.to_f64` parse twice (`Op::Parses`, then `Op::Parse`). The design's pools (`typed-runtime.md`, "Representations") are not built.
- **A `List<Str>` from R converts on every call.** R holds a `List<Str>` as a plain character vector, even one that `List.from_r` or typed code made, so every call converts it again through `typed::string_list`: one `ry_str_from_bytes` per element into a fresh native list. In typed `sales` (callgrind, warm iteration, release build with symbols) `string_list` is 32.4M of 262.1M instructions, about 12%, for the million-item `region` argument. `type_of`'s `NA` scan of the two numeric lists is another 16.0M (see "Two boundary costs" below); numeric lists already cross as views (`values::ry_list_view`) and copy nothing. `team_scores` pays about 150 of its roughly 1,420 instructions per row here. End shape: a `List<Str>` that typed code returns, or that `List.from_r` makes, reaches R as an ALTREP character vector over the native list, as `interp::Interp::deferred_string_class` does for deferred strings. Reading one element makes one CHARSXP, and the data pointer fills the whole vector once. The three conversion sites in `typed.rs` (`Arguments`, `to_box_in` and `write_converted`) recognise that class and passes the same list with one more reference, so a list that crosses twice converts once. Generated code keeps receiving a plain `List<Str>` borrowed for the call, with the same layout, so `unbox`, `fusion` and the list kernels need no second path. A write from R (`x[[2]] <- "z"`) materialises the vector and drops the link, as a filled lazy record does. A character vector R built itself still converts on each call; the oracle and the lazy-record gctorture coverage are the bar for the ALTREP class.
- **The depth guard of typed recursion counts frames, not bytes** (`typed::depth_budget`). A frame holds up to three bodies of a function lowered into itself, and each `Op::SplitBounds` adds a stack slot of 16 bytes per position up to the largest one read (at most 512 bytes), which the copies do not share. Frame sizes are unmeasured; a recursive function that reads a late piece of a split could use several megabytes of stack inside the budget. Either measure the frame the backend built and charge the budget in bytes, or have `ry_str_split_bounds` write only the positions read so the slot is 16 bytes per read.
- **List functions that box every item.** `List.reverse`, the take, drop and sublist family and `List.split_at` move packed items through `arrays::gather`, but most of the others (`List.sort_asc`, `List.map`, `List.keep_if`, `List.concat`, ...) still go through `list_elements`, which allocates one scalar per item of an `F64` or `I64` list. A million-item list pays about 60 MiB of transient scalars that way. The end shape is a packed path per item kind, as `gather` has.
- **Table values in typed code: open ends.** A call of a typed function is an operator operand only when its written result type starts with a table type's name (`helper() > 3` with `-> Expr`), because `lower::returns_table` reads the annotation text and the runtime keeps no alias definitions. A result type that is an alias of a table type, and a local function, an `if` or a `match` that gives a table expression, are refused there, and the check warns (`Uncompiled::TableOperand`). `Lowerer::is_table` resolves the callee while compiling, which forces a lazy-load promise of a package function that the call itself might never have forced. A table made per loop iteration stays rooted until the outermost typed call returns (`decisions/typed-stdlib.md`, "Typed code holds table values natively"). Each table call pays the R call protocol (a frame, an environment and promises per call), which is noise against a query over a million rows but not against a small one.
- **An installed typed function prints without its annotations.** An installed package keeps no source references, so `print(f)` deparses the closure, and the deparser writes `function (name, n)` for `function(name: Str, n: I64) -> Str`. The signature is at hand (`interp::annotations`, which a serialized closure carries as `STREAM_ATTRIBUTE`), so the deparser can write the annotation texts.
- **JIT-compiled callers skip native entries.** With the JIT on, a caller compiled to machine code calls `isTRUE` and the other `NATIVE_ENTRIES` closures as full closures (a loop of 17,000 `isTRUE(x)` calls takes no native entry); tier 0 takes the entry. Route compiled calls of base closures through `base_fast_paths` as tier 0 does.
- **A refused strict closure reads its arguments on every call.** `jit::native::call_typed` gathers the arguments (evaluating their pure code) and looks up the compile cache before it finds the per-type refusal, and the interpreter then evaluates the same code again: typed `orders` runs about 3% more instructions than without the read. A per-closure "refused" bit is not sound, because untyped callers pass one annotation in several representations (doubles for a `List<I64>`), and a closure refused for one can compile for another, in the typed core or the scalar natives.
- **Arguments that are unforced promises.** `typed::call` takes only values readable without forcing (constants, variables, forced promises, and base arithmetic and `c()` over them), so `f(c(1, 2))` from untyped code takes the native path but `evaluate(build(14, 8))` runs `evaluate` in the interpreter. A typed call is never read ahead: it may not finish, and R never runs an argument the callee does not force. A call from typed code forces its arguments first (typed ry is strict), so this is only about calls from R code, which must stay lazy; `expr_tree.typed.R`'s untyped `measure` lambda binds the tree to a variable first. The same holds for `f(List.from_r(x))` and for a list literal argument such as `f([1.0, 2.0])`; `RY_TYPED_LOG` gives the argument as the reason.
- **Call-time speculation checks walk every lookup chain.** `still_resolves` re-walks each speculation's environment chain on every call; a per-binding version stamp would make the check one compare.
- **Compiled closures stay rooted for the session.** Up to `MAX_COMPILED` closures are kept alive by their code; dropping the code with the closure needs a finalizer hook in the heap.
- **Updating a record's `List` field copies the list in typed code.** The interpreter updates it in place (`decisions/typed-stdlib.md`, "In-place updates through nested targets"), but natively `(..r, values = List.append(r$values, x))` reads the field with a dup while the old box still holds it, so `Op::Push` sees a shared list and copies it: a loop of such updates is quadratic (20,000, 40,000 and 80,000 appends take 0.045, 0.15 and 0.59 s). Growing in place needs a field read that moves the list out of a uniquely held box, which the update then consumes.
- **A stray tag from R in a non-tail `match`.** A `match` whose value is not the function's result (`y <- match (t) {...}`) has no declared type to judge its arms by, so an arm that a stray shape from R (`:One(2.5)` against `:One(I64)`) gives another type still makes the arms disagree, and the function refuses for the rest of the session. A tail `match` is handled (`lower::tail_matches`).
- **Mutual recursion caches a refusal.** A typed closure that calls one still being compiled refuses, and the refusal is remembered, so mutually recursive typed functions always take the native path.
- **Names a branch creates.** A name set in only one branch, or with different types in the two, is out of scope after the `if`, so reading it refuses; a loop body's names are out of scope after the loop. Local functions defined or rebound in a branch or loop refuse.

### Typed ry against Rust: where the next steps are

The bar is twice the Rust twin's time (`tools/bench/typed/`, `just bench typed` on a release build; a single program runs on any build with the typed harness prepended, `ry run main.R` in a scratch project). All seventeen programs are at or under it. The ratios below are typed time over Rust time on a release build of a shared machine, so read them as an order, not as results. Every step has to keep compile time flat: the typed core is single-pass, and the suite reports its total compile time beside `rustc`'s. Compare that total before and after any change to inlining, which is where it grows.

| Program | Ratio | Top cost | Next step |
| --- | --- | --- | --- |
| `orders` | about 1.8 | About 76 instructions per order line: three divisions by a constant (as multiplications), the region tag built by an `if` chain and matched again, the interrupt poll, and `i %% 7 + 1` computed twice (`decisions/typed-stdlib.md`, "Whole-number ranges decide which checks typed code needs"). | Match a tag where it is built (the `if` chain's arms jump to the `match` arms), and keep a value that several branches read in the block that assigns it. |
| `fib` | about 1.7 | Four calls per body after one level of inlining, each with a frame, a depth count and a status test, plus the overflow test on the sum of two call results. LLVM turns one of the two recursive calls into a loop and does not test for overflow. | A second level of inlining would halve the calls again at the price of a larger function to compile. Neither side is measured. |
| `log_rank` | about 1.5 | The counts over the 16,000-item lists, once per event time. Not profiled since the counts were fused into one pass. | Profile first. |

Removing instructions is not the same as removing time here: the range analysis cut 13% of `orders`' instructions and 7% of `fib`'s, and the release times moved less than the spread between runs. The likely reason, not measured here, is that the removed tests were branches the processor predicts. Judge a next step by its release time, and prefer steps that remove a call, a division or an unpredictable branch.

`team_scores` runs about 890 instructions per row since its three loops fuse into one (`decisions/typed-stdlib.md`, "A loop that fills a list is fused with the loop that reads it"), and two steps would cut more: a piece that is only parsed can be parsed from its offsets without being cut out of the string (`Op::Parse` over a range, about 150 instructions per row for the cuts), and the scan for the separators can compare sixteen bytes at a time (about 180 for the scan).

Loop fusion (`crates/typed/src/loops.rs`) leaves these lists in place:

- **A third stage after two mapped lists.** In `List.map` → `List.map` → a consumer, the first two loops fuse and the third does not. After the first fusion the second list's length is a copy of a copy of the source's length, and its `Set` index is the first consumer's counter, which is assigned from the producer's counter and is not the same variable. `set_source` needs to follow copy chains, and the pass needs a helper that renames an operand, which `crates/typed` does not have.
- **Loops that share an assigned variable** (a consumer that assigns what the producer reads) stay apart, also where an order of the two bodies exists that keeps every value.
- **A loop with a call** of R or of a typed function that was not inlined never fuses, because the call could observe the order of the two loops.
- **A counter nothing else reads.** `unbox::remove_unread` keeps `count = count + 1` when only that instruction reads `count`, so a fused list whose length is never asked for still counts its items.

Two boundary costs sit inside several ratios and belong to no single program: a `List<Str>` argument held by untyped code is converted to a native list on every call, and a numeric list argument is scanned for `NA` on every call (`typed::type_of`). `sales` and `team_scores` pay the first, and a benchmark whose numeric lists are held by untyped code pays the second. A list that records "no `NA`" once, at `List.from_r` or when native code built it, removes the scan.

The committed results of the kernels and applications suites (`tools/bench/kernels/results.json`, `tools/bench/applications/results.json`) were measured before `array_filter.typed.R`, `sales.typed.R` and `bond_book.typed.R` changed, so their typed columns, and the `typed_refused` flag on `sales`, describe the earlier sources at the commit each file names. The next release record (`just bench kernels applications typed --release ARCHIVE --write`) refreshes them and writes the first `tools/bench/typed/results.json`; only the recorder writes those files.

A typed element loop over a `List<F64>` that tests each item and adds to an `F64` total runs at about three times a vectorized Rust `filter().count()`, because typed ry has no autovectorizer. That is why `array_filter` counts with `List.count_if` (`decisions/typed-stdlib.md`, "The typed `array_filter` benchmark counts with `List.count_if`"); the loop form still pays an interrupt poll and a data-pointer reload per item, which a loop over a held list could hoist.

### What the `native` check pins and over-reports

`ry check` reports what keeps a native function off native code, as `native` errors for R values and `interpreted` warnings for ry the typed core does not compile yet (`decisions/typed-stdlib.md`, "Nativeness is a strict-mode diagnostic"). Each item here is a gap in the typed core that the check found; closing one means deleting its blocker in `semantics::native` (or its entry in `test_scenarios::NATIVE_CHECK_EXCEPTIONS`) and its row in `reference/type-system.mdx`, "Native functions".

- **The checker passes it, the typed core refuses it** (pinned in `NATIVE_CHECK_EXCEPTIONS`):
  - An R call whose value is unused compiles only as a statement of a `{` block that is not its last. As the last statement of a loop body, as a bare loop body, in an `if` branch or a `match` arm in statement position it refuses ("it uses the value of R's `cat` directly"), although nothing reads the value (`tests/native/pinned-r-statement-tails.R`).
- **Legitimate ry the typed core refuses, so the check warns `interpreted`:**
  - `try`, which lowers to an R call of `.ry_try`;
  - a `match` on a list (every list pattern), a nested pattern and an arm with an `if` guard;
  - about 195 standard library functions with no code in `Lowerer::builtin`, among them `List.first`, `List.last`, `List.get`, `List.drop_at`, `Num.floor`, `Set.contains`, `Result.map` and all of `Bool.*`. `native::COMPILED_STD` is the list of those that compile, and it grows to the whole library as the typed core gains them;
  - a variable that holds a tag and is assigned again, even one with a declared type, and a function-typed parameter;
  - a function with `-> NULL` that ends in a loop, `<<-`, `...`, named arguments or a left-out default in a call of a typed function, a `Dict` of anything but `I64`, `F64` and `Str`, and `List.map(xs, Num.to_f64)` by name;
  - a variable, parameter or loop variable assigned a value of another layout (`layout_differs` mirrors the lowerer's value classes), branches that set a name to different layouts, and the value of an `if` without `else`;
  - an `I64`, `List<I64>`, `Dict` or `Set` passed to an R function as it is, `xs[i]` with a position, a column read by position, a table value inside a record, tuple, tag or list, a standard library call with named arguments or a `ry::` prefix (`tests/native/blocked-*.R` holds one file per group).
- **Two signatures the runtime does not read.** A signature that is only the name of a type alias (`#: Pick`, `f: Pick <- function(n) n`) is resolved by the checker and not by the runtime, which reads types where they are written (`syntax::annotation::signature`); the check warns (`Uncompiled::AliasedSignature`). Closing it needs the alias's body at lowering time, for instance by resolving aliases defined in the same file in `syntax::annotation`. A typed function without parameters whose body is a constant or a bare name registers no signature at all (`interp::annotations::register_definition` has no language object to key it by), and the checker says nothing about it.
- **Over-reports.** A base R function the typed core computes itself (`Lowerer::builtin`: `length`, `sqrt`, `nchar`, `paste0`, `sprintf`, ...) is an R function to the rule, so its value needs a conversion that the compiled code does not. `a:b` is reported although `for (i in a:b)` compiles. Decide whether those base functions become part of typed ry or stay R.
- **Not judged: how a call enters native code.** A call from top-level or boundary code that the native entry declines has no finding, and the function then runs in the interpreter together with every call it makes with a computed argument. The common case is a ry literal at the call, as in `total([1, 2, 3])`: `jit::native::pure_value` reads ahead only constants, variables, arithmetic and `c()`, so a list, record or tuple literal stays a lazy promise. Two more declines have no finding: a native function that calls R declines a variable argument from interpreted code ("it calls R, which could change a variable an argument reads"), on every call, and the first call of a function that calls a not-yet-loaded R function or converts a value of unknown type waits on the interpreter once. The first one makes a function such as `report(label)` with a `cat` inside always interpreted when top-level code calls it with a variable, and its message advises what the caller already did. Fix the entry first (read ahead a list, record or tuple literal whose items it can read ahead, and convert record and tuple arguments), then report what still declines (a function argument, a `Dict` or `Set` result) as `interpreted` at the call.
- **Two lists are mirrored by hand.** `native::COMPILED_STD` against `Lowerer::builtin`, and `native::gives_table_value` against `gives_table_value` in `crates/typed/src/lower.rs`. The agreement test holds every `COMPILED_STD` name to a corpus function that runs natively, but it cannot see a function the typed core compiles and the list leaves out, which shows as a needless `interpreted` warning. A crate both sides can read (the names with native code as one table) would remove the mirror.

### Gaps the typed public ports found

Typed ports of the public suite (`tools/bench/public/*/*.typed.R`, plain idiomatic style) hit these. Every port runs natively and prints GNU R's checksum, because it writes around each gap as noted.

- **Not compiled natively; the whole function runs in the interpreter:**
  - a standard library function passed by name, as in `List.map(xs, Num.to_f64)` ("a mapped function that is not a typed closure"), so the ports wrap it in a lambda (`\(row) List.sum(row)`);
  - a `Dict` whose key or value is not `I64`, `F64` or `Str` (`Dict.empty` needs the checker's settled types, which `.ry_items(Dict.empty(), "Dict<Str, I64>")` carries).
- **One refusal spreads to the callees.** `tree` and `check` in binarytrees compile when a native caller calls them, but under a refused `execute` they run in the interpreter too. A `List<Record>` that an interpreted typed function passes on is "an R value of type `list` that typed code cannot hold" (nbody, fasta).
- **Literal typing in the checker:**
  - A local whose literal is reassigned later in a loop or branch stays `double`: `x <- 0` then `if (n > 1i64) x <- x + 1.5` against an `F64` result reports "expected `F64`, found `double`", so the ports spell `0f64`.
  - Literals inside a nominal constructor stay `double` (`@Tree(:Leaf(1))` against `:Leaf(I64)`).
  - A literal variable first read as a list index (`cprob_lookup[[j]]`) is decided as `double`, so a later `lookup[[i]] <- j` into a `List<I64>` is an error. The fastaredux ports spell `j <- 1i64`.
- **A recursive `alias` is refused**, with a message that prints an enormous expanded type; a nominal `type` works.
- **`Num.sqrt` stops on `NaN`** ("must be a single number, not `NaN`") instead of returning `NaN`.
- **regexdna matches by hand**, because typed ry has no regular expressions: the port parses each pattern into fixed-length alternatives of byte classes. It runs at about GNU R's time, the slowest typed port against GNU R. A callgrind profile shows about 450 instructions per byte position, most of them in the `CallTyped` to `matches_at` and in list copies around it, so the cost is the typed core's per-call overhead, not the matching.
- **A ry list holds no `NaN`**, so the mandelbrot ports freeze a point once `|z| > 2` (`List.map3`) instead of letting it overflow to `NaN` as the R vector code does; the count of points inside the set is the same.
- **An empty `List.map`, `List.map2`, `List.map3`, `List.join_map` or `List.chunks_of` result stops native code** (`fail_if` in each lowering), and the interpreter runs the call again. The interpreter builds those results with `arrays::from_items(&[])`, which gives `logical(0)`, and the stop keeps the two tiers' values identical. `List.repeat(x, 0i64)` has no stop, so it gives `double(0)` natively against `logical(0)` interpreted. Settle the interpreter's empty result from the checker's type, then drop the stops.
- **A walk accumulator's tags do not widen.** `List.walk(xs, :None, \(acc, x) :Some(x))` is "this function must return `:None`, but it returns `:Some(I64)`", `List.walk(xs, [:A], \(acc, x) List.append(acc, :B))` is "passed `List<:A>`, but its parameter `acc` accepts `List<:B>`", and a record state `(best = :None, n = 0i64)` fails the same way. The accumulator is left open (`widening_accumulator` in `check.rs`), but the step function's result is still checked against the initial state's tags.
- **Non-constant defaults.** A typed function whose parameter default is not a constant is not compiled by the typed core. (Unverified.)
- **Typed values outside elaborated code.** The preflight elaborates only code that `ry run`, the console, `ry install` and `ry build` take, so under `ry load` and `source()` these gaps show (`decisions/typed-stdlib.md`, "Typed ry has its own numbers, `Bool`, `Str` and `List`", item "The checker decides a literal's run-time type"; unverified):
  - a literal the checker types `I64` runs as `I64` only in elaborated code, so `ry load` and `source()` convert only annotated bindings;
  - a record or tag payload literal stays R-typed at run time;
  - an empty list keeps its item type only in elaborated code (`development/design/language-climb.md`, "Open questions").
- **A latin1 package's non-ASCII string literal installs as U+FFFD on ry's runtime.** A package with `Encoding: latin1` whose `R/` file binds `x <- "café"` (byte `0xE9`) installs with `ry install`, and `charToRaw(x)` gives `63 61 66 ef bf bd`, typed or not. The tarball keeps the latin1 byte, so the loss is in ry's `R CMD INSTALL` reading the code.
- **Smaller typed-language gaps** (unverified): a string literal compared with `==` against a `Str` list gives an R logical; record `..rest` patterns exist in the checker only; `[]` hovers as `List<T>`; f-strings build R `character`.
- **A literal accumulator through a generic call stays `double` inside a function.** In `total <- 0; total <- List.walk(xs, total, f)` the accumulator needs `total: F64 <- 0`. At the top level of a typed file `total <- 0` is an `I64`, so a walk over `F64`s needs `0.0` there. This is an open target of the language climb (`just climb`).

## Open: ahead-of-time compilation

`--aot` compiles typed ry before the program runs (user directive; `decisions/typed-stdlib.md`, "Typed code compiles ahead of time, and `ry build --aot --exe` writes an executable"). Built: `ry run --aot main.R` for a script's top-level definitions (`decisions/typed-stdlib.md`, "`ry run --aot` compiles a script's typed code before it starts"; `reference/cli.md`, "Compiling ahead of time"). Built: `ry install` and `ry build` put elaborated sources with canonical annotations into a typed package's tarball, so every function of an installed package generated from a typed benchmark program compiles at its first call, as the script does (`decisions/typed-stdlib.md`, "An installed typed package holds elaborated sources with canonical annotations"). Generate such a package with `tools/bench/typed_package.py`. Decided: untyped R gets no machine code ahead of time (`decisions/jit-ssa.md`, "Untyped R code is not compiled to machine code ahead of time"). The contract for the file is `development/typed-code-file.md`; read it first. The steps below are ordered, and each is one pull request. A prototype of install-time typed code that is not in the tree measured 50 typed functions of a generated package on an iterate build: first calls took about 1 ms from installed code against about 40 ms compiled in the session, and added 1.8 MiB of peak resident memory against 5.9 MiB.

**Rules for every step.** Run the tool before claiming what it does. Each typed program must print the same in every tiering, so extend `typed_scripts_agree_however_they_compile` (`crates/ry/tests/test_scenarios.rs`) with the step's new mode before relying on it. Gates: `cargo test -p typed`, `-p jit --test test_tiers`, `-p frames --test test_standard_library`, `-p ry-lang --test test_scenarios --test test_command_line --test test_docs`, and the oracle on `ry-jit` and `ry-gctorture`. Do not run `cargo fmt --all` (main is not rustfmt-clean). `crates/ssa`, `crates/jit` and the call path in `crates/interp/src/bytecode.rs` belong to the tier-1 track: tell it before changing them.

### Step 2: the code file for an installed package, loose ends

- **The copy path has not run on macOS.** It compiles for `aarch64-apple-darwin`; run the `install-typed-package` scenario there (with and without `RY_TYPED_FILE_COPY`) when a macOS machine or CI job is available. If macOS refuses `mprotect` to executable on the copied range, use `MAP_JIT`, do not chase entitlements.
- **The bench driver is plain R.** `ry_typed_file` in `tools/bench/bench.py` calls the installed package from an untyped script, because the checker does not resolve names `library()` attaches; each call converts its arguments at the boundary, which shows in `vector_arith`'s warm time. Make the driver strict once the checker resolves attached packages' exports (structure track, `crates/semantics`).
- **Published numbers.** The `ry_typed_file` column needs `just bench kernels --release ARCHIVE --write` and the same for `applications` on a release that has the file.

### Step 3: fewer first-call compiles, loose ends

- **Mutually recursive functions in the file.** A version whose direct callee is still attaching misses, so `even`/`odd` compile in the session while `fib` attaches. Attach a strongly connected component at once (all or none) when a real package shows such a cycle.
- **Script roots with record, tuple, `Dict`, `Set` or function parameters** compile ahead only when a compiled-ahead function calls them. Training covers the installed case; the script case needs the checker's call-site types.

### Step 4: `ry build --aot --exe main.R`, loose ends

- **macOS has not run it.** `sign` calls `codesign --force --sign -` on the output; run the `build-exe` scenario on a macOS machine or CI job when one is available.
- **Sourced files compile at run time.** A file the script sources is read from disk and compiles as under `ry run --aot`. Embed sourced strict files when a real program ships as an executable with them.

### Not planned

- **Machine code for untyped hot bodies.** Trained code images already carry the profile, so tier 1 compiles a hot body at its first calls. Storing its machine code too has a start condition, which the untyped decision names: the applications suite showing tier 1 winning, and its compile showing in a cold run.
- **Session `--aot` in parallel.** `ry run --aot` compiles on the main thread (1 to 6 ms per benchmark program, 51 ms for `pidigits`). `ssa::Relocatable` makes worker-thread code generation possible for the file writer; using it for a script would mean copying finished code into JIT memory, which is worth doing only if a real script's up-front compile exceeds about 100 ms.

## Open: Windows

Unverified until a Windows release smoke test runs:

- **lattice's vignettes failed to install.** Fixed, unverified on Windows. The install driver spliced the install directory into the `inst/` paths with `sub("^inst", instdir, ...)`, and a regular-expression replacement drops the backslashes of a Windows path, so the `doc` directory was made in the wrong place and `file(con, "w")` on `doc/index.html` failed. The driver now concatenates, `list.dirs` and `list.files` return `/`-separated names on every platform as GNU R does, and recursive `dir.create` splits on both separators on Windows (`test_system`).
- **The base code images were not written on Windows.** Fixed, unverified on Windows. The Windows front end took a stray `R` first argument; `r_arguments` in `crates/rcmd/src/base.rs` now adds it on Unix only.
- **Windows findings after lattice.** Fixed; the smoke test is unverified. The embedded base archive now keeps file modification times, because a code image is valid only for the lazy-load database's length and mtime and extraction used to reset them. Windows package DLLs link `-lintl -liconv` (`LIBINTL` in the Windows Makeconf), because a package that includes `libintl.h` gets `libintl_vsnprintf`, which GNU R's `R.dll` exports and ry's libR does not. Lock files open with write access (`LockFileEx` rejects append-only handles), and a new home's lock is released before the staging directory is renamed into place and retaken on the final path, because Windows cannot rename a directory holding an open file.
- Drop `cli.md`'s Windows `R_HOME` note for linear algebra once the no-`R_HOME` smoke check passes on a Windows release.
- **ry's own base R has never run on Windows.** Earlier Windows releases ran on GNU R's base, so the smoke test after a green base build is the first run of the bundled base there. Behind lattice wait 11 more recommended packages, the archive, and the smoke test.

### Crash reports

- **No crash report on Windows.** `crates/crash` installs signal handlers on Unix only. A Windows build needs `SetUnhandledExceptionFilter` plus `RtlCaptureStackBackTrace`/`SymFromAddr`, with the exception code and address in place of `si_code`, and a stack-overflow case (the filter runs on the overflowed stack unless a thread reserves guarantee space with `SetThreadStackGuarantee`). `interp::crash::write_traceback` is already platform neutral.
- **Crash report symbols.** macOS keeps `backtrace_symbols_fd`, whose `dladdr` may or may not see local symbols (unverified). On Linux a shared library rebuilt on disk after it loaded is named from the new file; checking the GNU build-id note at the load base against the file's would fall back to `dladdr` instead. The release size cost of the kept `.symtab` is printed by `optimize.sh` but not yet recorded.

## Open: Windows base R and package installs

- **lattice's vignettes failed to install (fix landed, unverified on Windows).** The install driver spliced the install directory into the `inst/` paths with `sub("^inst", instdir, ...)`, and a regular-expression replacement drops the backslashes of a Windows path, so the `doc` directory was made in the wrong place and `file(con, "w")` on `doc/index.html` failed. The driver now concatenates, `list.dirs` and `list.files` return `/`-separated names on every platform as GNU R does, and recursive `dir.create` splits on both separators on Windows (`test_system`).
- **The base code images were not written on Windows (fix landed, unverified).** The Windows front end took a stray `R` first argument; `r_arguments` in `crates/rcmd/src/base.rs` now adds it on Unix only.
- **Windows findings after lattice (fixes landed, smoke test unverified).** The embedded base archive now keeps file modification times, because a code image is valid only for the lazy-load database's length and mtime and extraction used to reset them. Windows package DLLs link `-lintl -liconv` (`LIBINTL` in the Windows Makeconf), because a package that includes `libintl.h` gets `libintl_vsnprintf`, which GNU R's `R.dll` exports and ry's libR does not. Lock files open with write access (`LockFileEx` rejects append-only handles), and a new home's lock is released before the staging directory is renamed into place and retaken on the final path, because Windows cannot rename a directory holding an open file.
- Drop `cli.md`'s Windows `R_HOME` note for linear algebra once the no-`R_HOME` smoke check passes on a Windows release.
- **ry's own base R has never run on Windows.** Earlier Windows releases ran on GNU R's base, so the smoke test after a green base build is the first run of the bundled base there. Behind lattice wait 11 more recommended packages, the archive, and the smoke test.
- **Landed:** the `fork` module is Unix-only again (a moved `#[cfg(unix)]` broke the Windows compile); the `Sys.which` fast path is Unix-only, because Windows base R defines its own `Sys.which`; the base tree carries GNU R's `Rblas.dll` and `Rlapack.dll`; it writes `etc/Rcmd_environ` and base's `Rprofile` with the lines GNU R's installer uncomments, so Rtools comes first on `PATH`; `shlib` runs `make` on the `PATH` that `Rcmd_environ` sets, which got grid and splines compiling (`shlib_runs_make_on_the_path_rcmd_environ_sets`).

## Open: name matching gaps against GNU R

- **`$` on an `NA` name.** GNU R's `$` matches an `NA` name as the text "NA" (`l <- list(1, 2); names(l) <- c(NA, "b"); l$N` is 1), and `w[["NA"]] <- 3` replaces an `NA`-named element; ry matches nothing and appends.
- **`[[` with `exact = NA`.** `list(abc = 1)[["a", exact = NA]]` warns twice in ry (`subset2_level` and `single_position` both match) with the wrong call, and an ambiguous partial match warns twice in GNU R and not at all in ry.

## Open: one Renviron reader

- ry has two Renviron readers, the interpreter's startup one in `crates/interp/src/session.rs` and `rcmd::command_environment`. The interpreter's lets an already-set variable win, which differs from GNU R's `process_Renviron` (a file line always overrides). Merge them into one reader with GNU R's semantics.

## Open: BLAS buffer addresses in package code

- Base's BLAS and LAPACK calls hand buffers at GNU R's addresses modulo 16 (`decisions/builtins.md`, "Base R's linear algebra", item "BLAS and LAPACK buffers sit at GNU R's addresses modulo 16"). A package's compiled code that calls BLAS on `REAL()` of a vector of 16 doubles or fewer still sees 0 modulo 16 where GNU R has 8, so on an OpenBLAS SSE2 kernel (Zen, Sandybridge, Nehalem and older x86 targets) its last bit can differ from GNU R's. Closing it means putting small vector data at 8 modulo 16 in the heap (`VECTOR_DATA_OFFSET`, the small-object slot sizes), which the JIT and the start-up image also depend on. Unmeasured on real packages.

## Open: what bundling base R left undone

- **Start-up image follow-ups.** An empty `ry run` takes about 29 ms from the mapped image (48M instructions, 7M of them the image load, of which `Frame::from_image` is the largest part; about 4,300 page faults against 368 for `ry --version`, most of them lazy faults on mapped image pages, `decisions/heap-gc.md`, "The start-up image is the heap's pages at a fixed address, mapped in place"), of which about 3 ms re-runs base R's profile and the utils, grDevices and stats hooks (`decisions/heap-gc.md`, "The start-up image is build output of the base tree, and every start re-reads the environment"; installed packages get no image, "Installed packages load through native fast paths"). Next: the first S3 dispatch of each generic costs about 0.3 ms against 6 µs for a plain closure call (`UseMethod` lookup); `.expand_R_libs_env_var` costs about 0.2 ms warm and could run natively; utils' `.onLoad` reads `etc/repositories` with `read.delim` (0.73 ms against GNU R's 0.41 ms) and `file_test` builds a data.frame through `file.info` (72 µs against 12 µs); spilled frames rebuilt per start (`Frame::from_image`, 1.2M instructions) could be laid out in the pages too; ship images for other default-package sets (`R_DEFAULT_PACKAGES=NULL` with methods starts slowly, 63 ms); let interactive consoles and sessions with profiles load the image and run the profiles after it; let `ry_start_session` embedders and `ry load` use it; CI could check that a release's embedded image matches its runtime identity.
- **Windows linear algebra comes from the building machine's GNU R.** No system LAPACK exists there, so `tools/base-r/lapack.sh` copies GNU R's `Rblas.dll` and `Rlapack.dll` beside the runtime and the base tree keeps them in `bin/x64`. Building R's reference BLAS and LAPACK (`src/extra/blas`, `src/modules/lapack`) instead would need a Fortran compiler in the release workflow.
- **CI must run GNU R 4.6.2.** The oracle's expectations are GNU R 4.6.2 output (`development/corpus-and-oracle.md`, "The reference GNU R"); CI runs `nix develop .#ci -c just ci`, and the flake's `ci` shell provides that release. Without GNU R, `oracle compare --engine ry` and `--engine ry-jit` pass 251 of 251 and skip the 12 cases whose packages ry's bundled library lacks (data.table, bit64, rlang, knitr, rmarkdown, ragg, lattice). Those 12 expectations print what the packages' CRAN releases printed when recorded, so a package release can change them.
- **Nix.** `flake.nix` fetches the tarball as a fixed-output derivation and builds the base in `postInstall`; `nix build` has not been run yet.
- **Old base trees stay in the cache after an upgrade**, as runtimes do (see below).

## Open: the benchmark packages under ry's runtime

`just bench-compare` gives the current numbers; "Measured state at the full benchmark size" below records the last ones.

- **xts peaks 34% above GNU R.** In the packages suite (`tools/bench/packages/results.json`), xts's tests peak at 451.7 MiB on ry against 338.0 MiB on GNU R, the only package far above GNU R's peak. Owner: B4 (xts and zoo), after ALTREP. Start with what xts allocates that GNU R shares or frees earlier: index copies through `[.xts`, attribute duplication, and survivors the collector promotes.

- **Package install speed.** `R CMD INSTALL` of Matrix takes 39 s on ry against GNU R's 177 s (74 s with `MAKEFLAGS=-j4`), and almost all of it is the compiler (`decisions/libr-rcmd.md`, "Package builds run `make` in parallel and link BLAS and LAPACK by versioned file"). The R half is about 10% slower than GNU R's on the same `tools` calls: MASS's Rd objects take 0.55 s and help indices 0.27 s; Matrix's S4 sourcing takes 2.7 s. Levers: Rd parsing (`tools:::.install_package_Rd_objects`), and the separate test-load process (about 0.2 s per package).

- **Quoted byte strings with a quote character.** GNU R prints a quoted `"bytes"` string that contains `"` with a stray trailing `\0` (`"q\"\\xff\0"`), an artifact of its escape buffer. ry prints it without the `\0`; this is deliberate.
- **Per-test time.** With the JIT off, finbench's tests take 1.15x GNU R's time. `sd(x)` on 20 doubles costs about 104.5k instructions against GNU R's 70.6k. Measure GNU R marginally (a run of N2 calls minus a run of N1, both above zero): GNU R byte-compiles a closure on its first calls, so subtracting an empty baseline charges that compilation to the loop and roughly doubles small per-item costs. ry's cost is spread over bytecode `execute_code` (its self cost is level with GNU R's `bcEval`), function lookups (93 per `sd`, through `frame_cell` and `cached_cell`), allocation (`allocate_uncollected`, `alloc_triple_keeping`, `define_formals`, `root`, the write barrier) and `mk_char`. A suite profile (`perf record` over `oracle/src/bench.R` with `RY_BENCH_FILTER`) is flat: `execute_code` 12%, `frame_cell` 4%, `allocate_uncollected` 4%, `Heap::drain` 3.4%, `mk_char` 2.7%. Promise code is compiled (`decisions/heap-gc.md`, the promise paragraph in "The runtime's object model, collector and native boundary"), but `sd` passes few compiled arguments, so it gains little there; arithmetic arguments run about 1.6x faster. `execute_code`'s self cost per `sd` grew by about 4.4k instructions when the record and `match` opcodes joined `step`, at the same instruction counts; a profile with debug info (`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`) should show whether the larger dispatch or register pressure in `step` is the cause. Next levers: fewer lookups per call (about 79 `lookup_function` calls per `sd`), fewer cells and promises per call (`define_formals`, `allocate_with`), and the callers of `mk_char` in the test code. The untyped stdlib templates (`tools/bench/tooling/templates`) show the same gap: the ledger fold costs about 15.8k instructions per fill on ry against about 6.1k on GNU R (both marginal), and readings generation about 7.4k per reading against 4.5k, now that compiled `out[[i]] <- v` updates a local vector in place (`assign_element_in_place`) instead of running `complex_assign`. `apply(m, 1, f)` on a 4-column matrix costs about 13.8k instructions per row against GNU R's 16.8k, after the `m[, j]` column fast path; the kernels session measured it at 1.9x GNU R's time before that, and xts `period.apply` (about 33k closure calls through `[.xts` and `mean.default`) at 1.3x, which is still to be re-measured. In the ledger fold, `frame_cell` runs about 25 times per fill for about 14 variable and 7 function lookups, because `lookup_variable` searches the local frame before consulting its cache; the per-instruction dispatch lines (opcode fetch, operand decoding, the handle-mark check) cost about as much as the work. `frame_cell`'s cost per `sd` moved by about 1k instructions between two builds at the same call counts; the likely cause is that the frame filter hashes symbol addresses, so its collisions change when allocation does (unconfirmed).

- **`install.packages()` with several packages installs nothing.** On ry, `install.packages(c("a", "b"), lib = ...)` returns without installing any of them and without an error; one package per call works, so the benchmark library was installed one package at a time in dependency order.

- **Old runtimes stay in the cache after an upgrade.** Each release extracts its runtime into a new `ry/runtime/<key>/` (tens of MB), and nothing removes the previous release's directory. R homes are pruned already (`rcmd::cached_home` removes stale homes no process holds); runtimes need the same treatment, without removing one a running session still maps.
- **Measured state at the full benchmark size** (`generate.py --lines 300000 --files 500`, about 306,000 lines each; the `codebases` suite, one round, release build). With the dependency closure installed except `polars`, `Rblpapi` and `esgtoolkit`, ry matches GNU R on every test (finbench 9,592, dsbench 9,416, biobench 8,927) and the suite times are 1.48x, 1.59x and 1.53x faster than GNU R's (135.3 s against 199.6 s, 63.0 s against 99.9 s, 73.8 s against 113.2 s); per-test geometric means are 1.44x, 1.59x and 1.57x. The JIT is within 3% of `RY_JIT=off` (131.5 s, 62.6 s, 73.5 s), because profitability gates keep it from compiling bodies that would mostly hand back to tier 0 (`decisions/jit-ssa.md`, "Bodies that would mostly hand back to tier 0 compile only for loops"). The earlier figure of 1.21x to 1.35x slower, from a run with 10,000 or more tests per suite, predates these and is not reproduced; whether the cause is the library, the test set or the interpreter work since is not isolated.
- **The scoreboard** is the `codebases` suite (`tools/bench/codebases/`, `just bench codebases`; it needs `cargo build --release -p oracle` and the library from `just corpus`, read from `RY_CORPUS_DIR` or `.corpus/library`). `--profile-summary` splits time by the entry count of the innermost closure body; `big-levers.md`, "Where the full-size code bases spend their time", has the three splits. Snapshot code that calls an entry point R 4.6's headers no longer declare (`ATTRIB`, `Rf_findVar`, `Rf_findVarInFrame`) truncates the returned pointer to an `int` and aborts under GNU R 4.6.2 too (xts's `x[2:4]`, Matrix's `Cholesky`), so `tools/corpus/install.R` reinstalls every package whose shared object imports such a function from the newest CRAN source (`MEMORY.md`, Long-term, "Testing").
- **Corpus packages that still fail to load under GNU R 4.6.2.** Missing system libraries: udunits2, proj, gsl, netcdf, Magick++, mysqlclient, protobuf and tesseract (add them to `tools/corpus/system-packages.txt`), plus rJava. spatstat.random's newest source needs spatstat.data >= 3.1.9. `tools/corpus/install.R` upgrades a dependency that fails an installed package's version constraint from CRAN, but it does not rebuild the packages already compiled against the old version of an upgraded `LinkingTo` dependency.
- **Next.** Re-measure on the release archive (`--write`), then run `ry check` over the generated code and treat every finding as a possible false report.

### Why the code bases run at 1.5x, ranked

`development/design/codebase-gap.md` has the measurements, the Amdahl table and the ceiling (2.7x to 2.8x with everything but native code at 3x). About 60% of each code base's time is in interpreter-bound package closures that run at 1.4x to 1.5x, because a closure call, an S3 dispatch and a `tryCatch` each cost about what they cost in GNU R. `compare_profiles.py run --no-profile`, then `run`, then `report --times` reproduces the per-closure comparison (`tools/bench/compare_profiles.py`'s docstring).

1. **Closure calls 3x to 5x cheaper.** Call setup is 12% of each code base at 310 to 330 ns per call, and a plain `f0()` takes 126 ns against GNU R's 96. This is the frame and promise representation redesign; it lifts the interpreter-bound majority, so nothing else moves the total as much (about +10% from setup alone, 2.0x to 2.4x with interpreter-bound closures at 3x).
2. **S3 dispatch at the cost of one call.** Lookup costs 1.1 to 1.6 µs per dispatch (4.5M dispatches). A name only base binds resolves with one frame read (`dispatch.rs`, `lookup_in_scope`), which cut the lookup share to 2.8%, 1.6% and 1.6%. Next: profile the rest (method names, `.Class`, the method's frame); "S3 method resolution by a dense per-generic index" below is the larger redesign.
3. **Conditions.** `withCallingHandlers`, `tryCatchList` and `tryCatchOne` run as prologues (`base_fast_paths.rs`), so `tryCatch(expr, error = ...)` costs 3.8 µs against GNU R's 5.2. Their self time in the code bases barely moved, because the cost in context is the test block's promise (item 5). The four `tryCatch` frames stay (MEMORY.md, long-term).
4. **Run-once code in the AST evaluator.** Test bodies run uncompiled on both engines (GNU R compiles a closure outside the global environment only on its second call), and ry's AST evaluator is 1.5x to 3x slower there: 0.9% to 1.8% of the time.

A wider tier 1 is not first: JIT-compiled code is 1.3% of finbench and 0.1% to 0.4% of the others, and the closures it would compile spend their time in calls, dispatch and builtins. ry defers S4 definitions from load to first use (`.ry_force_s4`), which puts 1.1 to 1.5 s (1% to 2%) of GNU R's load-time work inside ry's test blocks; that is a measurement asymmetry, not a gap.

## Open: the CRAN package corpus

`just bench-packages install` then `run` (`tools/bench/packages/`, 59 pinned packages). `fs` needs `USE_BUNDLED_LIBUV=1` where libuv's headers are absent. First run (one repeat, with a build competing for the cores): 17 packages pass on both engines at a geometric mean of 0.99x GNU R's speed (lgr 0.60x, cli 0.89x the slowest); foreach and snakecase fail on both.

- **Re-run rlang's tests** since the S4 `[[` fallback crash was fixed (purrr passes 718 of 718 on ry).
- **ALTREP reads on hot paths.** Two open items. First, the JIT extern element reader: the JIT reads compact integer and double sequences inline but side exits on every other ALTREP read (deferred strings, wrappers, package classes) until it calls `ry_altrep_int_elt` and the real and string forms on the cold side of its `OBJECT|ALTREP` guard, with the plain read unchanged in cost. Second, a compact read in tier 0 within 20 Ir of a plain one (today about 110 to 190 Ir more; with the JIT off, `for (i in seq_along(x)) x[i]` over `x <- as.double(1:n)` costs about 850 Ir per iteration against 750 for a plain `x`). Arithmetic on a wrapper over attributed data materializes and copies. `decisions/r-compat.md`, "Runtime semantics settled by the corpus package tests", item "ALTREP vectors are first-class at GNU R's thresholds".
  - Known divergence: `match()`, `unique()`, `duplicated()` and `split()` expand a compact sequence argument in place, through `values::materialize` in `crates/interp/src/sort.rs`, so `.Internal(inspect(x))` reports `(expanded)` afterwards where GNU R reports `(compact)`. Peak memory is below GNU R's (13.3 MB against 17.6 MB for `match(1:1e6, 5e5:1e6)`), and only `inspect()` shows the difference, so it stays as is. A fix would read through `values::read_contents`, a transient copy on every call, which costs instructions for no visible gain.
- **Compact-sequence reads in JIT loops.** A compact read costs a flag test, the info pointer, three loads and the element arithmetic, so convolution runs at about 1.06 to 1.08x the Ir of plain vectors. A preheader hoist of the sequence info (defined on the OSR entry path too, which enters with unloaded slots), loop-variable provenance for the index, and strength reduction of `x[j]` to a carried induction value should bring it to about 1.0x. Each is a single pass, which fits the compile-time budget. `decisions/r-compat.md`, "Runtime semantics settled by the corpus package tests", item "ALTREP vectors are first-class at GNU R's thresholds".
- **data.table on ry** (main.R, after the `as.name(NA)`, empty-rowname `rbind` and `na.print` fixes, not yet re-run): a factor join column returned as factor instead of character (1483.51/55/71); no error for `:=` when `.datatable.aware = FALSE` (2252.1); `:= NULL` on an unserialized data.table keeps the column (2351.1); and the 2306 group (`sort_by`, `selfrefok`, "unimplemented type 'list' in 'orderVector1'"), which runs only because ry reports R 4.6 where GNU R 4.3 skips it.
- **jsonlite passes 10 more tests on ry** because ry's datasets package is R 4.6's (`gait`, `penguins`, `penguins_raw`, `sunspot.m2014`); not a bug.
- **The progress dashboard's package datasets are thin.** `cran_popular` is empty until a test run over popular CRAN packages exists, and per-dependency test results are recorded only for data.table, RcppSimdJson, xts and PerformanceAnalytics (`development/releasing.md`, "The progress dashboard").

## Open: native methods core

The methods package's core runs in Rust behind hash-guarded prologues (`decisions/builtins.md`, "The
methods package's core runs natively behind hash-guarded prologues"). `new()`, the default
`initialize()`, `validObject()`, `is()`, `extends()`, `setGeneric()`, `setMethod()` (including on
primitives), `setClass()`, `setClassUnion()`,
`cacheMetaData()` and single-argument inherited dispatch (`.InheritForDispatch`) are native when
nothing observable would differ, and so is the implicit generic `setMethod` creates for a base
S3 generic. A package's held-back S4 definitions run in units: classes and methods on other
packages' generics at first S4 use, each package generic at its own first use
(`decisions/libr-rcmd.md`, "A load holds back S4 definitions and caches its index across processes", item "Each package generic is its own unit"). There is no S4 cache. On the S4-heavy
bench package (2,892 definitions, `just bench-s4-first-use target/s4heavy
lubridate,data.table,xts`) a cold first `new()` costs 80 to 87 ms.

- **Held-back units differ from an eager load in two edge cases.** A unit that errors midway
  stays marked as run and half-defined (its generic is not merged into `.genericTable`), where
  an eager `load_all` would have failed. Lazy generics forced alongside a failing unit report the
  failure on every use (`.ry_s4_failed_placeholder`). And a unit-0 statement that calls a lazy
  generic while it runs (`prototype = prototype(v = helper())` with `helper()` calling it) runs
  that generic's unit before the rest of unit 0, so its methods may register before their
  classes exist. Fix: compute unit dependencies through the call graph, not direct reads only.
- **Next: a native class table with dense ids** (`decisions/builtins.md`, "The methods package's core runs natively behind hash-guarded prologues", item "Per-generic units came before the class table"). Unit 0 is about 416M instructions, and `setClass` is 29% of it, spread over
  class-definition lookups, `tryNew` and the extension objects `setIs` builds. The R-level metadata becomes views produced from the table.
- **What still falls back** (`RY_LOAD_VERBOSE=true ry load .` names each with its reason):
  classes with a basic-type or other-package superclass (`contains = "numeric"`, `"list"`,
  `"data.frame"`, an old class), a second superclass that has superclasses of its own, a class
  under a union whose members are not all cached yet, a method that omits an argument its
  signature names, a generic whose body is not `standardGeneric("f")` (braced or not),
  `setOldClass`, a first union of a session defined while first-unit definitions are pending
  (they may add an `initialize` method), a union of unions,
  `setMethod("[")` whose formals need rematching, and `cacheMetaData` delegating classes whose
  cached definition differs, other packages' generics, group generics and primitives.
- A reload after editing a method or a class takes 0.2 to 0.45 s, and dispatch costs 2.3 to
  4.5 µs per call.
- **Per-object frames in `new()`.** Every `new()` of a class with a validity method makes four
  environments: `standardGeneric("initialize")` reifies the generic's slot frame
  (`dispatch::do_standard_generic` takes `call.env()`, and `methods::exec_method` copies its
  binding cells into the method's environment), and the prologues of `new`, the default
  `initialize` and `validObject` pass `environment()`. Fix: read the signature arguments and the
  formals' bindings from the generic's slot frame without reifying it, and give the prologues
  frame access that does not reify (`decisions/builtins.md`, "`validObject` runs validity methods without its slot loop, and `new()` copies the prototype shallowly").
- `setDataPart()` (R code, about 28 µs per call), which the default `initialize` calls for every
  data part, so `new("MethodDefinition", f)` costs about 50 µs (GNU R about 340 µs).
  `.mergeAttrs` decides the attribute order, so a native version must reproduce it exactly.
- `as()` and the coerce methods it caches in `coerce`'s `.MTable`. Until it is native, the default
  `initialize` bails whenever a slot value's class differs from the slot class and extends it (a
  union member, a subclass), which costs about 200 µs per `new()`.
- Multi-argument inherited dispatch (`.findInheritedMethods`), `getMethod`, `selectMethod` and
  `existsMethod`.
- Two methods-package tests fail on both the native and the R path, and pass on GNU R:
  `basicRefClass.R` (`environment(foo) <- a` with a reference object signals "replacement object
  is not an environment") and `namesAndSlots.R` (`names(a) <- "A"` on an S4 object without a
  `names` slot does not signal an error).
- Matrix's `load_all` from source stops in `load()` with `readChar` on an empty file name, on both
  paths, after its class definitions have run.

- **Measured state.** There is no S4 metadata cache: per-generic units bring a cold first `new()` on `target/s4heavy` to 80 to 100 ms. An eager load of the synthetic package (`just bench-s4-load`: 583 classes, 1,899 methods, a third of them with extra formals) spends about 0.2 s in S4 definitions, against about 1 s with `setClass`'s subclass bookkeeping and `setMethod`'s `.local` wrapper in R, and about 5 s on GNU R. `RY_LOAD_VERBOSE=true` names every definition that runs the R code and why, and `RY_METHODS_NATIVE=0` turns the native paths off.

## Open: the hot suite

`just bench hot` is the fast stand-in for the three code bases (`decisions/tooling-bench-ci.md`, "The hot suite replays the package entries the code bases spend their time in"; `testing.md`, "The hot suite"). Regenerate it with `just bench-hot-capture DIR` after the generator's templates change or when `hot.py track` stops agreeing with the code bases.

- **The suite predicts a lower speed-up than the code bases show** (1.34x, 1.39x, 1.47x against 1.62x, 1.68x, 1.62x), so some of a code base's time behaves unlike a replay loop. Leads, none verified: per sampled call, the capture's time on ry is 1.3 to 1.5 times the replay's JIT-off time overall, but the two were measured hours apart on a machine whose speed drifts by 15 to 20%, so only the differences between entries are solid. Short calls stand out (`testthat::expect_true` 2 to 3 times, `base::data.frame` 1.9 to 3.7, `base::vapply` 2 to 4.7, `base::mean` 1.8, `openxlsx::write.xlsx` 1.9) while long ones match (`PerformanceAnalytics::table.AnnualizedReturns` 0.97, `stats::nls` 0.99). The code bases' runner does not use testthat's reporter, so that is not the cause. A replay that takes one call of every program in turn, counted in instructions against the loop's count, would separate work that depends on what ran before (inline cache misses, collections) from cache coldness, which instructions do not see.
- **`hot.py track` runs each code base once per configuration, one configuration after the other**, so the machine's drift over the two hours lands on whichever configuration ran then: an identical second copy of the reference build read 14 to 16% slower. Interleaving the configurations over several rounds would let it resolve changes under 20%, at several hours per study.
- **Programs slower than GNU R in every round of a three-round run** (release build, JIT on; median and range of GNU R's time over ry's): `base::rank` 0.64x (0.64 to 0.71), `stats::glm` 0.74x (0.69 to 0.82), `RcppSimdJson::fparse` 0.81x (0.64 to 0.98), ``lubridate::`%/%` `` 0.89x (0.67 to 0.96), `PerformanceAnalytics::Return.calculate` 0.91x (0.61 to 0.95). Another fourteen have a median between 0.80x and 1.05x with a range that crosses 1, among them `stats::lm`, `zoo::rollapply`, `base::table`, `base::do.call` and `rlang::error_cnd`. `--breakdown` and `--instructions --filter NAME` say where each one's time goes.
- **Recorder limits.** A name lookup does not know the name is used as a function, so a variable that shadows a function is captured as the variable. A deferred promise forced inside an error stays unbound in the sample. Lazy S3 registration's `get` and namespace hooks appear as boundary calls. Every distinct recorded call object stays a collector root until the run ends (`capture::free_names`): one per call site, plus one per generic a dispatched method was entered through, and one per evaluation for a call object built at run time (`do.call`, `eval(parse(...))`), so a recording's memory grows with the number of such calls. `cli::format_inline` is never its own key. `R_GlobalEnv::test_that` appears as a key because the oracle's runner defines its own `test_that`. Package-built code that a package evaluates in a user environment counts as user code. An environment chain that ends at the empty environment counts as global.
- **Time the suite does not hold** (25 to 30% of each run). Keys (a key is one package entry: the owning namespace plus the name the call was made by) with no replayable sample: `NlcOptim::solnl` (its arguments are local closures), `openxlsx::saveWorkbook`, `readxl::read_excel`, `readr::read_csv` and `brio::read_lines` (they read a file an earlier call wrote, which a replay does not have), `testthat::expect_error`, `rlang::cnd_signal`, `base::tryCatch`, `tryCatchLog::tryCatchLog`, `withr::with_tempdir` (their arguments name user code). Capturing an argument promise's value when the call exits, in place of its expression, would make the last group replayable.
- **A captured directory the code base created is not recreated.** Relocation (`replay.R`, `relocate`) moves a path under a capture's `Rtmp` directory to the replay's `tempdir()` and creates its parent, but it cannot tell a directory the user code made with `dir.create` from a file name, so `writeLines("hello", file.path(directory, "README.md"))` with `directory` a captured `tempfile()` fails ("cannot open the connection") and the validator drops it. Recording whether each captured path existed, and as what, would fix it.
- **`pt(ncp =)` differs from GNU R by a few ulp** (`pt(qt(0.037/2, 23, lower.tail = FALSE), 23, ncp = 1.25, lower.tail = FALSE)` is 0.18560522334244556 on GNU R and 0.18560522334244578 on ry), which `uniroot` in `power.t.test` carries to the thirteenth digit of `n`. The suite's checksum tolerates it; nmath's noncentral t should match exactly.
- **The instruction pass takes about 80 s for the whole suite**, dealt over four harness processes. Every region toggles callgrind's instrumentation, which flushes its translation cache; counting whole processes per program would avoid that and would include the session load.
- **No published results yet.** `--write` records a tagged release archive, and the breakdown needs a release that has `.ry_bench_region`. The benchmarks page's hot section renders once `tools/bench/hot/results.json` exists.

## Open: collector throughput on the benchmark suites

- **`list()` construction costs 1,215 instructions per 5-element named list, about 21% of the `orders` workload** (warm Ir, one thread, before measuring anything else). `do_list` in `crates/interp/src/subset.rs` pays these costs per call:

  | cost | instructions |
  |---|---:|
  | two allocations | about 205 each |
  | nine element and attribute stores, each through the write barrier on a fresh object | about 38 each |
  | a names STRSXP plus `set_attr` | about 131 |

  The ceiling is about 600 per list, or about 10% of `orders`. There are two ideas:
  - a barrier-free initializing store for objects allocated since the last safepoint;
  - a per-call-site constant names vector shared under NAMED semantics.

  Measure the realistic gain first. Two properties must hold: the shared names vector stays immutable (`names(x)[1] <- "z"` must copy it), and a fresh-object store never lands in an older object. Report `orders`, testthat_full and RSS.
- **Top-level loops compile but run at tier 0.** A `for`, `while` or `repeat` in the global environment compiles to bytecode with no local symbols, so every read and write of a global is a binding lookup and arithmetic on globals stays boxed; the optimizing tier does not run this code yet. Loops in `eval()`, `local()` and `test_that` environments stay in the tree walker, which steps a compact `1:n` without expanding it. The follow-ups (wiring `analysis::Frame::Global`, then the three gaps it leaves) and the oracle cases broadening needs are in `development/design/top-level-loops.md`.
- **Minor collections mark every S3 method-cache entry.** `dispatch::trace_method_cache` marks about 2.5k entries (about 28k marks of old objects) at every minor collection, about 18M of testthat_full's 108.5M collection instructions at `RY_BENCH_REPEAT=5` (0.4% of the run). Marking only the entries written since the last collection, as `new_roots` does for root stacks, would remove it; it is under the bar for a change of its own.
- **Measure first.** Generate the suites (`python3 tools/bench/tooling/generate.py --out .bench --lines 300000 --files 500`), install the corpus library (`just corpus`; the polars R package builds from source and takes the longest), then run `just bench-compare .bench/finbench .bench/dsbench .bench/biobench` with `RY_GC_LOG=<file>` set. The report's last block gives each runtime's `gc.time()` total and share of suite time; the log gives every pause (kind, nanoseconds, young small and large bytes, old bytes, process id), from which pause p50 and p99 and collections per second follow. Fix the largest cost the numbers show, then check each object type's allocation path against `decisions/docs-process.md`, "Data-oriented design and explicit allocators" (open audit items: "Open: data-oriented layouts and explicit allocators").
- **Builder peaks still above GNU R's by more than 10%** (fresh process per case, 2e7 doubles, peak over the inputs in MB, ry against GNU R 4.3.3): `seq(1, n, by = 1)` 381/305, `vapply(l, function(v) v + 1, numeric(1))` over a 1e6 list 112/58, `unlist` 20/4. `resolve()` in `crates/interp/src/subset.rs` still allocates a position list for a general subscript of more than eight positions.
- **Collector throughput toward a tenth of today's collection time** (testthat workload: 9 minor collections, about 17 ms of pauses, `collect_keeping` 66.9M instructions). Lazy-load fetches are pretenured (`decisions/heap-gc.md`, "Pretenuring lazy-load fetches"), so `library(testthat)` runs no collection. Where the pauses go: a minor collection's `trace` visits tens of thousands of objects at about 260 instructions each, most of them old objects rescanned from the remembered set or the roots rather than young survivors (39k traced against 4.5k newly aged in a load's collection), so find which old objects are rescanned and why; dead returned frames are cleared one cell at a time, with a cache miss on each value's mark bit and refcount (about 7 ms over 20k frames and 110k cells); the sweep and the young string table about 3 ms (`decisions/heap-gc.md`, "Minor collections touch only what changed").
- **Next GC candidate, once escape analysis lands: promoted survivors in the `records` workload.** Collecting survivors that the young generation promoted is about a fifth of a warm run's wall time. Each warm run makes about 4.5 minor, 0.5 intermediate and 1 major collection, with about 37 ms of pauses (16.8 minor, 5.5 intermediate, 14.9 major; `RY_GC_LOG`, REPEAT 3 minus REPEAT 1, halved). `collect_keeping` is 10.4% of the warm run's instructions inclusive. Escape analysis should keep most of these record temporaries out of the heap. Once it does, measure again before tuning promotion.
- **An intermediate collection costs O(heap), not O(young), which makes `RY_GC_STRESS=young` quadratic.**
  - **The numbers.** Under young stress every allocation runs an intermediate collection. The oracle case `strings__repeated_pieces` makes 699,026 of them (23 major), at about 2.1 ms each, so the case runs for 1,488 to 1,538 s. Bisecting found no regression: two builds weeks apart gave the same collection count and 2.12 against 2.19 ms per collection.
  - **Why it is quadratic.** Each collection walks the whole heap, though at most one object is young. The per-allocation cost therefore grows with the live heap, and the total grows with allocations times heap size.
  - **Where the time goes.** On a 10,000-row variant of the case (symbols-kept profiling build, 77.8G Ir), `collect_keeping` takes 99.7%:

    | cost | share |
    |---|---:|
    | `StringTable::remove_dead`, which walks the whole intern table | 51.6% |
    | `sweep_all` (`sweep_page` alone is 20%) | 22.0% |
    | marking, which iterates every entry of the `symbols` hash map | 17.5% |
    | `unmark_untenured`, `forget_dead_tenured` and the large-object list | the rest |

    All of these walk every page or every entry.
  - **Normal runs pay it too.** An ordinary intermediate collection pays the same O(heap) cost, just less often.
  - **The target.** An intermediate collection should touch only the pages that hold untenured objects. It should also handle strings and symbols only for the young ones, the way a minor collection already treats the young string table (`decisions/heap-gc.md`, "Minor collections touch only what changed"). That makes the cost of one collection proportional to its young survivors, and young stress linear in allocations.
- **Pin `TESTTHAT_PARALLEL=false` for collection counts and Ir comparisons.** A package whose DESCRIPTION sets `Config/testthat/parallel: true` (vctrs, rlang) runs its tests in testthat worker processes, which share the `RY_GC_LOG` file and which callgrind does not follow without `--trace-children=yes`. Collection counts then differ between runs, and a profile of the parent misses the tests. With the runner off, two vctrs runs give identical logs.
- **After-marking work is 1.45% of testthat_full** (weak references, returned frames, sweep: two thirds of the collector's time in minor collections; `decisions/heap-gc.md`, "Escalation prediction and the dense precious set", item "Minor-collection cost by source"). Revisit if the collector's share of the run grows.
- **GC torture over the package test suites** is owned by the robustness work (`oracle packages` under `RY_GC_STRESS` and `gctorture(TRUE)`, `development/robustness.md`).
- **`strings__repeated_pieces` dominates the `ry-gctorture-young` oracle run.** Under `RY_GC_STRESS=young` the case runs for more than 240 s (it passes; the rest of the engine's cases take about 15 min together). It builds several character vectors of 200,000 elements. The likely cause is that young stress retraces every untenured object at each allocation, so the cost grows with the live young set times the allocations (unconfirmed). Options: shrink `n` in the case if it keeps covering the substr and formatC sharing thresholds, or make young stress cheaper on large live sets.
- **String-heavy C code is bound by memory traffic** (`str_split`, see "Package kernels" below): suspects are the heap's allocation slow path and string-table probes that miss cache with about 2e7 live CHARSXPs. A character subset of 1e6 unique strings takes 0.012 s against GNU R's 0.003 s.
- **Peak RSS is sensitive to when collections run**: report +3.8 MB from inlined base wrappers (gc() max-used unchanged, fixed offset), array_map from tier-1 loop state. One owner should look at the GC trigger policy for both.
- **Freed large vectors reach the OS only after mimalloc's purge delay** (owner: the GC and heap track). The heap frees a large vector through `release_large_memory`, which calls `std::alloc::dealloc` into mimalloc, and mimalloc keeps the pages resident until its purge delay runs out, so a change that frees more large vectors can raise peak RSS while live data falls, depending on when collections run. Freeing a shared tier-0 body's instruction vectors with its last closure (`decisions/jit-ssa.md`, "Closures made at one site share compiled code") showed it: on one build of main the testthat_full peak rose from 41.55 to 41.97 MiB under the default policy but fell from 40.53 to 40.38 MiB with `MIMALLOC_PURGE_DELAY=0`, with an old generation 0.3 to 0.4 MB smaller in `RY_GC_LOG`. On a later main the same change measures 41.71 to 41.62 MiB under the default policy and 40.67 to 40.68 with purge 0 (median peak anonymous RSS of 5 rounds at `RY_BENCH_REPEAT=2`). Release every freed large vector promptly (decommit or purge on free above a size threshold, or an explicit purge after a major collection), then remeasure the default-policy peak.
- **Closures that share a tier-0 body share its lookup hints, so R6-style objects miss them.** Every closure over one `function` expression runs the same code object, and each R6-style object's methods have their own enclosure, so `lookup_function` misses the inline cache that another object's closure filled. Against the main it merged into, shared bodies run the objects workload at +0.21% instructions (7,637.1M to 7,653.3M at `RY_BENCH_REPEAT=2`) and +0.35 MiB peak anonymous RSS (42.64 to 42.99 MiB, median of 5 rounds). By function, on a slightly older main (+0.13%, +10.2M): about 5.9M is `lookup_function` with unchanged code, and 3.4M is `hash_shape`, the check that a body was not changed in place before a new closure takes its code. What the extra resident memory holds is not yet attributed; the `Bodies` columns and the shared code that lives as long as any closure over its body are the suspects. A call protocol that keeps lookup hints per closure or per enclosure, rather than per code object, would recover the first part.
- **Heap page placement makes peak RSS follow collection timing, not live data** (owner: the GC and heap track). With `formatC()` calling its kernel directly, report allocates less, runs fewer minor collections (105 against 138 on a formatC-only loop), and peaks about 1 MB higher. The GC log's old-generation and nursery bytes and massif's malloc peak match main, and all the extra residency is in one 32 MB arena. Fix the placement so the peak tracks live data; `testing.md`, "Judge a change's peak RSS", exempts such rises until then.
- **Collector throughput does not bound package code.** `RY_GC_STRESS=startup` runs in about 7 s in a debug build, and `just gc-torture` passes every oracle case in about ten minutes on four cores. Against GNU R 4.3.3 (profiling build, one thread), data.table's `fwrite` (0.09 s against 0.13 s) and keyed join (0.014 s against 0.039 s) are faster and `fread` is level (0.064 s against 0.060 s), while its group sum (0.057 s against 0.037 s), its order (0.073 s against 0.048 s) and jsonlite's `toJSON` on 20,000 lists (2.23 s against 1.50 s) are slower. Callgrind of `toJSON` puts collection at 1% and allocation at about 8% of self cost, so what remains is interpreter work (S4 dispatch, argument matching, lookups). `oracle bench` prints each runtime's `gc.time()`, and `RY_GC_LOG` records every pause.
- **Memory guards.** `runtime_peak_memory_stays_near_gnu_r` (`crates/ry/tests/test_perf.rs`, release, `--ignored`) fails past 1.10x GNU R's peak, and `runtime_speed_stays_ahead_of_gnu_r` in the same file holds `vector_arith`, `loop_sum` and `sales` above a speedup floor, because a memory win once cost them half their speed. With large temporaries freed from their reference counts, the subset and data.table loops of the CI report peak about 20% below GNU R's.

## Open: performance targets the hot suite ranks

Ranked by what each is worth on the hot suite's weighted instruction totals, the deterministic stand-in for the code bases (`testing.md`, "The hot suite"). Measure every one with `just bench hot --rounds 0 --instructions --save-instructions FILE` before and `--baseline-instructions FILE` after (about 80 s), and price a single mechanism with `just bench-mechanisms --filter TEXT` (`testing.md`, "What one mechanism costs").

1. **Closure calls outside the fast path** (estimated 9% to 10% of the suite's instructions, plus about 5% for frames given an environment without need). The next section has the census and the order of work. Re-take the census once slot frames hand out a pending environment, because that changes which mechanisms leave the fast path.
2. **S3 dispatch at the cost of one call** (about 26,000 method applications per second of GNU R work, 6% to 7% of the instructions). A dispatch to a compiled method costs 6,400 to 7,100 instructions against 1,100 for a fast closure call, `NextMethod` 14,100, a builtin dispatching to a method (`length`, `[`, `$`, `format`) 3,800 to 6,400. The unfinished work is on branch `wip/s3-dispatch-feedback` ("Parked: S3 dispatch feedback" below).
3. **`tryCatch` and the condition helpers** (about 3,800 `tryCatch` per second, 4% to 5%). `tryCatch` with no condition costs 34,700 instructions, with `finally` 15,300, catching a `stop` 82,300, `try` 39,100, `withCallingHandlers` 8,500, `suppressWarnings` 11,600, a muffled `warning` 77,600, `signalCondition` 14,300. The unfinished work is on branch `wip/native-trycatch` ("Parked: native `tryCatch` on slot frames" below).
4. **Programs slower than GNU R**: `base::rank` 0.64x, `stats::glm` 0.74x, `RcppSimdJson::fparse` 0.81x, ``lubridate::`%/%` `` 0.89x, `PerformanceAnalytics::Return.calculate` 0.91x ("Open: the hot suite" has the ranges). ``lubridate::`%/%` `` is not a remainder problem: its instruction count is the same with and without the 64-bit whole-number path of `%%` and `%/%` (within 0.2%), so look at what surrounds the operator (unprofiled; the S4 group-generic dispatch and the period arithmetic are the suspects). `just bench hot --filter NAME --breakdown` and `--instructions --functions 40 --filter NAME` say where each one's time goes.
5. **Builtin calls.** `positional_call_builtin` and its closure hold 3.3% to 4.0% of the instructions, before any kernel runs. Per call from compiled code: `is.null` 170, `$` 170, `class` 230, `names` 240, `[[` 270, `length` 280, `[` by position 310, `attr` 380, `seq_len` 660, `%in%` 720, `c` 730, `sum` 950, `identical` 1,110, `list(a = i)` 1,210, `nchar` 1,400, `[` by name 1,550, `environment()` 1,810, `sprintf` 2,340, `paste0` 3,910, `unlist` 7,680. Replacements: an element 650, a list field 1,160, `attr<-` 1,250, `class<-` 3,640, `names<-` 3,750, a variable in an environment 4,330.
6. **Apply-family overhead per element**: `lapply` over four elements 19,600 instructions, `vapply` 20,000, `Map` 34,200, `sapply` 57,000 (it runs `simplify2array` and `unique` in R), `do.call` 12,000, `eval` of a quoted call 10,800.

The top of the function profile on the suite (share of a code base's weighted instructions, fat-LTO build with symbols, `--functions 120`): `bytecode::fast_call_generic` 2.8% to 3.8%, `Heap::frame_cell` 2.0% to 3.3%, `bytecode::positional_call_builtin` 2.3% to 2.7%, `eval::force_promise` 1.9% to 2.4%, `memset` 0.5% to 1.7%, `eval::leave` 1.5% to 1.6%, `bytecode::fast_return` 1.3% to 1.6%, `eval::match_plan` 0.9% to 1.6%, `values::alloc` 1.4% to 1.5%, `bytecode::run_get_var` 1.2% to 1.4%, `bytecode::run_call_generic` 1.2% to 1.4%, `eval::detach_reference` 1.1% to 1.3%, `eval::enter` 1.1% to 1.2%, `eval::reify` 0.8% to 1.0%, `dispatch::lookup_in_scope` 0.4% to 0.9%. The profile is flat: no function holds 4%, so the gains are in removing whole mechanisms per call (a frame's environment, a promise, a match, a scope walk), not in tuning one function.

Where the instructions go by entry (share of each code base's weighted instructions, `iterate` build, JIT on, over the 76 of 100 programs that produced a count in that run):

| Code base | Entries above 4% |
| --- | --- |
| finbench | `testthat::expect_equal` 22.8%, `PerformanceAnalytics::table.AnnualizedReturns` 17.8%, `ompr::solve_model` 11.7%, `SharpeRatio.annualized` 11.0%, `openxlsx::write.xlsx` 4.8%, `testthat::expect_error` 4.0% |
| dsbench | `expect_equal` 39.0%, `expect_error` 7.7%, `[.data.table` 7.4%, `stats::glm` 7.1%, `data.table::dcast` 6.3%, `write.xlsx` 4.8%, `base::mean` 4.3% |
| biobench | ``foreach::`%do%` `` 27.0%, `expect_equal` 22.5%, `write.xlsx` 12.3%, `expect_error` 4.7% |

- **`expect_equal` is the largest single entry and has no hot function.** It costs 2.17 million instructions per call. On a loop of 40,000 expectations over small values, with testthat's third edition set, GNU R takes 10.9 s and ry 4.44 s (2.46x), and every R function's self time is between 1.3x and 2.4x of GNU R's. The functions below 1x are small: `withRestarts` 0.37x, `modifyList` 0.45x, `quo_squash_impl` 0.71x, `%||%` 0.75x, `match.call` 0.89x. The entry gets faster only as the whole call path does (waldo, rlang, cli and the condition helpers).
- **Measure an expectation with the edition set.** With no edition, every expectation calls testthat's `find_edition(".")`, which looks for a `DESCRIPTION` through pkgload, desc and R6, and that lookup is 97% of the samples on both engines. The code-base driver and the hot suite both set the third edition (`crates/oracle/src/bench.R`, `tools/bench/hot/replay.R`); a hand-written script must call `testthat::local_edition(3L)` inside a function.
- **Tier 1 does not reach this code.** About 200 package bodies are planned for tier 1 and refused (too rare, nothing native, too little native), which is why JIT on and JIT off differ by 0.2% to 0.4% on the code bases. A 3x code-base result needs a compiler tier that covers package code (generic values, S3 dispatch, promises, conditions), not more tier-0 trimming: the candidates below are each worth 1% to 3%.
- **Candidate: a strict formal takes the caller's value.** `call_fast` hands a callee an argument reference (a pooled promise object) for every argument that is a caller slot. When the callee forces a formal in the straight-line prefix of its body, before any call that could observe laziness, binding the caller's value directly skips the reference, its release and the force. A per-frame bitmask of such formals generalizes `Frame.inlined`. The ceiling is 4% to 5% of a call-heavy entry and 1% to 2% of a code base. Every reader that expects a promise in an argument slot must be audited first: `slot_content`, `slot_formal`, `frame_args`, `use_method_in_slots`, `slot_substitute`, `box_slots` and tier 1's slot reads. A formal that the callee passes on as an argument is strict only if the next callee is strict in it too, so the analysis must follow the chain or refuse.
- **Candidate: captured-variable reads.** A closure created in a frame makes that frame leave its slots for an environment on the first read of a captured variable, so `lapply(x, function(i) ... y ...)` pays for an environment per outer call.
- **Candidate: number formatting**, about 3% of finbench (`format_shortest_opt`, `format_exact_opt`, `push_real_alone`, `do_sprintf`): `format` and `formatC` on a column format each element through the general path.
- **Candidate: small base closures called with call arguments.** `ifelse`, `isTRUE`, `isFALSE`, `unname`, `%in%` and `unlist` have prologues that finish the body natively once the frame has forced the arguments (`decisions/builtins.md`, "Small base closures with call arguments run their bodies natively behind prologues"). What is left is the call itself: `isTRUE(!is.na(x))` costs 3,421 instructions, the same as `function(x) x` with that argument, against 804 for `isTRUE(b)` through the native entry. Removing it needs the call path to evaluate an argument expression that is provably pure before entering the closure (as `native::gather` does for typed closures), which the generic call path owns. Left without a prologue: `unique` (the S3 lookup must be repeated to prove it reaches base's `unique.default`; about 1% of the kernel that calls it), and `lapply` and `vapply`, where only the `match.fun` call could go (it forces `FUN` in its own frame, so a prologue may skip it only when `FUN` is already a value).
- **Gap: 24 of the 100 programs produce no instruction count when several run in one harness process** (`--instructions` deals the programs over four processes). Two errors seen: `yieldcurve-nelson-siegel` stops with "bad TAG: a 'NULL' has no cells", and `zoo-rollapply` with "long vectors not supported yet". Each passes alone, so state left by an earlier program in the same process is the suspect. Until it is fixed, compare weighted totals only over the programs both runs counted.

## Parked: S3 dispatch feedback (branch `wip/s3-dispatch-feedback`)

- **What is there.** Two commits on top of the mechanism micro-benchmarks. The first ports the one-frame S3 method lookup (`lookup_in_scope`), its fuzz test and its oracle case, all of which are on main, so a rebase drops it. The second (`wip:`) gives each generic's row in `crates/interp/src/dispatch.rs` a four-entry dispatch feedback, most recently used first, keyed by the class vector (by pointer), the group, whether a default is allowed, the calling environment and the definition environment. A hit is checked against the lookup stamps of every candidate name the full lookup tried and against the methods table's stamp and binding, and the method is read again from its binding. Only exact lookups are recorded (the calling environment is a top environment, no active binding or local frame took part). Both shortcuts have a cross-check against a full lookup behind `enable_invariants()`.
- **State.** The second commit compiles (`cargo build --profile iterate -p interp --lib`) and nothing else was run: no test, no oracle run, no measurement. 
- **What the targets still need.** The lookup is the smaller part. `use_method_in_slots` materializes the argument references, allocates a renamed call and recurses through `apply_closure`; entering the method in place from `op_call_generic` (`enter_in_place`, restoring `previous_owner` afterwards) removes that. Then the same feedback for `NextMethod` (`do_nextmethod`) and for builtin dispatch (`apply_internal_method`, which allocates a context environment per dispatch). Targets set for the work: `describe(object)` and `format(object)` at most 2,500 instructions, `length(object)`, `object[1L]` and `child$a` at most 2,000, `describe(child)` at most 5,000.
- **To revive.** Rebase onto main, build, run `cargo test -p interp fuzz_s3` and the oracle on `ry,ry-tier0,ry-jit`, then measure with `just bench-mechanisms --filter dispatch`. Add oracle cases for a method defined after a site dispatched to the default and then removed, a method registered in a namespace, a method shadowed locally, and an object whose class changes between iterations of one site, and a fuzz program that dispatches through generics.

## Parked: native `tryCatch` on slot frames (branch `wip/native-trycatch`)

- **What is there.** A `wip:` commit in `crates/interp/src/base_fast_paths.rs`, `eval.rs` and `values.rs`: `do_try_catch` runs through a `TryCatch` plan whose `tryCatchList`, `tryCatchOne` and `doTryCatch` frames are slot frames, so their environments and closures are made only when something observes them (`sys.function`, `sys.call`, `parent.frame`, a handler that inspects frames). It adds a native prologue for `withCallingHandlers`, keeps `signalCondition` off a heap frame, removes the `ryTryCatchList` and `ryDoTryCatch` builtins, and adds four condition rows to `tools/bench/tooling/mechanisms.py` (`suppressMessages`, a two-handler `tryCatch`, a muffled `message`, a classed `stop`).
- **State.** Never compiled, never run. The oracle case `conditions__handler_frame_introspection.R` (frame introspection from handlers) has no recorded output; it ran under GNU R.
- **Overlap with main.** Main has heap-frame prologues for `withCallingHandlers` and `tryCatchOne` (`do_with_calling_handlers`, `do_try_catch_one`), and `do_try_catch_list` calls the next helper through `apply_closure` with forced promises. The branch's slot-frame plan replaces all three; keep main's oracle cases passing.
- **To revive.** Merge main in and fix the compile and merge conflicts, record the case, run the oracle on `ry,ry-tier0,ry-jit`, measure with `just bench-mechanisms --filter condition`. Still to write: a cross-check under `enable_invariants()` that the helper heads are `tryCatchList`, `tryCatchOne` and `doTryCatch`; a condition-frames fuzz program in `crates/oracle/src/fuzz.rs`; prologues for `suppressWarnings`, `suppressMessages` and `try`; native restarts, `stop`, `warning` and `message`. Targets set for the work: `tryCatch` with no condition at most 6,000 instructions, with `finally` 4,000, `try` 8,000, `withCallingHandlers` 3,000, `suppressWarnings` 4,000, `signalCondition` 4,000, a caught `stop` and a muffled `warning` 30,000.

## Open: closure calls that leave tier 0's fast call path

About seven in ten of the hot suite's closure calls from compiled code take `call_fast` (`crates/interp/src/bytecode.rs`), and the rest pay two to ten times its price. `just bench-mechanisms --filter call:` prices each shape. The census below came from temporary counters in `call_fast`, `return_fast`, `op_call_generic` and the frame entry paths, summed over one timing round of every hot program (`profiling` build, JIT on, unweighted).

| Closure calls from compiled code (5.5M) | Share |
| --- | ---: |
| takes `call_fast` (10.5 points of it from a frame that reads its variables by name) | 71.8% |
| the site has no plan: it stopped planning after `PLAN_MISSES` different callees, or its callee never gets one | 9.0% |
| reaches the generic handler without passing `fast_call_generic` (not classified; `op_call_rebinding` is one route) | 7.6% |
| callee has a native entry (a base closure with a native prologue) | 5.5% |
| plan stored but not prepared | 5.5% |
| plan prepared but not fast, a row that belongs to another plan, or a caller that got an environment mid-frame | 0.7% |

- **What the generic path then does with them** (shares of all closure calls): 12.5% have a plan and enter the callee in place, 7.1% are answered by a native entry and 5.0% are declined by one, 4.7% match their arguments per call (`MATCH_PER_CALL`, mostly `...`; counted before a site that passes `...` kept a spread plan, and not counted again since), 3.9% enter an unplanned body in place, and 3.5% are sites that gave up planning.
- **A top-level call site with an argument that is a call never takes the fast path.** A site settles its compiled arguments only when it runs for a function (`closure_arguments`), so `prepare_plan` never runs for it. Script-level loops pay the generic path for such calls.
- **Next questions, in the census's order.** Why a stored plan stays unprepared (5.5%), and whether a site that gave up planning can keep one plan per callee instead of none. A callee with a native entry that declines (5.0%) pays the native attempt and then the generic path; a plan could record that the entry declines for this site's argument shapes.
- **Returns.** `return_fast` takes 93% of the returns to a compiled caller. One in six of those discards the frame's pending environment on the way. What leaves: the frame has a complete environment (4.9%), the pending environment is held by something that outlives the call (2.2%).
- **Environments.** About a quarter of the frames get a pending environment (1.6M of about 6.6M frames), and nine in ten of those are discarded at return without ever being completed. On the testthat application workload (350,000 closure calls) 13% of the calls make an environment for their slot frame, each call frees about 4 objects, and 0.19 of them is an environment and 0.37 a promise. Those per-call object counts did not move with the fast-path coverage, so they come from the callee's own work (`...` pairlists, promises for arguments that are calls, binding cells), not from which call path was taken.
- **Mechanism prices, instructions per call:** no arguments 760, constant body 640, identity body 890, one argument and a branch 1,090, a default 990, arguments by name 980, three arguments 1,170, `...` forwarded 2,040, `...` collected 2,900, `missing()` 1,780, a closure made and called in the body 3,850, `on.exit` 2,200, `match.arg` 8,250, `stopifnot` 5,000, `parent.frame()` 1,900, `sys.call()` 2,500, `assign` to a local 8,700, `do.call` 11,000, `eval` of a quoted call 8,900. The `...` row is two calls, the wrapper's and the forwarded one, so forwarding costs about what a plain call does while the site's spread plan holds.
- **The fast path's own cost is the floor for everything above.** A fast call and return is about 750 instructions against the 200 the end shape names (`decisions/interp.md`, "Closure call setup: the 10x end shape"). The parts: the frame record `call_fast` fills, the resume record, the argument references, and the slot clearing at return.
- **What gives a slot frame an environment** (each costs about 2,400 instructions): `eval` 14%, a builtin dispatching to an R method 13%, a call of a special 9%, a promise over the frame 7%, a closure outliving the frame 6%, `$` on a field 6%, `vapply` and `lapply` 10%, the generic call's arguments 5%, a promise outliving the frame 5%, subassignment and replacement calls 6%, `parent.frame` 3%, `.External2` 2%. Only `eval`, `parent.frame`, `.External2`, `sys.frame` and a closure or promise that outlives the frame observe the environment; the others are mechanisms a slot frame could serve without one. These shares predate the fast path taking calls from frames that read by name.
- **The AST evaluator makes about one closure call for every six from compiled code** (`apply_call`: 0.9M beside 5.5M), for promise code that is not compiled against the frame's slots, run-once bodies and `eval`. 85% of them use the site's plan.

## Open: a string `switch` without the linear name scan
## Open: one probe to intern a new string (B4)

- **A compiled `switch` on a string selector scans its arm names on every call.** `switch_arm` in `crates/interp/src/bytecode.rs` goes through `eval::switch_selector`, then compares the interned selector against each name in the table by identity, and compares text only if none matches (a missing name, or another encoding). On the expr_tree workload, `switch_arm` and `switch_selector` together are at most 3.6% of warm Ir under callgrind, which bounds the gain. The likely shape is a selector-to-arm map built once with the table, or a dense string id (below) indexing it, plus inlining the string case of `switch_selector` before its generic vector checks. It touches `bytecode.rs` call sites, so coordinate with whoever is rewriting them.

## Open: a dense id for interned strings (strings track)

- S3 method lookup walks the calling function frames below the top environment on every dispatch, by name (`method_scope` and `lookup_in_scope` in `crates/interp/src/dispatch.rs`, about 260 and 180 instructions per call). It is most of the dispatch cost left in the validation workload, where `format()` dispatches from inside `vapply`. Closure frames in fixed slots could answer "no binding of this name in any function frame" without hashing, which belongs to the closure-frame work, not to dispatch (`decisions/interp.md`, "S3 methods are found through one row per method symbol").
- S3 dispatch maps each (generic, class) pair to its `generic.class` symbol through a hash map keyed by the generic's symbol and the class `CHARSXP` (`MethodCache::names` in `crates/interp/src/dispatch.rs`, about 69 instructions per lookup, 0.7% of the objects workload). A dense id per interned `CHARSXP`, stored in the string, would let that become an index, and would serve every other table keyed by an interned string. Interning the class as a symbol instead costs more than the hash (`decisions/interp.md`, "S3 methods are found through one row per method symbol").

## Open: builtins slower than GNU R

`tools/bench/builtins/slower.txt` lists every export of base, stats and utils that the builtins sweep finds slower than GNU R (or failing) in at least one case; `sweep.py ratchet --update` shrinks it as fixes land, and `sweep.py weights` orders the work by call frequency in the code bases and the oracle corpus.

- **Failing on ry in the sweep:** `R_compiled_by`, `aregexec`, `rsmirnov`, `ppr`, `arima0`, `ar.mle`, `prmatrix` and `memDecompress` of an unknown type, among others; `sweep.py compare` prints the full list.
- **`names<-` and list copies:** a list whose reference count is shared copies on each `names<-`, where GNU R's reference counting lets the copy be skipped; structural, in the heap's reference counts.
- **`e[["a"]] <- v` on an environment** takes the `*tmp*` replacement call (0.47 s against GNU R's 0.12 s for 1e6 assignments); only `e$a <- v` has the direct path in `assign_element` (`crates/interp/src/bytecode.rs`).
- **POSIX repeated groups:** `(a)?(ab)?b*` on "abb" fills the first optional group where TRE fills the second (`strings__posix_repeated_group`, a known failure).

## Open: base R kernels against GNU R

`just bench-base` times base R's kernels against GNU R with checksums (`tools/bench/internal/base`). Kernels that take a few milliseconds swing by 2x between runs, so judge them with `--rows 1e7`. At 1e7 rows these are still slower:

- **Instruction counts on the application workloads vary between runs, mostly from data.table's threads.** Under callgrind with default threads, a data.table script's Ir varies run to run from libgomp scheduling and spin-waits in data.table's OpenMP regions, and turning ASLR off (`setarch -R`) does not remove it. With `OMP_NUM_THREADS=1` and `R_DATATABLE_NUM_THREADS=1` a data.table micro script spreads by about 1e-5, like a pure-R script (2e-5), but the full `tables` workload still spreads by 0.04% warm and 0.02% cold over five runs; that remainder is not yet attributed. Pin both variables for any Ir comparison that touches data.table, and report the median and the range of several runs; nothing under `tools/bench` pins them yet.
- **Strings:** everything measured is at or below GNU R (`paste(collapse =)` 0.80, `format` 0.90, `sub` 0.93, `grepl(fixed = TRUE)` 0.97). `as.character(double)` is deferred on both, so it is timed forced. data.table's `tstrsplit(s, "_", fixed = TRUE)` on 2e6 strings took 1.1 s against GNU R's 0.46 s (one thread); re-measure.
- **Grouping:** `order` on two keys runs at 0.88 of GNU R's time at 1e6 rows. Carrying the second key's values through the first key's scatter would avoid the random gather that dominates it.
- **Matrix and time series:** xts `period.apply(xt, endpoints(xt, "hours"), function(x) mean(x))` on 2e6 minutes (33,335 periods) takes 1.4 to 1.5 s against GNU R's 0.89 s, at 250k instructions per period against 204k, so part of the gap is not instruction count. Callgrind's cache simulation (`--cache-sim=yes`, marginal over two run counts) puts it in the instruction cache: about 7,040 L1 instruction misses per period against GNU R's 3,716, while data misses are close (2,382 against 1,872 reads, 700 against 822 writes). The extra misses cost roughly 0.4 s. They are spread thinly (`execute_code` about 460 per period, then `run_closure`, `native_call`, `bind_arguments`, `match_args` and `mk_char` at 120 to 190 each), so outlining cold arms of `step` alone has little room; the open lever is function ordering and keeping cold paths out of the whole call path. The release build's PGO and BOLT layout (`decisions/heap-gc.md`, "The runtime's object model, collector and native boundary", item "The Linux release build is profile-guided") cut misses from 5,450 to about 3,050 and instructions from 185k to 160k per period, and period.apply 2e6 from 0.70 s to 0.56 s, measured on a faster machine than the figures above, which come from an earlier run of the plain build. The bench recipes measure the plain build. Closure-call overhead is a broader target than this one case: a closure call costs 0.75 µs against GNU R's 0.35 µs, `vapply` 8.2 µs against 3.4 and `pmin` 15.7 µs against 6.2, and this per-call cost is why ggplot2, lattice and PerformanceAnalytics take 1.2 to 1.9x GNU R's time to render. The aim is a closure call at or below GNU R, measured with `oracle gallery` (149 timed figures) beside period.apply. Two more levers are queued: the `frame_cell` lookups in the fill loop of the finance benchmark's `ledger` template (`tools/bench/tooling/templates/finance/ledger.R`, cost not yet measured in isolation), and the S4 metadata-cache-miss path (`slot<-`, `list()`, `possibleExtends`; see the start-up item in `MEMORY.md`). The remaining instruction cost is spread: `execute_code` 27.5k (level with GNU R's `bcEval`), `method_symbol` formatting and interning about 21 method names per period (4.3k), `is_top_env` probing `.packageName` (2.4k), and xts' own `Rf_install` calls (4.4k). `apply` over rows is below GNU R per row; zoo `rollmean` 1.4x.
- **Array subscripts with an empty dimension:** `X[, , k]` reads walk the selected offsets without listing them (`ArrayOffsets` in `crates/interp/src/subset.rs`), and `X[, , k] <- W` takes the slab write path, so both beat GNU R on a 1000 x 1000 x 20 double array. An empty subscript still resolves to a position list as long as its extent, which a range selection that never materializes would avoid.
- **Printing an array with a zero extent:** `array(integer(0), c(2, 3, 0))` prints nothing in ry; GNU R prints `<2 x 3 x 0 array of integer>` and the empty row and column headers (`print_array` in `crates/interp/src/print.rs`).
- **Dates, `pmin`/`pmax` and `attributes<-` differ from GNU R on rare inputs**, with and without the prologues. `strptime("1700000000", "%s", tz = "UTC")` gives NA (GNU R parses the epoch seconds). `%D` reads as `%m/%d/%y`, and GNU R's own parser reads it as `%y/%m/%d`. `as.Date("\t2024-01-02")` parses, and GNU R stops with "character string is not in a standard unambiguous format". `strptime(x, f, tz = NA)` is accepted (GNU R: "invalid 'tz' value"). `as.Date.character()` with `x` missing reports the call `charToDate(x)`, and GNU R reports the closure that was called without it. `pmax(1:3, integer(0), 2)` gives `numeric(0)` (GNU R `integer(0)`) and `pmin(NULL)` gives `integer(0)` (GNU R `NULL`). `attributes(x) <- list(comment = 1)` is accepted (GNU R: "attempt to set invalid 'comment' attribute"), and `attributes(y) <- list(row.names = 1:3)` stores the compact form `c(NA, -3L)` where GNU R stores `c(NA, 3L)`.
- **A warning raised under `NextMethod` names the default method as its call.** With `"[<-.tagged" <- function(x, i, value) NextMethod("[<-")`, a recycling `x[] <- c(1, 2)` on a three-element object warns `In \`[<-.default\`(\`*tmp*\`, , value = c(1, 2))` where GNU R prints `In NextMethod("[<-")`. zoo's and xts's `[<-` methods show the same label.
- **`||` and `&&` accept an operand GNU R 4.6.2 rejects.** `FALSE || NULL`, `NULL || TRUE` and `TRUE && NULL` give `NA` or `TRUE`, and `FALSE || "TRUE"` gives `TRUE`, where GNU R reports `invalid 'y' type in 'x || y'` (or `'x'`) with the operator call. `sort(x, na.last = NULL)` reaches it through `is.na(na.last) || na.last`.
- **A missing argument forced through a forwarded promise names the inner call.** With `k <- function(a) a; k2 <- function(d) k(d)`, `k2()` reports `argument "d" is missing, with no default` for the call `k(d)`, where GNU R names `k2()`, the function whose formal is missing.
- **String collation without ICU.** `capabilities("ICU")` is `FALSE`, so in a UTF-8 locale `order(c("ß", "Z", "z", "ss", "a", "B"))` gives `5 6 4 3 2 1` where GNU R's ICU collator gives `5 6 4 1 3 2`. In the C locale, strings of mixed marked encodings and `bytes` strings also order differently.
- **Small calls at or above GNU R's time** in a survey of about 110 one-line base calls (the rest run 1.3x to 10x faster): `Sys.time()` 2.3 µs against 1.5, `split(1:10, g)` 13.5 against 9.3 (the R body of `as.factor`), `Reduce("+", 1:10)` with the function named by a string 7.8 against 7.3 (the prologue declines it). At parity: `iris[2, "Sepal.Length"]`, `rbind(df, df)`, `do.call(rbind, dfs)`, `tapply`, `apply(m, 1, sum)` and `is.element`, all flat interpretation of base R closures with no single hot builtin.
- **Matrix products** (`linalg.rs`, `real_product`) are ry's own kernels in reference BLAS's summation order (`decisions/builtins.md`, "Base R's linear algebra"), so they run at reference BLAS's speed, not OpenBLAS's; not yet timed against a GNU R on OpenBLAS. A faster kernel may block over rows and columns and thread across columns, but must keep each entry's sum sequential in the inner index and use no FMA, or the bits move. CI has only OpenBLAS (setup-r installs it), so `test_linalg.rs` compares its LAPACK cases there with live GNU R; adding `libblas3 liblapack3` to the workflow's apt line (a human edit: workflow scope) would make CI check the pinned reference bits too.
- **Package kernels** (`kernels.R`, zoo, xts, lubridate, stringr, jsonlite, readr) at 1e7 rows: `rollmean`, `rollapply`, `na.locf`, `endpoints`, `to.period`, `str_detect`, `str_replace_all` and `read_csv` are at or below GNU R. Still slower: `str_split` 1.54x, xts `period.apply` 1.39x (per-call interpreter cost, tier-0 work), `ymd` 1.29x, `fromJSON` 1.26x and `toJSON` 1.20x. `str_split` executes only 1.14x GNU R's instructions, so the rest of its gap is memory traffic in `allocVector` and `mkCharLenCE` (the heap's allocation path and string table). The jsonlite simplify step and lubridate's format training are R-level interpreter cost. At 1e6 rows these ratios swing by 30% between runs.
- **Bessel and polygamma ports differ in the last bits.** `besselJ(2, 1)` is `0.57672480775687351` against GNU R's `0.5767248077568734`, and `psigamma(0.5, 0)` `-1.9635100260214227` against `-1.9635100260214231` (`sprintf("%.17g")`); `besselJ(1:5, c(0, 1))` differs at 16 significant digits in two of five values. `distributions__vector_paths.R` prints them at 12 digits until the ports follow GNU R's operation order.
- **Deparse ignores `scipen`.** `options(scipen = -10)` warns `invalid 'scipen' -10, used -9` on both, but GNU R deparses the call as `options(scipen = -1e+01)`, because its deparse formats numbers under the scipen in force, and ry prints `-10`.
- **Complex printing** does not share significant digits between the two parts as GNU R's `formatComplex` does: `print((1+2i)^0.5)` gives `1.27202+0.7861514i` against GNU R's `1.27202+0.786151i`, and `print((-1+0i)^0.5)` gives `6.123234e-17+1i` against `0+1i`. The values are bit-identical; only the formatter differs.
- **Peak memory** (`run.py --memory`) at 1e6 rows is within 1.13 times GNU R's for every kernel (`decisions/frames-table.md`, "String keys sort by rank, and merge pairs come from sorted codes"). The highest are `unsplit` 1.13 (184 MB against 163), `gsub(perl = TRUE)` 1.08, `xts period.apply` and `as.character(double)` 1.07. `make.unique` on 1e6 names peaks at 127 MB against GNU R's 92 MB; names that are not ASCII still take the `String` path.
- **`mkChar` on the C API boundary's direct path is measured and not taken.** Interning `mkChar`, `mkCharCE`, `mkCharLen` and `mkCharLenCE` without leaving the direct path, with the string-table probe inlined, cuts a 16-byte `Rf_mkCharLenCE` from 868 to 765 instructions (fresh) and 376 to 288 (re-interned), and the marginal cost of `str_split(s, "_")` on 1e6 strings by 4.1%. Whole package runs move by less than the bar (single process, `TESTTHAT_PARALLEL=false`): testthat -0.25%, purrr -0.17%, rlang -0.11%, and it adds about 40 lines. Revisit if a profile shows `mkChar*` call overhead above a few percent.
- **Long strings through `mkCharLenCE` are measured and not taken.** processx hands it chunks of 45 to 95 KB, about 2.2G instructions in vctrs' test run (1.7%). Per byte of a 64 KB string under callgrind, ry re-interns an existing string in 1.7 instructions (1.57 in the one pass that does the NUL test, the ASCII test and the hash, 0.14 in the `memcmp` of the hit) and creates a new one in 4.0 (the same pass, a 1.0 copy and the allocation), against GNU R's 14.1 and 15.6. A 32-byte NUL and ASCII test with the copy folded in, plus hashing only a sample of a long string, might bring the pass to about 0.15 (an estimate), which would save under 1% of that run. Hashing a sample would also chain strings that share their ends in one bucket.

## Open: failing tests on main

`just ci` stops at the first failing test binary. With `--no-fail-fast` (and GNU R's
`lib/` on `LD_LIBRARY_PATH` for the `gnu-r` runtime tests), these are the only tests that fail:

- **`test_load`: `held_back_s4_definitions_behave_as_if_run_eagerly` and
  `a_lazy_generic_whose_held_back_definitions_fail_reports_the_failure_on_every_use`.** The first
  use of a held-back class or generic stops with `could not find function ".ry_force_s4"`. That
  happens only after a `load_all(full = TRUE)` reload with `options(ry.load.lazy_s4 = TRUE)` in
  the same session. A first load, a reload with lazy S4 off and a non-full reload all work, and
  neither the JIT nor GC timing matters. The `force` closure that `.ry_defer_s4` hands to
  `ryLoaderDefer` cannot see the loader environment, although `environment(load_all)` binds
  `.ry_force_s4`. The likely cause, unverified: the full reload's `.ry_new_package` registers
  the new definitions under the same root key while something from the old load's state stays
  reachable, or the closure's enclosing frame is rewired once `.ry_defer_s4` returns. A plain R
  closure that a function stores and a later call runs keeps its enclosure. Reproduce with the
  `SHAPES` package from `test_load.rs`: `ry load DIR -e "options(ry.load.lazy_s4 = TRUE);
  load_all('DIR', full = TRUE)" -e "isVirtualClass('Sq')"`.
- **`oracle`: `recorded_outcomes_match_gnu_r` on `s4__deparse_class_definition`.** GNU R 4.6.2
  deparses `methods::as` where the recording has `as`, so the recording has drifted. Re-record
  the case.

## Open: the package loader

The loader runs on ry's runtime (the `ry load` default) and on GNU R (`crates/loader`,
`docs/.../guide.mdx`, step 3). Gaps, roughly in order of how often a real package hits them:

- **`ry test` workers under `--runtime gnu-r` still load with pkgload**, eagerly and without
  `ry.toml`'s switches, because a GNU R worker has no `.Internal(ryLoaderAttach())`. Attaching the
  GNU R loader in the worker hook (source `gnur.R` from the binary) would close it.
- **`ry test` in one process sources the testthat helpers twice**: once by the load, into
  `package:<name>`, and once by `test_dir()`, into the test environment. `devtools::test()`
  sources them once, because it loads with `helpers = FALSE`. `load_all()` takes `helpers` for
  the workers already; the main process's load (`crates/ry/src/load.rs`) and the watch reload
  need it too, without losing the reload's "a helper changed" signal.
- **`ry test --watch --jobs N` restarts its workers on every rerun.** testthat's parallel runner
  starts workers per `test_dir()` call, so each rerun pays worker startup and a lazy load per
  worker. Keeping the workers alive across reruns needs a queue of our own.
- **Every rebuild of a shared library maps one more copy** on ry's runtime: `dyn.unload` keeps
  the retired library mapped (`libr` `unloaded_dlls`) so a stale pointer into it does not crash.
- **`ry document`'s warm run is dominated by reading the block cache**: about 3 s of a 11 s no-op
  on finbench is `readRDS` of the 29 MB compressed cache. Splitting it per file, or a faster
  format, would cut it (`decisions/libr-rcmd.md`, "`ry document` is roxygen2 with block and topic caches and linear topic ordering").
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
  against under 60 ms for the first `new()`. `registerS3methods` runs natively (`decisions/libr-rcmd.md`,
  "Start-up and imports run GNU R's code, with its hot loops native"), so what remains is GNU R's other R code:
  - Process start: an empty `ry run` starts from the start-up image in about 34 ms (GNU R's
    `Rscript`: 152 ms on the same machine).
  - Imports: `.checkGroupSigLength` runs natively (`decisions/libr-rcmd.md`, "Installed packages load
    through native fast paths, not an image"), which brings `cacheMetaData` to about 2.9 ms per
    namespace (GNU R 16 ms).
    import's `loadNamespace` until a name is used is not exact: `.onLoad` side effects (options, S3
    registrations that dispatch on objects read back with `readRDS`) would move.
- **A statement reading a name before its defining statement** sees, on reload, the previous load's
  value (a fresh load fails on such code); documented as a limit.

## Open: false reports and gaps found by the documentation review

An adversarial review of every docs page ran each claim against the binary. The docs now state the
behavior below truthfully, most of it under "Where correct code is reported" in
the Limitations section of `reference/type-system.mdx`, so fixing an item means updating that page too.

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
- The Zed extension is not in Zed's registry yet. It downloads the newest full release's archive
  (`editors/zed/src/lib.rs`), and the first full release is 0.4.4, so publish it to Zed's registry
  no earlier than 0.4.4 (before that it would serve 0.1.1), and 0.4.4 must ship every platform's
  archive or the download fails on the missing ones.

## Open: what `ry check` still reports on the GNU R corpus

Running `ry check` with typing on over the `R/` sources of the corpus packages (`corpus-and-oracle.md`)
gives 39 errors: 25 true package bugs, 2 known limitations and 12 false reports on correct R. The
three generated benchmark packages (`tools/bench/tooling/generate.py`) report nothing. To reproduce, give
each package a `ry.toml` with `[check]` `typing = true` (typing is off by default) and run
`ry check --output json R` inside it. A fix also deletes its line here and, for a user-visible
gap, in the Limitations section of `reference/type-system.mdx`.

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
  guide's step 4 says so conditionally ("put `@new` in one constructor"). An
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
- `development/design/r-runtime.md` section 8 keeps `declare(type(...))` as a third annotation
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

- A nested in-place update (`state$seen <- Dict.insert(state$seen, k, v)`) or a record-field
  update (`r <- (..r, values = List.append(r$values, x))`) inside a function makes the frame's
  environment on its first run (`op_call_rebinding` falls back to `eval::rebind_call`), so the
  rest of the call runs on the environment path. It stays linear; updating through the slot
  would keep the frame's slots. Helper moves already work from slots (`moves::Binding::Slot`).
- Under GNU R (`ry load`, `crates/ry/src/load.rs`), `std::List.map` fails with R's "there is no
  package called 'std'" instead of the explanation bare names get, bare names are stubbed whatever
  `[check] prelude` says, and the stub message calls every `ry.exports` name (DataFrame, Arrays) part
  of the standard library.
- Qualified library calls are not fast-pathed: `library_function`/`library_call` in
  `crates/interp/src/bytecode.rs` recognize only a bare symbol, so `std::List.map` (the only spelling
  with `[check] prelude = false`) misses the compile-time leaf resolution and the JIT loops.
- A check-time diagnostic for library calls in a project that targets GNU R, once a project can
  declare its runtime (`decisions/typed-stdlib.md`, "The standard library runs only on ry's runtime"). At run
  time, `ry R` already stops with an error that names the cause.
- A helper that calls a closure it did not create (a parameter, a global) ends every move early,
  so each call copies. Ending only when the closure can reach the moved value would need an
  escape check on the closure's body; closures the helper creates itself are already covered
  (`decisions/typed-stdlib.md`, "Moving a collection into a helper's parameter").
- `List.walk(ks, d, fn)` with `fn` updating `d` copies `d` on the first iteration.
- `eval::rebind` re-implements part of `eval_call` (interrupt polling, `visible`, error
  signalling) for the reuse path. Fold it into `eval_call`.
- Compiled loops cover the `List` walks (backwards, until, with index, from), `map`,
  `map_with_index`, `keep_if`, `drop_if`, `any`, `all`, `count_if` and the `find` family, plus
  `Dict.walk`, `Dict.walk_until`, `Set.walk` and `Set.walk_until` (`decisions/typed-stdlib.md`, "Library
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
`tools/bench/internal/stdlib/results.md` has the numbers. These rows are still slower, grouped by cause.

- **Rerun the full bench.** `tools/bench/internal/stdlib/results.md` predates the materialized-double R
  inputs and the Array paths of `List.min`/`List.max`; regenerate it with a full `just bench-stdlib`
  (about 75 minutes) and update the rows below.
- **One string call per element against one vectorized call.** `Str.starts_with`,
  `ends_with`, `contains`, the `trim`, `replace`, prefix and suffix functions, `repeat`,
  `with_prefix`, the case conversions, `split_first`/`split_last` and the UTF-8, 16 and 32
  conversions run through `List.map` or a closure, one builtin call per string with a boxed
  result each (about 400 to 1,200 ns), while `startsWith`, `grepl(fixed = TRUE)` or stringi make
  one C call over the whole character vector (15 to 350 ns per string). A leaf callback already
  skips building a call (`decisions/typed-stdlib.md`, "The standard library is measured against GNU R, function by function"), and
  the leaves borrow their input. The next step is a kernel per unary `Str` leaf that `List.map`
  and the predicates run straight over a character Array's storage into one result vector, then
  the same inside compiled loops.
- **A callback loop entered from tier 0.** `List.walk`, `walk_until`, `walk_with_index`,
  `walk_backwards`, `walk_from`, `for_each`, `Dict.walk_until` and `Str.walk_utf8` are 1.1 to
  3.5 times GNU R's `for` loop when called once from code that is not compiled: the loop only
  compiles inside a compiled body (`decisions/typed-stdlib.md`, "Library traversals compile to loops"), so
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

`decisions/typed-stdlib.md`, "An Array is a flagged R vector, written `[...]`". Known gaps:

- The runtime checks that atomic items share one type but not that ry-value items do, so
  `[(x = 1), [1]]` builds at run time; only the checker rejects it.
- `List` functions added to `standard_library.rs` need an Array overload in `types/std.Rtypes`, and
  any that reuse their first argument's storage must not reuse an Array's.
- `class(xs)` is R's implicit class (`numeric`), and serialization (`saveRDS`) drops the flag, so
  a reloaded Array is a plain vector.
- C code can write `NA` into an Array through `REAL()` or `INTEGER()`; nothing re-checks it.
- The JIT and the bytecode fast paths treat an Array as an unknown value. A dedicated feedback kind
  would let compiled loops run strict arithmetic without the generic call.
- An empty `[]` or `List.map` result is a flagged `logical(0)` under `ry load` and `source()`, which
  run unelaborated text; elaborated code passes it through `.ry_items` (`preflight::elaborate`).
- An integer overflow in an Array operator prints R's "NAs produced by integer overflow" warning
  as well as the Array error, because the check runs after R's operator. Muffle the warning when the
  result is rejected.

## Open: strict mode's top-level rule

The rule ships (`decisions/syntax-semantics.md`, "A strict library file's top level holds only definitions, and purity is declared"). Follow-ups:

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

`crates/jit` compiles hot bodies and loops (`decisions/jit-ssa.md`, "The JIT tier"). In priority order:

- **Tried and dropped: wider direct calls and pure inlined internals.** Accepting more argument
  shapes in tier 1's direct calls (constants, pass-through variables, `list(...)` arguments) and
  letting pure `.Internal`s such as `paste0` and `match` keep the frame's variables moved no
  workload by 3% Ir: report +1.08%, functional -0.38%, testthat +0.11%. The native-share census
  says why: package time sits in bodies that run too rarely to repay compiling, not in refused
  direct calls. Raising native share there means compiling more bodies cheaply, not widening plans.

- **An optimizing layer for LuaJIT-class loops (not queued; starts only on the orchestrator's go,
  after the call-setup enter/leave contract lands).** The current tracks (call setup, tier-0 lexical
  addressing and unboxed scalars, slot frames) close the interpreter and value-representation gaps
  to LuaJIT. The three that remain need one JIT-optimizer track, because each depends on the others:
  - *Cross-closure inlining with deopt.* Built for callees without loops, `...` or defaults
    (`decisions/jit-ssa.md`, "Tier 1 leaves compiled code through frame states"); what is left is under
    "Open: JIT on against JIT off".
  - *Allocation sinking.* Escape analysis over the JIT IR, so a temporary vector, a scalar box or a
    short-lived list that never escapes the compiled region is never allocated, and is materialized
    only on a deopt exit. Builds on counted registers.
  - *Loop optimizations above Cranelift.* Cranelift does little loop work, so ry needs its own pass
    over its IR: hoist invariant guards and loads (`length(x)`, attribute and class checks), remove
    redundant type and NA guards, eliminate bounds checks on `x[i]` for `i` in `seq_along(x)` or
    `1:n` with `n <= length(x)`, and peel the first iteration so guards run once.
  - Compile latency stays a constraint: Cranelift takes milliseconds per function, so these passes
    run only in the optimizing tier.
  - Target: scalar and `x[i]` loops in the kernels and applications programs within 2x of the same
    loop in C, measured with `just bench kernels` and callgrind.

- **Typed natives, the rest of the end shape** (`decisions/typed-stdlib.md`, "Typed closures run as native
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
  (`decisions/jit-ssa.md`, "Native scalar functions: frameless calls with checks proven at compile time"), so `fib(30)` runs in 0.007 s against 1.12 s on GNU R.
  Every other call from compiled code still goes through `jit_call`: an environment, a binding cell
  per formal and the boxed arguments, and about 440 instructions per formal read (`jit_load`).
  Natives now loop, read plain vectors and inline small native callees (`decisions/jit-ssa.md`, "Native
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
  one load. `bytecode::step` dispatch is 6.6% self. Reproduce with `tools/bench/tooling/generate.py` and
  `just bench-compare`.
  Microbenchmarks against GNU R 4.3 (JIT on) show the per-call gap: 200k closure calls take
  190 ms against 83 ms, 50k S3 dispatches 100 against 59, 20k `tryCatch` 415 against 102, and a
  two-expectation `test_that` block about 60 ms against 28. The likely wins, in order: no binding
  cell or promise per argument for a closure whose frame cannot escape, a cached global function
  lookup, and less `Heap::root` and handle traffic per call.
- **Static types as early profiles** (`r-runtime.md`, "How the static types feed the JIT"). A
  direct call's result type comes from the callee's declared return type when the call site has no
  feedback yet (`call_result_at` in `crates/jit/src/analysis.rs`), so typed `fib` compiles on its
  first try, and typed Arrays compile to direct item loads and stores (`decisions/jit-ssa.md`, "The JIT tier", item "Arrays are their own JIT types"). A native call skips the argument and return checks because its
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

- **`x <- f(x, ...)` compiles to `CallRebinding`, and tier 1 inlines it only when `x` is proven plain** (`crates/jit/src/analysis.rs`, `crates/interp/src/bytecode.rs`). A rebinding call on a value whose plainness is unknown runs slower than the same call without the rebinding, which is a regression for update loops.

## Open: data-oriented layouts and explicit allocators

The rule and its limits are in `decisions/docs-process.md`, "Data-oriented design and explicit allocators". This
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
4. **The interpreter's closure call adopting `Heap::reserve`** is the next cut on the allocation fast path.
6. **Attributes in a dense table instead of a pairlist.** Every object keeps its attributes as a
   pairlist, so `getAttrib` walks cells and `setAttrib` of a new name allocates one. A side table
   keyed by object index, holding (symbol index, value) pairs inline for the usual one to three
   attributes, would make both a short scan with no allocation. It changes the image format, so it
   needs the AOT image owner's agreement first; the C API's direct paths (`decisions/libr-rcmd.md`, "Hot C
   API entry points take a direct success path") already avoid everything else on that path.

**Measured ranking of the evaluation path.** An inventory of every hash map, cache, `Vec<bool>`
and short heap list in `interp`, `heap`, `jit`, `libr` and `typed` found 307 sites, 211 of them
touched per operation rather than per load. Ranked by callgrind instructions (profiling build, JIT
on, one repeat), what costs time is frame and lookup layout, not hashing: hash-map code, inlined
or not, is 0.50% of instructions on the mean `just bench applications` workload and 0.83% on `testthat_full`.
The 21-package corpus `--profile-summary` puts the time in the same places (builtins 28 to 32%,
tier 0 23 to 27%, closure call setup 20 to 22%, GC 8 to 9%, S3 lookup about 2%) but cannot name a
structure. In the table, "real" is the mean share over the nine `just bench applications` workloads, "tt" is
`testthat_full`, and a share is inclusive of the functions the site calls. JIT inline caches and
type feedback are the design and are not listed.

| # | Site | real | tt | Data-oriented replacement | Owner |
| -: | --- | -: | -: | --- | --- |
| 6 | GC entered from the allocation slow path (`Heap::maybe_collect`; `trace` 1.6 self, `Space::refill` 0.8). Escalation prediction is done (`decisions/heap-gc.md`, "Escalation prediction and the dense precious set"): `records` 7,028M to 6,366M instructions. Per-object marking is down to about 50 instructions (`decisions/heap-gc.md`, "Marking a traced object costs a few table loads"), and no marking source reaches 2% of testthat_full (`decisions/heap-gc.md`, "Escalation prediction and the dense precious set", item "Minor-collection cost by source") | 3.36 | 2.91 | none in marking; see the after-marking line under "Open: collector throughput" | B2 |
| 8 | Variable lookup by scanning `Frame` bindings (`Heap::frame_cell`, self 2.1 / 1.9), from `find_var`, `find_called_fun`, `define` and `resolve_s3` | 2.35 | 2.04 | lexical addressing: compile a closure's local reads to slot indices and global or base reads to the symbol's binding cell behind a binding epoch, so only `get()`, `assign()` and `eval()` on an arbitrary environment scan | T0 |
| 11 | Frame teardown (`release_frame` 0.89 / 0.61, `release_slots` 0.67 / 0.77, `truncate_frames` 0.27), with an `FxHashMap` probe of `Roots.precious` per release | 1.83 | 1.65 | frames on a bump stack reset to a watermark, with a bitset of the slots that need a release; `precious` as a dense vector whose index lives in the object header | AOT, B2 for `precious` |
| 12 | The callee looked up by name at each tier-0 call (`bytecode::lookup_function`) | 1.56 | 1.47 | the binding cell at the call site, checked against the binding epoch, as in row 8 | T0 |
| 13 | S3 dispatch (`resolve_s3`) through `MethodCache`'s three `FxHashMap`s (`entries` keyed by a 4-tuple, `names`, `sites: FxHashMap<Sexp, Vec<SiteEntry>>`) plus `implicit_class`. Hashing itself is only 0.03 | 1.20 | 1.78 | one monomorphic entry per call site (class vector, epoch, method) in a dense array indexed by site id | AOT |
| 14 | Tier-0 bytecode compilation (`bytecode::compile`, behind the `COMPILED` map) | 1.14 | 3.44 | not a layout problem: the map is 0.01. Compiling `if` arms lazily was tried and loses (`decisions/jit-ssa.md`, "Tier 0 compiles whole bodies"); what is left is the cost per compiled body and keeping code with installed packages (`.ryc`) | T0 |
| 15 | Locals appended one at a time (`Heap::define_unbound`, `Frame::insert` 0.81 / 0.58) | 1.07 | 1.42 | preallocate every local the body assigns from the compiler's slot table, so a first assignment is an indexed store | T0 |
| 16 | `warning()` builds a call and evaluates `.signalSimpleWarning` (`warning_call`; 8.2 on `conditions`) | 0.97 | 2.31 | signal natively when no calling handler for `warning` or `condition` is on the stack; check first that GNU R shows no observable difference | C |

The rest of the inventory measured cold. The largest hash maps are the large-frame index in
`Frame` (0.08 / 0.16), the `CHARSXP` table behind `install_char` (0.05 / 0.07), the string-keyed
`primitive_formals` probe in `call_builtin_checked` (0.04 / 0.05) and `Roots.precious` (0.05 /
0.06). `Vec<bool>` sites are below 0.1: `match_builtin_args` 0.09, the `keep` vector in
`subset::resolve` 0.06. `primitive_formals` is indexed by builtin index and both `Vec<bool>`s
are bitsets. Left: give `precious` the dense layout of row 11 (B2).

**The call path after slot frames bind on demand: what still allocates.** A slot frame's
environment binds a variable only when a lookup asks for it (`decisions/interp.md`, "A slot frame's
environment binds a variable only when a lookup asks for it"; `development/design/slot-frames.md`).
On finbench's tests (JIT on, 49.4 million closure calls) the run frees 217 million objects: 47.0
million pairlist cells, 19.4 million binding cells, 13.9 million language objects, 13.4 million
promises, 10.5 million environments and 5.7 million `...` objects. 83.5% of package calls run in
slots with no argument promise and no environment. 6.6 million slot frames make an environment
that binds nothing, 1.63 million go on to read their variables by name, 5.3 million variables move
into a cell, and 0.94 million frames take every variable. Sampled allocation call paths
(`RY_ALLOC_SITES=2048` beside a profile summary) name the sources, as a share of each kind:

| Kind | Sources |
| --- | --- |
| pairlist | an attribute set on an object (`set_attr`, `attach`) 37%, the attribute list of a copied object (`duplicate_with`, `duplicate`) 35%, calls built by C code (`Rf_lang2`, `Rf_cons`) 6%, `values::call` 4% |
| language | the renamed call of an S3 dispatch (`renamed_call`) 34%, copies (`duplicate`, `duplicate_with`) 21%, calls built by C code 11%, `do_force_and_call` 5% |
| promise | `closure_args_into` 21%, `detach_reference` 20%, `slot_promise` 15%, the `tryCatch` prologues 14%, `promised_args` 9%, `evaluated_arguments` 4%, `replace_with_closure` 4% |
| environment | `pending_env` 61%, heap frames (`frame_environment`) 21%, environments R code makes (`new.env`, `alloc_environment_with_capacity`) 18% |
| binding cell | `define_formals_uncollected` 61%, `define_unbound` 39% |
| `...` | `bind_plan` 72%, `matched_from_plan` 20%, `match_positional_args` 8% |

Most pairlist cells are attribute cells: the 37% made when an attribute is set, and most of the 35%
made when an object with attributes is copied (the rest of those are the argument cells of a
copied call). Attributes are a pairlist because R's C API hands the list out
(`ATTRIB`), so removing these cells means keeping an object's attributes in a compact record and
building the pairlist only for C code that asks for it. That is an object-model change in `heap`
and `libr` and has no design yet.

The frames that go on to read by name do so for these reasons (share of the 1.63 million): a
promise or environment outliving the frame 23%, a closure outliving the frame 22%, `parent.frame`
9%, `substitute` 8%, a nested or `<<-` assignment (the `<-` special) 7%, `standardGeneric` 6%, an
`.Internal` that receives the environment 5%, subassignment 5%, `.External2` 3%.

What is open, largest first. Each is worth 0.1 to 0.5% of run time on its own, because call
setup is about a tenth of the run, so take them for the objects they remove:

- **Replacement calls that still build their calls.** A one-level replacement on a slot frame's
  variable runs on the slots, and `complex_assign` calls a primitive replacement with its values
  (`decisions/interp.md`, "Replacement calls and `lapply`/`vapply` element calls run on a slot
  frame's slots" and "A complex assignment calls a primitive replacement with its values"). What
  still builds a getter or setter call, its argument cells and a promise per value is 256,523
  assignments a finbench suite run (profile section "Complex assignments that built their calls"):
  - A nested target's getter levels (`attr(attr(x, a), b) <- v` on an xts object 97,915,
    `names(l$a) <- v` 33,060, `l[[i]][j] <- v` 8,551). Call a primitive getter on an object
    without a method with its evaluated arguments, as `replace_through_primitive` does for the
    setter. An error names the getter call with `*tmp*` (`` `*tmp*`[[5]] ``), so publish the
    target for `unwind::resolve_call` to build that call from, as `Interp::assignment_call` does
    for the setter.
  - An object with a method for the replacement (`dimnames<-` and `[<-` on xts, `[[<-`, `$<-` and
    `[<-` on a data frame: about 55,000). The method needs the promises, so the gain there is a
    cheaper method call, not a skipped one.
  - A nested target also gives a slot frame its environment, because the compiler leaves it to
    the `<-` special: 113,502 of the 1.63M slot frames that move to reading by name. Lower it to
    getter and setter register code when every subscript is free of side effects (the general
    assignment evaluates a nested subscript twice), and branch to `Op::ComplexAssign` when any
    level is an object with a method or has a getter or setter that is not a builtin.
  - A target that is an object without a method, behind a computed subscript
    (`s3[[names(x)[1]]] <- 2`): the probe before the subscripts (`Op::BranchIfObject`) branches
    on any object. A probe that asks for the method (`dispatch::has_internal_method`) keeps
    these in slots, and it has to be taught to tier 1 as well (`jit/src/analysis.rs`,
    `codegen.rs`). Profile row `subassignment`: 75,333 frames on finbench.
  - A closure setter with computed arguments, and a setter whose body needs an environment
    (`diag<-` calls `.Internal`, `body<-` is an S4 generic). Profile row `replacement call`:
    38,391 frames on finbench.
- **Environments that only stand for a frame.** Three quarters of all environments come from
  `pending_env`, and most are never looked into. Its callers in a 225-test slice of finbench
  (667k calls): the generic call protocol 100k, `slot_promises_into` 105k, a builtin that
  receives the calling environment 74k, a closure made in the frame 55k, `Call::env` 52k, a
  promise whose code the compiler did not compile 46k, `parent.frame` 38k, `$` 34k, a detached
  reference 25k. A caller that only passes the environment on (the generic protocol, `$`, a
  builtin that dispatches) could pass the frame's index and make the object when the callee
  reads it.
- **Detached references.** `detach_reference` makes a fifth of the promises left: an argument
  reference becomes a promise object when the callee keeps the argument past the call or hands
  it on as a promise (S4 dispatch, `NextMethod`, a method that does not take the generic's slots).
- **The renamed call of an S3 dispatch.** `use_method_in_slots` copies the call with the
  method's name for `sys.call()`, one language object and its cells per dispatch. Keep the
  generic's call and the method's name in the frame record, and build the renamed call when
  something reads it.
- **A frame reads every variable by name after one by-name lookup.** The header register is the
  single switch, so one `parent.frame()$x` from a callee moves the frame's own code to name
  lookups for the rest of the call, although every other variable is still in its slot. A
  per-slot moved bit would keep the slot reads, at the cost of a check in tier 0 and in tier 1's
  variable probes (`jit/src/analysis.rs`, `runtime.rs`, `native.rs` test `gp::PENDING_FRAME`).
- **In-place reads from a closure.** A closure made in a slot frame reads its maker's variables
  through `eval::slot_binding` on every read (1.76 million calls in the slice), because the
  inline cache holds binding cells and a slot is not one.

**Allocations per operation against GNU R.** Loops of 20,000 and 40,000 iterations of each operation,
differenced. GNU R (4.6.2, its loop byte-compiled) counts `allocVector3`, `CONS_NR`, `cons`,
`mkPROMISE` and `NewEnvironment` calls under callgrind; ry counts heap objects freed by kind from
`--profile-summary` and Rust allocator calls under callgrind. Bytes per operation were not
measured.

| Operation | GNU R objects | ry tier 0 | ry JIT | GNU R malloc | ry Rust malloc | GNU R instr | ry tier 0 instr | ry JIT instr |
| --- | -: | -: | -: | -: | -: | -: | -: | -: |
| empty loop | 1 | 1 | 0 | 0 | 0 | 430 | 1044 | 52 |
| `f()` | 2 | 1 | 0 | 0 | 0 | 1695 | 2211 | 44 |
| `f(x)` | 5 | 4 | 0 | 0 | 0 | 3046 | 3705 | 56 |
| `f(x, y, z)` | 11 | 8 | 0 | 0 | 0 | 4544 | 4355 | 51 |
| `f(a = x, b = y, c = z)` | 11 | 8 | 9 | 0 | 0 | 4849 | 5596 | 5540 |
| `f(...)` with 3 | 12 | 8 | 8 | 0 | 0 | 3916 | 3613 | 3119 |
| S3 generic and method | 20 | 11 | 12 | 0 | 0 | 11848 | 9341 | 9290 |
| `x[i]` | 4 | 3 | 0 | 0 | 0 | 2519 | 2209 | 106 |
| `x[[i]]` | 3 | 2 | 0 | 0 | 0 | 2455 | 2042 | 138 |
| `x[1:3]` | 3 | 1 | 1 | 0 | 0 | 3202 | 1917 | 1114 |
| `x[-i]` | 7 | 2 | 2 | 4 | 1 | 9362 | 4508 | 4531 |
| `x[lgl]` | 5 | 1 | 1 | 3 | 0 | 7287 | 3544 | 2737 |
| `x + 1` scalar | 0 | 2 | 0 | 0 | 0 | 320 | 1465 | 53 |
| `x + 1` vector | 1 | 1 | 1 | 1 | 0 | 3768 | 2245 | 1493 |
| `x[i] <- v` | 3 | 2 | 0 | 0 | 0 | 1634 | 1725 | 118 |
| `x[[i]] <- v` | 4 | 2 | 1 | 0 | 0 | 1840 | 1711 | 309 |

ry allocates fewer objects than GNU R everywhere except three places, each with a fix:

- Scalar arithmetic in tier 0 boxes two doubles where GNU R's bytecode keeps the result unboxed on
  its stack. Keep scalar results unboxed in tier-0 registers, or reuse an unshared result in place
  (T0).
- The JIT allocates one object more than tier 0 for named-argument and S3 calls, because it falls
  back to the generic call path for them (and costs as much as tier 0 for `...`). Compile these
  calls natively or hand them to tier 0's path (JIT, with AOT for the call plan).
- `x[-i]` makes one Rust allocation for the excluded-index set (GNU R makes four). Give it inline
  stack capacity in `subset::resolve` (B5).

Every other measured operation makes no Rust heap allocation in the steady state, and the call
path allocates less than GNU R: a closure call with three arguments makes 8 objects against 11.
Where tier 0 still loses is instructions on the empty loop and on `f()`, which is dispatch
overhead, not allocation.

- **Options as a symbol-indexed array.** `getOption` (`envir::option_value`) walks the `.Options` pairlist and compares names by CHARSXP pointer, which is linear in the number of options. The O(1) shape is a redesign, not a side table: `.Options` becomes a view over an array indexed by the option's symbol, so `GetOption`, `.Options<-` and `options()` keep GNU R's semantics with one source of truth. Rank it once a profile gives `getOption`'s measured share of the benchmark suites.
- **Columnar record Arrays.** Store `Array<record>` as one buffer per field once the JIT fuses `xs[[i]]$field`; decided and deferred in `decisions/typed-stdlib.md`, "Array storage stays row-major; record Arrays go columnar only with JIT field fusion". Measure a million-item loop both ways first.
- **Typed Array loops trail untyped ones slightly.** `python3 tools/bench/bench.py --filter array` (GNU R / ry untyped / ry typed and strict, seconds, 4-core Xeon): array_dot 0.188/0.021/0.022, array_filter 0.167/0.022/0.026, array_map 0.247/0.038/0.041, array_sum 0.116/0.024/0.027. The JIT compiles Array element loops natively, but the typed program still pays 5 to 15 percent over the plain-vector one; find where (the strict operator check, `as.Array`, or the annotation checks) before calling Arrays a speed feature.

## Open: untyped R against GNU R

- **Differences the `api` and `metamorphic` fuzz kinds report** (`oracle fuzz --kind api|metamorphic`):
  - *The call an error or warning of a primitive names.* A primitive called directly inside another call reports itself (`` tryCatch(`dimnames<-`(1:3, list()), error = conditionCall) ``), where GNU R's `error()` names the enclosing closure call unless the caller is byte-compiled. Also `"sum"(1, 2)`, `` `attr<-`(1, "comment", 2) ``, `` `length<-`(NULL, 2) ``, `` `units<-`(NULL, "a") `` and `` `Encoding<-`(NULL, "UTF-8") ``. The oracle cases compare messages only for these.
  - Other messages: `matrix(sum)` (GNU R's `as.vector` fails first), `x <- NULL; x[3] <- sum`, `qr.influence(1:3, 2)`.
  - `print(1:10, max = 0)` prints one element, where GNU R prints none (`print.rs` clamps `max` to 1 because the array paths divide by it).
  - `D(expression(), "x")` returns `NA`, where GNU R fails with "attempt access index 0/0 in VECTOR_ELT"; `forceAndCall("a", sum, 1, 2)` gives no "NAs introduced by coercion" warning.
  - Condition classes: a missing argument is not a `missingArgError`, and an `eval` error is not an `evalError`.
  - `structure(1:.Machine$integer.max, names = letters[1:8])` is killed for memory, where GNU R reports "cannot allocate".
  - *Parser.* Input that ends inside a multibyte sequence (`parse(text = "\xe4")` is `expression()` on GNU R); `function(a, a` (GNU R's `repeatedFormal` comes before the end of input); a backquoted name holding a NUL (GNU R truncates); byte columns of `parse(text =)` for unmarked non-ASCII text; `ry run -` does not read standard input.
  - *Files (`--kind io`).* `sort()` puts a string starting with U+FEFF first (collation ignores it on GNU R); `substr()` of a latin1 string is UTF-8, not latin1; regex and `strsplit` on a latin1 string with a byte Windows-1252 leaves undefined do not warn "unable to translate"; a C-level error message is not cut at `warning.length` outside `scan` (`get(strrep("a", 2000))`); `file(encoding = "UTF-8-BOM")`; `readChar` on invalid UTF-8; `writeBin(size = 2)` on raw, complex and double; an ASCII `serialize` of a compact sequence; `writeBin` to a path is visible; `cat(fill =)` width with invalid bytes; an "ASCII" output connection writes the text before the unconvertible character.
  - GNU R artifacts the fuzzers must not learn: a closure compiled at its third call changes the call of a `<<-` warning to `<-` and merges the constants `-0` and `0`, and `x[1, 2L, 1] <- sum` on a list array is undefined there. Compiling each generated closure up front (`compiler::cmpfun`) in the fuzz programs would remove the timing noise.
- A builtin generic passes an S3 method promises of the call's code where GNU R passes values: with `length.cls <- function(x) deparse(substitute(x))`, `length(obj)` is `"obj"` on ry and the deparsed object on GNU R, and `` `names<-.cls` `` sees `substitute(value)` as `v` where GNU R sees `"a"`. The same in every tier (`dispatch.rs`, `internal_dispatch`).
- `mapply` and `Map` pass the function values, where GNU R passes promises whose code is `dots[[2L]][[1L]]`: `Map(function(a, b) deparse(substitute(b)), 1:2, 3:4)` gives `"3L"` and `"4L"` on ry (`envir.rs`, `do_mapply`).
- A compiled `x[i] <- v` decides between the builtin and a method by reading `x` before the subscripts run (`bytecode.rs`, `element_assignment`). A subscript that turns `x` into an object as a side effect (`x[{class(x) <<- "k"; 1}] <- v` from a nested function) dispatches on GNU R and runs the builtin compiled. Nothing real is known to do this. A subscript that is a local the compiler counts as bound (`Compiler::assigned`) is read before the target, so with `drop_k <- function() rm("k", envir = parent.frame())`, `k <- 1; drop_k(); z[k] <- v` on an object `z` fails from the second call on with "object 'k' not found" where GNU R's method may never force `k`. A body that calls `rm()` itself records no bound locals.
- The dispatch branch of a compiled replacement reads the target twice, once to probe it and once in `complex_assign`, so an active binding as the target runs its function twice where GNU R runs it once.
- When `base_primitive` cannot settle that `[<-` or `[[<-` is base's (an unforced promise or an active binding on the way, `bytecode.rs`, `replacement`), the compiled replacement takes the `BranchUnlessBuiltin` path, which runs the eager `Replace` for base's builtin even on an object target, so a method would see forced subscripts. Suspected from the code, not reproduced.
- A subscript that is a formal is forced before a compiled `x[i] <- v`, `x[[i]] <- v` or `x[i, j] <- v` reads its target (`slot_subscript` and `subscript_value` in `bytecode.rs`), on a slot frame and on one with an environment. A default that assigns the target therefore runs first: `o <- function(x, i = { x[1] <- 100; 2 }) { x[i] <- 0; x }` gives `100 0 3` for `o(c(1, 2, 3))` from the second call on, where GNU R gives `1 0 3`. `slot-frames.md` names the gap.
- A closure that keeps the value promise of a replacement call run by `eval::complex_assign` sees the caller's later change of the variable: with `` `cb<-` <- function(x, value) { x$cb <- function() value; x } ``, `f <- function() { obj <- list(); v <- c(5, 6); cb(obj, 1 + 1) <- v; v[1] <- 99; obj$cb() }` gives `99 6`. GNU R's byte code gives `5 6` and its AST evaluator `99 6`. Compiled, a setter with constant arguments (`cb(obj) <- v`) runs through `replace_with_closure`, whose promises count, and gives `5 6`.
- `lapply`'s element calls read the `X` the internal was called with, so a `FUN` that rebinds `X` in `lapply`'s frame through `parent.frame()` changes nothing (`ElementCalls::plain`, `envir.rs`), where GNU R reads the new `X` for the next element. Removing `FUN` there, a non-function `FUN`, and a warning from a special `FUN` report the call `FUN(X[[i]], ...)` where GNU R reports the `lapply(...)` call.
- `` lapply(list(1, 2), `repeat`) `` loops forever where GNU R raises an arity error, and `` `(`(x, ...) `` with an empty `...` raises "2 arguments passed to '('".
- `m[3e10, 1] <- 5` on a matrix is "subscript out of bounds" where GNU R warns and assigns nothing.
- A subscript of a nested assignment that takes a reference to the target lets a later level
  change the alias: `x <- c(a = 1, b = 2); names(x)[{ keep <- x; 2 }] <- "z"` leaves `keep` with
  the names `a z` on ry and `a b` on GNU R, and `levels(x)[{ keep <- x; 1 }] <- "z"` does the
  same to a factor. `eval::complex_assign` decides whether to copy the variable before the
  subscripts run; GNU R's replacement primitives test the reference count when they write.
- `<<-` through a replacement into a locked binding changes the value before the assignment
  fails: with `g <- c(1, 2); lockBinding("g", globalenv())`, `(function() g[1] <<- 5)()` fails
  on both engines but leaves `g` as `5 2` on ry. `complex_assign` reads `locked` from the local
  frame's cell, not from the binding `<<-` writes. Base's bindings are not locked either:
  `(function() T[2] <<- FALSE)()` succeeds where GNU R fails with "cannot change value of locked
  binding for 'T'".
- S3 replacement methods defined as locals of the function that assigns are never called:
  with `` `[<-.k` ``, `` `$<-.k` ``, `` `[[<-.k` `` and `` `names<-.k` `` assigned in `f`'s body,
  `x[1] <- 2`, `x$a <- 3`, `x[[1]] <- 4` and `names(x) <- "q"` on a `k` object in `f` run the
  builtins on ry and the four methods on GNU R.
- Smaller differences in complex assignments, each the same in every tier: `exists("*tmp*")` in
  a subscript is `FALSE` (GNU R binds `*tmp*` for the length of the assignment); `x[c(TRUE, NA)]
  <- 1:2` warns and assigns where GNU R fails with "NAs are not allowed in subscripted
  assignments"; `m[[1, TRUE]] <- 0L` on a 2 x 2 matrix writes the whole first row where GNU R
  writes one element; `names(m)[2] <- "a"` on a matrix leaves it without names; a formal nobody
  passed as the target (`f <- function(a) { a[1] <- 1 }; f()`) fails with "object of type
  'symbol' is not subsettable" where GNU R says `argument "a" is missing, with no default`;
  `..1[1] <- 1` fails with "object '..1' not found" where GNU R says "..1 used in an incorrect
  context"; `p$v <- 1` on an S4 object without a method says "no '$<-' method for this S4
  class" where GNU R says "no method for assigning subsets of this S4 class" against the
  generated call; `body(x)[[1]] <- 1` on a number makes a closure where GNU R fails; and
  `storage.mode(x)[2] <- "double"` fails with "invalid 'value' argument" where GNU R leaves
  `x` unchanged.
- Closures over equal but distinct body copies compile again. `Bodies` (`crates/interp/src/bytecode.rs`) shares code by body identity, but a lambda inside a testthat `expect_*()` argument is a fresh copy of its expression at each evaluation, so `testthat_full` compiles about 43 bodies per warm iteration (about 4M Ir of its 629M). GNU R's JIT cache hashes the structure and confirms with `identical`, so it shares them. A structural lookup at the compile decision, confirming with a deep compare of body, formals and srcref, would close it.
- `readChar` reads one byte per device call: an 11 MB file takes 150 ms against GNU R's 29 ms (`readChar(f, file.size(f))`). Its ceiling on measured workloads is 0%, since `testthat_full`, the strings app and the report app never call it, so a bulk read (one device read per string) waits for a workload that does.
- A missing argument forwarded through two calls is reported on the wrong call: with `leaf <- function(x) x; mid <- function(a) leaf(a * 2); wrap <- function(q) mid(q)`, `wrap()` fails with `argument "q" is missing, with no default` on both engines, but GNU R's condition call is `wrap()` and ry's is `leaf(a * 2)`. The same in every tier.
- `nargs()` evaluated in another function's frame counts nothing: with `f <- function(p) eval(quote(nargs()), parent.frame()); g <- function(a, b, c) f(a)`, `g(1, 2, 3)` is 3 on GNU R and 0 on ry. The same in every tier.
- An out-of-bounds `v[[k]]` in a closure is signalled one frame deeper than on GNU R: with `pick <- function(v, k) v[[k]]`, an `options(error = )` handler counts 2 frames on GNU R for `pick(c(1, 2, 3), 7L)` and 3 on ry (the extra one is `stop`). Every tier does it.
- `integer(NA_real_)` fails with "invalid 'length' argument" where GNU R says "vector size cannot be NA/NaN", and `matrix(1, -1L)` returns a 0 x 0 matrix where GNU R fails with "invalid 'nrow' value (< 0)".
- `integer`, `character` and the other vector constructors are builtins on ry and closures on GNU R, so an error inside one shows a frame less (`Calls: f` against `Calls: f -> integer`).
- GNU R calls an active binding twice on the second call of `f <- function(x) x * sc` (the cause is unverified; the second call is the one GNU R byte-compiles `f` at); ry calls it once. Low priority: nothing should depend on it.
- `tracemem()` never reports a copy: it returns the address, and `untracemem` and `retracemem` do nothing (`crates/interp/src/system.rs`), so `x <- numeric(10); tracemem(x); y <- x; y[1] <- 1` prints nothing where GNU R prints a `tracemem[...]` line. Copy checks use `RY_GC_LOG` collection counts or `gc()`'s max used instead.
- `.Call` into a package routine costs about 1.5x GNU R per call (`truelength()` x300k: 0.21 s
  against 0.14 s). What remains is the boundary itself: `call_native`'s native scope, rooting and
  `ry_call_native` in `crates/libr/src/boundary.rs`.
- Divergences found while pinning fast paths: `.row_names_info(structure(data.frame(u = 1:3),
  note = "hi"))` is 3 on GNU R and -3 on ry, because ry's `attributes<-` keeps row names compact;
  `.Call("crc64", "x", PACKAGE = "utils")` errors on GNU R (utils registers its routines with
  symbol lookup off) but runs on ry.
- Bench environment: a multi-package `install.packages()` call under ry installs nothing without
  an error, `ry R CMD INSTALL` fails with "cannot read CMD", and the testthat workload fails on
  both engines with testthat 3.3.2 ("attempt to use zero-length variable name").
- Uncompiled calls of `vector("nope", 1L)` and `choose("a", 1)` report their error against the
  internal's call (`vector(mode, length)`, `choose(n, k)`), where GNU R names the closure's call,
  because those internals use `call.error` and GNU R's use `error()`. Compiled sites inline the
  wrapper with the original call and agree; promise code and a closure's first, uncompiled call
  do not. Fix in the internals (`crate::unwind::current_call()`, as `normalizePath` does).
- The unary builtin path and native entries skip `call_builtin_checked`, so `--profile-summary`
  counters and Rprof do not see those calls. The `Reduce` prologue never binds `ind` in its frame,
  which only an `f` that reads `parent.frame()` can observe.

- Two public programs run slower than GNU R (`tools/bench/public/results.json`); neither regressed
  since the public record before it (same-machine A/B). Profiles from `ry run --profile-summary`:
  - fannkuchredux (0.88x): 51% JIT code, 21% builtins, 15% closure call setup. Each flip calls
    the `rev.default` closure, and `[` (3.6M calls) and `:` (3.3M) run as builtins that allocate
    a vector, where the JIT could reverse a prefix in place and index a range without making it.
  - binarytrees_naive (0.92x): 66% tier-0 instructions, 16% call setup, `c` 8% and `[[` 5%. The
    recursive `tree` and `check` closures, defined inside `execute`, never leave tier 0.

Target: plain, untyped R at least 1.5 times faster than GNU R. A private 300K-line code base runs
about 3% slower than on GNU R, and `just bench applications` (`tools/bench/applications/`, results in
`benchmarks.mdx`) reproduces the gap on production-shaped workloads. `ry run --profile-summary
FILE` (`crates/interp/src/profile_summary.rs`) gives the time split without revealing code, so a
user can send one; recalibrate the workloads when a summary from a real code base arrives.

Where the time goes, JIT on, from `--profile-summary` on the nine workloads: tier 0 instructions
23 to 44%, closure call setup 12 to 24%, builtins about 25%, S3 method lookup up to 17%
(`timeseries`, xts and zoo), native code 14% (`tables`). Tier 1 runs under 3% of the time on all
but `functional`: most hot code sits in closures called a few thousand times, under the entry
threshold, so tier 0 and the call path decide the result.

The testthat workload (`tools/bench/applications/testthat.R`, 36 `test_that()` blocks per repeat) stands in
for package CI. Its `--profile-summary`: tier 0 33%, builtins 26%, call setup 25%, GC 4.8%,
compiled code 0.7%; 1.33M closure calls create 1.73M promises (1.30 per call, against 1.42 in a
user's profile taken before promise elision), and a quarter of the calls go through the
argument-match cache because they pass names or `...` (the counter counts cache hits too). Its
callgrind profile is flat, with no function above 2% of instructions, so the gains are fewer
ops per call (inline predicate fast paths) rather than one hotspot. Open:
promise elision covers few testthat arguments, because most are calls rather than constants or
evaluated symbols; about 8 objects are allocated per closure call (10.9M freed over the run).

Public suites (`just bench public --filter /`, one run, JIT off against GNU R 4.3.3):
awfy 0.66x, rcp 0.55x, shootout 0.98x geometric mean; JIT on 1.35x, 2.48x and 1.29x. The
JIT-off losers are scalar loops (rcp vector-while 0.35x, fannkuchredux_naive 0.38x,
mandelbrot 0.41x), which tier-0 dispatch and scalar allocation decide.

The open work, in build order with the gap each item targets, is the roadmap in
`r-runtime.md` ("Untyped performance roadmap"); update its status column with each change. Numbers
behind its rows, kept here: `closure_args_into` 3.8% and `args_pairlist` 2.7% of a 3-million-call
microbenchmark, `scalar_real` from `fast_arithmetic` 2%, page sweep and refill 3.4%, `run_closure`
6% self (srcref walk, `stacker` check, annotation lookup, three `Rc` clones, a 9-field `Frame`
push); `lookup_method` walks the calling frames to the top environment and probes a hash map per
class; complex assignment 2.7 to 6.6 times slower than GNU R (two `*tmp*` promises and two
pairlists per level, through `apply_call`); the shared dispatch jump about 28% of a JIT-off scalar
loop; feedback one `i32` per code word. Per `seq(1, 10, by = 2)` call (141.6k instructions, GNU R
about 106k): `execute_code` self 36.6k (about 680 ops), `call_builtin_reusing` 9.5k over 65 builtin
calls, `lookup_function` 4.4k, `alloc_vector` 4.8k, 117 function guards.

- **Vector kernels: fuse the remaining consumers and widen `exp`.** `exp(-x^2/2) * k` runs about 4x faster than GNU R and is bounded by `exp` itself, at 0.84 ns per element. The next steps toward 10x are fusing `sum`/`mean` consumers into the fused pass and an AVX-512 `exp` (`crates/interp/src/kernels.rs`, `vector_math.rs`; `r-runtime.md`, "The vector kernel layer").
- **Builtin calls above GNU R's instruction counts.** `x[[i]]`, `x[[i]] <- v`, `x[i] <- v` and `.Call` (2x) cost more instructions per call than GNU R's. All of it is generic call machinery: the function lookup and its guard, frame lookups and the result's allocation. `structure()`, `format.default` and `Reduce`'s callee are dominated by closure frame setup. Both groups are frame and call-convention work. Measure per call with callgrind against GNU R on a loop of the builtin, then rank with `ry run --profile-summary` on each `tools/bench/applications` workload.
- **Closure calls.** A closure call on the testthat workload costs about 8,100 instructions, spread thin (`decisions/interp.md`, "Cheap promises: measured, not redesigned; call setup and package loads trimmed instead"). The next levers are `frame_environment`, `run_closure`'s own bookkeeping and `bind_arguments`, at about 300 instructions each. A plain closure call with promise elision takes 0.33 to 0.39 µs against GNU R's 0.525 µs, while "Open: base R kernels against GNU R" quotes 0.75 µs against 0.35 µs for a closure call with the JIT off. The two figures come from different sittings and disagree, so measure both in one sitting before quoting either.
- **Promise elision fires only for effect-free callees** (`crates/interp/src/effects.rs`; `decisions/jit-ssa.md`, "The effect-free fact is stored per code object and resolved per enclosure"). `seq()` gains nothing, because the helpers it calls are not effect-free. Measure how often the eager path fires on `just bench applications` before widening `EFFECT_FREE_BUILTINS`.

## Open: performance and memory

### Tier-0 call entry and dispatch

- A tier-0 closure call to a compiled body continues in the dispatch loop through `enter_in_place` and `finished`, and a prepared site takes `call_fast` and `return_fast`, from a slot frame and from a frame that reads by name (`slot-frames.md`, "The fast call path"; `decisions/interp.md`, "Closure call setup: the 10x end shape"). A call entered from a builtin, from `Rf_eval`, from `UseMethod` or from the general binding path nests through Rust and pays `run_at`, about 167 instructions, mostly building `Activation` (about 20 stores, shared with the JIT) and the `Machine`. The register clear is about 16 of `grow_registers`' 57 and must stay.
- Superinstructions and guard dedup were measured and do not pay (`decisions/jit-ssa.md`, "Tier-0 superinstructions do not pay"). Trimming `Activation` saves about 2 instructions per call, because the fields the baseline JIT reads sit at fixed offsets.
- Leaf code (a closure body that makes no call) could skip saving and restoring the environment flags and pushing and releasing the handle.
- **A framed call costs about 830 instructions of machinery against the goal's 200, and no single part of it is large.** On the planned fast path the call op is 535, the return op 225 and `release_slots` 69; `decisions/interp.md` ("Closure call setup: the 10x end shape", "Measured: where a framed call's instructions go") has the split. The `Frame` write is 47 of them, so the frame columns pay only together with the rest. What the fast path has: arguments written once, counts from the plan row, one observer byte. What is unbuilt, on every path:
  1. **The frame as depth-indexed columns.** A call writes the whole 21-field `Frame` (about 112 bytes), including an `on_exit: ThinVec`, two `Rc` clones of the handler and restart stacks, and a `srcref` found by walking the closure's attributes. The target is a 40-byte hot record plus cold columns, one `Interp.on_exit` stack, and handler and restart arenas with a parent index.
  2. **Pooled default promises and `u32` reference columns.** `eval::default_promises` allocates a promise per default that is code, and a reference's target (frame, slot, header) is a heap integer vector read on every use.
  3. **A smaller resume record and machine switch** (83 instructions on entry), and a register window that needs no clearing (37).
  4. **The packed code header**, which folds the guards on the closure and the plan row (37) and removes the `pool_ints` reads that `enter_in_place` makes on the generic path.
- **The generic path is half of validation's closure calls.** Of 284K `CallGeneric` executions, 115K take the fast path; 55K come from a caller whose frame has an environment, 42K hit a site whose plan word is zero because its body was compiled for a fresh closure, 12K hit a site not prepared yet, and 11K call a closure with a native entry. Those build an `Arg` buffer that `push_frame` copies and test their observers one by one in `eval::enter`. Planning on the first call and entering from a frame with an environment would cover 97K of those 120K. Functional style shows the same gap from the other side: with the JIT on it costs 1.3% more instructions than before calls ran in the loop, because 124K of its 168K `CallGeneric` executions pay for the fast path's tests and then take the generic path, and half of its closure calls are made by builtins and nest through Rust.
- **In-loop promise forcing waits for item 2.** Running a promise's code in the dispatch loop instead of a Rust frame was built and measured as a net loss on every program tried (`decisions/interp.md`, "Closure call setup: the 10x end shape"), because its per-force overhead (a forcing record, a resume record, two heap roots, a walk of the caller's live registers, and the heap read of the reference target) costs about what the Rust frame did. Three of those five go away with pooled promises, so build it again then, not before. Validation forces 772K promises against 284K call instructions, so this is where its gain is.

### Argument lists collected into `eval::Args`

- Collecting into the `SmallVec<[Arg; 10]>` costs about sixty instructions a call in capacity checks, before any work. `eval::with_positional_args` writes up to eight untagged arguments into a stack array instead; the positional builtin handler and `jit::runtime::jit_call` use it (`decisions/builtins.md`, "Positional builtin calls read their arguments from the registers").
- `bytecode::slot_subject_call` still collects into `eval::Args` (0.42% of bond_book_full in SmallVec code). It needs a different shape: its arguments carry tags, their count is known only after walking the call, and it stops early and drops the refcounts it took. Owned by the call path.

### Small-vector base builtins (open, tier 0)

- On bond_book_full (warm, one thread, about 2,189M instructions per repetition), base builtins on length-200 vectors cost per call: `rep` about 640 (`repeat_plain` about 535), `sum` about 615 (`add_all` 122, `summary_args` 98), `exp` about 680 (`math` 420), `seq_len` about 280. `kernels::binary` runs 1.6M calls per 4 repetitions at about 226 instructions in `real_arithmetic` and 163 in `alloc_uninit` (allocation, owned by GC).
- The argument handling and dispatch around these calls is estimated at a ceiling of about 10 to 13% of bond_book_full. A realistic estimate goes to the speed lead before building.

### Queued with measured ceilings

Shares are of each application's instructions (profiling build, one process); a ceiling is the item's whole inclusive cost, so a realistic gain is about half.

- **S3 method resolution by a dense per-generic index.** `dispatch::resolve_s3` is 2.50% of testthat (62,000 calls at 1,659 instructions) and 2.47% of validation (270,000 calls at 870). The plan keys methods by interned class-symbol id per generic, which drops building `generic.class` names (`method_name`) and walking scopes (`lookup_in_scope`) on every dispatch. A whole `UseMethod` round trip already costs 15,700 instructions against GNU R's 36,500. Unowned.
- **Widen `direct_c` to the shapes that reach `do_c`.** In validation `combine::do_c` is 3.04% (126,000 calls at 2,292 instructions), against about 900 per call through `direct_c`; testthat spends 0.09% there. ry is at GNU R's cost already (`c(a = 0.6, b = 0.4)` 4,100 against 3,500; three strings 2,100 against 2,200), so this is a validation-only win of about 1.5 to 2%.

An independent review profiled before it proposed. Every number below was taken on a 4-vCPU
container with other work running, so treat each as an upper bound. Every in-container timing is
effectively a two-core number.

### A first strict `ry load` costs the strict check's wall time

- On typed finbench (4 cores), a load with no usable check record takes 1.6 to 1.9 s at full size, against 0.4 to 0.9 s untyped and 0.6 to 0.7 s after `ry check`; at 60,000 lines it is 0.45 to 0.55 s against 0.25 to 0.35 s untyped (`decisions/libr-rcmd.md`, "The strict load check analyses only the package"). The check skips tests and scripts and frees its memos off the verdict path, so what is left is checker CPU: at 60,000 lines the check alone is about 0.85 CPU-s (parse about 100 ms, `item_annotations` 63 ms, `file_annotation_references` 48 ms, folds 43 ms, parse-stage diagnostics 40 ms, HIR 39 ms, lints 37 ms). Lowering the thread's priority and changing the worker count were measured without gain, so the lever left is the analysis core's own speed. At full size the loader reports about 1.26 s, most of it waiting for the verdict, and the check of the 502 package files alone costs about 3.0 CPU-s (`ry check R --no-cache` takes 0.9 to 1.0 s; an `R/`-only copy of the project costs the same, so tests are already out of it). A `perf` profile of that check is flat: salsa bookkeeping (`QueryEdge` inserts, `SyncTable::try_claim`, `add_read`, interning) is about 21% of self time, mimalloc and page faults about 12%, the lexer and tree building about 6%, and no checker function exceeds 2%. So the next cut is structural: fewer tracked queries per item (each of the 50,000 items mints about a dozen memos, plus per-name `frame_binder`, `package_name` and `global_scheme` memos), not a hot loop.

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

What remains is the semantics.
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
- **Coverage-guided fuzzing covers 15 of 17 targets, shallowly on the slow ones.**
  `just fuzz-all 60 runtime` runs them, locally or in the staged `test.yml` once it is accepted and
  someone starts it or tags a release. What it leaves open:
  - `csv` and `unserialize` (libR's reader) run nowhere. Their build instruments Polars, which is
    killed for memory in a 15 GB container with four build jobs.
  - `ide` runs four to seven inputs a second, so 60 seconds read about 300 of its 5,300 seeds and
    never reach mutation. An optimized build without instrumentation replays all of them in 147
    seconds, so the cost is the instrumented build and not the harness. Seven targets share those
    seeds (`scripts/seed-fuzz-corpus.rs`), and a smaller seed set would bound the replay.
  - Leak detection is off for every target that links `heap` (`__lsan_default_options` in
    `crates/heap/src/space.rs`), because LeakSanitizer does not scan the mapped object space.
    Registering the space as a root region should restore it (not tried).
  - `reload` and `load_index` also run without leak detection, and they break the rule that no
    harness reads the machine or starts a thread it cannot step: `Loader` reads a load index from
    the user's cache directory when one is there and then parses ahead on a detached thread
    (`loader::parse_detached`). LeakSanitizer reported that thread's own bookkeeping as leaked
    when the process exited while it ran (seen once on `reload`; inferred for `load_index`, which
    shares the path). The fix is a cache directory and a parse-ahead that the harness controls.
  - `interp`'s reader checks a vector's length against the rest of the input only for a slice
    (`Reader::expect_elements` in `crates/interp/src/serialize.rs`). Read from a connection, a
    made-up length is allocated before the read fails (from reading the code, not from a run).
  - The same reader returns a `CHARSXP`, a byte-code object and a primitive without reading the
    attributes their flags announce, and ignores an ALTREP object's flag bits. An old stream with
    attributes on a `CHARSXP` would fall out of step (from reading the code; libR's reader
    handles it). `crates/rdb` and `crates/libr` have their own readers, which got none of the
    kind checks.
  - A compact sequence is materialized when read, so three numbers ask for any amount of memory.
    `check_unserialize_input` lowers the heap's allocation ceiling for that reason. The product has
    no total vector limit (GNU R's `mem.maxVSize`), and keeping the sequence compact needs the
    ALTREP classes that live in `libr`.
  - Find-references skips a file or item whose text does not contain the name. `may_spell` in
    `crates/ide/src/ide.rs` covers a backticked name with an escape for a global, and the S4
    target and the own-package `pkg::name` branch have the same text test without it (not
    reproduced).
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
ignored suite, so a human moving it into place closes this (`decisions/tooling-bench-ci.md`, "CI runs `just gate` against GNU R 4.6.2, and nothing ignored").

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

Gaps in the simulation, fuzzing and scenario design that `development/robustness.md` specifies.

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
`users/feedback-*.md` files from that round. What follows is what is still open, by theme, with the tester who found it
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
designing traits rather than accreting them. `development/design/open-questions.md` holds the
reasoning.

## Open: what the adoption reviews left

Three independent black-box adoption reviews simulated real projects from the docs and `--help`
only, with no source access. They were an analysis-script user, a CRAN package author and a
numerical-computing user, and they converged on the same walls. What remains is below, ranked by how often a real user hits it.

### The apply family is unblocked but still untyped where its result shape depends on a value

Overload selection with a flexible argument works now, through fact-versus-guess probing that
`decisions/syntax-semantics.md` records. `sapply`, `mapply`, `Map` and `tapply` stay `Any`, because `simplify = FALSE`
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
`development/design/open-questions.md`. The right fix is a "supports this operator" constraint in
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
answer. `decisions/syntax-semantics.md` holds the record and the diagnostics page documents it. The open follow-up is
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
which `development/design/inline-type-syntax.md` records. The message says to lift the function to
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
These are the open findings, ordered by severity.

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

What follows is what the review left open.

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
  duplicate-type walk that was quadratic in file count is gone. Every
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
- **The README's diagnostics are not checked.** `docs/` samples are tests (`test_docs`), but the
  two diagnostics in `README.md` and `editors/code/README.md` are pasted text that goes stale when
  a message or blame range changes; re-capture them after such a change.
- **Rework the typing reference's presentation.** `reference/type-system.mdx`
  wants tables and short bullets instead of prose subsections, preserving every normative claim.
  This was deliberately deferred out of the docs restructure so the contract got a dedicated pass.

## Open: REPL

The console ships behind `ry run` (ry's runtime) and `ry R` (GNU R), with `ry repl` kept as a hidden alias, and `development/design/repl.md` holds the architecture and the
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
`development/design/repl.md` has the recipe, covering Rstart callbacks through `R_DefParamsEx`'s
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

- **The 1e8 joins fit, with about 3 GB to spare; what would widen the margin.** At 1e8 rows the
  five db-benchmark joins peak at 11.6 GB from R and 12.0 GB from Rust, and data.table is killed
  at 14.3 GB before its first answer (`development/design/r-runtime.md`, section 7;
  `tests/test_memory.rs` in `crates/table` pins the bytes per row). What remains, largest first:
  - **q5 takes 5 to 7 s where q1 to q4 take 0.7 to 1.5 s.** It buckets `join_big`'s 1e8 rows over a
    key range of 2e8 and gathers 13 columns for 9e7 output rows through random row indices. Not
    profiled yet. Candidates: partition both sides by the key's high bits so each partition's
    buckets fit the cache, and gather the built side's columns in bucket order.
  - **The reader's peak is about 2.5 times what the table holds** (86 against 32 bytes per row
    for `join_x` in `test_memory.rs`). During `Interner::finish` a factor column holds its u32
    codes, one 16-byte view and one 16-byte sort key per level, and a rank per level, and the
    factor columns of a file finish in parallel. A partition whose labels all fit a view inline
    could turn its views into keys in place, and a column with few levels could keep u8 or u16
    codes from the first slice on instead of narrowing at the end.
  - **A lookup with a few rows and a wide key range gets bucket bounds sized by the larger
    table.** `indexable` in `crates/table/src/join.rs` allows a code range of 8 times the rows
    of both tables up to 2^27 codes and twice the rows beyond, at 4 bytes per code, whichever
    side is bucketed. Ten lookup rows whose keys span 2e8 values, joined to 1e8 rows, would
    take 0.8 GB of bounds (derived from the rule, not measured). Bounding the range by the
    bucketed side's own rows would send such a join to the Polars plan, so it needs a hashed
    kernel path first. `test_memory.rs` has no step of this shape.
- **Only `:=` reads a column's row index.** A filtered update whose filter is `column == literal`
  finds its rows in the index `Table::row_index` builds on the second such filter
  (`decisions/frames-table.md`, "A filtered `:=` reads a row index and writes in place"). A filtered select
  (`x[id4 == 5L]`, `DataFrame.filter`), a filtered aggregate and a grouped `:=` still scan the
  column, and so does every other filter shape: `%in%`, a range, or two equalities joined by
  `&`. data.table's automatic index serves `==` and `%in%` wherever `i` uses them (its vignette
  on secondary indices; not measured here). The index holds 4 bytes
  per row until the column changes or the table is dropped, with no way for the user to drop it
  earlier.
- **Some calls leave one reference count on a frame, so the frame's next `:=` copies once.**
  The copy is a fresh object, so the updates after it write in place; the cost is one column
  copy, or one per pass when the call sits inside the loop. data.table never notices, since its
  `:=` writes in place whatever the count says. `print`, `format`, `head`, `summary`,
  `as.data.frame`, `nrow` and friends leave the count alone. Each of these was run and is
  followed by a copy:
  - A local list that held the frame: `capture.output(x)` (its `withVisible` result is a list),
    `str(x)`, `nrow(list(x)[[1]])`, `do.call(nrow, list(x))`, `Map(nrow, list(x))`,
    `for (y in list(x)) NULL`, and any `function(v) { kept <- list(v); ... }`. Five such calls on
    a fresh vector read a count of 6 against 1, in ry and in GNU R alike, because neither lowers
    an element's count when the list that held it dies with its frame; only a collection frees
    the list, and a collection lowers no count. Closing this needs the heap to release a list's
    elements when a frame's clearing drops the list to zero holders, which is sound only where
    the list cannot be the frame's result or sit on the evaluation stack.
  - The frame passed through `...` into a builtin: `(function(...) dim(...))(x)` and
    `(function(...) length(list(...)))(x)`.
  - A call that fails after forcing the argument: `try(f(x))` where `f` reads its parameter and
    then stops.
  - A function that updates its parameter, `function(a) { a[id == 1L, v := 5L]; NULL }`: the
    parameter's update goes to a fork, as it must, and the caller's frame keeps one count.
- **Releasing `...` lists and detached references at frame exit costs 0.6% of the testthat
  workload's warm instructions** (`decisions/frames-table.md`, "A filtered `:=` reads a row index and writes
  in place", cost of the exact counts). The part that is not release work is the test
  `release_slots` (`heap.rs`) makes on every slot for a `...` list. A compiled body knows which
  slot can hold one, so the release could take that slot's index and test nothing else.
- **A chained `:=` is an error.** `x[id == 1L, v := 5L][id == 2L, v := 6L]` applies the first
  update and then fails with "`:=` updates a DataFrame stored in a variable", because the second
  bracket's subject is a call. data.table code chains updates this way, and
  `ry migrate data-table` does not split the chain.
- **A `:=` whose value has the wrong length warns from R's `[<-`.** `x[id == 1L, v := 1:3]`
  with two matching rows falls back to the R path, whose warning names
  `.Primitive("[<-")(c(10L, 20L, 30L, 40L), c(1L, 3L), 1:3)` instead of the user's call, and then
  recycles as R does. The kernel should report the length mismatch itself.
- **An aggregate over a filter that keeps no rows gives one row.** `x[v > 5, .(s = sum(v))]`
  with no matching row prints one row with `s = 0`; data.table gives a table with no rows (and
  `x[v > 5, sum(v)]` gives `0` in both). The verb form, `DataFrame.filter` then
  `DataFrame.select(Expr.sum(...))`, gives one row as Polars does, which is right for the verbs,
  so the fix belongs to the `[` path alone.
- **`ry migrate data-table` checks a name's uses in the current file only** (`uses_are_safe` in
  `crates/ide/src/migrate.rs`). A top-level `sales <- fread(...)` in one file that another file
  queries with `sales[, z := 1]` is rewritten, which breaks the other file. The safety check
  should see every file of the project that can read the name.
- **A Polars `Enum` converted to a factor loses its level order** (`convert` in
  `crates/table/src/table.rs`): it goes through `from_strings`, which sorts the levels by bytes,
  so an Enum read from Parquet with a non-alphabetical order is reordered.
- **A text column is joined from its slices at the end**, which copies its strings once
  (`Builder::Text`); building the final `Utf8View` buffers directly would avoid it.
- **Where Polars' streaming engine is faster than the kernels** (`decisions/frames-table.md`, "Group-bys and
  joins run on ry's kernels, not Polars' streaming engine"; `db_benchmark --polars-streaming`,
  built with `--features polars-streaming`, reproduces it). At 1e8 rows group-by q4 takes 0.82 s against 0.22 s: three double means over
  100 groups go through `dense_by_code` in `crates/table/src/group.rs`, where every thread scans
  all row codes for its own code range so that doubles are added in row order as data.table
  adds them. A blocked or pairwise sum whose result does not depend on the thread count would
  let the per-thread dense states serve doubles too; decide first whether bit-equality with
  data.table's sums is worth the scan. Group-by q10 at 1e7 rows (0.64 to 0.78 s against 0.54 s) pays the
  same scan. Join q3 (the left join) takes 1.29 s against 1.07 s.
- **A query's large buffers are faulted in afresh each time.** The global allocator maps every
  block of 1 MB or more and unmaps it when freed (`heap::large::Mapped`), on huge pages while a
  table operation runs, so a query pays one fault per 2 MiB instead of per 4 KiB. A pool of freed mappings would reuse
  touched pages (group-by q10 at 1e7 rows without LTO: 0.43 s against 0.73 s) but keeps memory
  resident between queries, and the 1e8 joins have about 3 GB to spare (`decisions/frames-table.md`, "Table
  operations map their large blocks on huge pages"). Keeping scratch small (the grouped
  median) and a per-query arena are the alternatives. Fresh huge pages also make a query's
  time depend on the kernel's compaction: the 1e8 joins took 8.1 s in one run and 11.1 s in
  another on the same build.
- **API gaps against Polars:** no string or date namespaces (`str.contains`, `dt.year`, so
  `lubridate::floor_date` runs on R vectors in the `trials` module), no `cast`, `n_unique`,
  `top_k` (`Expr.head` after a sort covers the per-group case), `unique`, `rename`, `drop`,
  `unpivot`, or `when/then/otherwise` chains beyond `Expr.if_else`. None of the db-benchmark questions needs them. `LazyFrame.scan_*`
  has no parquet predicate statistics use beyond what Polars' reader does.
- **IDE:** rename of a column (across `Expr.col("x")` strings and `$x`) and goto-definition from a
  column string to the verb or constructor argument that created it are not implemented; hover and
  completion are.
- **Checker:** an expression held in a variable (`e <- Expr.col("x") * 2`) leaves a verb's columns
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
  and `enrol` (`development/design/r-runtime.md`, "Real code: the benchmark modules"). Numeric
  columns could share R's buffers once the collector can pin them for a Polars buffer's life.
- **R precedence trap:** `Expr.col("a") * 2 |> Expr.alias("b")` pipes only the `2`. A lint that
  recognizes a pipe into an `Expr.` function whose left side is a literal would catch it.

- **Query features:** a `by` that needs R code, `:=` with `by` on the R path, and a join whose `j`
  needs R code (including under `.EACHI`) stop with an error.
- **Native code gets copied columns.** `crates/libr/src/frame_altrep.rs` can expose columns as
  ALTREP, but `frame.rs` still converts a column into an R vector when R code reads it.
- **A frame satisfies a `list` parameter in the checker** so that `lapply` and `Map` accept it,
  which is unsound for code that indexes the argument with `[` (rows on a frame). Overloads that
  take `frame` directly would let the compat go.
- **R code evaluated while lowering can run twice.** A sub-expression that names no column is
  evaluated eagerly during lowering, and if a sibling then falls back to R, the whole `i` or `j`
  runs again in the data mask. An `i` or `j` that updates the queried frame during lowering
  leaves the IR built against the old schema.
- **Checker:** `.SDcols` as positions or `patterns()`, `(cols) := ...` through a variable, and
  `c(.N, lapply(.SD, f))` as `j` give a frame with unknown columns.
- **Checker nits:** `x < "b"` on a factor gives the generic comparison message, and `factor == 1` is
  flagged although R allows it.
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
  `tools/bench/internal/table/ops.R` needs more than 15 GB of memory, since `rbindlist(list(x, x))` holds
  the 5 GB table and a 10 GB result at once.

- **Measured state against data.table** (`tools/bench/internal/table/ry.R` beside `datatable.R`, release build, 4 cores, best of two, same checksums; `just bench-table` regenerates the tables in `development/design/r-runtime.md`, section 7). At 1e7 rows the verbs beat data.table on every group-by and join question, 6.3 s in total against 21.3 s, and lose one `:=` loop, the 1% row-subset update (2.4 s against 0.41 s). At 1e8 rows the group-by questions take 17.3 s against 63.0 s at an 8.5 GB peak against data.table's 12.5 GB, and the five joins take 10.8 s at an 11.6 GB peak (8.1 to 11.1 s across runs), where data.table is killed at 14.3 GB before its first answer. Only the `:=` loops use data.table syntax, and data.table for the run lives in a scratch library passed as `RY_BENCH_RLIB`.
- **Helpers measured from Rust.** `ops_benchmark` beats `tools/bench/internal/table/ops.R` on each data.table helper at 1e7 rows except `setkey` (about 10% behind) and `dcast` (parity).
- **Benchmark modules end to end** (`just bench-frames`; `r-runtime.md`, "Real code: the benchmark modules"). The DataFrame variants in the `tables`, `market_data` and `trials` templates take 11.6 s typed, against 17.8 s for data.table on ry and 20.4 s on GNU R, at 1e7 rows.
- **Migration fuzz** (`just bench-migrate`). At 300,000 lines only finance has data.table code to rewrite: `ry migrate data-table --write` makes 826 rewrites in 398 files, and the 821 table module instances give the same checksums after migration.
- **Editor latency on column strings.** Completion inside `Expr.col("")` at the end of a 1,650-line pipeline script in a 30,000-line project takes about 30 ms for the first request after opening, 2 ms settled, and 2 ms at the median (14 ms at the 95th percentile) right after typing in the string, in a release build. A debug build is about 25 times slower.

## Open: the runtime

What steps 2 to 4 of `development/design/r-runtime.md` left for later, most pressing first.

- **stringi was last seen to segfault on load in the cloud containers, even with `RY_JIT=off`** (unverified), which stops the `strings` benchmark and stringr there. Reproduce with `library(stringi)` under `ry run` before relying on either.
- **Audit `libr` entry points for arguments C code passes as `NULL`.** `R_getVarEx`'s default
  was the one found (`decisions/libr-rcmd.md`, "A C API argument that may be a C `NULL` is typed
  `Option<Sexp>`"); a `Sexp` parameter that receives `NULL` is undefined behaviour, not an error.
  Grep the corpus's `src/` for calls with a literal `NULL` in a `SEXP` position and type each as
  `Option<Sexp>`.
- **Install-time native code: the gaps.** The image's native section is written
  (`crates/jit/src/ahead.rs`), but it compiles only scalar numeric closures for guessed argument
  types (annotations, constant defaults, then double, integer and logical shapes), not the
  checker's inferred types, which the install cannot query yet. Records are refused because each
  process numbers record layouts anew. Coverage on real packages is small (rlang 16 of 1,294
  functions, cli 28 of 897, testthat 13 of 1,101): the refusals are about the value domain
  (`...`, strings, lists, base functions native code lacks, calls of closures that are not
  native), not about missing facts, so checker types and argument facts lift only a few dozen
  functions per package. Baseline code for every body from its first entry measured 8.06G
  against 7.96G instructions on the testthat workload, so install-time baseline code is no lever
  until baseline code inlines fast paths.
- **Decoding an image's code costs about as much as compiling it** (about 65k instructions per
  closure on CRAN packages), so an untrained image saves nothing measurable. The decoder reads
  every instruction word and feedback word as a varint and re-interns each symbol constant by
  name. Storing words as fixed-width little-endian runs that copy in bulk, and symbols as their
  preorder position in the body, should make decoding a fraction of compiling; measure with
  callgrind on `library()` plus a first call of the twelve packages in `decisions/heap-gc.md`, "Code images: compiled code beside the lazy-load database".
- **Where `library()` time goes, and what code images cannot remove.** On `library()` of twelve
  CRAN packages (1,200.7M instructions for the whole process without images), compiling is 74.5M
  (6.2%), and images remove only 4.0M of it, because most compiling during a load is base-package
  code and code `.onLoad` runs once. Lazy-load fetches are 313M (26%), almost all of it
  `serialize::unserialize` (278M) decoding the records the namespace forces. Collection is 118M
  (9.8%). Fetches and collection are the load cost. An eager namespace image was measured not to
  help (`decisions/libr-rcmd.md`, "Installed packages load through native fast paths, not an image"), so the lever is a
  faster record reader and fewer forced fetches (S3 registration forcing every method), not
  another image.
- **A training run with other types than real use makes a trained image slower.** The JIT
  compiles at the first call from the trained feedback, deoptimizes on the new type, then refuses
  the recompile ("a for loop over something other than a plain vector") for the merged feedback,
  so the body stays in tier 0 for the session: a loop trained on `1:50` and run on doubles took
  72 ms against 45 ms without the image. The same refusal happens without an image when a session
  calls with integers first, so the fix is in the JIT (recompile for the widened type after a
  deoptimization), not in the image.
- **An untrained image costs instructions on load.** `library(survival)` plus one `survfit` call is
  2,091.9M instructions with the recommended packages' images and 2,064.4M with
  `RY_CODE_IMAGE=0` (callgrind, whole process), so cheaper decoding (above) matters before
  images pay off without training.
- **Code images are about six times the size of the lazy-load database** (MASS: 706 KB `.ryc`
  against a 120 KB compressed `.rdb`), because instruction words, constants and feedback are
  stored uncompressed. Compress payloads, or pack the instruction stream, if install size matters.
- **Symbols are not compacted with the start image.** Symbols keep their addresses because frame
  filters, slot indexes, hash tables and record layout tags hash them (decisions/heap-gc.md, "The start
  image is compacted in touch order at write time"). Moving them too needs every such table rebuilt
  after the move; an empty start still touches 821 image pages where perfect packing would touch
  about 480.
- **Unforced base bindings still compile at run time.** The start image holds compiled code for
  every closure bound in a loaded namespace, but lazy-load bindings that start-up never forces
  (most of stats and utils) are fetched and compiled when a session first calls them. Forcing them
  all at image write would move that cost to the build, at the price of a larger image; measure
  the image size and a full collection's page touches before doing it.
- **Inline annotation checks have known holes** (typing reference, "Annotations at run time").
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
  payload), plus one that calls the removed `C_pKS2`; `development/corpus-and-oracle.md` lists them.
- **ry's console evaluates a chunk only once all of it parses** (`interp::session::run_console`): a
  `-f` script that ends inside an unfinished expression runs none of its complete expressions until
  the user finishes it, where GNU R runs each expression as soon as it is complete.

- **Corpus package tests: what still fails** (`oracle packages`).
  - `R` without `--no-echo` does not echo its input. GNU R prints each line it reads after the
    `> ` or `+ ` prompt (and a last `> ` at the end of input), so `R --silent -e 'expr'` output
    starts with `> expr`; sys's "Jobs can be nested" reads the second line of that output.
  - tcltk is unavailable: `dyn.load` refuses GNU R's own `tcltk.so` (`decisions/r-compat.md`, "Package-test compatibility details that look optional but are not"), so code that needs Tcl/Tk gets an error. A tcltk built against ry
    would bring it back.
  - Parse errors name the token GNU R's parser stops on, but ry's parser recovers differently in
    a few places: `x <- function( {` reports the end of the input, where GNU R reports the `{`, and
    `a ;; b` parses where GNU R rejects the second `;`.
  - POSIX (TRE) patterns still run on fancy-regex through a translation (`perl = TRUE` is PCRE2,
    `decisions/builtins.md`). A non-constant-size pattern runs on fancy-regex's backtracking VM because of
    `leftmost_longest`; linking TRE itself would give GNU R's exact POSIX semantics.
  - `getParseData` columns and srcref bytes differ from GNU R on lines with non-ASCII text. GNU R
    counts a multibyte character inside a string literal as its bytes, and whether it counts
    columns in characters depends on the text's declared encoding (`known_to_be_utf8`); ry counts
    characters. 26 of rlang's test-file rows differ for this reason alone.
  - lobstr's `obj_size` counts a closure's environment a few kilobytes higher than GNU R, because
    ry's environments and bytecode have their own layout. Only relative comparisons are portable.
  - Each ALTREP string or list element read through `Sexp::element` pushes a handle that lives
    until the enclosing scope ends, so a builtin walking a 10M-element nanoarrow column pushes 10M
    handles. Materialize once in bulk paths, or root only where the element survives an
    allocation.
  - The TRE matcher now compiles three regexes per pattern, which costs `grepl` in a loop with no
    matcher cache.
  - Tier-1 code is freed once its enclosure dies (`compiled_for` in `crates/jit/src/jit.rs`), on
    the reasoning that a running activation keeps its closure, and so its enclosure, alive. An
    activation whose closure's environment is replaced while it runs (`environment(f) <- e`
    inside `f`) would break that. No repro yet; write one before changing the code.
  - Two unserializers: `interp::serialize`'s reader and libr's `R_Unserialize`. The writer is
    already libr's alone (`decisions/libr-rcmd.md`, "One serializer writer, in libr"); move the reader the
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
    `nlminb` now runs PORT's drivers (`crates/stats/src/port_minimizer.rs`); rerun these
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
    objective misses GNU R's "NAs introduced by coercion" warning (`crates/stats/src/port.rs`, `Objective`).
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
- **Top CRAN packages: what still fails** (`just top-packages`, `.corpus-top/tests/summary-installed.md`).
  The 100 most-downloaded CRAN packages outside the closure (`tools/corpus/top-packages.txt`), built
  from source on ry: 86 have tests. In a container with the system libraries, GNU R passes 84 and
  ry passes 82 of those 84 (measured while dplyr and janitor still failed). In the cloud container
  GNU R passes 79 and ry passes 78 of them once QuickJSR's library is reinstalled with the ry under
  test (below); sandwich is the one real failure. Of GNU R's failures there: units, sf, terra and ggplot2's `coord_sf` tests need system libraries
  (udunits, GDAL) the container lacks, and parallelly and future.apply fail on both.
  - Run the suites with `LC_ALL=C.UTF-8`, as the oracle does: in the C locale, desc reports
    "Duplicate DESCRIPTION fields" on ry, a false failure.
  - QuickJSR fails only when ry prints its notice about packages installed by another ry build on
    a subprocess's stderr, which the test compares. Reinstall the library with the ry under test.
  - sandwich, `vcovPL.R` (runs since plm loads): collapse reaches into GNU R's object layout
    through its own `internal/R_defn.h`, writing a vector's data at `(SEXPREC_ALIGN *) x + 1`
    and its attributes and object bit through `x->attrib` and `x->sxpinfo.obj`. ry's vectors
    keep their data elsewhere, so `collapse::gsplit` writes past its results and reads back
    zeros or crashes. Repro: `str(collapse::gsplit(1:9, collapse::GRP(rep(1:3, c(2, 3, 4)))))`
    gives `1 2`, `3 4 5`, `6 7 8 9` on GNU R and `0 0`, then a SIGSEGV, on ry. plm's `vcovSCC`
    then fails with "'names' attribute [500] must be the same length as the vector [496]".
  - `oracle packages` reuses a GNU R result recorded for the same package version, so a changed
    dependency (an upgrade by the constraint pass) needs `--rerun-gnu-r`.
  - ggplot2, `test-geom-smooth.R:92` ("Failed to fit group 1" expected): GNU R's loess on fewer
    points than degrees of freedom warns "span too small. fewer data values than degrees of
    freedom.", "pseudoinverse used at ...", "all data on boundary of neighborhood. make span
    bigger" and more, and returns NaN fits, so `predict` then errors. ry's
    `crates/stats/src/loess.rs` fits the points exactly. Port the netlib `lowesd`/`ehg`
    checks and their `ehg182` warnings. Repro:
    `fitted(loess(y ~ x, data.frame(x = c(1, 2), y = c(1, 3))))` gives `NaN NaN` and 13 warnings
    on GNU R, `1 3` on ry.
  - BiocManager's `test_install.R:166` fails under GNU R too (it needs a writable site library),
    so it does not count.
  - Not run: StanHeaders, rstan and shinystan (Stan's C++ takes hours to build), arrow (needs the
    Arrow C++ library), V8 (needs libv8) and pak (builds its own dependency tree). quantreg
    passes, but its rgl tests skip because rgl needs libGLU.
  - `read.dcf` marks a non-ASCII value as UTF-8 where GNU R leaves it "unknown": in a UTF-8
    locale, `Encoding(read.dcf(textConnection("A: M\u00fcller"))[, 1])` is "unknown" on GNU R
    and "UTF-8" on ry. Comparisons agree, so nothing fails on it yet.
  - `oracle packages` reports the first error in a `.Rout`, which can be one `try()` caught; the
    error that ended the run is the one just before "Execution halted"
    (`crates/oracle/src/packages.rs`).
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
  families (the distributions record in `distributions.rs`'s module comment).
- **`R -f` and `R CMD BATCH` do not echo input.** `ry_run` runs a file like `Rscript`, so
  `R -f` without `--no-echo` and `BATCH` output omit the `> ` echoed code GNU R prints.
- **`exists(name, registry)` on the namespace registry segfaults** (its enclosure is NULL, as in
  GNU R, where the lookup errors "use of NULL environment is defunct" instead).
- **`lazyLoad()` returns visibly** (prints `NULL` at top level); GNU R's result is invisible.
- **Connections gaps:** `fifo()` and non-file URL connections are not implemented. `xzfile()`'s
  negative (extreme) compression levels use the ordinary preset.
- **ALTREP `Extract_subset`** is registered but subsetting does not call it yet.
- **`R_withCallingErrorHandler` runs its handler after unwinding**, not before (`decisions/libr-rcmd.md`).
- **A task callback's warnings print under the plain "Warning message" header.** GNU R prints them
  under "warning messages from top-level task callback 'name'", with the call shown as `cb(...)`.
- **Indexing a compact sequence materializes it.** `1:n`, `n:1`, `seq_len` and `seq_along` of at
  least 2^20 elements are compact ALTREP objects (`sequence` in `crates/interp/src/arith.rs`;
  doubles past `INT_MAX`), but `x[i]` and `x[[i]]` on one expand the whole vector once, keeping
  the expansion in data2, because subsetting does not call `Elt` or `Extract_subset` (the item
  above). Subsetting `seq_len(2^31)` therefore allocates 16 GB where GNU R reads elements.
- **Approximate matching has two engines.** POSIX patterns with TRE bounds such as `{+}` run on
  `ApproximateRegex` (`decisions/builtins.md`, "TRE approximate regular expressions"), but `agrep`,
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
- **Graphics gaps** (`decisions/builtins.md`, "The default packages' utils, tools, grDevices and graphics routines", item "One graphics engine for base graphics, grid and device packages"). Base graphics, grid, lattice and ragg match GNU R pixel for pixel on the `grid__*`
  oracle cases; what is left:
  - Raster compositing allocates a canvas-sized layer for every mark drawn with an operator other
    than `over` and two per group use, re-renders a mask whenever the clip and mask pair changes,
    and renders a definition again at every use, so a chain of groups that each use the one before
    twice is exponential (`draw_items`, `coverage` in `crates/graphics/src/render.rs`). Layers sized to the
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
    bodies (`decisions/jit-ssa.md`), so the gallery takes 6.99 s with the JIT on against 7.10 s off.
    Closing the gap is interpreter call-path work (`decisions/interp.md`, "The interpreter's call
    path, lookups and allocation"), not a graphics fix.
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
  follows the Rtools conventions but has only been verified cross-hosted (`decisions/libr-rcmd.md`, "Windows `R CMD INSTALL`: the home's `Makeconf` decides the platform"): the release smoke test should install data.table from source with Rtools on
  the Windows runner, and `R CMD config` there needs Rtools' `make` on `PATH`.

## Open: stale code comments

- The comment above `nightly-publish` in the `justfile` says a nightly is "a normal release ... never a pre-release" and that the Zed extension installs the newest pre-release. The recipe passes `--prerelease --latest=false`, the extension reads `pre_release: false` (`editors/zed/src/lib.rs`), and `development/releasing.md` ("Nightly builds") describes that. Correct the comment.
- The doc comment in `crates/libr/tests/test_start_image.rs` names CI's `setup-r` as the writer of a `~/.Rprofile`. CI runs in the flake's dev shell and has no `setup-r` step, so the comment should name a user profile in general.
- The fuzz batteries' cost is recorded in the knowledge base as re-parsing the stub corpus for each fresh database. That claim is unmeasured on the current tree: profile one battery and either confirm it or remove the bullet.

## Explicitly out of scope for now

- Modelling S3 dispatch, meaning `UseMethod`. It is the prerequisite for an honest `print`,
  `summary` and `plot`.
- Column-level typing for a data.frame, matrix dimensionality, and real S4 typing.
- Traits and typeclasses. The tripwire is the third constraint kind.
- Auto-generating CRAN stubs through R introspection, R-version-keyed corpora, and stubtest
  validation. All three need R.

## Benchmark code bases (measured)

`just bench` generates the three 300K-line code bases (about 413K lines with tests) and prints this
table; `tools/bench/tooling/lsp_latency.py` drives `ry server` over stdio. Release build, 4-core container,
typing on:

| code base | cold check | peak RSS | findings | body edit first / settled | signature edit first / settled |
|---|---:|---:|---|---:|---:|
| finance | 1.36-1.6 s | 406 MiB | 1005 `unused` (genuine), 848 `object-system` | 1 / 44 ms | 2 / 39 ms |
| data science | 1.5-1.65 s | 400 MiB | 2337 `object-system` | 2 / 41 ms | 2 / 39 ms |
| biostatistics | 1.4-1.7 s | 403 MiB | 784 `object-system` | 1 / 41 ms | 1 / 40 ms |

The `object-system` warnings are the intended lint on the generated R6 and S4 classes. The cold check is CPU-bound (about 4.2-5 s of CPU on four cores, about 3 times its wall time). The latest profile of the finance bench (1.4 s, 4.6-4.8 s of CPU): item checking is about a third of samples, parse about 12%, the interface reads about 8% (most of it frame lookups and top-level names), naming about 8%, and no function above about 4% exclusive. The syntax and check passes keep all four cores busy; about 0.1 s between the syntax pass and naming runs at half the cores. The struct-of-arrays HIR lowered from the green tree (`decisions/syntax-semantics.md`, "HIR as struct-of-arrays, lowered from the green tree") measured, in interleaved runs against the list-owning HIR, 7 to 12% less CPU, 6 to 10% less wall time and 23 to 35 MiB less peak RSS on all three code bases, with identical findings; the edit latencies did not move. Keeping literal text in a per-module buffer instead of interning it took off about another 0.1 s and 0.2 s of CPU; against a main build in the same interleaved runs the median cold check went from 1.74-1.86 s to 1.58-1.60 s, 5.3-5.7 s to 4.8-4.9 s of CPU, and 425-440 MiB to 399-403 MiB. After it, `item_hir` is about 9% of samples and parse about 12%, and parse is now the largest cost of the syntax pass. The machine was busier for these numbers than for the older rows below: in the same interleaved runs the build before hash-consed types measured 1.75-1.9 s, 5.1-5.6 s of CPU and 439-450 MiB. The profile is flat: item checking is about 35% of samples, then HIR lowering (13%), parse (11%), naming (8%), the syntax lints and the annotation queries, and the hash-consing table itself is about 3% exclusive. The syntax pass ran in batches with eviction between them and kept about 78% of the cores busy; it is now one parallel loop like the naming and check passes. Levers left toward 1 s: a cheaper parse (done since: the flat preorder tree replaced rowan, see "Check cache follow-ups"), re-interning names the HIR already holds as `Symbol` (naming's `mint_binding` and `GlobalEnv::scheme` take `&str` and intern again, about 2%), the `GlobalEnv::scheme` and `layers` lookups (about 11% inclusive), and the syntax pass's idle cores. Hashing `Symbol` by its index (ordering stays by spelling, so sorted output is unchanged) and the compact HIR (name sites in one side array, `NonZeroU32` ids, a 16-byte expression kind and an 8-byte argument; `decisions/syntax-semantics.md`, "HIR as struct-of-arrays, lowered from the green tree") together measured, in interleaved runs against a main build that already kept trees resident, medians of 1.52-1.58 s against 1.61-1.66 s, 5.0-5.3 s against 5.5 s of CPU, and 533-538 MiB against 563-568 MiB, with identical findings. After them the heap peak on finance is 490 MiB under the system allocator: resident green trees 127 MiB, HIR kinds 30 MiB and ranges 15 MiB, the `item_check` memos about 50 MiB, closed top-level schemes 21 MiB (2.3 million allocations, `check.rs` where `top_level_bindings` is built), and the expression-type tables 15 MiB. Tried without gain: a per-check cache of common scalar types, mimalloc's purge and huge-page options. A thread-local front cache for the type table measured no wall-time gain alone, but sampling showed the shard mutexes contended, so it is kept (`decisions/syntax-semantics.md`, "Every host keeps every tree"). Before the green-walk fusion, immortal interning, frame-binder query, one-batch passes and input durability, main measured 2.3-2.5 s, 6.4-7.0 s of CPU and 510-517 MiB on the same machine. Before tree
eviction, compact names and durability the same machine measured 3.4, 3.6, 2.8 s, about 790 MiB, and
47 to 63 ms settled; before the cold-path work, 6.1, 6.7, 6.2 s, about 1 GiB, and 95 to 120 ms.

Before the fixes the same code bases reported 3514, 2421 and 1185 false errors. Remaining debt:
- Peak RSS of `ry check` is 417-419 MiB on the benchmark code bases (`decisions/syntax-semantics.md`, "`ry check`
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

- **Records, tuples and tags: what is not done.** In the checker, a missing number or string case of a `match` is reported only as `_`. In the runtime, a payload-free tag that the AST evaluator builds allocates per evaluation (compiled code shares one constant), and `$<-` on a record always copies instead of reusing an unshared value. The libr side (`TYPEOF` 27 and the opaque accessors) has no C-level test. `crates/oracle/cases/syntax__ry_lookalikes_stay_r.expected` was written by hand and needs re-recording under GNU R.

## Open: editor parity

- Completion at two characters is weakest on call heads (httr2 72%, purrr 76%): a package's own functions share long prefixes (`req_`, `resp_`, `oauth_flow_`), so two characters cannot pick among them. Measured and rejected: ranking by words a candidate shares with the enclosing function (helps at half the name, hurts at two characters) and spellings before calls. A signal beyond use counts (the types the call site expects) would be needed.
- An installed package's `exportPattern` is read with the `regex` crate, which accepts most but not every POSIX extended regular expression R allows; a pattern it rejects exports nothing (logged).
- Semantic tokens: `Expr.col("name")` strings are not colored as properties (columns read inside a `DataFrame` `[` query are).
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

Measure with `python3 tools/bench/tooling/lsp_latency.py --ry target/release/ry --root .bench/finbench
--quiet 2` against the LSP, and with `ry debug analysis-stats` in process (its "editor requests"
table). Generate the code bases with `tools/bench/tooling/generate.py --lines 300000`. A `--quiet` under
about 2 s lets the first edit overlap the startup cold check and reports a false one-second spike.
- Keystroke to settled diagnostics on finbench (4 cores, 40 runs) meets the targets (median at most
  30 ms, 95th percentile at most 100 ms): a body edit settles at a 4 ms median and 6 ms p95, a
  signature edit at 4 ms and 5 ms. The slow spots left are the first open below and a cold hover
  (342 ms max, the first hover after startup).
- A new top-level name still re-runs `package_definition_index` (one pass over every file's definitions), the `package_name` projections the edited file reads, and `project_typo_suggestion` for each unresolved name a re-checked file has (a scan of every project name of a similar length). About 4 to 6 ms on dsbench in `examples/keystroke_probe.rs`. Chunked per-file-group indexes would make the fold proportional to one chunk.
- A file opened at startup settles 170 to 240 ms after the open on the three benchmarks (4 cores; `decisions/syntax-semantics.md`, "A file's check names only what its interface reaches"). What is left is parsing every package file in the warm-up (`warm_project_folds`) and the file's own check, about 0.25 s on one thread after parsing (`crates/ry/examples/cold_open_probe.rs` breaks it down per query). Earlier samples of the old naming warm-up showed about a fifth of helper time spinning on salsa 0.28's per-ingredient claim mutex (`SyncTable::syncs`) and another fifth in mimalloc's page slow path; re-sample the new warm-up before acting on that.

## Open: false errors on real CRAN code

`ry check R` on 25 packages from the benchmark dependency set (purrr, rlang, cli, httr2, zoo, xts, lubridate and others; sources fetched from CRAN, each with `[check] typing = true`) went from 669 errors to 175 after the call-checking, name-resolution and inference fixes (`decisions/syntax-semantics.md`). What remains, by message shape:
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
  points and green walks in the annotation and lint passes (`decisions/syntax-semantics.md`, "One-shot checks record
  coarse dependencies"). (The red nodes these figures counted are gone with rowan; see the flat-tree
  item below.) What is left is flat: item checking about 7.7 G
  inclusive, parse about 3.1 G (lexing 0.7 G, about 105 instructions per token in the tree builder
  and 300 per node), naming 1.9 G, interface edges and reads about 3 G, the syntax lints 1.7 G, and
  mimalloc about 1.5 G over 19 M allocations. A real third needs fewer allocations in the checker
  (`map_child_types`, `resolve_rec`, the `Ty` maps' rehashing) and a cheaper parse, not one cut.
- The checker's working maps now live in a pooled bump arena and its names are interned
  (`decisions/syntax-semantics.md`, "The item check's working maps live in a pooled bump arena"), which took the
  finance bench from 19.78 M to 15.82 M mimalloc calls and 23.59 G to 22.68 G instructions,
  with wall time unchanged within noise (median about 1.34 s). The
  1 s target still needs about 35% less CPU, and no single function is above about 1.2% of
  self cost. What is left is structural:
  - Done: rowan is replaced by ry's own flat preorder tree with windowed item trees, one-shot name
    lookups read the project maps once per query, unlisted frame binders are built once per item,
    and overload probes reject by call shape before instantiating (`decisions/syntax-semantics.md`, "The syntax
    tree is ry's own flat preorder arena" and "Overload probes reject by call shape first"). The
    finance bench is now 22.5 G instructions and 11.1 M `mi_malloc` calls under whole-process
    callgrind (from 23.6 G and 19.8 M), wall median 1.11-1.15 s against 1.35-1.45 s for the previous
    build in interleaved runs, 3.6-3.7 s of CPU and 371 MiB, with byte-identical findings. About 10% of wall time remains to the 1 s target.
  - Done: a cold check holds one parse tree per worker, skips the per-item syntax memos and the
    editor-only check facts, and purges freed memory after 10 ms (`decisions/syntax-semantics.md`, "A cold check
    holds one parse tree per worker"). Peak RSS is 298-306 MiB on the three benches, under the
    300 MiB target, but with only 1-3 MiB of margin on finance and data science; wall time is
    1.1-1.3 s on a noisy 4-core machine, and instructions moved only 23.4 G to 23.0 G.
  - salsa's per-query edge bookkeeping that remains is per item, behind the firewalls
    `test_incremental.rs` pins. Every host runs the same queries (`decisions/syntax-semantics.md`, "Decision
    record: every host resolves names through one path, and trees live in the host's store"), so
    a cut here is a cut for the server too. Salsa edges are about 29 MiB of the heap peak, 8 MiB
    of it `item_check`.
  - Done: kind-column scans, phase overlap, one-allocation HIR, memchr lexing and a one-pass tree
    finish took finance to 17.6 G instructions and 222-228 MiB in release (`decisions/syntax-semantics.md`, "The
    one-shot check scans kind columns and overlaps its serial phases"). Left: the parser and
    `TreeBuilder` self cost (about 1.4 G), the lexer's ident scans and per-token work (about
    0.65 G), and salsa fetch overhead spread across queries.
  - checker work itself: `infer_ordinary_call` is still the largest inclusive cost; `match_arguments`
    and type instantiation dominate it.
  - small `Vec` growth spread across many sites (`map_child_types` alone reserves about 88k
    times on the slice), which a small-buffer type would absorb.

## Open: the VS Code extension has no eslint configuration

`bun run lint` in `editors/code` fails because there is no `eslint.config.js` (eslint 9 needs one); no config has ever been committed. Add one before relying on the lint script.
