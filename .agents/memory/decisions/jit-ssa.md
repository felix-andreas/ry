# Decisions: JIT and SSA

Tier 0, the Cranelift tier, the shared SSA IR, deoptimization, and what compiles when.

## The JIT tier

**Status:** built, except the per-instruction plan as the compiler's input and the unconditional first-call compile, which "One SSA IR and one install-time pipeline for typed and untyped compiled code" and "A typed closure compiles at its first call only when that call can run long" supersede.

`crates/jit` compiles hot tier-0 bytecode with Cranelift. `development/design/r-runtime.md`, "What tier 1 does today", is the contract for what it does; this records why.

- **Thresholds are 3000 entries and 10,000 backward jumps** (`crates/jit/src/jit.rs`). At 10 and 500 a package load compiled the methods package's bodies that each `setClass` runs a few dozen times (about 200 ms of a cold `ry run`). finbench slices then picked 3000 (callgrind against `RY_JIT=off`): on 2 files entries 300 cost +2.9%, 1000 +1.4%, 3000 +0.6%, 10,000 +1.0%; on 21 files 1000 cost +2.7% and 3000 +1.5%; back edges at 30,000 changed nothing. The remaining overhead is per-call tier bookkeeping (`enter`, `compiled_for`, `valid`) and Cranelift, not deoptimization. Lower them only together with a cheaper first tier or background compilation.
- **A fully typed closure may compile at its first call** (`fully_typed`, `first_call`), so typed code never runs slower than the same code untyped. Declared types of formals and of a called closure's return seed the analysis where feedback is empty (`analysis.rs`, `load_ty`). Guards stay, because inline types are not sound in default mode. Only scalar types (`annotations.rs`, `SCALAR`) are seeded as unboxed. A `COLD` or nothing-native refusal does not mark the body refused, so the threshold path can still compile it with real feedback.
- **The tier interface is a hook table, not a dependency.** `interp` exposes `bytecode::Tier`, `set_tier`, `Machine`, `TierExit`, `step_one` and `load_variable`; `jit` depends on `interp`, never the reverse, and `libr` installs it.
- **Compiled code shares tier 0's register window.** Boxed values live in the window or in slot root cells after it, so the collector sees them without stack maps.
- **Deoptimization returns to tier 0's loop instead of calling into it.** Running the rest of the body in tier 0 from inside the compiled frame nested native frames on every deopt-and-OSR loop iteration until the stack overflowed.
- **Rare slow paths hand off to tier 0** (reason `HANDOFF`, not invalidating): integer overflow, an out-of-range read, `sqrt`/`log` of a negative. They warn, and a calling handler may read or rebind any variable the loop keeps in a register.
- **Plain arithmetic and pure builtins check for calling handlers.** They can warn, so under a handler they write back and forget the frame like a generic call; without one a warning is only deferred.
- **Variables live in slots with a runtime status** (unloaded, local, enclosing, dirty). The environment is the truth for everything outside the body, written back before any operation that can observe it and forgotten after any that can change it, which keeps `parent.frame()` and `sys.function` through callees correct without proving them absent.
- **The first read of a formal whose default is an expression writes back and forgets**, because the default runs in the function's frame and can read or assign the body's variables (`[.data.frame`'s `drop` reads `cols`). Supplied arguments evaluate in the caller's frame and need no write-back.
- **A refused body is not retried, except for `COLD`**; any other refusal, or a fifth compile, is final.
- **A body that would run nothing natively is refused** (`Plan::runs_natively`, `analysis::NOTHING_NATIVE`; a srcref store does not count, or every `keep.source` body would qualify). It would only call the helpers tier 0 calls. Most base R wrappers are such bodies. `RY_JIT=force` still compiles them so the tier tests cover them.
- **Function speculation is validated by the binding epoch, then by per-symbol stamps.** Compiling marks each speculated symbol and cell `WATCHED`; assigning a watched cell, or defining a watched symbol in any function frame, bumps one global epoch and the symbol's `lookup_stamp`. The epoch also moves on every non-function-frame binding (constantly, in a test suite), so `valid` (`runtime.rs`) then compares stamps and re-resolves only a moved one, from the environment its lookup started in (an inlined callee's, not the compiled body's). Re-walking every lookup per epoch move cost more than the compiled code saved. A lookup that passes a non-function binding is not speculated (`resolve_function`), because only the binding found is watched.
- **A call to a speculated closure is direct and binds plain values** (`jit_call`, `eval::bind_arguments`). Eager evaluation is invisible only when the callee cannot assign what an argument reads before forcing it, so the callee must be effect-free (`interp::effects::effect_free_lookups`, `frame_safe`) and no calling handler may be established. Such a callee cannot call `substitute()`, `missing()` or `sys.call()`, so no promise is needed, and the frame records no argument list because only `nargs`, method dispatch and `standardGeneric` read it. S3 dispatch from those builtins on a classed argument is the one accepted hole.
- **A value boxed on an edge into a join is dirty**: it is a fresh box the frame does not hold, so it must be written back before an in-place element write (`x[1] <- 9; get("x")`, pinned in `bytecode__hot_loops.R`).
- **A helper that cannot allocate parks the error**; later helpers return their fallback, and the return from compiled code raises it, so compiled code never branches on a nil box.
- **Static types do not remove guards.** A static `double` admits integers and logicals by promotion, and stubs claim scalar results for functions that can return vectors. Strict typing costs about 200 instructions per annotated call for the boundary checks.
- **A cold body waits instead of compiling with boxed guesses** (`analysis::COLD`). `fib` turns hot on its way down, before any call returns, so compiling then types every call result as boxed. `TIER_STATE` counts compiles in its low byte and cold refusals above it (`runtime::COLD_REFUSAL`); after three the body compiles with what it has.
- **A declared return type is the call result's early profile** (`annotations::declared_return_type`), a profile and not a proof, so typed recursion compiles on the first try.
- **Interrupts deoptimize** to tier 0's backward `Jump`, so one place handles them.
- **Rejected for compile latency:** Cranelift `opt_level = "none"` as a first tier (lowering and register allocation dominate and `none` keeps them); background compilation (compiled code embeds heap pointers and speculations validated against the interpreter's state at compile time, so it would need a frozen heap or a revalidation step); a persistent machine-code cache (those embedded pointers make the code process-specific; it would need relocations for every constant and speculation). A back-edge threshold of 2000 instead of 500 moved instruction counts by under 4% either way.
- **Inlining keeps annotation checks.** An inlined call has no frame, so `annotations::enter`/`check_return` never run; a typed closure inlines only when `annotations::admits_inline` proves every check passes (`test_tiers.rs` pins both directions).
- **A builtin's repeated calls check the signature once.** `lapply`/`vapply` over an atomic vector resolve `annotations::entry_for` once (`eval::KnownFirst`); a list's elements differ, so lists check every call.
- **Arrays are their own JIT types** (`Ty::IntArr`, `DblArr`, `LglArr`, from `feedback::INTEGER_ARRAY` and friends, which is why feedback is 16 bits per instruction). The guard reads only the kind and flag bits, because the Array bit promises no attributes. `x[[i]]` reads and stores directly only for the exact item type and an in-range position; anything else goes to tier 0, which applies the Array rules. `[` and arithmetic stay generic because their results must be Arrays checked for missing values. `jit_duplicate` keeps the Array bit.
- **A forgotten variable keeps its type as a guess** behind a guard, so a load on a branch tier 0 never ran does not leave its arithmetic to tier 0 for good.
- **A failed guard after a load resumes after the load**: the load may have forced a promise, and only that forcing set the visibility `withVisible(x)` reads.
- **A closure's type names its source body**, not the shared compiled code its `function` site swaps in, or the closure guard never matches and the body recompiles until it gives up.
- **A backward jump compiles impatiently.** The cold refusal applies only to compiles at entry, because at a back edge the loop is the hot code and waiting ran three more rounds of back-edge thresholds in tier 0; a back-edge compile that reaches a loop that never ran guesses an integer sequence behind the guard, because any non-cold refusal disables the body for good.

## Native code sees a compiled closure's body as its expression

**Status:** built.

- `BODY` and `R_ClosureBody` return the source expression of a compiled body, as `R_ClosureExpr` does. ry's bytecode holds binding cells and feedback with no GNU R type, so native code that walks objects (`lobstr::obj_size`) failed on them.

## Native scalar functions: frameless calls with checks proven at compile time

**Status:** built, except the scalar-only limit, which "Native functions loop, read vectors, and inline small callees" supersedes.

- **Shape.** `crates/jit/src/native.rs` compiles a closure whose body is a pure computation over parameters and locals (arithmetic, comparisons, `if`, `&&`/`||`, a few math builtins, calls of itself or other such closures) to `fn(budget, args...) -> (status, value)` over unboxed values, specialized per closure and argument types. A compiled caller emits `Action::NativeCall`; recursion stays in machine code.
- **Why no frame is observable.** The body reads only its parameters and locals and calls nothing that can look at a frame. Everything R would warn or raise on (`NA` conditions, overflow, `sqrt`/`log` of a negative, the depth limit, a pending interrupt, a left `NA` operand of `&&`/`||`) returns `FAILED` before anything visible happened, and tier 0 reruns the call and reports it as R does. The depth budget counts down per nested call so the limit matches tier 0 call for call. An argument with an assignment or `return` inside refuses, since eager evaluation would show.
- **Annotations** are decided when the native compiles (`entry_for`, `annotations::return_admits`); a function whose checks would fail is never compiled. `entry_for` and `admits_inline` refuse an Array mask for a plain scalar. Types make no scalar function faster than its profile does; they only decide checks at compile time.
- **Invalidation.** A native's function lookups, its callees' included, join every caller's speculations. Natives keep their closure rooted (the cache key is its address) and are capped at 4096 per session.
- **Not built:** unboxed records, tuples and tagged unions as native arguments and results.

## Compiled function lookups also check the activation's own frame

**Status:** built.

- R can bind a name in a function frame with no assignment in the body (`assign` into `parent.frame()`, a default argument, the locals `UseMethod` copies), so re-resolving only from the enclosure kept calling a stale global. `valid` takes the activation's frame and refuses when a speculation starting at the enclosure finds a possible function binding there. A loop entry and the first run after a compile walk the speculations regardless of the epoch, because the frame may have bound the name before the compiler watched it.
- Pinned by `environments__hidden_function_bindings` and `run_time_binding_shadows_cached_global_function` (`crates/jit/tests/test_tiers.rs`).

## An integer and a double joined stay unboxed

**Status:** built.

- **Shape.** `Ty::join(Int, Dbl)` is `Ty::Num`: an `f64` plus an "is integer" flag. Boxing, write-back and deoptimization rebuild exactly the R value the flag names (`runtime::jit_box_num`), so `typeof`, overflow and `NA` behave as in tier 0. An edge from `Int` converts, and an integer `NA` becomes the double `NA`. Widening to a plain double was rejected because R code observes `typeof` and integer arithmetic. Motivation: a boxed join ran a mixed loop in 0.67 s per 2M iterations against 0.10 s when both arms agree.
- **Readers.** Only `Move`, `SetVar` and arithmetic whose result is a double regardless of the flag (a comparison, `/`, `^`, or `+ - *` with a double operand) read a `Num` unboxed; `Builder::box_operands` and `Plan::boxings` box it for every other reader. An inlined body refuses a `Num` slot, and a direct call passes it boxed from its slot cell after the flush.
- Pinned by `test_tiers` seeds that observe `typeof`, `NA` and integer arithmetic after the join.

## The JIT leaves a lossy double modulus to tier 0

**Status:** built.

- GNU R warns "probable complete loss of accuracy in modulus" when `|a / b|` exceeds 2^63. Bytecode codegen and native code call `runtime::jit_modulus_loses_accuracy` (`interp::arith::modulus_loses_accuracy`, the single rule) and hand the operation to tier 0 when it holds.

## Compiled bodies share one deoptimization exit

**Status:** built.

- Each guard's cold block only stores the raw values into a stack scratch area and jumps, with its exit number, to one shared block calling `runtime::jit_deopt_exit`. Each exit's layout lives in `Compiled::exits` (`runtime::Exit`), and the runtime boxes and flushes from that table. The layout is static per guard and boxing is cold, so doing it in Rust shrinks every body (IR about 31% smaller, Cranelift time about 40% lower) without slowing a hot path.

## Native functions loop, read vectors, and inline small callees

**Status:** built.

- **Shape.** `native.rs` also takes plain integer, double and logical vectors (as object pointers; the caller roots them and the body allocates nothing) and compiles `for` over `seq_len`/`seq_along`/`a:b`/a vector, `while`, `repeat`, `break`, `next`, `length`, and `[[`/`[` with a scalar index. A typed tier-1 caller passes vectors straight through (`Argument::Unboxed`/`Argument::Slot` in `Action::NativeCall`). Every case R would warn or fail on (an `NA` or out-of-range index, a length above `.Machine$integer.max`, a fractional `a:b` start, a pending interrupt at a back edge) returns `FAILED` and tier 0 reruns the call.
- **Scoping.** A local first assigned inside a loop, an `if` branch or the right operand of `&&`/`||` goes out of scope at that region's end, so a later read refuses the function rather than guess whether R would find it bound.
- **Inlining.** A callee with at most 24 calls and operators and no `return` is walked in place, up to three levels and never recursively. Typing and emitting decide the same way because the decision reads only the callee's body.
- **Strict mode does not drop guards**: values from stubs or untyped packages can still differ from their declared types.

## Closures made at one site share compiled code

**Status:** built.

- **Shape.** A function literal that is a whole call argument compiles as promise code (`compiles_as_promise`, `crates/interp/src/bytecode.rs`), so `Op::Closure` gives every closure from that site one bytecode body. Compiled code is *portable* when each speculation finds its binding in a long-lived environment (`long_lived` in `jit.rs`) and then runs for any enclosure; `Compiled::checked` is a weak reference to the enclosure last proven, and a new enclosure makes `runtime::valid` walk every speculation from it (`Compiled::rooted`). Code that speculates on a function local to its enclosure stays per-enclosure.
- **Why a weak reference, not the enclosure's address:** a dead enclosure's address can be reused by a new environment nobody checked.
- **Long-lived means global, base, a namespace or its imports, or the search path.
- **Copies run the original's code** (`body_to_copy`). `compile` returns `R<Sexp>`; the only by-design refusal is a non-call body (`Uncompilable::NotACall`), anything else is a typed error, never a silent fallback. AST-made closures over one `function` expression find shared code in `Bodies` ("Closures over one body expression share its compiled code").

## Fast-path prologues return through an op, not an unwind

**Status:** built.

- A native prologue (`.Internal(ryX())` as a body's first statement) compiles to `Op::Prologue` for a zero-argument builtin marked `BuiltinDef::prologue`, not inside promise code. The op marks the running frame in `interp().prologue`, and `base_fast_paths::finish` returns the value instead of raising `Jump::Return`, which materialised a pending environment and deoptimized tier 1 thousands of times per workload. The outer marker is saved and restored for nested prologues; prologue builtins use `Visibility::Keep` because `finish` sets visibility.

## Bodies that would mostly hand back to tier 0 compile only for loops

**Status:** built.

- `compile_in` refuses with `MOSTLY_TIER0` (`analysis.rs`) when the plan has fewer native actions than tier-0 steps, unless a hot back edge or `RY_JIT=force` triggered it, and sets `runtime::LOOPS_ONLY` so only a hot loop can still compile the body. Each `jit_step` costs more than tier 0 running the instruction, and package bodies (`[.data.frame`, S4 `is`) are mostly calls tier 1 cannot specialise; a loop's native header pays for its steps every iteration. On a finbench slice these bodies cost about 0.6 s of Cranelift and the gate cut the JIT-on gap against `RY_JIT=off` from about 8% to about 4.5%. A fully typed closure's first-call compile goes through the same gate. The refusal criterion is refined by "A body compiles only when its native work covers what compiled code costs".

## Tier 0 records type feedback only for warm bodies

**Status:** superseded by "Tier 0 records feedback only for bodies halfway to a threshold". Kept for the `NEVER_HOT` and `LOOPS_ONLY` follow-ons and for why a first-call `MOSTLY_TIER0` verdict is not final.

- A body refused as `MOSTLY_TIER0` gets the entry count `runtime::NEVER_HOT`, so tier 0 stops asking the tier about its calls.
- `runtime::invalidate` resets the entry count to half the threshold, not zero, so the deoptimizing activation records the type that broke the speculation; from zero the body could recompile the same speculation until it ran out of compiles.
- A `LOOPS_ONLY` body compiled for its loop still runs its calls in tier 0 (`compiled_for` returns early), because entering from the top would hand most instructions back one by one.
- A first-call `MOSTLY_TIER0` refusal plans without a profile, so it leaves the tier state alone.

## A typed closure compiles at its first call only when that call can run long

**Status:** built.

- A first-call compile is refused as `BRIEF` (`analysis.rs`) unless `Plan::may_run_long` (a loop, or a direct call of a closure or native function, recursion included). The refusal is not counted, so the body tiers up at the threshold like any other. Compiling every typed `alpha + beta` at its first call cost about a millisecond each (183 compiles on a finbench slice) for bodies most tests call a handful of times. `compile_in` takes a `Trigger` (first call, hot entry, hot loop) so each gate states which triggers it applies to.

## Strict-proven types do not drop JIT guards yet

**Status:** built.

- **Rule.** No guard drops because of a strict-mode proof until a per-function strict-clean verdict reaches the runtime. Then only a SEXP-type or scalar/vector-kind guard on a non-escaping local or parameter, after its force point, whose type derives from checked sources alone, may drop. Guards on `Any`, stub returns, cross-package values, `@trust`/`@if-unknown` bindings, class, attributes, the object bit, length beyond a checked scalar, `NA` and global lookups always stay.
- **Why not built.** A parameter read runs one fused check (SEXP type, no attributes, no object or ALTREP bit, length 1), so dropping the type compare saves one compare per read, and typed and untyped warm times are equal on every why-ry row. Revisit when a profile shows guard cost.

## Untyped R code is not compiled to machine code ahead of time

**Status:** built, as a decision not to build: `--aot` compiles typed code only, and untyped code runs on tier 0 with tier 1 compiling hot functions during the run.

- **Why.** Without a profile, ahead-of-time code for untyped R has only the bytecode, which is the baseline tier that measured slower than tier 0 ("No baseline tier, and code images hold bytecode only"). Tier 1's speed comes from observed types and call targets. Measured on the ry-owned suites: on the applications tier 1 buys nothing yet (JIT off over on 0.999; compile 0 to 31 ms per program), and on the kernels (JIT off over on 5.2) its compile is 2 to 9 ms per program, the most an up-front compile could remove, and only with the same profile.
- **Profiles already travel as build output.** A trained code image (`R CMD INSTALL --ry-train=...`, `crates/interp/src/code_image.rs`) stores feedback and counters, so a body hot in training reaches tier 1 at its first calls (after Leyden's JEP 515, `prior-art.md`). Storing tier 1's machine code beside it becomes worth building once the applications show tier 1 winning (JIT off over on well above 1) and its compile showing in a cold run. Facts-based code (checker types, closed bindings of locked namespaces) is the typed path's business, where the types are sound.

## Bytecode is build output, not a cache

**Status:** built, except the base image's code section: base R's compiled closures live in the start image, per "The start image keeps base R's closures compiled".

- `ry install` / `R CMD INSTALL` write bytecode beside the lazy-load database as code images, stamped with the ry build; another build ignores the stamp and compiles at first call. `ry load` and scripts compile at first call, so that tier must be cheap. A content-hashed on-disk cache was rejected: a second copy of each package's code that needs invalidation and a key computation on every load. Spec: `r-runtime.md`, "Where bytecode comes from".

## Tier 0 runs on facts fixed before the code runs

**Status:** built, except the single `interpret` loop with `tri!`, which the tail-call handlers in "Step 2 of the untyped roadmap: slot frames rejected on measurement, setters compiled, step 3 pulled forward" supersede.

Facts that do not change between calls are computed once, not per instruction (`crates/interp/src/bytecode.rs`; `r-runtime.md`, "Tiers"):

- An op word is `op | operand_count << 8` (`op_word`, `op_of`), so decoding needs no table.
- Each `GetVar`, `SetVar` and function lookup site owns a slot hint (`HINTS`, one `u32` per site). `Frame::find_hinted` checks the symbol filter, then the hinted slot, then searches and updates the hint. A hint is never trusted for correctness (the symbol at the slot is compared every time), so it needs no invalidation and is not a cache. A frame binding a symbol twice (duplicate formals, where the first cell answers) sets `SHADOWED` and ignores hints; `fuzz_hinted_lookups_match_plain_lookups` (`crates/heap/tests/test_frames.rs`) pins hinted against plain lookups.
- `SetVar` assigns through the cell the hinted lookup found (`eval::assign_local`), and `Heap::define_unbound` adds a binding without a second search.
- `BuiltinDef::checks` is derived at registration.
- A frame keeps a hash index only above 32 bindings (`INDEX_THRESHOLD`, `crates/heap/src/env.rs`): behind the filter and hints a scan is cheaper than an index rebuilt on every call of a base closure binding about 20 names.
- Lookup-cache stamps are read as `u64` bits of a double vector (`LookupCache::epochs`); all-ones means empty.

## The untyped performance roadmap is ordered by measured gap, and the baseline tier is Sparkplug-style

**Status:** built, except the baseline tier, which "No baseline tier, and code images hold bytecode only" supersedes.

- **Single source of truth:** the "Untyped performance roadmap" table in `development/design/r-runtime.md`, one row per idea with status and the measured gap it targets. `prior-art.md` holds per-engine reasoning; `backlog.md` keeps only raw measurements.
- **Order rationale.** Call setup and lookup come before optimizing tiers because `--profile-summary` puts closure call setup at 12 to 24% and S3 lookup up to 17% of real workloads against tier 1 under 3%: the code that matters runs a few thousand times and never reaches tier 1.
- **Order:** finish the tier-0 call path, then call facts ahead of time (version-stamped bindings, slot frames, promise elision, argument-match caches, S3/S4 inline caches), then quickening and tail-call dispatch, then the optimizing tiers, then mapped images.
- **Rejected: a separate call plan for `run_closure`**, whose annotation, `on.exit`, move-tracking and depth checks cost 1.3k of a `seq(1, 10, by = 2)` call's 141.6k instructions, so it would save under 1%; the rest is frame push/pop that slot frames replace.
- **Rejected: copy-and-patch** (CPython's PEP 744): its gain over a good interpreter is modest, and its stencils need a Clang build step that a Rust workspace does not have.

## Step 2 of the untyped roadmap: slot frames rejected on measurement, setters compiled, step 3 pulled forward

**Status:** built, except the slot-frame rejection, which "Slot frames: closure locals live in registers until something asks for the environment" supersedes.

- **Version-stamped guards.** `Op::Guard` compares the lookup slot's stamp and enclosure (`cached_cell`), then the cell's value. A cached cell never becomes active (`makeActiveBinding` refuses a regular binding) and a binding added in front moves the symbol's stamp, so nothing more is needed.
- **One-level setters compile** to `Op::Replace` when the target is a variable and the other arguments are side-effect free. `replace_variable` mirrors `complex_assign` for one level (builtin replacement on argument values, objects fall back; a closure replacement gets the uncounted `*tmp*` and value promises). An argument register that is the target itself forces a copy, because registers count no reference.
- **Tail-call handlers.** Tier 0 runs one handler per op chained with `become` (`#![feature(explicit_tail_calls)]`, pinned nightly). A handler takes only `(&mut Activation, pc)` and returns nothing; code, registers, the `Interp` pointer and the result live in the activation (`Activation::exit`, `STOP` ends the chain). Passing code, pc and registers as arguments with an sret result kept five values live across every call and a six-register prologue per handler, cancelling the dispatch saving. Outlining the bodies of instructions that call out was worse (131k against 121.4k per `seq()`), because the wrapper still needed a frame. The structure stays because a stackless tier 0 and quickened handlers build on it. The error path (`failed`) is cold and out of line.

## One SSA IR and one install-time pipeline for typed and untyped compiled code

**Status:** built, except install-time code: for untyped R, "No baseline tier, and code images hold bytecode only" supersedes it, and for typed code it is not built.

- **Shape.** `crates/ssa` is the one mid-level SSA IR (`ssa.rs`: SSA construction after Braun et al., a first-class guard whose exit is a cold deoptimization block), the one lowering to Cranelift (`lower.rs`) and the one code module per thread (`ssa::with_module`). Tier 1 (`codegen.rs`, `native.rs`) and the typed core (`crates/typed/src/backend.rs`) build it, and no other crate depends on Cranelift. Typed facts enter unguarded; untyped speculations carry guards and exits. Two forms would duplicate the instruction set, lowering and passes and drift.
- **Install-time typed code** is to go through the typed AOT pipeline in `typed-runtime.md`, storing deoptimization metadata and watched bindings so `unlockBinding`/`assignInNamespace` invalidate it like JIT code. The run-time JIT stays for fallbacks and new profiles, which is why `prior-art.md` rejects compiling every fallback at build time.
- `crates/ssa/tests/test_ssa.rs` fuzzes random programs with branches, loops and mid-block guards against direct evaluation. The IR is runtime build output, never a salsa input.

## The effect-free fact is stored per code object and resolved per enclosure

**Status:** built.

- **Stored half.** The bytecode compiler records a `SUMMARY` constant (`crates/interp/src/effects.rs`, `summarize`): nil when the body assigns non-locally or calls anything other than a symbol, otherwise the names it calls. Computed once per `function` expression.
- **Resolved half.** `effect_free` resolves the names from the closure's environment: each must be `<-`, `=`, `[<-`, `[[<-`, an `EFFECT_FREE_BUILTINS` entry, or an effect-free closure (depth 8, recursion keyed on code and enclosure). Dependencies are marked watched and their `lookup_stamp`s stored in the call site's cache, so any rebinding invalidates the proof.
- **Home keying.** Helpers created per call (`is.logint` in `seq.default`) get a new enclosure every call (an enclosure-keyed proof missed every time, at about 950 instructions per miss), so a proof is keyed on the first ancestor that is not a function frame unless frames in between bind a called name (`Proof::HoldsHere`); a failing proof is always keyed on the home.
- **Eager binding** (`op_call_generic`): the site is marked eager (at least one symbol argument, and every argument is a symbol, a constant or a nested call of effect-free builtins), the callee is effect-free, and no calling handler is established. A value or forced promise passes the value, an unforced promise passes itself, and a missing, unbound or active binding keeps its promise (`eager_argument`).
- One computation serves tier 0 and the JIT.

## Why the JIT made code slower, and the three fixes

**Status:** built, except the baseline tier as the fix for compile cost, which "No baseline tier, and code images hold bytecode only" supersedes.

- **Sibling enclosures share a proof.** Closures from one `function` expression alternate enclosures constantly, and re-pointing the proof walked every speculation each time. An enclosure whose parent is the checked enclosure's parent shares the proof and checks only its own frame for the rooted names (`runtime::sibling_of_checked`, `binds_rooted_function`); re-pointing had made such a loop 2.7x slower than tier 0.
- **`still_valid` skips `valid` when the epoch did not move**, since any binding that could change a function lookup moves it.
- **A call head that is a call compiles** (evaluated into a register, called through `Op::CallGeneric`), so `pkg::f(x, function(v) ...)` gets compiled promise arguments and the lambda shares one body instead of recompiling every run.
- Compile cost remains: Cranelift takes 0.5 to 1 ms for a tiny body and 263 ms for `rbind`'s (343 KB of code), which warm runs do not pay but scripts and tests do.

## Tier 1 probes the frame inline for a variable load

**Status:** built.

- Compiled code reads the site's hint (`bytecode::lookup_hints`) and probes the activation's frame, then an enclosing function frame (`codegen.rs`). A plain value or forced promise is taken inline with no write-back, feedback or validity check; anything else calls the runtime. `SetSrcref` outside a debugger stores the srcref inline. A value of an unspeculated type is recorded through `jit_record` before deoptimizing, so the recompile sees it. A load whose feedback saw both an integer and a double scalar is `Ty::Num`, so closures from one site with different argument types keep arithmetic compiled.

## Bodies wait in proportion to their size before tier 1 compiles them

**Status:** built.

- On first reaching a threshold a body waits one more threshold per 250 words (`SIZE_WAIT_WORDS`, `runtime::SIZE_WAITED`). Compile cost is linear in size and the per-entry saving is not, so a large body must run longer to repay it (V8 scales its tier-up budget by bytecode length the same way). `rbind` (about 9,700 words) no longer compiles in any script that calls it 3000 times; base `Reduce` (2,400 words) still compiles, because its loop is hot.

## Tier 0 records feedback only for bodies halfway to a threshold

**Status:** built, except the half-threshold entry window, which "Tier 0 decides at scheduled entries and stops recording at tier-up" supersedes with the last eighth.

- One profile word, `profile::RECORDING` (`bytecode.rs`), replaces a per-activation `warm()` test. It is on during the first activation of a body with a loop, inside the entry recording window, and from half the back-edge threshold; it is off whenever tier-1 code is installed.
- **Why the first activation of a loop body records.** Starting at half the back-edge threshold left the instructions before the loop without feedback, so the on-stack compile handed the loop's stores to tier 0 (10x slower). Recording in every activation of a loop body made base `Reduce`, `Filter` and `Map` pay a type test per value on every call.

## Bodies that could run nothing natively never record

**Status:** built.

- `analysis::shape` (`Shape::may_run_natively`) reads the instruction words alone: a loop, an instruction kind the plan may lower natively, or a `CallBuiltin` naming a builtin with a native form (`has_intrinsic`, which also gates `intrinsic_for`, so the lists are one). Its `match` over `Op` has no catch-all, so a new opcode forces the decision. `Tier::worth_recording` runs it when the recording window opens and `compile_in` before planning, so a hot body that skipped the window (thresholds below 8) is refused without a plan too; the refusal (`NEVER_NATIVE`) is final, including at a typed first call. `RY_JIT=force` keeps such bodies.
- **Known gap.** A builtin called through another name (`f <- length; f(x)`) is judged by the call's name, which costs only speed.

## A loop-free hot body compiles only when its call rate repays the compile

**Status:** built, except the baseline threshold in the back-edge check, which "No baseline tier, and code images hold bytecode only" supersedes. The saving per call (100 per native instruction below) is replaced by the model of "A body compiles only when its native work covers what compiled code costs". `repays` is `unpaid`, which also gives the final refusal ("The cost model counts frames, and free steps count nothing").

- **Shape.** At the entry threshold `compile_in` asks `repays` before planning a loop-free body. Expected calls are the body's rate over its recording window (calls of it per closure calls made meanwhile, `Interp::calls_ever` stamped into `profile::WINDOW`) times a fixed horizon of 2^19 calls. A refusal (`RARE`) is not final; a recompile skips the check.
- **Why calls are the clock.** An allocation clock moved tiering decisions when slot frames cut allocation per call. An optimization must not move tiering decisions, so the clock counts closure calls (one increment per frame push), which is deterministic.
- **Why a fixed horizon.** The session's age (Lindy) underestimated the functional workload's first callbacks 13-fold. `isTRUE` runs once in 63 calls and the slowest functional callbacks once in 9; those need a horizon of 390,000 calls and `isTRUE` would need 600,000, so 2^19 sits between the slowest callbacks worth compiling and `isTRUE`, with about 1.3x margin either side; revisit when a workload lands near the line.
- **Rejected: never compiling loop-free bodies at entry**, or a flat threshold rise: every body the functional workload compiles is loop-free, and those compiles are its win (delaying them to 10,000 entries cost 19M instructions, to 30,000 149M).
- **Backoff.** After the n-th refusal a body waits the threshold times 2^n (`runtime::RARE_REFUSAL`), or a rare body re-records every 3000 entries all session.
- **Back edges** count inline (`back_edge_due`) and call the cold `back_edge` only at the counts it acts on; an out-of-line call per jump cost testthat 1.3M instructions.
- **Remaining gap:** the small JIT-on excess on testthat is feedback recording in the first windows (`run_get_var`, `run_call_builtin`, `run_call_generic`), which the native execution model replaces rather than tier-0 tuning.

## Tier 1 keeps the 3000-entry threshold

**Status:** built.

- Thresholds from 1000/3000 to 10000/30000 moved instructions against `RY_JIT=off` by under 0.6% on `bench-real`. A lower threshold compiles more bodies that do not repay; a higher one delays long-running programs. The threshold is not what decides JIT on against off: most time is builtins and call setup that tier 1 hands to tier 0. Revisit once tier-1 compiles get cheaper.

## Tier 1 inlines block-bodied closures, and a failing inline reruns the whole call in tier 0

**Status:** built.

- A body `{ a <- e1; b <- e2; result }` inlines (`lower_block`) when every statement but the last assigns a symbol through base `<-` or `=` and the last may be `return(value)`. Callee identity is guarded by type for a local closure and by the binding epoch for a global one.
- An inlined effect-free body that fails a guard hands the whole call back to tier 0, which reruns it from the start; inlining callees with effects needs frame states ("Tier 1 leaves compiled code through frame states").

## The native execution model: one design for frames, calls, unboxing and install-time code

**Status:** planned. Built so far: the slot window in tier 0 and tier 1, tier-0 call plans, unboxed scalars and per-exit state in tier 1 (`runtime::Exit`); machine-to-machine sibling calls are missing, and "No baseline tier, and code images hold bytecode only" supersedes the install-time entries.

Frames, the calling convention and unboxing constrain each other, so they are designed together; `r-runtime.md`, "The native execution model", is the spec.

- **Frame layout.** Every tier runs a call on one register window: header (environment or `NULL`), slots (formals, numbered locals, `...` as its `DOTSXP`), body registers; missing bits in `Frame.missing`. The environment is made only by a reify trigger (`frame_env`, `frame_sys_parent`, `Call::env`) and is then the only source of truth.
- **Machine-to-machine calls (planned):** a direct call to a sibling's entry in a locked namespace, pure arguments unboxed and the rest as promises over the caller's slots.
- **Refused bodies** keep heap frames: those binding names the compiler cannot number (`assign`, `eval`, `local`, `with`, `source`) or handing out the frame (`environment()`, `sys.frame(s)`, `lockBinding`). A slot path is planned for non-literal missing defaults (promises over the slots), `sys.function`, `on.exit`, `match.arg` and `do.call` with a function value and no `envir`.
- **Deoptimization** boxes from a per-exit frame-state map, writes the slots and resumes tier 0 on the same window without making an environment. Machine code checks the header after any call that can reify and leaves when it is set, because the slots are then dead.
- **Boxing.** Attribute-free length-1 doubles, integers and logicals stay unboxed until they escape (a slot write before an observing operation, a non-native callee, a vector or list store, a promise, a return to tier 0).
- **Ownership.** Frames, calling convention and reification live in `crates/interp`; Cranelift codegen for unboxed scalars, direct calls and exits in `crates/jit`.

## Tier 0 decides at scheduled entries and stops recording at tier-up

**Status:** built.

- **Shape.** Each tier decision happens at a known entry count, so the profile word `profile::NEXT` holds the count at which the next decision is due (`i32::MIN` means the next entry). The hot path is one comparison; `entry_event` (`#[inline(never)]`) decides and derives the next count. Anything else that moves a counter or the tier state (back edge, compile, `runtime::invalidate`) sets `NEXT` to `i32::MIN`. Without a tier, code asks every 4,096 calls so a tier installed later still reaches it. Code images keep the first four profile words, and `NEXT` starts at `i32::MIN` on decode.
- **Recording.** `RECORDING` is set only with a tier installed; a compile turns it off and a discard turns it back on for a body that can recompile. The type that broke a speculation is recorded by `jit_record` before deoptimizing either way.
- **Window.** Entry recording covers the last eighth of the entry threshold (`bytecode::recording_start`): with half, 96 bodies on the testthat workload started recording and 57 never reached the threshold, recording for the rest of the run. A `SIZE_WAITED` body stops recording until its counter re-enters the window. Loops record from half the back-edge threshold.

## Tier-0 element reads and stores as fast handlers, not new ops

**Status:** built.

- **Shape.** Fast handlers in the `fast_paths!` table for `BranchUnlessPrimitive`, `BranchIfObject`, `CallBuiltin` on base `[[` and `[` (`element_access::element_position`, `element_value`: a numeric scalar position, or for `[[` an ASCII name, in a plain atomic or list vector) and `SubAssign` (same-kind scalar into an unshared vector, list stores by position or existing ASCII name). Each declines to the generic handler with no side effect, so the op stream the JIT reads is unchanged.
- **Cheap declines.** `CallBuiltin` is every builtin call's op, so the handler checks count and identity inline and reads out of line (`read_element_call`); inlined, its register setup ran for every builtin call it declined. Moving the store out of line too, or screening the subscript inline, cost the taken path more (about 50 instructions per store, 20 per read) than it saved. Declines pay for the attempt (`s <- x[-1L]` went from 1761 to 1853 instructions per iteration).
- **Why no fused element op:** it would change the op stream the JIT's analysis reads for little more than the handlers give.
- **Names by pointer.** Every ASCII `CHARSXP` is one interned object whatever its encoding mark (`heap::strings`), so pointer equality is exactly `[[`'s and `$<-`'s exact match; `NA_STRING` is never interned. Non-ASCII, empty or absent names decline (`element_access::name_position`).
- Pinned by `builtins__compiled_element_reads.R` and `assignment__compiled_element_stores.R`.

## Computed subscripts stay in bytecode

**Status:** built.

- **Shape.** `complex_assign` takes the forced arguments of a one-level target (one promise per subscript holding its code and value, built by `bytecode::evaluated_arguments`), so a declined in-place store reuses the registers' values and `substitute()` in an S3 `[<-` method still sees the code. The compiler accepts every subscript except `...`, `..N`, an empty argument and code that assigns the target (`Compiler::lowerable_subscript`). A register holding a symbol counts as a formal-lookup marker only when it is the subscript's own code. Which subscripts are evaluated before the target is probed is decided by "A compiled replacement probes the target before any subscript runs".
- **Evaluation order.** GNU R evaluates the value, reads the target, then evaluates the subscripts, so an unbound target errors before a subscript's side effect.
- **Accepted difference.** A subscript calling a closure that rebinds the target with `<<-`: GNU R assigns into the value it read first, the in-place path into the binding after.
- **Appends in place.** `assign_element` appends at `length(x) + 1` in place when the vector is unshared and attribute-free (`subset::append_copy` reserves `alloc_with_room`'s headroom) and widens integer/logical stores without leaving the fast path. The character read sits behind a tail call to a cold function, because an inline kind test turns the numeric read's dispatch into a jump table.
- Pinned by `assignment__computed_subscript_compiled.R`, `builtins__character_element_reads.R`, `builtins__combine_lists.R`, and `tier0_computed_subscript_stores_stay_near_gnu_r` (`crates/ry/tests/test_perf.rs`, within 1.5x GNU R's instructions).

## Compiled code stores list elements and kind-checked vector elements itself

**Status:** built.

- **Why:** `rcp/vector-for` ran at 0.38x GNU R with the JIT on, because every store to a slot whose vector type changed stepped out to tier 0.
- `analysis::sub_assign_store` picks one of three stores. `Typed`: the slot's vector type takes the scalar. `Checked`: unknown or changing vector type; compiled code reads kind, attributes and flags at run time, stores in place when the kind takes the scalar, and hands conversions and non-plain vectors to tier 0 (`sub_assign_in_tier0`). `List`: `x[[i]] <- value` through `runtime::jit_store_list_element`, which stores or appends within spare capacity and declines for `NULL` (deletion), attributes, a shared list, growth past capacity, and a value that is the list itself (`x[[2]] <- x` must copy first).
- **Closures with native entries never tier on hot entry** (`NATIVE_ENTRIES`, `analysis::NATIVE_ENTRY`, final): their body runs only for declined calls, and its first compile paged in megabytes of the code generator.
- Pinned by `crates/oracle/cases/assignment__element_store_kinds.R` in all three JIT modes.

## The road to 3x

**Status:** planned. Built so far: tier-0 quickening, the vector distribution kernels with batched samplers, and the `unlink()` rewrite; "No baseline tier, and code images hold bytecode only" supersedes the install-time machine code row.

- **Goal.** The next milestone is 3x GNU R on every bench-real suite; the 3x row of the roadmap table in `development/design/r-runtime.md` sets each bucket's factor (R code 5x, builtins 3x, ry's ports 3x, GC and start-up 2x). 3x is a checkpoint: every track designs for the 10x row (R code 30x, builtins and ry's ports 10x, GC and start-up 10x), and a plan that can only reach 3x in its bucket is rewritten before it lands.
- **Why R code needs a new tier.** Tier 1 runs under 3% of R-level time because workload closures are called a few thousand times; tuning tier 0 cannot reach 5x.

| Bucket | 10x end shape |
| --- | --- |
| R code (inlining tier) | Almost all R-level time runs as machine code; callees inlined across closures and S3/S4 dispatch; frames, promises and environments materialize only on deoptimization |
| R code: tier 0 | 10 to 20 instructions per simple op (about 120 today): pinned interpreter state, quickened slot-local typed ops, feedback recorded only until a site is quickened |
| Builtins: call machinery, subassignment, `[`/`[[`, apply family | No allocation on any fast path, arguments without pairlists, results written in place when unshared |
| Builtins: element-wise kernels | Fused element-wise temporaries and SIMD kernels |
| Builtins: strings | Interned strings with no temporaries |
| ry's own ports | Vector d/p/q/r paths, SIMD with bit-identical results, batched RNG with identical streams |
| GC | Young-survivor tracing at a fraction of today's 70 instructions per object |
| Start-up | A mapped start-up image with no inflate and no per-object decode |
| Corpus I/O and waiting | Done: `unlink()` follows GNU R's `R_unlink`; the waiting is the suites' own `Sys.sleep` |

- **Inlining tier.** Inline across closure calls, `UseMethod`/`NextMethod` and S4 dispatch, guarded by version-stamped bindings and the receiver's class; escape analysis removes inlined frames, promises, environments and boxes; deoptimization rebuilds what GNU R would show (`sys.calls`, `on.exit`), and `sys.function`, `parent.frame`, `environment()`, `eval`, `substitute` or `missing` either deoptimize or are answered from the metadata. Thresholds drop to the hundreds; target over 60% of R-level time in tier 1. Method JIT per `prior-art.md`; trace compilation stays rejected.
- **Progress tracking.** Each merge reports the bench-real suites with their intervals and the bucket shares from `ry run --profile-summary`, against the 3x row.

## Tier 0 runs frameless fast handlers, quickened arithmetic and out-of-line loop fallbacks

**Status:** built, except the baseline tier it mentions, which "No baseline tier, and code images hold bytecode only" supersedes.

- **Frameless fast handlers.** Each hot instruction (`GetVar`, `SetVar`, `Guard`, `JumpUnless`, `Jump`, `Constant`, `SetSrcref`, `EndStatement`, the quickened arithmetic) has a fast handler with an `#[inline(always)]` fast path that makes no calls at all, then dispatches or tail-jumps (`become`) to its generic handler in `GENERIC_HANDLERS`. Any call in the fast path, even on a rare branch, makes the compiler save registers in the whole handler's prologue; that is why the heap has `Space::allocate_from_run` and `Heap::try_alloc_scalar` (bump or return `None`, never refill or collect). Generic handlers and `jump` are `#[inline(never)]`, or the compiler merges them back.
- **Quickening.** A generic `Arithmetic` site that saw two plain scalars (`object::is_plain_scalar`, one masked header compare: kind, no OBJECT/ALTREP/ARRAY flags, no attributes, length 1) has its opcode byte rewritten to one of 19 forms (`QUICK_ARITHMETIC`: integer and double `+ - * /` and the six comparisons); a miss writes the generic opcode back (`arithmetic_miss`) and after 4 undos the site stays generic. Only the opcode byte changes, and `op_of` reports a specialized word as `Arithmetic`, so the verifier, code images and tier 1 never see one. Rejected: a quickened copy of the code, because tier 1 keys feedback and deopt targets by pc.
- **Compare and branch.** A quickened comparison followed by `JumpUnless` on its result takes the branch itself and records its direction (`feedback::BRANCH_TAKEN`, `BRANCH_NOT_TAKEN`).
- **Feedback** is a monotone OR of bits (`bytecode::feedback_bits`); feedback, entry and back-edge counters stay keyed by the compiled pc.
- **Loop layout.** Cold fallbacks of in-loop instructions are emitted after the loop's back edge, each ending in a `Resume`, so the hot body is contiguous.
- **Step seam.** `bytecode::step(machine, pc)` runs exactly one instruction. Any pc is a valid entry and no dispatch state lives in host locals across instructions; `interp::testing::check_resume_at_every_pc` and `fuzz_tier_zero_resumes_at_every_pc` pin it by leaving through `TierExit::Resume` after every N steps.
- **For the 10x shape:** the guard folded into a per-body version stamp checked once per entry, pinned handler arguments (code, registers, pc), and the visible flag set once per statement rather than per instruction.

## Tier 1 compile throughput: linear SSA construction and one compile per body

**Status:** built, except the weak reference from a body to its code, which "Closures over one body expression share its compiled code" supersedes.

Compile cost is linear in body size (about 7 µs per word; one compile of `[<-.data.frame`, 14,819 words, about 125 ms), so no size cap or hotness threshold is needed:

- `ssa::Builder::read_variable` walks single-predecessor chains once predecessors are known, instead of giving every block between use and definition a parameter for trivial-parameter removal to delete.
- Slot runtime statuses live in a stack array of 4-byte cells (`Translator::statuses`), not SSA variables, which gave every merge a parameter per slot.
- Register allocator by loop: a loop-free body uses Cranelift's single-pass allocator (`ssa::Allocator`), a body with a loop the backtracking one; single-pass everywhere made loop kernels 1.5 to 2.4x slower. Cranelift fixes the allocator per module, so each has its own pool. If big cold looping bodies matter, compile them single-pass and recompile when their loops get hot, rather than capping size. (Refined by `SINGLE_PASS_FROM` in "A body compiles only when its native work covers what compiled code costs".)
- A deopt exit writes only the loops around its pc, since tier 0 reads a loop's words only inside it; writing every loop made each exit grow with the body's loop count.
- Trivial-parameter removal is a worklist over each parameter's users rather than repeated sweeps.
- A variable load inlines only the slot-frame hit; walks run in `runtime::jit_probe`.

Why one body compiled up to four times, each fixed at its source:

- A failed speculation on a stepped instruction was never recorded, so recompiles made the same bet; `interpret::<SINGLE>` records always.
- `long_lived` must require the `name` attribute an imports environment has, or every call frame of a base function counts as long-lived.
- A slot frame's closure holds the frame's pending environment (`eval::pending_env_of`), not the nil environment field.
- The first time a body's code meets a second enclosure its tier state gets `runtime::SHARED`, and compiles keep only speculations every enclosure resolves the same way.

## A compiled element op costs no more than tier 0's handler

**Status:** built.

- **Rule.** A JIT-compiled element operation never costs more instructions per iteration than tier 0's handler for the same shape. Pinned by `element_ops_cost_no_more_than_tier_zero` (`crates/jit/tests/test_tiers.rs`), which runs each shape under cachegrind (the cloud VMs have no hardware counters) and passes with a note without valgrind or in a debug build, whose unoptimized instruction counts say nothing about the design.
- **Shape.** A step first calls `bytecode::fast_step` (in `runtime::jit_element_step`), which runs tier 0's own fast handler (`call_builtin_fast`, `sub_assign_fast`, `field_fast`) and skips write-back and reload when it applies; `Action::Step` carries `Fast::{No, Read, Store(slot)}`, and a store writes back only its own variable. The fast handler is skipped where it can only decline: a `[`/`[[` read whose index is typed as a vector, and a store the compiled store already declined. One handler serves both tiers, so they cannot drift. Where it declines, `bytecode::quiet_step` runs the generic handler without write-back when it cannot run R code (no calling handler, no object, ALTREP, environment or promise operand); this keeps a range subscript, a converting append and a partial `$` match under tier 0's cost. `$` steps as pure unless observed, like `[[`.
- **One helper, one guard.** `jit_element_step` runs the fast handler outside the panic guard and only `quiet_step` under it, so a fast hit pays no guard frame. A builtin call that runs no R code goes through `runtime::jit_call_builtin`, which runs tier 0's `op_call_builtin` body (`bytecode::call_builtin_step`) without the step table.
- **Instruction count traps** (callgrind on the fat-LTO `profiling` build). An activation whose address escapes on any path is stored to memory on every path (about 27 instructions), so `fast_step` builds one per arm and `quiet_step` its own. A closure called from two sites does not inline, so `guarded` and `step_unsignalled` call their body from one site. The boxed write-back in `jit_flush` skips the guard and hands compiled code's reference over to the slot (`Heap::hand_over_slot`); every other write-back is out of line (`flush_guarded`).
- **The builtin step records no feedback where compiled code checks the result's type**: `Translator::refine` records the type in a cold block when the check fails, which is the only new type a recompile needs. Where the result is unchecked (`Boxed`), the step records it, or a site that first ran compiled would stay untyped through every recompile.
- Boxing for tier 0 reuses an object tier 0 already has (`Translator::box_for_tier0`). Write-back uses the slot's compile-time position (`Compiled::frame`).
- **Shared code is found by the body's address, and the collector drops dead bodies' entries** (`Bodies::clear_dead`, `bytecode.rs`). A body has no spare field for a dense index. `clear_dead` runs before anything is freed, so no address is reused while a stale entry exists; a weak pair failed here because the collector clears its key to `NULL` first and `NULL` counts as live, so a new body at a dead body's address ran the dead body's code. Pinned by `a_body_at_a_dead_bodys_address_runs_its_own_code`.

## Tier 0 compiles whole bodies

**Status:** built.

- Tier 0 compiles a body whole, at its second call or its first if it has a loop (`bytecode::compile_before_call`). Deferring large `if` arms to stubs compiled at first run was built and lost: most compiled code in a hot body is on paths that run, and each run of a deferred arm pays a child `Machine` entry. Measured: compile fell 34.5M to 29.8M instructions but `library(testthat)` rose 229.0M to 230.0M (`testthat_full`: compile -6M, run +11M), with `op_deferred`, `execute_code` and `grow_registers` about 17M. Recompiling with hot arms inline at their eighth run doubled compile on `testthat_full` (109M to 235M) and added 96M of collection; raising the call threshold to 4 did not help (the bodies compiled during load are hot or loop). Do not retry without a new idea. Installed packages carry code in `.ryc`, so their load compiles nothing.

## Tier-0 call entry: raw pool reads, an out-of-line tier schedule, and a cleared register window

**Status:** built.

- **The entry reads the constant pool raw** (`pool_entry`, `pool_ints`): the compiler builds the pool and its fixed vectors, so none is ALTREP; debug builds assert it.
- **The scheduled entry is out of line** (`run_scheduled_entry`), so `execute_code` inlines into `run_at`.
- **`holding` counts references in place** over the held registers, which belong to the running activation.
- **A global `GetVar` reads through the site's cached cell**, as `GetFun` does; a cached forced promise answers with its value.
- **The register window is cleared on every grow, and must stay so.** Without the fill, unwritten registers hold what a returned call left there and the collector traces it, so finalizers ran later than in GNU R (`bytecode__finalizer_under_wide_window`, `bytecode__register_reuse_under_gc`). Written and unwritten registers cannot be told apart without liveness maps. Growing without a fill (nil only above a high-water mark) was built and is sound on the non-moving heap, and still rejected for this. The clear is about 16 of `grow_registers`' 57 instructions per call. The slot-frame binders rely on it: the header reads as "no environment yet".
- **GNU R's compiled `for` keeps its last values on its node stack**, so finalizer timing after a loop differs between GNU R's JIT modes; oracle cases about collection timing make objects with `lapply` or a call.

## Tier-0 global guards read two bits and the base binding

**Status:** built.

- **Shape.** The heap answers on the write side:
  - `gp::BOUND_ELSEWHERE` on a symbol is set the first time an attached non-symbol frame binds it. `Frame::insert`, `insert_formals` and `from_image` are the only code that adds a binding and are crate-private, so every write route (R-level, namespace loading, the C API) passes through them. Detached frames mark nothing on insert, because GNU R's `.ArgsEnv` and `.GenericArgsEnv` hang off the empty environment and bind every primitive; eager marking there marked 80% of guarded names. `set_enclosure` marks a frame's symbols when it becomes attached. The test is a byte load and a branch before the store.
  - `gp::DETACHED` on an environment means its chain may not reach base; inherited from the enclosure, recomputed by `set_enclosure`, set by `drop_enclosure`.
  - Detaching an environment that already has children sets `Heap::everything_bound_elsewhere` once (persisted in the start image).
  - With neither bit set and no local probe, the guard compares the base binding cell's value (`symbol.cdr().cdr()`, looking through a forced promise); otherwise the per-site stamp check, then the full lookup.
- **Monotonic by design.** The symbol bits are never cleared, so there is no reset path to get wrong; a name once bound outside base keeps the per-site check. `DETACHED` is cleared only on reparenting onto an attached chain. Lexical probes keep their own check.
- **Ceiling.** Guards are 2.8% of `testthat_full` and a passing bit test does about the loads of the stamp check, so at most the slow path (1.0%) could go; measured -0.31% instructions (`run_guard` 26.1M to 13.7M), and up to +0.13% on `records`, where most guarded names are bound outside base. An environment write costs 940 instructions against 934.
- **Known gap.** A cyclic enclosure chain keeps whatever bit it had when the cycle closed (GNU R loops forever on such a lookup).
- Pinned by `fuzz_shadowed_guards_agree_with_the_ast_evaluator` (`test_fuzz.rs`) (removing either mark makes it fail) and `environments__guarded_base_calls_see_rebinding`, which covers every write route, `rlang::env_bind` and the global reparenting.

## Tier-0 superinstructions do not pay

**Status:** built.

- Eight fused pairs covering 38.8% of dispatches moved nothing measurable (+0.08% instructions; the dispatch group fell only from 11.68% to 11.50%; wall +2.86% ± 3.46% on `testthat_full`): tier-0 dispatch is already threaded, so fusing saves one indirect jump while both fast paths run in full. The lever is work per instruction, not dispatch count.
- **Guard dedup is not worth building:** guard is about 19% of dispatches, but a repeat guard with no impure op since is 1.5% of guards (3.9% counting variable reads and writes as pure), the same symbol guarded anywhere earlier in the frame only 27.5%, and no guard is loop-invariant because every loop body makes a call; hence the cheap guard instead ("Tier-0 global guards read two bits and the base binding").
- The design is kept on branch `perf/tier0-superinstructions`: one `superinstructions!` table, a rewrite of only each pair's first word so every pc and deopt point is unchanged, and a fuzz check against the unfused pairs.

## Closures over one body expression share its compiled code

**Status:** built, except "formals and srcref are the whole key", which "Compiled code binds GNU R's inlined base functions at compile time" supersedes.

- **Shape.** `Bodies` (`crates/interp/src/bytecode.rs`) keys tier state by the body expression: call counts across all closures over it and, once compiled, the code, the formals and srcref it compiled with, and a structural hash. A later closure takes that code when formals and srcref are the same objects and the hash matches. The entry holds all four untraced, and `Bodies::clear_dead` (from `RootSource::clear_dead`) drops it when any dies, so code lives exactly as long as a closure that runs it. R6 copies each method into every object, which is why per-closure compilation was ruled out: against it, the objects app went from 53.8 to 42.6 MiB anonymous peak (-21%) and 6,605 compiles to 246. The same body under other formals compiles separately; the first entry stays. A weak reference from body to code kept code alive as long as its source and cost about 1 MiB peak RSS per app.
- **In-place changes.** A body changed through the C API (`SETCAR`) keeps its address, so the structural hash (`shape`: calls and pairlists, symbols by identity, scalar constants by value, everything else by address, as GNU R's `hashexpr1`) is checked when a closure takes shared code. A closure already running the code keeps it, as in GNU R's `JIT_cache`. Rejected: a mutation hook in libr's setters. The walk is iterative and pushes only nested calls, so a long left-nested chain cannot overflow the stack.
- **Against GNU R and a slot on the call.** GNU R's `JIT_cache` is a 1,024-entry table keyed by structural hash and confirmed with `identical`, so it shares code across fresh copies of an expression; `Bodies` shares only one body object's closures but needs no eviction. A slot on the `LANGSXP` was rejected: it grows every pairlist node by a fifth and pairlists dominate the heap.
- **Peak RSS.** Freeing code with its last closure returns buffers to mimalloc pages not yet purged at the peak, so `testthat_full`'s peak median rose 41.56 to 41.99 MiB against the weak-reference shape while the old generation shrank; with `MIMALLOC_PURGE_DELAY=0` it is 40.52 against 40.39 MiB.
- **Code images** write a closure's shared code (`bytecode::compiled_body`) and name base closures held as constants (`tag::BASE_FUNCTION`, `serialize.rs`).
- **Residual.** Lambdas inside `expect_*()` arguments are fresh copies via the quosure path and compile once per test block; GNU R shares them (`backlog.md`, "Open: untyped R against GNU R").
- Pinned by `bytecode__shared_body_environments`, `bytecode__shared_body_r6` and `bytecode__shared_body_mutation`.

## Top-level loops compile per evaluation, without a call frame or a tier

**Status:** built.

- **Why.** A loop the tree walker evaluated (top level, `source()`, `eval()`) ran about 6x slower than GNU R, which compiles every global loop.
- **Shape.** `bytecode::run_loop`, called from `do_for`, `do_while` and `do_repeat`, compiles a loop evaluated in the global environment (only there, as in GNU R) with no local symbols and runs it in an activation without a call frame. Design and the open broadening step: `development/design/top-level-loops.md`.
- **No code cache and no marker on the call.** The code is dropped after the evaluation, as in GNU R. A gp bit on the call would be serialized (`libr/src/serialize.rs` writes a language object's gp bits as levels) and change `digest()` of a quoted loop.
- **No optimizing tier.** `profile::TIER_OFF` keeps tier 1 away, because its frames and resume points assume a call frame; the planner already takes `analysis::Frame::Global { env }` (no locals or slots, variables as tier-0 steps, lookups speculated from the global environment), and wiring the entry is the remaining step.
- `do_for` tries `run_loop` before deciding whether to expand a compact sequence. Pinned by `base__top_level_loops_compile`.

## No baseline tier, and code images hold bytecode only

**Status:** built.

- **Decision.** Two tiers: tier 0 and tier 1 (Cranelift through `crates/ssa`). Code images hold bytecode, profiles and streams only (`interp::code_image`). Ahead-of-time machine code is planned only for strict typed code, where types are facts (`backlog.md`, "Open: ahead-of-time compilation"). Speed for untyped code comes from cheaper op bodies, not from removing dispatch.
- **Why install-time native code went.** Install-time native functions for scalar numeric package closures covered 5 to 11% of closures (rlang 57 of 1,281, cli 112 of 868, testthat 38 of 1,089) and measured neutral: the testthat workload 7.850G against 7.834G instructions, six package loads 1.546G against 1.536G. A top-level loop of 300,000 calls to scalar package functions ran 4.78G against 7.22G, so the lever was coverage (checker types, call arguments, vectors), not faster native code. Untrained and trained tier-0 images measured neutral too, because loops tier up within their first call, and Bun's own AOT runs 7 to 23% behind its JIT on warm servers (1.44x median on untyped idioms). Its upkeep was a position-independent section, a link table, a check manifest and a global check epoch bumped by every binding unlock, `SET_ENVFLAGS`, `SETLEVELS` and debug flag.
- **Why the baseline tier went.** Baseline code can remove at most tier 0's dispatch, measured at 2.66% of `testthat_full` instructions for dispatch proper (code-word reads 1.24%, state loads, stop check, table address, indirect jump) and about 3.1% with next-pc bookkeeping. Forced baseline ran +7.5% instructions (+3.1% for execution alone once its compile of about 150k per body is excluded; the run-time tier measured +6.7%), mostly because each step it hands to tier 0 costs more than tier 0 running the instruction. Its code footprint also added instruction-cache pressure: about 30 bytes of per-body code per instruction, against tier 0's roughly 24 KB of shared handlers; I1 misses on `functional` were 423M JIT off, 462M JIT on and 454M baseline (about 3% of instructions in every mode), and lowering the baseline threshold made it worse (1.075x and 1.091x JIT off's time), so thresholds were not the fix. An opt-in nobody should turn on is maintenance (an assembler per architecture, a step copy of every handler) with no user.
- Tier 1 keeps `bytecode::interp_layout`, `heap::object::header_layout` and `heap::env::layout`. `RY_JIT_THRESHOLDS` is `ENTRIES,BACK_EDGES`. A binding unlock bumps no global epoch.

## Compiled code calls an active binding at every read

**Status:** built.

- **Detection.** `eval::cell_value` counts active-binding calls in `Interp::active_binding_reads`. `jit_load` compares the count around the load; when it moved for a variable not in the running code's volatile set, it sets `feedback::ACTIVE_BINDING` and returns `LOAD_INVALIDATED`, whose deoptimization resumes after the load so the binding is not called twice. A count rather than a cell check keeps `load_variable` unchanged.
- **Compiled shape.** `collect_slots` marks such a slot volatile: every read loads boxed with `forcing` set (the binding runs R code) and leaves the slot unloaded. Feedback is never cleared on recompile, so the first bailout is the last. The volatile set lives in `Compiled`, not in a fourth `jit_load` argument, which made `array_dot`'s inner loop one instruction longer (+1%) under the backtracking allocator.
- Pinned by `active_bindings_are_called_at_every_compiled_read` (`test_tiers.rs`; compiles and one deoptimization allowed only in the first of five calls) and `environments__active_bindings_in_loops`; GNU R's byte code calls a binding more than once per read in a loop, so the case checks at least one fresh value per read.

## Compiled code binds GNU R's inlined base functions at compile time

**Status:** built.

- **Rule.** ry binds exactly where GNU R 4.6.2 at its default level would, on GNU R's schedule (`bytecode::schedule`, mirroring `R_CheckJIT`/`JIT_score`; gp bits `CLOSURE_MAYBE_JIT`/`CLOSURE_NO_JIT` at GNU R's positions) and rules (`base_binding_at_compile`, `compiler_locals` for `findLocals`, `handler_binds` for the `::`, `local` and `browser` exceptions). A function masking `+` mid-loop must not take effect, as in GNU R. Compile-time frames are `Compiler::binding`; a nested function compiles eagerly with its outer one (`nested_body`). Bound code carries `gp::BINDING_CODE`. Top-level loops compile at their start with the loop's own `findLocals` as their one frame. Code GNU R never compiles, and typed code, keep ry's counted schedule and guards. Spec: `r-runtime.md`, "When code compiles and what it binds".
- **Sharing bound code.** `Bodies` stores, for bound code, the closure's top environment and a sorted snapshot of the names its formals and local frames bound (`compile_environment`). Another closure takes it only under GNU R's `jit_env_match` (`binds_like`): same top environment, every local name it binds in the snapshot. Checking only the enclosure would stop R6 methods sharing; checking nothing would let a closure that masks `+` locally run code that bound it. Guarded code serves only guarding closures (`binds_at_compile`); a failed match compiles guarded code (GNU R marks it `NOJIT`).
- **One guard mechanism.** `Compiler::guard` decides bind or guard; a bound site records `UNGUARDED`, so bound code has no guards or cold blocks.
- **Known gaps.** Promise code binds at its call site's first run. Structurally equal global bodies do not share code as in GNU R's cache.
- Pinned by `base__compile_time_binding`.

## Tier 1 leaves compiled code through frame states

**Status:** built, except the list under "Not built" below: frame states in the SSA IR with one shared exit path, closure calls inlined without a frame, exits that rebuild every inlined frame, and lazy arguments passed by name are built.

- **Frame states.** An exit point is a `FrameState` of the SSA IR (`crates/ssa/src/ssa.rs`): `Source` cells in one flat `sources` array and an optional parent (the caller's state at the inlined call). The front end declares states (`Builder::frame_state`, `Builder::exit_block`); `Builder::emit_exits` writes all exit code once, saving each frame's cells into one stack buffer outermost first, ending in one block that calls `jit_deopt_exit`. `Compiled.exits` (`runtime.rs`, `Exit`) says what each cell is; the runtime boxes, flushes dirty slots and writes loop words.
- **Why frame states and not re-running the call.** Re-running is sound only when nothing observable happened, which limited inlining to effect-free callees. With a frame state the callee may write variables, force arguments and allocate, because the interpreter continues mid-body. This is Ř's PIR framestates, V8 Maglev's eager deopt frames and JSC DFG's OSR exits (`prior-art.md`).
- **Inlined calls without a frame.** The planner plans a known callee's bytecode as a nested `Plan` (`analysis.rs`, `Action::Frame`) and `codegen.rs` (`inlined_call`) runs it in place on its own window above the caller's. Returns join one continuation, unboxed when all returns share a scalar type. No environment, promise or frame record is allocated while it stays compiled. `jit_enter_frame` returns null when the call needs its frame from the start, and the call goes to tier 0.
- **Unobserved paths are exits, not refusals.** A callee instruction that could run R code, look at the call or capture its environment compiles to an exit when its feedback is empty (it has not run). Tier 0 records feedback at a `function` expression for this reason alone. Such an instruction never runs through a helper in an inlined call, since the helper would look for a frame that does not exist (`jit__inlined_frames_closures.R`). The planner refuses only what the callee does every call: such an instruction with feedback, a loop, `...`, a call needing a frame, or arguments tier 0's call plan cannot bind without running code (`eval::direct_sources`). `RY_JIT_LOG=1` prints each refusal.
- **A step in a call without a frame signals nothing.** A signal is observable (`Calls:`, the `error` option, handlers and traceback read `sys.calls()`), so while a helper (`jit_step`, `jit_call_builtin`, `jit_element_step`) runs for a frameless call `Interp.signalling` is `Deferred`: a builtin's error returns unsignalled (`eval::builtin_failure`) and `resume_slots` signals it under the rebuilt frames. A warning whose signalling runs R code sets `Abandoned`, and the exit re-executes the instruction in tier 0, safe because these instructions write nothing before they warn. Pinned by `jit__inlined_frames_error_stack.R`.
- **Laziness without promises.** The caller evaluates an argument early only when it is effect free. Whether a formal was forced is observable, so each inlined frame keeps a bit per formal (`Frame.forced`; more than 64 formals gets no frame); a read through an argument computed from caller formals sets the caller's bits too (`Action::Frame.forces`), and copying a formal into the callee window is not a read. `enter_inlined` binds forced formals as forced promises and others as unforced promises over the caller's environment, as tier 0 would. An argument with possible effects is passed unevaluated only when the callee never read that formal (`has_read`), and an argument mentioning a formal under a condition stays unevaluated. Pinned by `jit__inlined_frames_forced_arguments.R`.
- **A formal passed on is passed by name** (`Passing::Variable`): the callee's load (`jit_load_passed`) reads the outermost frame's binding and forces only what runs no R code (`eval::settled_without_code`: a forced promise, a promise of a constant, or of a bound name, followed up to 16 links); otherwise it exits with `reason::UNFORCED`, charged to the root, whose recompile then stops passing unloaded formals (`UNFORCED_FORMALS`). Pinned by `inlined_frames_keep_passed_formals_lazy` and `jit__inlined_frames_lazy_arguments.R`.
- **Evidence and budget.** A callee with all-zero feedback is refused (tier 0 runs small closures without an activation, `eval::run_inlined`, and records nothing); a typed callee is refused (the typed core compiles it); `MAX_FRAME_WORDS` (512) per callee, `FRAME_BUDGET` (4,096) words per compilation, refused callees included, `MAX_INLINE_DEPTH` (3). Without the budget, bodies with many call sites raised peak RSS by megabytes.
- **Exits rebuild frames outermost first** (`runtime.rs`, `materialize`): write back the root, push each inlined frame with `eval::enter`, resume the innermost in tier 0, continue each caller with the result or raise at the call. Free variables load through `jit_load_free`.
- **A frame that keeps failing is given up, by cause.** `frequent` needs at least `MAX_FRAME_HANDOFFS` (16) exits and more than one entry in eight; an absolute count would retire a frame failing once in a thousand. Callee causes mark the callee (`NO_ELISION`, `NEVER_ELIDED`) and discard the root's code. Context causes (calling handler, debugger, object systems off, stack limit) mark nobody; the root gets `NO_FRAMES`. Argument causes are `UNFORCED_FORMALS`. `retire` discards a root once. Pinned by `inlined_frames_outlast_calling_handlers`.
- **Not built.**
  - Entering an inlined call without a helper call (one bumped word for the watched state), after which `Action::Inline` and `effects::effect_free_lookups` go away.
  - Materializing an inlined frame and staying compiled; an exit finishes the whole root call in tier 0.
  - Frames under a calling handler: `jit_enter_frame` declines, so code under `withCallingHandlers`/`suppressWarnings` (every testthat expectation) calls for real. The end shape uses the direct-call path when entry declines, removing `NO_FRAMES`.
  - Callees without feedback, with a loop, `...`, a code default that is read, a call needing its own frame, or recursion.
  - Argument expressions over variables with no feedback (a variable read only in promise code records none).
  - Per-call-site callee specialization; compile-time resolution of free variables under a guard; R-level value nodes with value numbering and scalar replacement.

## An inlined call starts with one leaf helper call

**Status:** built. The single-word check under "Not built" is planned. A calling handler and the activity log are not among the tested state ("Tier 1 on generic code is frames for generic callees, not a third compiler").

- **What a start does is decided by what can change between two starts.** The window, activation record and handle mark cannot: inlined registers sit at a fixed place above the body's, so the body reserves and clears the extent once per entry, writes each frame's `Child` record at its first start (`first_start`, which cannot panic and runs without the guard, because a body that calls its callee once per entry starts every call there), and every helper leaves the handle stack as found. What can change (debug bits, body and environment, a pending frame, the debugger, move tracking, the object-system switch, stack depth) is tested by `jit_enter_frame`, a leaf without a panic guard; the cold `declined` counts the cause.
- **Srcref** is saved and restored by compiled code in the `Child` record.
- **The call marker** (`Interp.assignment_call`, read by `sys.call()` and error/warning calls for a frameless call) is set by `guarded` around a helper whose activation is an inlined frame, since only helper code can read it. A deoptimization gets the root activation, so resumed tier-0 code never runs under it.
- **The window is cleared inline** up to `CLEARED_INLINE` (16) words, else `jit_clear_window`; the collector scans the whole extent, so a stale word would keep an object alive.
- **Slots are marked unbound when a deoptimization builds the frame** (`enter_inlined`). Sibling inlined calls share one window, so a call with fewer slots leaves `NULL` where another keeps locals, and tier 0 reads any non-unbound slot as bound. Filling them at the first start instead showed tier 0 a never-assigned local as `NULL` after a sibling returned. Pinned by `jit__inlined_sibling_frames`.
- **Free variables are found inline** by repeating the compile-time `heap::Probe` walk against frames' filters, slots and the symbol's base/global cell; moved slots, active bindings, unforced promises and live slot-frame environments (`gp::SLOT_FRAME`) go to `jit_load_free`.
- **Frames bind what tier 0 would bind** (`eval::direct_sources`): named and omitted arguments and literal defaults; a code default read by the callee hands the call to tier 0.
- **An entry of optimized code skips the schedule**: `entry_event` returns while the body keeps its code, without recomputing `profile::NEXT`.
- **Not built.** One word bumped by every change of the watched state (a debug bit, `environment<-`, the debugger, `options(expressions=)`) would make the start one compare. A missed site would be a wrong result, so it waits for a workload where the call matters.
- `RY_JIT_CODE=<directory>` writes each compiled body's machine code to a file named after its address, to line a callgrind `--dump-instr=yes` profile up with a disassembly.

## A body compiles only when its native work covers what compiled code costs

**Status:** built; the constants, the calls made direct and the entry are superseded by "The cost model counts frames, and free steps count nothing", and a loop is also asked "A loop compiles when its backward jumps repay the body".

- **Why.** Entering compiled code from the dispatch loop (`count_entry`, `entry_event`, `jit::enter`, `compiled_for`, `jit::run`, register growth, result boxing) costs about 570 instructions more than starting the body in tier 0, and each `jit_step` about 300 more than tier 0 running the instruction. A one-line hot closure therefore ran slower compiled (`x^2`: -325 instructions per call; seven operations over two locals: +575), and several application programs were slower with the JIT on. After the model no application program runs more than 0.55% more instructions with the JIT on.
- **Model.** `analysis.rs`, `Work` counts native instructions, calls removed or made direct, and steps. `jit.rs`, `saving` values them at `SAVING_PER_NATIVE` (130), `SAVING_PER_CALL` (1000) and minus `STEP_COST` (300); `saving_per_call` subtracts `ENTRY_COST` (570), so a step-free body breaks even at five native instructions or one call. `repays` multiplies by expected calls. `Plan::work` (`BodyWork`) counts the whole body, the part outside loops, and the loops, where depth `d` weighs `LOOP_WEIGHT` (8) to the `d - 1`, so one stepped instruction in an outer loop does not refuse a native inner loop.

  | body | refused when | refusal | final |
  |---|---|---|---|
  | no loop, hot by entries | `saving_per_call` of the whole body is zero | `SMALL` | yes |
  | loops | `saving` of the weighted loop work is zero | `STEPPING_LOOP` | yes |
  | loops, entered at the top | the work outside loops has steps and its `saving` is zero | `MOSTLY_TIER0` | no: a hot loop still compiles |

  `SMALL` is also decided before planning from `Shape::work`. `RY_JIT=force` skips the decision.
- **A refused body stops recording**: `back_edge` sets a negative-state body's back-edge counter to a value that reaches no threshold, or its loops reopened the recording window with no compile to close it.
- **A cold body with an empty window is refused once**: an all-zero feedback vector after a window means a native prologue finishes every call, so another window cannot differ (else up to `MAX_COLD_REFUSALS`).
- **A refused body keeps its feedback**, so a compiled caller still inlines it as a frame, which is where a one-line closure pays.
- **Allocator by size.** Loop-free bodies up to `SINGLE_PASS_FROM` (2000) SSA instructions use the backtracking allocator, because single-pass spilled every value (compiled `x^2` was 100 instructions); above it single-pass keeps compiles cheap.
- **Rejected: trimming the entry chain first.** Even a 200-instruction entry leaves a one-operation body even with tier 0, so the model is needed either way.
- **A call handed to tier 0 that never returned a value counts as no work** (`Plan::work` reads the feedback): a `stop()` under a check costs nothing until it runs and charges no `STEP_COST` against a small body.
- **Entry looks up the code before the counters** (`specialized`, `jit.rs`): an entry whose enclosure the compiled code was made or last checked for runs it without `compiled_for`'s state cases.
- **The constants hold.** Admitting every body (`ENTRY_COST` and `STEP_COST` zero) ran 4.9% more instructions on the finbench hot suite: a compiled thin wrapper costs more than tier 0 (a loop calling two builtins, 2,395 instructions per iteration compiled against 1,627: about 400 for entry and 80 more per builtin call).
- **Not built.** A lean entry (code pointer in the code header, window pre-sized, unboxed scalar return, about 400 instructions to save); cheaper steps. Recalibrate the constants when either lands (backlog, "JIT on against JIT off").
- Pinned by `a_body_that_could_run_nothing_natively_never_records`, `a_hot_body_that_runs_rarely_waits_to_compile`, `fully_typed_closure_compiles_at_its_first_call`. No test pins the constants; they are calibrated on the application programs and kernels (`array_sum` is the kernel a wrong loop weight refuses; `vasicek`'s loop steps eleven instructions, is refused, and runs as fast in tier 0).

## A compiled return writes locals back only when something can still read the frame

**Status:** built.

- **Who reads a frame after return.** Tier 0 finishes a frame (`finish_frame`) only when it has an environment, registered exit code, or a pending environment (what a promise or closure over a slot frame holds until reification). Otherwise the slot frame is dropped with its registers.
- **Why.** Flushing every dirty slot at return cost a body with two assigned locals about 450 instructions per call for values nothing read.
- **Shape.** At the body's own `Return`, `finish_slots` (`codegen.rs`) calls the leaf `jit_frame_read_later` when a slot may be dirty; if nothing reads the frame, the return only releases the counted references dirty boxed values hold.
- **Why a run-time test.** Whether exit code is registered or the frame captured depends on callees (`do.call(on.exit, ..., envir = parent.frame())`, `environment()` in a default), which the planner cannot see. Four loads against a flush per local.
- Pinned by `jit__locals_read_after_return`.

## A call compiled code cannot inline enters the callee in tier 0's dispatch loop

**Status:** built.

- **Shape.** A stepped closure call enters the callee in place (`enter_in_place`, `bytecode.rs`) in the loop the step started, with a `Resume` record on `State::resumes`; `return_fast` leaves that record to `resume_caller`, so a tier-0 callee never runs on into the compiled caller's instructions. Compiled code entered this way grows the native stack as a closure call does, or deep recursion through compiled bodies overflows (`recursion__and_recall` on `ry-jit`).
- **A formal passed as a subscript is the `Argument` instruction**: the empty argument for a missing formal, else `GetVar`. It records `feedback::MISSING`, and tier 1 compiles it as a load unless that bit is set, handing off to tier 0 when a compiled load meets a missing formal.
- **A local a join needs as an object is forgotten, not written back**, because writing it back replaced a formal's promise and cleared its missing bit, changing `substitute(x)` and `missing(x)`.
- **A stepped callee returns as tier 0's own return does** (`leave_fast::<_, STEPPED>`, `bytecode.rs`): the result goes into the caller's register and the activation stops with the next instruction, without `eval::leave`. A compiled body that tier 0 entered in place leaves the same way.
- **A positional `CallBuiltin` step runs without an activation** (`call_builtin_step`): the unary or direct entry reads only the registers, so building the activation cost as much as the call.
- **A `GetFun` of a name the frame may bind is not speculated on** (`bound_locally`, `analysis.rs`): a frame binding a function there (an argument named like a global) failed the speculation and invalidated the code at every call (`a_loop_calling_an_argument_named_like_a_global_compiles_once`).
- Pinned by `jit__stepped_calls_reach_the_caller` and `jit__inlined_frames_defaults`.

## A native recursive function's depth budget shrinks with the interpreter depth

**Status:** built. Bounding native recursion by the native stack is not built.

- **Shape.** The budget (`runtime.rs`, `jit_depth_budget`) is the smaller of the frames left under the `expressions` limit and `RETRY_NESTING` (8,000,000) divided by the interpreter depth. When tier 0 reruns a failed native call, each recursive call retries natively one frame deeper; with the plain limit budget, a recursion past a raised limit was quadratic (minutes against 1.6 s in tier 0). The cap bounds the total retry work. At the default limit of 5000 the limit is always the smaller bound.
- **Rejected: never retrying after a failure**: shallower calls of the same function still succeed natively and are the common case.
- **Not built.** The native stack does not bound native recursion: the budget does not know a native frame's size, and only a 128 KiB red zone is guaranteed below the stack check.
- Pinned by `a_recursion_past_a_raised_depth_limit_fails_as_in_tier_zero`.

## Parked: cheaper global bindings in top-level loops (tier 0)

**Status:** planned, parked on its branch until a workload clears the bar.

- Branch `perf/global-loop-tier0`: global reads and writes through the site's cached cell, validated by `Heap::lookup_stamp`; every fast path reads the cell's flags because `lockBinding` and `makeActiveBinding` move no stamp; `x <- left op right` on an unshared scalar global overwrites in place. Its oracle case `base__global_loop_bindings` lives on the branch. `s <- s + i` falls from 856 to 495 instructions (GNU R 249).
- **Why parked.** Top-level loops may be rare in the workloads, and compile cost and RSS are unmeasured. The bar is 3% on a named workload (2% on testthat).

## A compiled replacement probes the target before any subscript runs

**Status:** built.

- **Rule.** A fast path may evaluate an argument eagerly only once it knows a builtin receives it. GNU R reads the target's class before evaluating arguments, and a forced promise hides an inversion from `substitute` (data.table's `[<-` evaluates `i` in the table's scope). `Recall` (forwards its own promises) and the name argument of `$`, `$<-`, `@`, `@<-` (a promise of a string) follow the same rule.
- **Shape.** When a subscript is not self-evaluating, compiled code reads the target right after the value and branches before any subscript runs: `Op::BranchIfObject` for `[<-`/`[[<-` (base's setter pinned by a guard), `Op::GetFun` plus `Op::BranchUnlessBuiltin` for any other `f<-`. The taken branch runs `Op::ComplexAssign` cold, with subscripts as code; tier 1 treats it as an opaque step and speculates the branch away until taken.
- **Eager exceptions.** A constant subscript, since a promise of a constant cannot be told from the constant. In `[<-`, `[[<-` and the matrix form, a local the compiler knows is bound (`Compiler::assigned`: an enclosing `for` variable or an earlier `name <- value` in an enclosing block), because reading it runs nothing and cannot fail and the fallback hands a method the forced promise of its code. Any local that might be unbound still probes: `z[late] <- 1` before `late <- 1` must reach a method that never forces `late`. A body that calls `rm()` or `delayedAssign()` records no bound locals. Known gap: a callee that unbinds the local in its caller's frame between assignment and subscript.
- **Cost.** A computed subscript pays one target read and one branch: a local `x[i %% 64L + 1L] <- 9` loop costs 756 instructions per iteration against 714 in tier 0, and the `bench-mechanisms` computed-subscript row 1,579 against 1,317.
- Pinned by `replacement__index_promises_repeated_calls`, `replacement__index_promises_shapes`, `replacement__index_promises_local_subscripts`, `promises__recall_and_accessor_names`, and the replacement block of every `oracle::fuzz::program`.

## A call native code evaluates repeatedly compiles at its third evaluation

**Status:** built.

- **What.** libr's `eval` hands a language object to `bytecode::run_call`, which counts evaluations per call object and at the third compiles it with `compile_with_locals(call, pseudo_closure, None, None)` and runs it with `run_promise(code, env)`. data.table evaluates a grouped `j` once per group from C, so `j` runs as tier 0 instead of on the tree walker. Other kinds of object keep the walker. Closure bodies compile at their second call (`COMPILE_AFTER_CALLS`), and a native call object waits one evaluation longer (`COMPILE_AFTER_EVALUATIONS`) because data.table substitutes `j` afresh for every `[` call, so a call evaluated twice is often a one-group query.
- **One code object for every environment.** The pseudo closure has no formals and its environment is only a compile-time hint: with `binding: None` every choice the compiler makes from the enclosure (`base_primitive`, `enclosing_function`, `library_function`, an inlined internal) is guarded with a cold fallback, and `nested_body` refuses without a binding. Call sites inside the code get plain promises, since the code runs with `function: nil`.
- **Eligibility.** `compiles_as_promise` (no loop, `break`, `next`, `return` or nested `function`), an environment that is not a slot frame, and no active debugger. An ineligible call is marked never and walks from then on.
- **In-place changes.** Native code may change the call between evaluations (`SETCADR`, writes through `INTEGER()`, an argument cell retyped by `LCONS`). The table records `call_shape` at compile time: the identity, kind and tag of every cell, the identity of every leaf, and a one-element leaf's attributes and value (for a string, its `CHARSXP`, because the compiler bakes a one-string constant's text into code). It hashes identity rather than structure, as `hash_shape` does, because the code holds the call's own cells and leaves for `sys.call()`, `match.call()` and condition calls, so a structurally equal replacement must recompile too. The compiler references multi-element constants instead of baking them, so an in-place write to one needs no recompile. A mismatch recompiles from the live object, and after two recompiles (`MAX_RECOMPILES`) the call is marked never.
- **Cell normalization has one owner.** `bytecode::normalize_argument_cells` retypes `LANGSXP` argument cells before the walker runs or the compiler reads the call, and runs only on the uncompiled path or after a shape mismatch, so a compiled call with a matching shape pays nothing.
- **Stack.** Compiled code runs the closures a call invokes without native recursion, so a call that evaluates itself through native code (`eval_bare(q, env)` inside `q`) would use native stack with no closure entry to ask for more. `run_call` therefore runs compiled code through `eval::on_large_stack`, and the frame limit ends a runaway recursion with R's error.
- **Lifetime.** A compiled entry holds its code in an ephemeron (`Heap::alloc_weak_ref` with the call as key, `Heap::set_weak_value` on recompile), so the code lives exactly as long as the call and a dead call's entry is freed in `clear_dead`, which runs after `process_weak_refs` and before the sweep can reuse its address. Counting entries hold nothing, and their number is capped at `MAX_TRACKED_BODIES`.
- **Cost.** On 200,000 rows in 40,000 groups, `DT[, spread(v), by = g]` takes about 35 ms compiled and 48 on the tree walker (GNU R 60), and `DT[, v[1L] + w[.N], by = g]` about 36 ms and 48 (GNU R 39).
- Pinned by the oracle cases `native_eval__datatable_grouped_j`, `native_eval__datatable_frames` and `native_eval__purrr_rlang`, and by `fuzz_native_code_reevaluates_a_changing_call` in `crates/libr/tests/test_fuzz_native.rs`, which changes a call in place between evaluations and compares each result with the walker's on a fresh duplicate.

## A handler compiled code steps outside `scoped` leaves no handle behind

**Status:** built.

- **The rule.** Compiled code runs most helpers inside `scoped` (`crates/jit/src/runtime.rs`), which marks the heap's handle stack, runs the helper and releases to the mark. `jit_element_step` tries `bytecode::fast_step` first, outside `scoped`, because a fast handler allocates nothing that needs a root. A fast handler must therefore return with the handle stack at the height it found.
- **Why it matters.** A stepped call that enters its callee in place returns through `leave_fast` or `resume_caller`, which release to the compiled caller's `machine.mark`. A handle a fast handler left above that mark is released there, under the mark `jit_step` took for the call, and the check in `scoped` aborts ("a helper released handles compiled code holds").
- **The case that showed it.** Reading one element of a deferred string (`as.character(<numbers>)`, an ALTREP vector) converts that element through libr's `deferred_storage`, which roots the vector. `read_altrep_element_call` in `bytecode.rs` takes a handle mark before the conversion and releases to it after, since the element stays reachable from the vector. Under the forced JIT, `x[2L, , drop = FALSE]` on a data frame with such a column aborted at the second call.
- **Guard.** `jit_element_step` asserts in debug builds that `fast_step` left the handle stack where it was, so the oracle suite under `ry-jit` names a handler that breaks the rule.
- Pinned by the oracle case `jit__deferred_string_read_before_call`. Found by the metamorphic fuzz kind.

## Tier 1 on generic code is frames for generic callees, not a third compiler

**Status:** built (stage 1 of `development/design/generic-tier.md`); native forms on boxed values, stretches and framed calls from frameless calls are planned there.

- **Problem.** A test user's application test suite ran no compiled code, and forcing compilation made it slower. Its bodies call builtins and small closures on lists, objects and strings, so the numeric speculation has nothing to win, and the measured costs sat in three places: the call compiled code made itself, inlining rules that refused generic callees, and the cost of what compiled code hands back.
- **Rejected: a baseline compiler.** Dispatch and operand decoding are a small share of a generic body's instructions, and entering compiled code costs more than they save on a short body (measurements in the design note; same verdict as "No baseline tier, and code images hold bytecode only").
- **Previous shape and what was weak.** A call the planner could not inline became a direct call (`Action::Call`, `jit_call`): compiled code evaluated the arguments and pushed the callee's frame through the interpreter's general entry. It cost more than tier 0's in-place call while the cost model credited it as a saved call, so wrappers compiled and ran slower. An inlined frame was refused for a callee without feedback and for a constant argument that is not a number, and declined at run time while a calling handler was established or the activity log was on, so a test suite, which runs every expectation under a calling handler, inlined nothing.
- **Target shape.** A call compiles as an inlined frame, a native call, or tier 0's own call instruction entered in place; compiled code has no call path of its own. A callee without feedback is planned from its instructions, with boxed values and its unobserved instructions as exits. Any constant argument is passed as the object the call holds (`ArgumentSource::Literal`), which is what tier 0 binds.
- **Why a calling handler does not need the frame.** An instruction run for a frameless call signals nothing (`Signalling::Deferred`, `step_unsignalled`): a plain error comes back unsignalled, and a condition signalled where it is raised (a warning, or an error with a class of its own such as a subscript out of bounds) abandons the instruction (`Signalling::Abandoned`), and the exit repeats it in tier 0 under rebuilt frames. A handler therefore runs only after an exit. `runs_no_r_code` tests the established handler only under `Signalling::Immediate`.
- **Every signal entry point asks `conditions::waits_for_frame` first** (`signal_error`, `signal_condition`, `warning_call`). A classed error that skipped the test ran its calling handler on a stack without the inlined callee. A new entry point that can reach a handler must ask it too.
- **Why the activity log does not need it.** A profile that changes which path runs measures another program. An inlined callee's time counts as compiled code in its caller's row.
- **Rebuilding a frame never runs the call whole** (`eval::Entering`). `eval::enter` decided alone whether a call runs whole on the general path (the activity log, object systems off, the namespace loader, a debugged closure), which is right for a call that starts and wrong for an inlined call that is half run: under the profile summary the rebuild ran the callee from its first instruction and aborted. The caller says where the call stands: `Entering::Start` keeps that decision and `Entering::Midway` (`enter_inlined`, `crates/jit/src/runtime.rs`) pushes the frame whatever the session counts or debugs, because the call's start (`start`) already refused what the general path would have. Pinned by the scenario test `inlined_frames_rebuild_under_the_profile_summary`.
- **A callee with a native entry keeps tier 0's call** (`gp::NATIVE_ENTRY`, refused in `plan_callee`). Tier 0 runs such a base closure in native code without its body and without a frame, which costs less than any inlined frame of the body; with the feedback requirement gone the planner inlined them. Pinned by `native_entries_hold_in_slot_frames`.
- **Expected impact.** Correctness: unchanged contract, pinned by the oracle cases below on all engines. Performance: call-shaped patterns gain with the JIT on, most under a handler; the workload's compiled share moves little until frames apply to callees with `...`, tier-0-only instructions and framed calls (stage 4).
- Pinned by `inlined_frames_run_under_calling_handlers` (`test_tiers.rs`) and the oracle cases `jit__inlined_frames_under_handlers` (which shows a classed error's handler the callee's frame) and `jit__inlined_frames_constant_arguments`.

## A stepped instruction runs on a kept view of the activation

**Status:** built.

- **Shape.** `bytecode::Stepper` holds tier 0's `Activation` view for one compiled activation (the root's, and one per inlined call's `Child`). It is built at the first step and reused; `step_one`, `quiet_step` and `call_builtin_step` take it. A step ends as `StepEnd` (`Next`, `Returned`, `Failed`); a failure's unwind stays in the view until `Stepper::failure` takes it.
- **Why.** Building the view per step cost about as much as a simple instruction, and returning `R<Option<Sexp>>` moved a wide result through memory for the common end.
- **What invalidates the view.** A stepped closure call that enters its callee in place points the view at the callee's code, so `step_call` clears `built`. Nothing else moves what the view holds while the activation runs compiled: the register file does not move, and the frame header is read through its address.
- **A closure call is recognized by its opcode byte** in `step_one`, because tier 0 quickens no closure call's word. A quickened closure call would need `op_of` there.
- **Every failed step's unwind is taken**, by `jit_step` (parked, or dropped when the instruction gave up before a signal) or by `quiet_step`, so the stepper has no drop.

## Compiled code that keeps leaving is retired

**Status:** built.

- **Rule.** An exit of the body's own frame for `reason::PENDING` or `reason::HANDOFF`, and an inlined call's failed materialization, count in `Compiled::leaves` (`runtime::count_leave`). Past `MAX_FRAME_HANDOFFS`, when the leaves are `frequent` among the entries since `Compiled::leaves_since`, the code is retired and the body's tier state refused, so it runs in tier 0 for the session. When they are not frequent the count and its entry mark start again, so only leaves close together retire the code: counted from the compile, sixteen leaves spread over a long run would retire code that gains on nearly every entry.
- **Retirement is final.** A recompile of the same body from the same feedback leaves at the same instructions, and the model has no evidence for a second attempt. A callee whose inlined frame keeps handing off is a different case with a different remedy: `count_handoff` marks the callee and the caller recompiles with the call as a step.
- **Why.** Such an exit is not a failed speculation, so nothing recompiled or discarded the code, and each one costs more than several entries save. `tryCatchOne` leaves whenever its expression signals and `is()` returns through `return()` inside `switch`; the two took nearly every exit of the application workload.
- **Loops are exempt** (`compiled.loops != 0`): a loop entered by on-stack replacement runs compiled without an entry being counted, so entries do not measure how much the code ran. A loop body that leaves on every call therefore keeps its code; counting its leaves against the loop's iterations is open (`backlog.md`).
- **Counted at the exit sites, not in `jit_deopt`**, which an inlined call's hand-off also passes through with its own counter (`count_handoff`).
- Pinned by the oracle case `jit__code_that_keeps_leaving` and by `a_body_that_leaves_on_every_call_is_retired` (`test_tiers.rs`), which bounds the exits of a body whose inlined callee fails and of one that fails itself.

## The cost model counts frames, and free steps count nothing

**Status:** built. Supersedes the constants and the direct-call credit of "A body compiles only when its native work covers what compiled code costs".

- **Constants** (`jit.rs`; the measurements are the table "What compiled code costs against tier 0" in `development/design/generic-tier.md`): `ENTRY_COST` and `STEP_COST` fell with the leaner entry and the kept view, and `SAVING_PER_FRAME` is new. `Work` counts `frames` apart from `calls`, and an inlined frame adds its callee's own work (`Plan::work` reads the callee's feedback).
- **A read from the frame is charged at the entry.** `SAVING_PER_NATIVE` assumes operands in machine registers, and each call of a loop-free body reads its arguments from the frame once, the first read of a promise through the runtime. Without the charge, and with an entry cost read from a body without arguments, a body of four additions on five arguments compiled and ran more instructions than in tier 0. `Work.loads` counts the reads outside loops (`Action::GetVar` with a load), `saving_per_call` subtracts `LOAD_COST` for each, and `ENTRY_COST` and `SAVING_PER_FRAME` follow from two bodies that differ only in native instructions (the design note's table). A read inside a loop is not counted: it reaches the frame on the first pass only.
- **Steps that cost nothing count nothing**: a call that never ran while tier 0 recorded and the `GetFun` that feeds it, an element instruction with a tier-0 fast handler, and a builtin call that runs no R code. Counting them refused bodies that gain (two guards with `stop()` around native work).
- **Only paths the body has taken count** (`Plan::ran`). The model summed every planned instruction, so a body that returns early on every call (`match.fun` given a function) compiled for native work and a frame on a path it never runs, then paid the entry on each call. `Plan::ran` follows the directions each `JumpUnless` recorded (`feedback::BRANCH_TAKEN`, `BRANCH_NOT_TAKEN`) from the entry; a branch that recorded neither ends the path, which is the stretch every call of an unrecorded body runs. The feedback stays the one source of truth: the bitset is derived per query, nothing is stored.
- **`unpaid` gives both refusals.** A saving per call of zero or less over the whole horizon is `SMALL` and final; a positive saving that the expected calls do not cover is `RARE` and asked again. A `RARE` verdict for a body that could never pay made it record and ask all session.
- **A body with an inlined frame is planned before the model is asked**, because only the plan knows what the callees save; the pre-plan filter (`Shape::work`) applies to bodies whose shape has no closure call. Refusing single-frame wrappers before planning lost the bodies whose frames nest.
- **Forced mode is not at tier 0's cost per body**: a body of steps runs more instructions compiled than in tier 0, which stretches fix (stage 3 of the design note).

## Portable code with no lookup from the enclosure is proven for every enclosure

**Status:** built.

- **Previous source of truth:** `Compiled.checked`, the weak reference to the enclosure the epoch and stamps were last proven for, decided for all portable code whether an entry is the lean one (`specialized`, `jit.rs`).
- **What was weak:** a closure made by each call of its parent has a new enclosure on every call. Its entries missed the lean path, took `compile_at_entry` and `compiled_for`, and walked `prove` as a sibling, although code without a speculation that starts at the enclosure (a body of arithmetic on its arguments) has nothing an enclosure can change.
- **Target shape:** `Compiled.tied` says whether a speculation starts at the enclosure (`rooted`). Code that is not tied holds no `checked` reference and `proven_for` (`runtime.rs`) is true for every enclosure, so its epoch and stamps are shared by all of them. Tied code keeps the checked enclosure, and `specialized` also takes a sibling of it, whose own frame `prove` checks.
- **Impact:** an entry through a fresh closure costs what any other entry costs, and no weak reference is allocated for code that needs none.

## A loop compiles when its backward jumps repay the body

**Status:** built.

- **Previous shape and what was weak.** A body's loop compiled at the back-edge threshold whenever the weighted loop work saved anything (`saving(work.looped) > 0`), after a wait by size (`SIZE_WAIT_WORDS`). With free steps counting nothing and only taken paths counted, large base R bodies with a loop of steps passed that test for one native instruction per pass (`rbind.data.frame`, `apply`), and the compile of such a body is a visible pause in whichever call crosses the counter. Entries had a repay test (`unpaid`) and loops had none.
- **Target shape** (`jit.rs`, `unpaid_loop`). At a loop's request on a body that never compiled, the saving of one pass through that loop, times the backward jumps expected, must cover `COMPILE_FIXED + COMPILE_PER_WORD * words`. Expected is the larger of `HORIZON_EDGES` and the backward jumps taken. A refusal (`UNPAID_LOOP`) is not final: the back-edge counter restarts so the loop asks again once the taken jumps would cover the compile and have at least doubled, which keeps the attempts few on a body whose plan keeps changing. A pass that saves nothing waits for the doubling alone (another loop of the body may be the one that pays).
- **One pass, without nested loops** (`BodyWork.hot`, `Plan::work`). The body has one back-edge counter for all its loops, so an inner loop's jumps are already in the count; weighting the hot loop's work by nesting (`LOOP_WEIGHT`) counted the inner loop twice and admitted `rbind.data.frame`.
- **Taken jumps are `EDGES_BEFORE + BACK_EDGES`** (`bytecode.rs`, `profile`). `BACK_EDGES` is the counter tier 0's fast `Jump` handler compares with the threshold and the tier restarts it to set a wait, so it cannot also be the total. `runtime::restart_back_edges` is the one writer on the tier's side and keeps the sum; the interpreter's cold `back_edge` folds the count before it asks. The fast handler is unchanged.
- **Why a fixed horizon and not the jumps taken alone.** Compiling only when the taken jumps already cover the compile (ski rental) bounds the loss at twice the optimum, but delays every small numeric loop from the threshold to about twice it, which the kernels pay. A multiple of the taken jumps admitted the large bodies at their first request. With the horizon, a body of a few hundred words compiles at the threshold for one native instruction per pass, and a body that fails it waits until tier 0 has run the passes the compile costs, so the compile costs at most what tier 0 has lost by then.
- **A looping body that turns hot by its calls** compiles when its mean pass (the loop work at weight one, over the number of loops) covers the compile within `HORIZON_EDGES`: such a body compiles at the back-edge threshold anyway, and a body trained into a code image arrives hot by entries with no backward jumps counted in the session (scenario `install-code-image`). Otherwise it is asked `unpaid` with the work outside its loops plus the loops' weighted work. `SMALL` on such a body stops its entries from asking and leaves its loops free to compile (`LOOPS_ONLY`), as `MOSTLY_TIER0` does; a final refusal would have closed the loops too.
- **Expected impact.** Correctness: none, the rule only moves when a body compiles. Performance: no compile of a large stepping body inside a short run; numeric loops compile where they did. Incremental analysis: untouched.
- Pinned by `a_loop_compiles_once_its_backward_jumps_repay_the_body` (`test_tiers.rs`).


## A back edge is counted in tier 0's fast jump handler

**Status:** built.

- **Previous shape and what was weak.** With a tier installed, every backward jump left the fast `Jump` handler for the generic one, which sets up a frame, polls for an interrupt, counts the edge and releases handles. A loop the tier refuses (a `for` over `names(x)`) paid that on every iteration, so it ran more instructions with the JIT on than off: callgrind showed about 60 per back edge against about 6.
- **Target shape.** The fast handler's `jump_fast` counts the edge itself (`profile::BACK_EDGES`) and falls to the generic handler only when an interrupt is requested or the count reaches one `back_edge` acts on, in which case it leaves the count unwritten for the generic handler to count again. It reads the profile as the plain vector it is (`pool_constant`), because an accessor with an ALTREP test can call, and a fast handler with a call on any path sets up a frame on every path: a first version that used the checked accessors made every forward jump pay for one. The count costs 17 instructions per back edge.
- **Impact.** Loops in bodies the tier refuses or has not reached run at close to their cost without a tier. Handles are released at the next generic handler, as they are without a tier.
