# Decisions: JIT and SSA

Tier 0, the Cranelift tier, the shared SSA IR, deoptimization, and what compiles when.

## The JIT tier

**Status:** built, except the per-instruction plan as the compiler's input and the unconditional first-call compile, which "One SSA IR and one install-time pipeline for typed and untyped compiled code" and "A typed closure compiles at its first call only when that call can run long" supersede.

`crates/jit` compiles hot tier-0 bytecode with Cranelift. `development/design/r-runtime.md`
(section 5, "What tier 1 does today") is the contract for what it does; this records why.

- **Compile thresholds are 3000 entries and 10,000 backward jumps** (`crates/jit/src/jit.rs`).
  At 10 and 500, a package load compiled the methods package's bodies that each `setClass` runs a
  few dozen times, and compiling took half of a large package's S4 definitions (callgrind) and
  about 200 ms of its cold `ry run`. finbench test slices then picked 3000 over 1000
  (callgrind instructions against `RY_JIT=off`): on 2 files, entries 300 cost +2.9%, 1000 +1.4%,
  3000 +0.6% and 10,000 +1.0%; on 21 files, 1000 cost +2.7% and 3000 +1.5%, and raising back
  edges to 30,000 changed nothing. The
  remaining overhead is per-call tier bookkeeping (`enter`, `compiled_for`, `valid`) and
  Cranelift, not deoptimization. Hot code crosses 3000 calls within milliseconds, so the
  benchmarks time the same. Lower them only together with a cheaper first tier or background
  compilation.
- **A fully typed closure compiles at its first call** (`jit.rs`, `fully_typed` and
  `first_call`). Typed code must never run slower than the same code untyped, and before this a
  typed body paid the argument checks and still waited for 1000 entries. The declared types of the
  formals and of a called closure's return seed the analysis wherever feedback is empty
  (`analysis.rs`, `load_ty`), so the compile needs no warm-up. The guards stay because inline
  types are not sound in default mode; a failed guard deoptimizes as for any feedback. A scalar
  type (`double`) and a vector type (`double[]`) differ in the runtime mask (`annotations.rs`,
  `SCALAR`), so only scalars are seeded as unboxed. A refusal because an operand is still unknown
  (`COLD`) or because nothing would run natively does not mark the body as refused: the threshold
  path may still compile it once real feedback exists. Measured: `fib` typed 0.013 s before,
  0.007 s after, equal to untyped; `closures` typed 0.37 s against 0.42 s untyped.
- **The tier interface is a hook table, not a dependency.** `interp` exposes `bytecode::Tier`
  (`enter`, `back_edge`, the back-edge threshold) and `set_tier`, plus `Machine`, `TierExit`,
  `step_one` and `load_variable`. `jit` depends on `interp`, never the reverse, and `libr`
  installs it after the interpreter. Tier 0 changes from other work land without touching the JIT
  as long as these stay stable.
- **The typed plan is the IR.** One action plus one abstract state per bytecode instruction, with
  Cranelift `Variable`s building the SSA form. A PIR-style R-level IR was deferred because the
  first goal, unboxed loops, needs no rewriting across instructions; fusion, bounds-check
  elimination and inlining will need it.
- **Compiled code shares tier 0's register window.** Boxed values live in the window or in slot
  root cells after it, so the collector sees them without stack maps, and deoptimization at any
  instruction is a matter of boxing the unboxed live values in place and returning
  `TierExit::Resume` with the pc.
- **Deoptimization returns to tier 0's loop instead of calling into it.** An earlier shape ran
  the rest of the body in tier 0 from inside the compiled frame, so a loop that deoptimized and
  re-entered through on-stack replacement on every iteration nested native frames until the stack
  overflowed. Returning the resume pc and any pending unwind keeps the native stack flat.
- **Rare slow paths hand off to tier 0.** Integer overflow, an out-of-range element read and
  `sqrt` or `log` of a negative number deoptimize to the instruction (reason `HANDOFF`, which does
  not invalidate) instead of calling a helper, because they warn, and a calling handler may read
  or rebind any variable the loop keeps in a register. Tier 0 runs them with the frame written
  back, and the next backward jump re-enters compiled code.
- **Plain arithmetic and pure builtins check for calling handlers.** They can warn, so when a
  handler is established they write back and forget the frame like a generic call. Without a
  handler a warning is only deferred, so the common case pays one helper call.
- **Variables live in slots with a runtime status** (unloaded, local, enclosing, dirty). The
  environment stays the truth for everything outside the body, so it is written back before any
  operation that can observe it and forgotten after any that can change it. This keeps reflection
  through callees (`parent.frame()`, `sys.function`) correct without proving it absent.
- **The first read of a formal whose default is an expression writes back and forgets.** The
  default is evaluated in the function's own frame and can read or assign the body's variables
  (`[.data.frame`'s `drop` reads `cols`). Supplied arguments evaluate in the caller's frame and
  need no write-back.
- **A refused body is not retried, except for `COLD`.** A `for` loop over a value with no type
  feedback yet is a temporary refusal; every other refusal and a fifth compile mark the body as
  never to compile.
- **Every hot body compiles, loop or not, unless it would run nothing natively.** Loop-only
  compilation was a stopgap while entering compiled code meant forcing every argument through a
  promise helper, which made `function(a, b) a + b` slower compiled than in tier 0. Direct calls
  removed that cost, so the loop-only rule went with it. A body whose plan has no loop and no
  inlined call, direct call, arithmetic, comparison, intrinsic, element assignment or `&&`/`||`
  (`Plan::runs_natively`; setting a srcref does not count, or every body parsed with
  `keep.source` would qualify) would only call the same helpers tier 0 does, so it is refused
  (`analysis::NOTHING_NATIVE`, permanent) instead of costing a compile; most base R wrappers are
  such bodies. A body none of whose instructions could plan as native work under any profile
  (`analysis::shape`) is refused before it records or plans, as
  `analysis::NEVER_NATIVE` (see "Bodies that could run nothing natively never record"). `RY_JIT=force` still compiles them, so the tier tests cover them. Measured with
  callgrind over the why-ry programs (whole process): about 170M instructions fewer per program,
  on top of the higher entry threshold's 280M.
- **Function speculation is validated by the binding epoch, then by per-symbol stamps.**
  Compiling marks each speculated symbol and binding cell `WATCHED`, and the heap bumps one global
  epoch, and the symbol's lookup stamp, when a watched cell is assigned or a watched symbol is
  defined in any function frame (which could shadow the speculated binding). An unchanged epoch
  proves every speculation with one compare. The epoch also moves on every binding outside a
  function frame, which a test suite does constantly, so `valid` (`crates/jit/src/runtime.rs`)
  then compares each speculated symbol's `Heap::lookup_stamp` with the one recorded at its last
  check; re-walking every lookup on each epoch move cost a finbench test slice more than the
  compiled code saved. Only when a stamp moved is each speculation re-resolved from the
  environment its lookup started in, which for a function looked up inside an inlined or directly
  called closure is that closure's environment, not the compiled body's. A lookup that passes a
  non-function binding is not speculated (`resolve_function`), because only the binding found is
  watched, as for the tier-0 inline caches.
- **A call to a speculated closure is direct and binds plain values.** Arguments that are
  effect-free are evaluated eagerly in the caller and bound as values into the callee's frame,
  skipping promises (`jit_call` in `crates/jit/src/runtime.rs`, `eval::bind_arguments`). Early
  evaluation is only invisible when the callee cannot assign anything an argument reads before
  forcing it (`g <- function(x) { assign("y", 99, envir = parent.frame()); x }`), so the callee must
  be effect-free (`interp::effects::effect_free_lookups`, read by `analysis.rs` `frame_safe`): it calls only itself and `EFFECT_FREE_BUILTINS`, each
  lookup recorded as a speculation from the callee's environment, and the call defers to tier 0
  while a calling handler is established. A frame-safe callee cannot call `substitute()`,
  `missing()` or `sys.call()`, so no promise is needed, and the frame records no argument list
  because only `nargs`, method dispatch and `standardGeneric` read it. S3 dispatch from those
  builtins on a classed argument is the one accepted hole.
- **Small local closures inline.** A call of a closure defined by a function expression in the same
  body, whose arguments and body compute a scalar from scalars without effects, runs inline on
  unboxed registers. Any failed check hands the call back to tier 0, which evaluates it lazily.
- **Environment frames live inline in the environment object.** Small binding sets are stored
  without a separate allocation, which cut a call's allocations, the dominant per-call cost.
- **A value boxed on an edge into a join is dirty.** A slot that is an unboxed local on one path and
  boxed on another gets a fresh box the frame does not hold, so the slot must be written back before
  an in-place element write is visible to the frame (`x[1] <- 9; get("x")`). The oracle case in
  `bytecode__hot_loops.R` pins it.
- **A helper that cannot allocate parks the error.** Compiled code does not branch on a nil box;
  later helpers return their fallback while an error is parked, and the return from compiled code
  raises it.
- **Static types do not remove guards.** A static `double` still admits an integer or a logical
  through promotion, and stubs claim scalar results for functions that can return vectors, so a
  static type proves neither the storage type nor the length the compiled code specializes on.
  Strict typing costs a little at run time instead (about 200 instructions per annotated call for
  the boundary checks). Using types as early profiles, or as guarantees backed by boundary checks,
  is still the plan in `r-runtime.md`.
- **A cold body waits instead of compiling with boxed guesses.** A hot body whose arithmetic reads
  an operand with no feedback, or whose `for` loop never ran, refuses to compile as `COLD`
  (`analysis::COLD`), which resets its counters without spending a compile. This is the recursive
  case: `fib` turns hot on its way down, before any call has returned, so compiling then types
  every call result as a boxed unknown. `TIER_STATE` counts compiles in its low byte and cold
  refusals above it (`runtime::COLD_REFUSAL`); after three cold refusals the body compiles with
  what it has, so a body that never fills its feedback still compiles.
- **A declared return type is the call result's early profile.** When a direct call's site has no
  feedback, its result type comes from the callee's declared return annotation
  (`annotations::declared_return_type`). A declared `double` still admits an integer through
  promotion, so this is a profile, not a proof, and the usual guards stay. Typed recursive code therefore compiles on
  the first try instead of waiting.
- **Interrupts deoptimize.** A compiled backward jump reads the interrupt flag and deoptimizes to
  tier 0's backward `Jump`, whose poll decides whether to raise it, so there is one place that
  handles it.
- **Compile latency: what was measured and rejected.** Cranelift's `opt_level = "none"` as a
  first tier did not shorten whole-process time on the why-ry programs (compiling is dominated by
  lowering and register allocation, which `none` keeps), so there is one tier. A back-edge
  threshold of 2000 instead of 500 changed instruction counts by under 4% either way. Not built:
  background compilation (compiled code embeds heap pointers and speculations validated against
  the interpreter's state at compile time, so a second thread would need the heap frozen or a
  revalidation step) and a persistent code cache keyed by body hash (the same embedded pointers
  make machine code process-specific; it would need relocations for every constant and
  speculation).
- **Inlining keeps annotation checks.** An inlined call has no frame, so `annotations::enter` and
  `check_return` never run for it. The inliner therefore inlines a typed closure only when
  `annotations::admits_inline` proves, from the argument and result storage types it inferred,
  that every check would pass; otherwise the call stays a real call. Before this, a typed callee
  inlined into a hot loop silently skipped its checks (`test_tiers.rs` pins both directions).
- **A builtin's repeated calls check the signature once.** `lapply` and `vapply` over an atomic
  vector call the same closure with one argument whose storage type is the vector's, so
  `eval::KnownFirst` resolves `annotations::entry_for` once per function and later calls run as
  `Entry::Checked`, skipping the per-call parameter checks. A list's elements differ, so lists
  check every call.
- **Arrays are their own JIT types.** `Ty::IntArr`, `DblArr` and `LglArr` come from three
  feedback kinds (`feedback::INTEGER_ARRAY` and so on, which is why feedback is 16 bits per
  instruction, stored as an integer vector). The guard reads the kind and the flag bits only,
  because the Array bit promises no attributes. `x[[i]]` reads the item directly and
  `x[[i]] <- v` stores it when `v` has the Array's exact item type and is not missing; a missing
  value, another type or a position out of range goes to tier 0, which applies the Array rules
  (a double position past the last item is an error, not truncated into range). `[` and
  arithmetic on Arrays stay generic, because their results must be Arrays checked for missing
  values. A copy for a shared Array keeps the bit (`jit_duplicate`). `List.len` is the `length`
  intrinsic on an Array or list.
- **A forgotten variable keeps its type as a guess.** An opaque call forgets every slot, since
  it may rebind anything, but the slot keeps the type it had. A later load where tier 0 has
  never read the variable (a branch not taken yet) speculates on that type behind a guard,
  instead of loading it boxed and leaving its arithmetic to tier 0 for good.
- **A failed guard after a load resumes after the load.** The load may have forced a promise,
  and only that forcing set the visibility `withVisible(x)` reads, so tier 0 continues at the
  next instruction with the boxed value instead of loading again.
- **A closure's type names its source body.** A `function` site replaces its body constant with
  the shared compiled code once it has run, so the analysis maps that code back to its source
  before building `Ty::Closure`. Otherwise the closure guard never matches and the body
  recompiles until it gives up, with every call left to tier 0.
- **A backward jump compiles impatiently.** The cold refusal (wait for code that has never run to
  record its types) applies only to compiles at entry. At a loop's back edge the loop is the hot
  code, and waiting for the code after it ran three more rounds of back-edge thresholds in
  tier 0. An impatient compile that reaches a loop which has never run guesses an integer
  sequence behind the guard rather than refusing, because any refusal other than a cold one
  disables the body for good.

## Native code sees a compiled closure's body as its expression

**Status:** built.

- `BODY` and `R_ClosureBody` return the source expression of a compiled body, as `R_ClosureExpr`
  does. ry's bytecode holds binding cells and type feedback that have no GNU R type, so native
  code that walks objects (`lobstr::obj_size`) failed on them.

## Native scalar functions: frameless calls with checks proven at compile time

**Status:** built, except the scalar-only limit, which "Native functions loop, read vectors, and inline small callees" supersedes.

- **Previous shape.** A compiled call of a closure that could not be inlined (any body with an
  `if`, so every recursive function) went through `runtime::jit_call`: a frame, promises holding
  the evaluated arguments, the callee's own compiled entry, and boxed results. Annotation checks
  were skipped only when `entry_for` proved them, and the result was refined by a guard. Typed and
  untyped code ran at the same speed, because the profiles already knew what the types said.
- **Target shape.** `crates/jit/src/native.rs` compiles a closure whose body is a pure scalar
  computation (constants, parameters, locals, `if`, `&&`/`||`, arithmetic, comparisons, `abs`,
  `sqrt`, `exp`, `log`, `floor`, `ceiling`, and calls of itself or other such closures) to a
  machine function `fn(budget, args...) -> (status, value)` over unboxed `i32`/`f64` values,
  specialized per closure and argument types. A compiled caller with unboxed arguments emits
  `Action::NativeCall` instead of `Action::Call`; recursion stays in machine code.
- **Why no frame is observable.** The body reads only its parameters and locals, has no effect,
  and calls nothing that could look at a frame. Everything that would warn or raise in R (an `NA`
  condition, integer overflow, `sqrt`/`log` of a negative, the expression-depth limit, a pending
  interrupt) returns `FAILED` before anything visible has happened, and the caller hands the call
  to tier 0, which reruns it and reports the problem exactly as R does. The depth budget is
  `max_depth - frames.len()` at the outermost call, decremented per nested call, so the limit
  matches tier 0's call for call. A left `NA` operand of `&&`/`||` also fails, rather than
  reproducing R's three-valued rule. Closure arguments are evaluated eagerly, which R's promises
  make observable only through an assignment or a `return` inside the argument, so those refuse.
- **Annotations.** Argument checks are decided by `entry_for` and the return check by
  `annotations::return_admits` when the native compiles, from the specialization's types; a
  function whose checks would fail is never compiled, so its calls go the checked way.
  `entry_for` and `admits_inline` now refuse an `Array` mask for a plain scalar.
- **Invalidation.** A native's function lookups (its callees' included) join every caller's
  speculations, and the cache revalidates them on each lookup, recompiling when a callee was
  rebound. Compiled natives keep their closure rooted (the cache key is its address) and are
  capped at 4096 per session.
- **Impact.** why-ry `fib` (`fib(30)`) runs in 0.007 s against GNU R's 1.12 s; `fib(25)` went
  from 0.072 s through `jit_call` to under 0.001 s. It speeds typed and untyped code equally: types make no scalar function faster than its
  profile already does, they only decide at compile time what an untyped function never checks.
  Next: unboxed records, tuples and tagged unions as native arguments and results.

## Compiled function lookups also check the activation's own frame

**Status:** built.

- **Previous shape.** A compiled body's speculations resolve a called name from the closure's
  enclosure, because the compiler proves the body never binds the name itself. Validation
  (`runtime::valid`) re-resolved from the same place whenever the binding epoch moved.
- **What was weak.** R can bind a name in a function frame without any assignment in the body:
  `assign` into `parent.frame()`, a default argument, or the locals `UseMethod` copies into the
  method's frame. The epoch moved, but the re-resolution skipped the frame, so the cached global
  function kept being called (oracle case `environments__hidden_function_bindings`).
- **Target shape.** `valid` takes the activation's frame and refuses when a speculation that starts
  at the body's enclosure finds a binding in that frame that may hold a function. Defining a
  watched name in a function frame already moves the epoch, so the fast path is unchanged. A loop
  entry, and the first run after a compile, walk the speculations regardless of the epoch, because
  that frame may have bound the name before the compiler watched it.
- **Impact.** Correct shadowing at no cost on the hot path; pinned by the oracle case and
  `run_time_binding_shadows_cached_global_function` in `crates/jit/tests/test_tiers.rs`.

## An integer and a double joined stay unboxed

**Status:** built.

- **Previous shape.** `Ty::join(Int, Dbl)` was `Boxed`, so a variable that is an integer on one path and a double on another (a `match` whose arms return `v` and `0`, `x <- if (c) i else 0.5`) was boxed on every edge into the join, and arithmetic on it ran as an opaque tier-0 step: 0.67 s per 2M iterations against 0.10 s when both arms agree.
- **Shape.** The join is `Ty::Num`: the value as an `f64` in the register's or slot's double variable and an "is integer" flag in its integer variable. An edge from `Int` or `Dbl` converts (an integer `NA` becomes the double `NA`), and boxing, write-back and deoptimization rebuild exactly the R value the flag names (`runtime::jit_box_num`, `call_flush`), so `typeof`, integer overflow and `NA` behave as in tier 0. Widening to a plain double instead would change `typeof` and integer arithmetic, which R code observes.
- **Who reads it unboxed.** Only `Move`, `SetVar` and arithmetic whose result is a double whatever the flag says: a comparison, `/`, `^`, or `+ - *` with a double operand. `Builder::box_operands` retypes every other reader's `Num` operand as boxed and `Plan::boxings` tells code generation to box it first, so no other action ever sees a `Num`. An inlined body refuses a `Num` slot, and a direct call passes one boxed from its slot cell after the flush.
- **Impact.** The mixed `match` loop runs compiled; correctness is pinned by `test_tiers` seeds that observe `typeof`, `NA` and integer arithmetic after the join, and the fuzzer's final statement can print the types.

## The JIT leaves a lossy double modulus to tier 0

**Status:** built.

- **Previous shape.** Compiled `%%` on doubles computed `a - floor(a / b) * b` directly.
- **Why that was wrong.** GNU R warns "probable complete loss of accuracy in modulus" when the finite quotient `|a / b|` exceeds 2^63, and compiled code skipped the warning.
- **Shape.** Both the bytecode codegen and native code call `runtime::jit_modulus_loses_accuracy` (`interp::arith::modulus_loses_accuracy`, the single rule) and hand the operation to tier 0 when it is true, so the warning has one implementation.

## Compiled bodies share one deoptimization exit

**Status:** built.

- **Previous shape.** Every guard's cold block boxed the live unboxed registers and slots itself,
  with one runtime call per value, then called `jit_deopt`. A body with many guards carried that
  boxing code once per guard, so the cold blocks were most of the IR Cranelift compiled.
- **Target shape.** Each guard's cold block only stores the raw values it must hand back into one
  stack scratch area (8 bytes per cell) and jumps, with its exit number, to a single shared cold
  block that calls `runtime::jit_deopt_exit`. The layout of each exit (registers and slots with
  their representations, and the bytecode pc) lives in `Compiled::exits` (`runtime::Exit`), so the
  runtime boxes the values and flushes dirty slots from that table.
- **Why a table.** The layout is static per guard, so it costs nothing to keep beside the code,
  and boxing is cold work: doing it in Rust from a table instead of in emitted code shrinks every
  compiled body without slowing any path that runs often.
- **Impact.** On the why-ry programs the IR shrank by about 31% and Cranelift time by about 40%.

## Native functions loop, read vectors, and inline small callees

**Status:** built.

- **Previous shape.** Native functions (`decisions.md`, "Native scalar functions") took scalars
  only and refused every loop, so a typed kernel such as a dot product or a running sum went
  through `jit_call` with a frame and boxed arguments.
- **Target shape.** `crates/jit/src/native.rs` also takes plain integer, double and logical
  vectors (as their object pointers; the caller roots them and the body allocates nothing), and
  compiles `for` over `seq_len`, `seq_along`, `a:b` or a vector, `while`, `repeat`, `break`,
  `next`, `length`, and `[[`/`[` with a scalar index. A typed tier-1 caller passes vector
  arguments straight through (`Argument::Unboxed`/`Argument::Slot` in `Action::NativeCall`).
  Every case R would warn or fail on (an index out of range or `NA`, a length above
  `.Machine$integer.max`, a fractional `a:b` start, a pending interrupt at a loop's back edge)
  returns `FAILED`, and tier 0 reruns the call.
- **Scoping.** A local first assigned inside a loop, an `if` branch or the right operand of
  `&&`/`||` goes out of scope where that region ends, so a later read refuses the function rather
  than guess whether R would find it bound.
- **Inlining.** A native call of a callee whose body has at most 24 calls and operators and no
  `return` walks the callee's body in place (its own locals, loops and lookup environment), up
  to three levels deep and never into a callee already being inlined. Both walks (typing and
  emitting) decide the same way, because the decision reads only the callee's body.
- **Strict mode does not drop guards.** A strict-clean program is a hint, not a contract: values
  from stubs or untyped packages can still differ from their declared types at run time. Typed
  code is faster because its closures compile at their first call and their calls go frameless,
  not because guards disappear.
- **Impact.** `sq <- function(x) x * x + 1` summed over 100,000 doubles 50 times runs in 0.035 s
  on ry against 1.99 s on GNU R; the same loop through `g <- function(x) sq(x) - sq(x - 1)` runs in
  0.056 s against 5.44 s, with both `sq` calls inlined into `g`'s 144-byte native function.

## Closures made at one site share compiled code

**Status:** built.

- **Previous shape.** Compiled code hung off a weak reference keyed on the enclosure it was
  compiled for, and ran only for that enclosure. A `function(v) ...` passed as an argument was a
  promise of its source, so the AST evaluator made each closure with its own body, which compiled
  its own bytecode and, under `RY_JIT=force`, its own machine code on every call: `vapply` over
  an anonymous function cost 306 µs against 8.2 µs on tier 0.
- **Target shape.** A function literal that is a whole call argument compiles as promise code
  (`compiles_as_promise` in `crates/interp/src/bytecode.rs`), whose `Op::Closure` gives every
  closure from that site one bytecode body. Compiled code is *portable* when each speculation
  starts from the enclosure or a long-lived environment and finds a binding in a long-lived one
  (global, base, a namespace or its imports, the search path; `long_lived` in
  `crates/jit/src/jit.rs`). Portable code holds what it embeds strongly and runs for any
  enclosure. `Compiled::checked` is a weak reference keyed on the enclosure whose lookups the
  epoch and stamps were last proven for; a new enclosure resets them, so `runtime::valid` walks
  every speculation from that enclosure (`Compiled::rooted`) before trusting them. Code that
  speculates on a function local to its enclosure keeps the per-enclosure scheme.
- **Why a weak reference, not the enclosure's address.** A dead enclosure's address can be reused
  by a new environment whose lookups nobody checked.
- **Impact.** The `vapply` case runs at tier 0's speed under `RY_JIT=force` (about 15 µs per
  call of ten elements), and base R's `pmin`, whose body makes such a closure, is no slower with
  the JIT on than off.
- **Copies run the original's code.** A closure's body slot holds its compiled code once
  compiled, and a closure copy (`duplicate`, `environment<-`) takes as its body the code the
  original runs, or the code closures over its body share (`body_to_copy`), so copies run one
  code object. `compile` returns `R<Sexp>`: the only by-design refusal is a body that is not a
  call (`Uncompilable::NotACall`), and anything else is a typed error, never a silent fallback.
  Closures that the AST evaluator makes from one `function` expression share their body object
  and find its code in `Bodies` (see "Closures over one body expression share its compiled
  code").

## Fast-path prologues return through an op, not an unwind

**Status:** built.

- **Previous shape.** A native prologue (`.Internal(ryX())` as a body's first statement) finished
  the call by raising `Jump::Return`. On a slot frame that materialised a pending environment for
  the unwind target, and in tier 1 it was a failed step and a deopt: about 11.5k deopts per two
  repeats of the testthat workload.
- **Target shape.** The compiler emits `Op::Prologue dst function call` for a zero-argument
  builtin marked `BuiltinDef::prologue` (not inside promise code). The op sets
  `interp().prologue` to the running frame (`ClosureFrame::running`), calls the builtin, and
  `base_fast_paths::finish` returns the value instead of jumping when the frame matches, clearing
  the marker. The op then returns `Flow::Done`, `step_one` reports `Some(value)` and tier 1 sees
  `outcome::RETURNED` and returns `dst`. The outer marker is saved and restored, so a prologue
  inside a prologue's argument works. Prologue builtins use `Visibility::Keep`, because `finish`
  sets visibility itself.
- **Impact.** Deopts on the testthat workload fall from about 11.5k to about 2.6k per two repeats;
  the rest are real condition unwinds (`invokeRestart`, `signalCondition`, `doTryCatch`).

## Bodies that would mostly hand back to tier 0 compile only for loops

**Status:** built.

- **Previous shape.** A hot body compiled whenever its plan had at least one native action. In
  package code (`[.data.frame`, `[.zoo`, S4 `is`, ggplot2 internals) most instructions are calls
  tier 1 cannot specialise, so the compiled body was a chain of `jit_step` calls back into tier 0.
  Each such step costs more than tier 0 running the instruction itself, and Cranelift spent about
  0.6 s compiling these bodies on a 12-file finbench slice, which made the suite about 8% slower
  with the JIT on than off.
- **Target shape.** `compile_in` refuses with `MOSTLY_TIER0` (`crates/jit/src/analysis.rs`) when
  the plan has fewer native actions than tier-0 steps, unless the compile was triggered by a hot
  back edge or `RY_JIT=force` is on. The refusal sets `runtime::LOOPS_ONLY` in the body's tier
  state, so later hot entries return at once and only a hot loop can still compile it, because a
  loop's native header pays for its steps on every iteration. A fully typed closure's first-call
  compile goes through the same gate.
- **Impact.** On the finbench slice the gate refuses about four in five compiles and most of the
  compile time, and the JIT-on wall gap against `RY_JIT=off` drops from about 8% to about 4.5%.
  The why-ry programs are unaffected: their bodies are loops or mostly native.

## Tier 0 records type feedback only for warm bodies

**Status:** superseded by "Tier 0 records feedback only for bodies halfway to a threshold". Kept for the `NEVER_HOT` and `LOOPS_ONLY` follow-ons and for why a first-call `MOSTLY_TIER0` verdict is not final.

- **Previous shape.** With the JIT installed, tier 0 tested the type of every value an
  instruction produced and stored a bit for it, in every body from its first entry. Almost no
  body of package code ever reaches a compile threshold, so this was pure overhead: about 25M
  instructions of a 4.2G ggplot2 render, most of what made the gallery slower with the JIT on.
- **Target shape.** `Decoded::of` (`crates/interp/src/bytecode.rs`) hands out the feedback
  pointer only when `warm` holds: the body has a loop, has compiled code, or is halfway to the
  entry threshold and the tier has not given up on it. A body refused as `MOSTLY_TIER0` gets the
  entry count `runtime::NEVER_HOT`, so tier 0 stops asking the tier about its calls.
- **Two follow-ons keep the gates from trapping a body.** `runtime::invalidate` resets the entry
  count to half the threshold rather than zero, so the activation that just deoptimized records
  the type that broke the speculation and the recompile sees it; from zero, the body could
  compile the same speculation until it ran out of compiles. And a `LOOPS_ONLY` body that did
  compile for its loop still runs its calls in tier 0 (`compiled_for` returns early), because
  entering compiled code from the top would hand most instructions back one by one.
- **A first-call `MOSTLY_TIER0` verdict is not final.** The first call plans without a profile,
  so that refusal leaves the tier state alone and the body tiers up normally later.
- **Why loops always record.** A loop can turn hot inside a body's only activation, and its
  compile needs the types of the instructions that ran before it. Switching recording on at
  half the back-edge threshold left those without feedback, so the loop was refused as cold
  every time and the why-ry array programs ran 5 to 40 times slower.
- **Why half a threshold.** A compile reads the feedback of the activations before it; half the
  threshold leaves 1,500 entries of history, which sees every type a
  specialization needs, while cold bodies pay one comparison per activation.
- **Impact.** The ggplot2 render's JIT overhead fell from +55M to +26M instructions, and the
  gallery is no slower with the JIT on than off (6.99 s against 7.10 s, three interleaved runs).

## A typed closure compiles at its first call only when that call can run long

**Status:** built.

- **Previous shape.** Every fully typed closure compiled at its first call, because its
  annotations give the types a profile would otherwise collect. On a finbench slice that was
  183 compiles of about a millisecond each, of bodies such as `alpha + beta` whose call costs a
  microsecond and which most tests call a handful of times.
- **Target shape.** A first-call compile is refused as `BRIEF` (`crates/jit/src/analysis.rs`)
  unless the plan has a loop or calls a closure or native function directly
  (`Plan::may_run_long`), the cases where the first activation itself can run long, recursion
  included. The refusal is not counted, so the body tiers up at the entry threshold like any
  other. `compile_in` takes a `Trigger` (first call, hot entry, hot loop) in place of the
  `in_loop` flag, so each profitability gate states which triggers it applies to.
- **Impact.** The why-ry typed programs are unchanged (their bodies loop or recurse); typed
  test-suite code no longer pays for compiles that never run again.

## Strict-proven types do not drop JIT guards yet

**Status:** built.

- **Rule.** No runtime guard drops because of a strict-mode proof until a per-function strict-clean
  verdict reaches the runtime. Once it does, only a base SEXP-type or scalar/vector-kind guard on a
  non-escaping local or parameter, after its force point, may drop, and only when the type derives
  from checked sources alone. Guards on `Any`, stub returns, cross-package values, `@trust` and
  `@if-unknown` bindings, class, attributes, the object bit, length beyond a checked scalar, `NA`,
  and global lookups always stay.
- **Why.** A tier-1 parameter read forces the promise and then runs one fused check (SEXP type,
  no attributes, no object/ALTREP bit, length 1), so dropping only the type compare saves one
  compare per read; every other guard is a lookup guard that the rule keeps. Native functions
  already skip annotation checks, because the specialization's types decide them at compile time.
  Typed and untyped warm times are equal on every why-ry row, so compile latency, not guards, is
  where the time goes.
- **Consequence.** The verdict plumbing is not built. Revisit it when a profile shows guard cost.

## Untyped R code is not compiled to machine code ahead of time

**Status:** built, as a decision not to build: `--aot` compiles typed code only, and untyped code runs on tier 0 with tier 1 compiling hot functions during the run.

- **The question.** Whether `ry run --aot` and `ry build --aot` should also compile untyped R to machine code before the program runs.
- **What ahead-of-time code for untyped R can use.** Without a profile it has the bytecode and nothing else, which is the baseline tier that measured slower than tier 0 ("No baseline tier, and code images hold bytecode only": forced baseline +7.5% instructions on `testthat_full`, with tier 0's whole dispatch at 2.66% of instructions as the ceiling). Tier 1's speed comes from the types and call targets a run observed, which do not exist before the run.
- **Measured: what an up-front compile could save.** Runs of the two ry-owned suites on one machine, each program with `RY_JIT=on` and `RY_JIT=off`, cold (first run of the measured function) and warm.
  - **Applications** (the north star): warm time with the JIT off over on has geometric mean 0.999, so tier 1 buys nothing there yet and compiling it earlier buys nothing either. Tier-1 compile time is 0 to 31 ms per program, a mean of 3.2% of the cold run, and cold over warm is 1.18.
  - **Kernels:** JIT off over on is 5.199, so tier 1 matters, and its compile is 2 to 9 ms per program (mean 9.07% of the cold run, cold over warm 1.289). That is the most an ahead-of-time compile could remove, and only with the same profile tier 1 had.
- **Decision.** No native ahead-of-time code for untyped R. On the applications there is nothing to win, and on the kernels the win is bounded by single-digit milliseconds per program while the code would need the run's profile to be as fast as tier 1's.
- **Profiles already travel as build output.** A trained code image (`R CMD INSTALL --ry-train=...`, `crates/interp/src/code_image.rs`) stores each body's type feedback and counters, so a body that was hot in training reaches tier 1 at its first calls in a new process, after Leyden's ahead-of-time method profiling (JEP 515, `prior-art.md`). That covers the part of ahead-of-time compilation that helps untyped code: the profile. What it leaves in the run is tier 1's compile, the single-digit milliseconds measured above. Storing tier 1's machine code beside the profile becomes worth building when the applications suite shows tier 1 winning (JIT off over on well above 1) and its compile time showing in a cold run. Facts-based code (checker types, closed bindings of locked namespaces) is the typed path's business, where the types are sound.

## Bytecode is build output, not a cache

**Status:** built, except the base image's code section: base R's compiled closures live in the start image, per "The start image keeps base R's closures compiled".

- **Previous shape.** `r-runtime.md` planned to cache bytecode and runtime summaries on disk under
  content hashes; no code implemented it. Installs do not byte-compile today: every closure
  compiles at its first call.
- **What was weak.** A cache is a second copy of each package's code that needs invalidation and a
  key computation on every load, and the performance principles in `AGENTS.md` treat a cache as a
  sign of a slow design.
- **Target shape.** `ry install` / `R CMD INSTALL` write bytecode beside the lazy-load database,
  stamped with the ry build; a different build ignores the stamp and compiles at first call. Base R
  is compiled into a section of the base image keyed by the runtime hash (`base-image.md`). `ry
  load` and scripts compile at first call, so that tier must be cheap. `r-runtime.md`, "Where
  bytecode comes from", is the spec. Installed packages are implemented as code images
  (below); the base image's code section is not.
- **Impact.** One source of truth (the package's R code plus the install output derived from it),
  and no invalidation logic.

## Tier 0 runs on facts fixed before the code runs

**Status:** built, except the single `interpret` loop with `tri!`, which the tail-call handlers in "Step 2 of the untyped roadmap: slot frames rejected on measurement, setters compiled, step 3 pulled forward" supersede.

- **Previous shape.** Each bytecode instruction went through a `step` function that returned a `Flow` value the caller matched again, reloaded the frame, code and registers on every step, and looked up the operand count in a side table. Every variable lookup searched the frame (a linear scan, or a hash index above 16 bindings), function lookups probed every enclosing frame, and every builtin call re-derived its arity, names and native-argument checks from the builtin's name.
- **Weakness.** All of it was work recomputed per instruction or per call from facts that do not change between calls: the operand count of an opcode, the position of a local in a frame of one function, and the checks a builtin needs.
- **Target shape.** (`crates/interp/src/bytecode.rs`, `docs/src/content/docs/development/design/r-runtime.md`, "Tiers")
  - An op word is `op | operand_count << 8` (`op_word`, `op_of`), so decoding the next position needs no table.
  - `interpret::<SINGLE>` is one loop with the hot state in locals. Errors leave through `tri!`, which breaks to one exit that calls the handler table (`continue_after`); the debugger's single step is the same loop with `SINGLE = true`, so the two cannot drift. A backward `Jump` is the only back edge, and its tier-up bookkeeping is outlined into `back_edge`.
  - Each `GetVar`, `SetVar` and function lookup site owns a slot hint (the code's `HINTS` vector, one `u32` per site). `Frame::find_hinted` checks the symbol filter first, then the hinted slot, then searches and updates the hint. A stale hint only costs a search, so hints need no invalidation. A frame that binds a symbol twice (only duplicate formals do, and the first cell answers) sets the `SHADOWED` bit and ignores hints; `fuzz_hinted_lookups_match_plain_lookups` in `crates/heap/tests/test_frames.rs` pins hinted lookups to plain ones.
  - `SetVar` goes through `eval::assign_local` with the cell the hinted lookup found, and `Heap::define_unbound` adds a new binding without a second search.
  - `BuiltinDef::checks` (`Checks`, `NativeArgs`) is derived at registration from the definition and the builtin's name.
  - A frame keeps a hash index only above 32 bindings (`INDEX_THRESHOLD` in `crates/heap/src/env.rs`). With the filter and hints in front, a scan of up to 32 cells is cheaper than the index that a base closure binding about 20 names (`seq.default`) rebuilt on every call.
  - A lookup cache's stamps live in a double vector but are read and written as their `u64` bits (`LookupCache::epochs`), so the stamp check is one integer compare instead of a conversion. All-ones bits mean empty; no stamp reaches that value.
- **Why not caches.** A hint is part of the code object, written by the lookup it accelerates, and never consulted for correctness: the symbol at the hinted slot is compared every time. No epoch or invalidation exists to get wrong.
- **Impact.** `seq(1, 10, by = 2)` (12.5 closure calls, about 680 ops, 65 builtin calls per call) fell from 160.4k to 141.6k instructions under callgrind; GNU R takes about 106k. Remaining per-call costs, largest first: builtin calls (about 9.5k), dispatch at about 14 instructions per op, and 117 function guards; the untyped performance roadmap in `r-runtime.md` orders the work that removes them. Correctness: the oracle suite passes as before, and the debugger's stepping uses the same loop.

## The untyped performance roadmap is ordered by measured gap, and the baseline tier is Sparkplug-style

**Status:** built, except the baseline tier, which "No baseline tier, and code images hold bytecode only" supersedes.

- **Previous source of truth.** Open untyped-performance work lived as a numbered list in `backlog.md` and as two verdict tables in `prior-art.md`, which disagreed on status (slot hints listed as open after they were built) and said copy-and-patch was the candidate baseline tier.
- **Target shape.** One table, "Untyped performance roadmap" in `docs/src/content/docs/development/design/r-runtime.md`, with one row per idea: source engine, status, the gap it targets from `--profile-summary` and callgrind, and the code once built. `prior-art.md` holds the reasoning per engine and points at the roadmap; `backlog.md` keeps only the raw measurements.
- **Order.** Finish the tier-0 call path, then move call facts ahead of time (version-stamped bindings, slot frames with lazy environments, promise elision, argument-match caches, S3/S4 inline caches), then quickening and tail-call dispatch in tier 0, then the optimizing tiers, then mapped images. Call setup and lookup come first because `--profile-summary` puts closure call setup at 12 to 24% and S3 lookup at up to 17% of real workloads, against tier 1 under 3%: the code that matters runs a few thousand times and never reaches tier 1.
- **Baseline tier.** (Superseded: built, measured and removed; see "No baseline tier, and code images hold bytecode only".) A single-pass compiler from bytecode that keeps tier 0's frame layout and feedback (V8's Sparkplug), so it needs no deoptimization metadata. Copy-and-patch (CPython's PEP 744) is rejected: its gain over a good interpreter is modest, and its stencils need a Clang build step that a Rust workspace does not have.
- **Impact.** One place to read what is next and why; each pull request that changes an item updates its status there.
- **No separate call plan for `run_closure`.** Its annotation, `on.exit`, move-tracking and depth checks cost 1.3k of the 141.6k instructions of a `seq(1, 10, by = 2)` call and already exit early; the rest is the frame record push and pop and moving `R<Sexp>` values, which the slot-frame redesign replaces, so a per-closure plan would save under 1%.

## Step 2 of the untyped roadmap: slot frames rejected on measurement, setters compiled, step 3 pulled forward

**Status:** built, except the slot-frame rejection, which "Slot frames: closure locals live in registers until something asks for the environment" supersedes.

- **Measurement.** Callgrind, JIT off, instructions per call. A plain closure call (`f(x, y = 2) x + y`) costs 3,968 on ry and 4,602 on GNU R, and times at 0.445 µs against 0.525 µs (median of 9 interleaved runs of 3 million calls), so closure call setup is no longer behind. `seq(1, 10, by = 2)` costs 135.0k on ry and about 106k on GNU R.
- **Slot frames rejected for now.** Binding cells cost about 420 of a closure call's 3,968 instructions. Holding values inline with cells reified on demand would save 100 to 150 of them (about 3%), while about 180 call sites read frame cells and reifying would need allocation behind `&self` heap APIs. Revisit if a profile shows frame cells as a top cost again.
- **Version-stamped guards.** `Op::Guard` compares the lookup slot's stamp and enclosure (`cached_cell`) and then the cell's value; a cached cell never becomes active (`makeActiveBinding` refuses a regular binding) and a binding added in front moves the symbol's stamp, so no further check is needed.
- **One-level setters compile.** `f(x, ...) <- value`, with `x` a variable and the other arguments free of side effects, compiles to `Op::Replace`. `replace_variable` mirrors `complex_assign` for one level: a builtin replacement is called on the argument values (objects fall back, since they may dispatch), a closure replacement taking only `x` and `value` gets the same uncounted `*tmp*` and value promises, and everything else falls back to `complex_assign`. An argument register that is the target itself forces a copy, because registers count no reference.
- **Pivot.** No remaining step-2 row targets a gap the size of `seq()`'s 29k: dispatch is about 11k (13 instructions per op over about 860 ops), and `pmin()` alone is 12.4k behind (mostly `vapply()`). Op-count and per-op work (step 3: quickening, superinstructions, out-of-line slow paths) comes forward, interleaved with the remaining step-2 rows (argument-match caches, S3/S4 inline caches).
- **Tail-call handlers.** Tier 0 runs one handler per op, chained with `become` (`#![feature(explicit_tail_calls)]`, the pinned nightly). A handler takes only `(&mut Activation, pc)` and returns nothing; code, registers, the `Interp` pointer and the result (`Activation::exit`, with `STOP` as the next pc that ends the chain) live in the activation. The first shape, which passed code, pc and registers as arguments and returned `R<Sexp>` through an sret pointer, kept five values live across every call and gave each handler a six-register prologue, which cancelled the dispatch savings (121.5k against 120.8k per `seq()`). Outlining the bodies of instructions that call out made it worse (131k), because the wrapper still needed a frame to keep its arguments across the call. The two-argument shape measures 121.4k, level with the match loop: handler cost equals the old loop's, and the remaining per-op cost is the instruction work, not the dispatch. The structure stays because a stackless tier 0 and quickened handlers build on it; the error path (`failed`) is cold and out of line.

## One SSA IR and one install-time pipeline for typed and untyped compiled code

**Status:** built, except install-time code: for untyped R, "No baseline tier, and code images hold bytecode only" supersedes it, and for typed code it is not built.

- **Previous shape.** Tier 1 lowers bytecode to its own SSA (`crates/jit/src/analysis.rs`, `codegen.rs`), and the typed core lowers checked ry to a second SSA form (`crates/typed/src/ir.rs`, `lower.rs`, `backend.rs`). Both emit through Cranelift, so the two forms duplicate the instruction set, the lowering to Cranelift and the optimization passes, and they can drift.
- **Target shape.** One mid-level SSA IR. Typed facts enter it unguarded; untyped speculations, taken from training-run profiles or run-time feedback, carry guards and deoptimization exits. Install-time code for untyped R goes through the typed AOT pipeline in `typed-runtime.md` (the same `ry install` step and code file: position-independent, mapped read-only, a per-process table, build and interface stamps, written by rename; the same Cranelift backend; no code cache). The code file stores the deoptimization metadata and the bindings each function watches, so `unlockBinding` and `assignInNamespace` invalidate install-time code exactly as they invalidate JIT code. The run-time JIT stays for fallbacks and new profiles, which is why `prior-art.md` rejects compiling every fallback at build time.
- **Built: the IR.** `crates/ssa` holds the IR (`ssa.rs`: SSA construction from mutable variables after Braun et al., and a first-class guard whose exit is a cold deoptimization block), the one lowering to Cranelift (`lower.rs`) and the one code module per thread (`ssa::with_module`). Tier 1's translator (`codegen.rs`), its native scalar functions (`native.rs`) and the typed core's backend (`crates/typed/src/backend.rs`) build that IR, and no other crate depends on Cranelift. `crates/ssa/tests/test_ssa.rs` fuzzes random programs with branches, loops and mid-block guards against direct evaluation.
- **Not built: install-time code** through the typed AOT pipeline.
- **Impact.** One source of truth for the instruction semantics and the lowering (correctness), one pass pipeline instead of two (simplicity), and typed facts that remove guards from untyped code that calls typed code (performance). Analysis is unaffected: the IR is runtime build output, never a salsa input.

## The effect-free fact is stored per code object and resolved per enclosure

**Status:** built.

- **Previous source of truth.** The JIT walked a callee's body itself (`analysis.rs`, `safe_expression` and `FRAME_SAFE_BUILTINS`) to decide whether arguments may be bound eagerly. Tier 0 had no such fact, so every closure call built a promise per argument.
- **Stored half.** The bytecode compiler records a summary in the code object's `SUMMARY` constant (`crates/interp/src/effects.rs`, `summarize`): nil when the body assigns non-locally or calls anything other than a symbol, otherwise the names it calls. It is computed once per `function` expression and shared by every closure that expression creates.
- **Resolved half.** `effect_free` resolves the called names from the closure's environment: each must be `<-`, `=`, `[<-`, `[[<-`, a builtin in `EFFECT_FREE_BUILTINS`, or a closure that is itself effect-free (depth 8, recursion keyed on code and enclosure). Each dependency's symbol and cell are marked watched, and the proof stores their `lookup_stamp`s in the call site's lookup cache, so any rebinding invalidates it.
- **Home keying.** Helpers created per call (`is.logint` inside `seq.default`) get a new enclosure every call, so a proof keyed on the enclosure missed every time at about 950 instructions per miss. A proof is keyed on the first ancestor that is not a function frame unless the frames in between bind a called name (`Proof::HoldsHere`), and a failing proof is always keyed on the home.
- **When a call binds eagerly** (`op_call_generic`): the compiler marked the call site eager (at least one symbol argument, and no argument can assign: symbols, constants, and nested calls of effect-free builtins), the callee is effect-free, and no calling handler is established. A symbol argument bound to a value or a forced promise passes the value; an unforced promise passes itself; a missing, unbound or active binding keeps its promise (`eager_argument`).
- **Impact.** One computation of the fact for tier 0, the JIT and later the parallel apply family. The JIT's private walk is deleted.

## Why the JIT made code slower, and the three fixes

**Status:** built, except the baseline tier as the fix for compile cost, which "No baseline tier, and code images hold bytecode only" supersedes.

- **Measured.** `bench-real` put JIT on at 1.13x GNU R against 1.19x with it off. Callgrind (one cold and one warm run) showed JIT on costing 4 to 5% more instructions on `functional` and `records`. Wall time on the measuring VM varies by about 10% run to run, so most of the per-workload gap in the table was noise; what was real is below.
- **Sibling enclosures re-walked every speculation.** Portable code (`Compiled::portable`) proved its lookups for one enclosure, and an activation from any other enclosure allocated a weak reference, re-pointed the proof at itself and walked every speculation. Closures from one `function` expression alternate enclosures constantly (`lapply(1:50, make_scaler)` then calling each), which made such a loop 2.7x slower than tier 0. An enclosure whose parent is the checked enclosure's parent now shares the proof and checks only its own frame for the rooted names (`runtime::sibling_of_checked`, `binds_rooted_function`): 0.74 s to 0.32 s on that loop (tier 0: 0.23 s).
- **Revalidation after every helper.** Each variable load, tier-0 step and call from compiled code ran `valid`, about 50 instructions, even when the code it ran bound nothing. Any binding that could change a function lookup moves the binding epoch (watched names included), so `still_valid` skips the check when the epoch did not move. `functional`: 140M instructions of `valid` gone.
- **`pkg::f(...)` calls were not compiled.** The bytecode compiler sent every call whose head is not a symbol to the AST evaluator, so a `function` argument of `purrr::map_dbl(x, function(v) ...)` became a new closure with its own body on every call: it compiled to bytecode, recorded feedback and compiled in tier 1 again on every run (24 of `functional`'s 33 tier-1 compiles). A call head that is a call now evaluates into a register and calls through `Op::CallGeneric`, so its arguments are compiled promises and the closure shares one body.
- **What remains is compile cost.** Cranelift takes 0.5 to 1 ms for a five-instruction body and 263 ms for `rbind`'s (343 KB of code), 78% of `functional`'s and 40% of `records`' extra instructions. Warm runs do not pay it, but scripts and tests do. The baseline tier (the Sparkplug-style decision above) is the fix, not a cheaper Cranelift setting.

## Baseline code inlines variable access and scalar comparisons, and its cost is code footprint

**Status:** superseded by "No baseline tier, and code images hold bytecode only". Kept for the instruction-cache footprint measurement and the tier-1 entry cost.

- **Previous shape.** Baseline code called tier 0's step for `GetVar`, `SetVar` and every arithmetic instruction.
- **Chosen shape.** `crates/jit/src/baseline.rs` emits the fast paths inline, with the slow path (a step call) placed after the body: `GetVar` of a variable the activation's own frame binds through its lookup hint (`probe_frame` reads `heap::env::layout`), `SetVar` into such a binding with tier 0's reference-count rules, and the six comparisons on two plain length-one integers or doubles. Anything else, including a body still recording type feedback, takes the step. Arithmetic that allocates its result stays a step call, because allocation has no inline path yet.
- **Measured: instruction-cache footprint, not instruction count, keeps the baseline opt-in.** Under `RY_JIT=baseline`, `testthat` installs 972 bodies with 9.67 MB of code (median 2.66 KB, largest 460 KB). The per-pc jump table counts toward that size but is only read as data. Each instruction costs about 30 bytes of code unique to its body, while tier 0 runs every body through about 24 KB of shared handlers. On `functional` (callgrind, cache simulation), the I1 misses are 423M with the JIT off, 462M with it on and 454M with the baseline, at about 3% of instructions in every mode, and the extra misses spread over unrelated functions, which is general pressure rather than one hot spot. Lowering the baseline threshold made it worse (1.075x JIT off's time at 1 entry, 1.091x at 50), so thresholds are not the fix. What would fix it: shorter encodings (rel8 branches through relaxation, `cmp` with an 8-bit immediate), a step convention that returns 0 for fall-through so each instruction needs one test instead of a compare against its next pc, and inline paths for the instructions that remain calls, so a body's code replaces handler code instead of adding to it.
- **Tier 1's entry cost.** `functional` enters tier-1 code 943K times, and each entry spent about 300 instructions in `enter`, `compiled_for`, `valid` and `reserve_registers` before the compiled code ran. The common case of `valid` (unchanged binding epoch, checked enclosure) and of `reserve_registers` (no growth) are inline now, and the walks are out of line.

## Tier 1 probes the frame inline for a variable load

**Status:** built.

- **Previous shape.** Every tier-1 load called `jit_load`, which ran tier 0's `load_variable` (frame search, feedback recording, a validity check) at about 200 instructions a load. `SetSrcref` went back to tier 0 through `jit_step`.
- **Chosen shape.** Compiled code reads the lookup slot's hint (`bytecode::lookup_hints`) and probes the activation's frame at that position, then an enclosing function frame, as the baseline's `probe_frame` does (`crates/jit/src/codegen.rs`). A plain value or a forced promise is taken inline with no write-back, feedback or validity check; anything else takes the old call. A value of a type the code did not speculate on is recorded through `jit_record` before the deoptimization, so the recompile sees it. A forcing load's after state joins the unflushed state, because the inline probe leaves dirty variables dirty. `SetSrcref` outside a debugger stores the srcref inline. A load whose feedback saw both an integer and a double scalar is `Ty::Num`, so closures from one site with different argument types (`make_scaler(2)`, `make_scaler(0.5)`) keep their arithmetic compiled. Bodies tier 1 refuses or gives up on stop recording feedback.
- **Measured.** Callgrind, bench profile, `RY_BENCH_REPEAT=2`, billions of instructions, JIT on: `functional` 13.106 to 12.440 (JIT off 12.671), `testthat_full` 7.160 to 7.134 (JIT off 7.047). JIT off is unchanged within 0.05.

## Bodies wait in proportion to their size before tier 1 compiles them

**Status:** built.

- **Previous shape.** Every body compiled at 3000 entries or 10,000 back edges, whatever its size, so `rbind`'s 9,731 words (200 ms of Cranelift, 343 KB of code) compiled in any script that called it 3000 times.
- **Chosen shape.** When a body first reaches a threshold in `RY_JIT=on`, it waits one more threshold per 250 words (`SIZE_WAIT_WORDS` in `crates/jit/src/jit.rs`, flag `runtime::SIZE_WAITED`) before it compiles. Compile cost is linear in size and the saving per entry is not, so a large body must run proportionally longer to repay its compile, which is how V8 scales its tier-up budget by bytecode length. A five-instruction body is unaffected.
- **Impact.** The `rbind` compile leaves `records`; base `Reduce` (2,400 words) still compiles, because its loop is hot.

## Tier 0 records feedback only for bodies halfway to a threshold

**Status:** built, except the half-threshold entry window, which "Tier 0 decides at scheduled entries and stops recording at tier-up" supersedes with the last eighth.

- **Previous shape.** `warm()` decided at every activation whether tier 0 should record type feedback, from the code's other constants: about 25 instructions per call, 30 to 60M instructions on `objects`. Bodies with a loop recorded from their first entry.
- **Chosen shape.** One profile word, `profile::RECORDING` (`crates/interp/src/bytecode.rs`). It is on during the first activation of a body with a loop, when a body enters the recording window before the entry threshold, and when its loop reaches half the back-edge threshold (inside the running activation). It is off whenever tier-1 code is installed (see "Tier 0 decides at scheduled entries and stops recording at tier-up").
- **Why the first activation of a loop body records.** Starting only at half the back-edge threshold left the instructions before the loop without feedback, and the on-stack compile handed the loop's `SubAssign` back to tier 0: a scalar loop ran 0.40 s instead of 0.04 s. Recording in every activation of a loop body instead cost base `Reduce`, `Filter` and `Map` a type test and store per value on every call.
- **Impact.** The tier machinery with nothing compiling (`RY_JIT_THRESHOLDS=0,0,0`) costs 0.0 to 0.9% more instructions than `RY_JIT=off` on the `bench-real` workloads (geomean 0.5%). Cachegrind on `functional` shows I1 misses up 3.5% and D1 misses up 1%, so the 4 to 5% wall gap single runs show is noise.

## Bodies that could run nothing natively never record

**Status:** built.

- **Previous shape.** A body whose plan ran nothing natively (`NOTHING_NATIVE`) recorded type feedback through the last eighth of the entry threshold, then paid a full plan, only to be refused.
- **Chosen shape.** `analysis::shape` (with `Shape::may_run_natively`) reads the instruction words alone: a loop, an instruction kind the plan may lower to a native action, or a `CallBuiltin` whose call names a builtin with a native form (`[`, a `List.`/`Dict.`/`Set.` traversal, or a name `has_intrinsic` lists, which also gates `intrinsic_for`, so the two lists are one). Its `match` over `Op` has no catch-all, so a new opcode forces the decision. The new `Tier::worth_recording` hook runs it at the entry that opens the recording window, and `compile_in` runs it before planning, so a hot body that skipped the window (thresholds below 8) is refused without a plan too. The refusal (`NEVER_NATIVE`) is final, including at a fully typed body's first call. `RY_JIT=force` keeps such bodies.
- **Known gap.** A builtin called through another name (`f <- length; f(x)`) is judged by the name in the call, so such a body is refused when its plan might have used an intrinsic. That costs only speed.

## A loop-free hot body compiles only when its call rate repays the compile

**Status:** built, except the baseline threshold in the back-edge check, which "No baseline tier, and code images hold bytecode only" supersedes. The saving per call (100 per native instruction below) is replaced by the model of "A body compiles only when its native work covers what compiled code costs".

- **Previous shape.** Every body that reached 3000 entries compiled. A loop-free body saves a bounded amount per call, so one that turned hot slowly never repaid its compile: on testthat, `isTRUE` (188 words) compiled for about 11M instructions and then ran about 1400 more times, and JIT on cost 17.6M more than JIT off.
- **Chosen shape.** At the entry threshold, `compile_in` asks `repays` before planning a loop-free body (`analysis::shape` reads the words once and counts the instructions the plan may run natively). Expected calls are the body's rate over its recording window, 375 calls per however many closure calls the session made meanwhile (`Interp::calls_ever`, stamped into `profile::WINDOW` when the window opens), times a fixed horizon of 2^19 calls. The body compiles when calls times 100 instructions per native instruction reach 2M plus 50k per word. A refusal (`RARE`) is not final: the count restarts and the next attempt measures the rate afresh. A recompile skips the check.
- **Why calls are the clock.** The clock was the heap's allocation total at first. Slot frames then cut the allocation per call, so the same `isTRUE` looked 2.3 times as frequent (one call per 23 KiB instead of 54) and compiled again for 11M instructions on testthat. An optimization must not move tiering decisions, so the clock counts closure calls instead: one increment where a frame is pushed (`Interp::calls_ever`), next to the hundreds of instructions the push costs. It is deterministic and measures interpreted work as directly as allocation did.
- **Why a fixed horizon.** Taking the session's age as the horizon (Lindy) underestimated the functional workload's first callbacks 13-fold, below `isTRUE`'s estimate. The rate alone separates them: `isTRUE` runs once in 63 calls, the slowest functional callbacks (`x^2`, `Reduce`'s) once in 9. Those callbacks need a horizon of 390,000 calls and `isTRUE` would need 600,000, so 2^19 sits between them with a margin of about 1.3 either side, and it is the constant to revisit when a workload lands near the line. Against 2^18 it saved functional 10.8M instructions and left testthat unchanged.
- **Rejected: never compile loop-free bodies at entry.** Every body the functional workload compiles is loop-free (`x * factor + offset`, `x %% 2 == 0`, ...), and they make JIT on 190M instructions cheaper than JIT off there. Delaying them to 10,000 entries cost 19M and to 30,000 entries 149M, so a flat threshold rise is out too.
- **Impact** (callgrind, millions of instructions, JIT on/off): testthat 4810.6/4805.0 against 4821.0/4800.8 before this and the never-native refusal; functional 8376.6/8571.5 against 8381.5/8573.5.
- **Backoff.** Each attempt records a fresh window of feedback, so a refused body waits the entry threshold times 2^n entries after its n-th refusal (`runtime::RARE_REFUSAL`, four bits of the tier state). Without it a rare body re-recorded every 3000 entries for the whole session.
- **Back edges.** Every backward jump counts inline (`back_edge_due`: an increment and three comparisons) and calls the cold `back_edge` only at the counts it acts on (half the threshold, the baseline threshold, the threshold). The out-of-line call per jump cost testthat 1.3M.
- **Remaining gap.** testthat_full at `RY_BENCH_REPEAT=2` runs 6962.2M with the JIT on against 6956.6M off. The 5.6M is feedback recording in the first windows (`run_get_var`, `run_call_builtin`, `run_call_generic`, about 1M each), which the native execution model replaces rather than tier-0 tuning.

## Tier 1 keeps the 3000-entry threshold

**Status:** built.

- **Measured.** Callgrind, eight `bench-real` workloads, two in-process repetitions, geomean of instructions against `RY_JIT=off` with the baseline off: 1000 entries and 3000 back edges 1.015, 2000/6000 1.016, 3000/10000 1.013, 6000/20000 1.010, 10000/30000 1.010.
- **Decision.** Keep 3000 and 10,000. A lower threshold compiles more bodies that do not repay their compile, and a higher one saves a few tenths of a percent on these short runs while delaying long-running programs. What decides JIT on against off on this code is not the threshold: tier 1 compiles few bodies here, and most time is builtins and call setup that tier 1 hands back to tier 0. Revisit once the baseline inlines fast paths and tier-1 compiles get cheaper.

## Tier 1 inlines block-bodied closures, and a failing inline reruns the whole call in tier 0

**Status:** built.

- **Previous shape.** `analysis.rs` inlined a call of a closure whose body is one expression computing a scalar from scalars.
- **Chosen shape.** A body `{ a <- e1; b <- e2; result }` inlines too (`lower_block`): every statement but the last assigns a symbol through base `<-` or `=`, each assignment rebinds the name in the inlined scope, and the last statement may be `return(value)`. Callee identity is guarded as before: a closure made in the same body by its type, a global one by the binding epoch.
- **Deoptimization.** An inlined body has no effects, so a guard that fails inside it does not materialize the callee's frame: the whole call hands back to tier 0, which reruns it lazily from the start. Inlining a callee with effects would need frame materialization in the deoptimization exit, which is not built.

## Install-time machine code from whole-program facts, beside the JIT

**Status:** superseded by "No baseline tier, and code images hold bytecode only". Kept for the coverage and neutral-speed measurements that entry cites.

- **Previous shape.** Install-time images (`code_image.rs`) carry tier-0 code and training-run profiles, and the plan was to write speculative machine code from those profiles into the reserved native section. Untrained images measured no speedup (decoding costs about what compiling does), and trained profiles did not help either, because loops tier up within their first call.
- **Chosen shape.** A hybrid after Bun's `--aot` (`prior-art.md`, "Bun's compiler"). `ry install` compiles machine code from facts that need no run: the checker's types, the closed bindings of the locked namespace, and the effect-free fact per function. A value from outside the package is tested once where it enters, and a failed test runs that call in tier 0, so there is no deoptimization metadata for this code. Code goes through the one SSA IR (`crates/ssa`) and `cranelift-object` into the image's native section, mapped read-only and shared between processes. Tier 0 keeps `eval` and non-standard evaluation, and the run-time JIT keeps hot loops, where profiles beat whole-program facts: Bun's own measurements put ahead-of-time code 7% to 23% behind its JIT on warm servers and at a 1.44x median slowdown on untyped idioms in a large program. `ry check --explain` reports by-name lookups and unproven tests.
- **Order.** It runs in parallel with making the baseline tier beat tier 0, because both share the IR. It is measured on the testthat workload and package load time.
- **Impact.** Start-up and short runs (package CI, the testthat workload) get compiled code without a warm-up, and code pages are shared by the processes of one CI machine. Training profiles become optional input rather than the source of speculation.
- **Built shape.** `crates/jit/src/ahead.rs` drives the native-function compiler (`native.rs`) into a `cranelift-object` module (`ssa::ObjectModule`) while `ry install` writes the image, and `ssa::section` links the object into position-independent code: calls between its functions are resolved at install, and every runtime helper is read through a link table placed 64 KiB past the code and filled when the section is mapped. Records are refused because each process numbers record layouts anew. Each function needs checks, which the manifest in the section's metadata names: a base builtin, or a sibling closure named by record entry and ordinal. A call checks them once and keeps the pass until `interp::code_image::check_epoch` moves, provided every binding read and every frame a lookup walked was locked. The epoch moves when a binding is unlocked (`unlockBinding`, `assignInNamespace`, `R_unLockBinding`), when C clears an environment's lock bit (`SET_ENVFLAGS`, `SETLEVELS`, as rlang and pkgload do), and when a closure is flagged for the debugger or a trace, because native code calls siblings directly; a sibling that carries such a flag, or whose binding holds an unforced promise while unlocked, fails its check. A damaged section is reported on the console rather than signalled, so `options(warn = 2)` cannot make a lazy-load fetch fail. The section carries an FNV-1a hash that is checked once at load, because a flipped byte in machine code is not caught by any decoder. A mapped file that differs from the hashed bytes, because it was replaced between read and map, is overwritten in the private mapping with the hashed bytes. The report is `ry install --explain` (`R CMD INSTALL --ry-explain`) rather than `ry check --explain`, because it lists what the install's compiler actually refused, which the checker cannot know.
- **Gaps.** Argument types are guesses (annotations, constant defaults, then double, integer and logical scalars and vectors, up to four specializations per closure), not the checker's inferred types, which the install cannot query yet. An argument is read at the call (`native::gather`, shared with the JIT's native entry) when it is a value, a constant, a variable, or a pure expression over them (`native::pure_value`: base `(`, `+`, `-`, `*`, `/` and `c()` of numbers without attributes), because evaluating it then runs no R code and signals nothing; integer overflow and any double operation with a `NaN` or `NA` operand are left to tier 0, which warns, or picks the payload the way its compiled arithmetic does.
- **Measured** (callgrind, images on against `RY_CODE_IMAGE=0`, JIT off). The testthat workload runs 7.850G against 7.834G instructions and `library()` of testthat, xts, data.table, purrr, stringr and lubridate 1.546G against 1.536G: few of those packages' functions are scalar numeric (rlang 57, cli 112 and testthat 38 native specializations out of 1,281, 868 and 1,089 closures), and the images' tier-0 sections already measured neutral. A top-level loop of 300,000 calls to a package's scalar functions runs 4.78G against 7.22G instructions, and the JIT on gains nothing over that, because the loop is at top level. The lever for real packages is wider coverage (checker types, arguments that are calls, vectors), not faster native code.

## The native execution model: one design for frames, calls, unboxing and install-time code

**Status:** planned. Built so far: the slot window in tier 0 and tier 1, tier-0 call plans, unboxed scalars and per-exit state in tier 1 (`runtime::Exit`); machine-to-machine sibling calls are missing, and "No baseline tier, and code images hold bytecode only" supersedes the install-time entries.

- **Question.** Package code ran 5 to 11% of its closures as machine code (rlang, cli, testthat); the rest paid tier 0's call path, a heap frame and promises per call. Frames, the calling convention, unboxing and install-time compilation each constrain the others, so they are designed together (`r-runtime.md`, "The native execution model").
- **Frame layout.** Every tier runs a call on the same register window: header (environment or `NULL`), slots (formals, then numbered locals, `...` as its `DOTSXP`), body registers. Missing bits in `Frame.missing`. The environment is made only by a reify trigger, through `frame_env`, `frame_sys_parent` or `Call::env`, and is then the only source of truth.
- **Calling convention.** Tier 0 to tier 0: a per-site plan writes arguments into the callee's slots (built). Machine to machine: a direct call to a sibling's entry in a locked namespace, guarded by the image's check epoch, pure arguments unboxed and the rest as promises over the caller's slots (planned, codegen side).
- **Refused bodies.** Permanent heap frames for bodies that bind names the compiler cannot number (`assign`, `eval`, `local`, `with`, `source`, ...) or hand out the frame (`environment()`, `sys.frame(s)`, `lockBinding`). A slot path is planned for non-literal missing defaults (promises over the slots), `sys.function` (reads `Frame.function`), `on.exit`, `match.arg` (default from the closure, value from the slot) and `do.call` with a function value and no `envir`.
- **Deoptimization.** Each exit of machine code carries a frame-state map (per live slot and register: in the window, or a machine value and its type); leaving boxes, writes the slots, and resumes tier 0 on the same window without making an environment. Machine code checks the header after any call that can reify and leaves when it is set, because the slots are then dead.
- **Boxing.** Length-1 doubles, integers and logicals without attributes stay unboxed until they escape: a slot write before an observing operation, a non-native callee, a vector or list store, a promise, or a return to tier 0.
- **Install time.** Each typable closure of a locked namespace gets an entry with its guards and argument-type checks, a body on a slot window, direct sibling calls, and its frame-state maps in the image. A failed entry guard runs the call in tier 0.
- **Ownership.** Frames, the calling convention and reification live in `crates/interp`; Cranelift codegen for unboxed scalars, direct native calls and deoptimization exits lives in `crates/jit`; vector kernels are a separate module behind a narrow interface.
- **Known gap.** Tier-1 and baseline bodies take heap frames because their code reads variables through the environment. On the functional workload with the JIT on, that is about 600M instructions of `bind_arguments`, `define_formals`, `release_frame` and `clear_frame` that the same bodies avoid with the JIT off. Closing it is the frame-state map above: machine code reading and writing slots by index, and deoptimization materializing the environment through `reify`.

## Tier 0 decides at scheduled entries and stops recording at tier-up

**Status:** built.

- **Previous shape.** Every call of compiled bytecode tested each tier decision in `run`: the start and end of recording, the first-call compile, the entry-threshold compile, a body with tier-1 code, baseline code. That was about 30 instructions per call with nothing compiling (`RY_JIT_THRESHOLDS=0,0,0` ran 44M more instructions than `RY_JIT=off` on the testthat workload, 40M of it in `run` and `execute_code`). A body with tier-1 code also kept recording type feedback in tier 0, and `interpret` tested whether a tier was installed before reading `RECORDING`.
- **Chosen shape.** Each decision happens at a known entry count, so the profile carries one more word, `profile::NEXT`: the count at which the next decision is due, or `i32::MIN` for "at the next entry". The hot path is one comparison, and `entry_event` (`#[inline(never)]`) makes the decisions and derives the next count from the counters as they are afterwards. Anything outside an entry that moves a counter or the tier state (the back-edge path, a compile, `runtime::invalidate`) sets `NEXT` to `i32::MIN`. Without a tier, code asks again every 4,096 calls, so a tier installed later still reaches it. `RECORDING` is set only with a tier installed, a compile turns it off, and a discard turns it back on for a body that can recompile; the type that broke a speculation is recorded by `jit_record` before the deoptimization either way. Code images keep the first four words, so their format is unchanged, and `NEXT` starts at `i32::MIN` on decode.
- **Recording window.** Entry recording covers the last eighth of the entry threshold (`bytecode::recording_start`), not half of it: on the testthat workload 96 bodies started recording at half the threshold and 57 of them never reached it, so they recorded for the rest of the run. A body waiting to compile because of its size (`SIZE_WAITED`) stops recording until its counter re-enters the window. Loops still record from half the back-edge threshold.
- **Impact** (callgrind, testthat workload, JIT on minus JIT off on the same build): 60.0M on main, 20.1M with this change; `RY_JIT_THRESHOLDS=0,0,0` minus off went from 29.3M to 2.2M. JIT off itself runs 7.7M fewer instructions, and `functional` with the JIT on 52M fewer. Most of the remaining gap is the plans and Cranelift work for one compile and 38 refusals.

## Tier-0 element reads and stores as fast handlers, not new ops

**Status:** built.

- **Previous shape.** Tier 0 compiled `x[[i]]` and `x[i]` to `GetFun`, `BranchUnlessPrimitive`, the subject, `BranchIfObject`, the arguments and `CallBuiltin`, and an element store to two `Guard`s and `SubAssign`. Each op ran its general handler: the function lookup walked the cache through `slot_function`, `CallBuiltin` built the builtin call, and `SubAssign` went through `assign_slot_element`. Around a 130-instruction read sat about 300 instructions of dispatch, so a scalar read or store cost 1.4 to 1.7 times GNU R at tier 0.
- **Chosen shape.** Fast handlers in the `fast_paths!` table for `BranchUnlessPrimitive`, `BranchIfObject`, `CallBuiltin` on base `[[` and `[` (`element_access::element_position` and `element_value`: a numeric scalar position, or for `[[` an ASCII name, in a plain double, integer, logical or list vector, boxed in the nursery) and `SubAssign` (a same-kind scalar into an unshared slot vector, a list's `[[`, `$` or `[` store, by position or by existing ASCII name). Each declines to the generic handler with no side effect, so the op stream, the single-step handlers and the JIT, which reads the same ops, are unchanged.
- **Cheap declines.** `CallBuiltin` is every builtin call's op, so the handler checks the argument count and the builtin's identity inline and does the read in `read_element_call`, out of line: inlined, its six-register frame setup ran for every builtin call it declined. The read checks the subscript before the subject's flags, and the store finds the position right after the target's kind. Moving the store out of line too, or screening the subscript inline before the read, was measured and rejected: each cost the taken path more (about 50 instructions per store, 20 per read) than it saved a decline.
- **Why no fused element op.** A fused `x[[i]]` op would change the op stream the JIT's analysis reads, for little more than the fast handlers give: what is left per read is the dispatch of five ops at about 10 instructions each.
- **Names by pointer.** Every ASCII `CHARSXP` is one interned object whatever its encoding mark (`heap::strings`), so for an ASCII name, pointer equality is exactly `[[`'s and `$<-`'s exact match. `NA_STRING` is never interned and matches no name. A non-ASCII name, an empty name or a name no element has declines (`element_access::name_position`).
- **Measured** (callgrind, instructions per loop iteration, tier 0, main before and after, GNU R 4.6.2 in parentheses; an empty loop costs 180 and 99): `s <- x[[i]]` 846 to 599 (553), `s <- x[2L]` 886 to 587 (492), `x[[i]] <- 5` 726 to 467 (519), `x[i] <- 5` 727 to 470 (519), `x[[i]] <- 5` on a list 708 to 486 (687), `x[i] <- list(5)` 1917 to 1050 (1941), `x$b <- k` 1006 to 678 (878), `s <- x[["b"]]` 556 to 546 (1293). Net of the loop, every scalar read and store is below GNU R at tier 0 except `x[2L]`, 4% above. What the handlers decline pays for the attempt: `s <- x[-1L]` 1761 to 1853 (2113) and a name append, `x[["b"]] <- 2` on `list(a = 1)`, 3565 to 3672 (5243).
- **What is left, and whose it is.** Under the JIT, `x[i] <- list(5)` (2428), `x$b <- k` (1080), `x$b` (1026) and `x[["b"]]` (657) cost more than at tier 0, because the JIT's fallback for those sites calls the general path in `crates/jit`. The tier-0 loop itself (`ForStep` and its frame bookkeeping, 180 against GNU R's 99) is the slot-frame machinery. Oracle cases `builtins__compiled_element_reads.R` and `assignment__compiled_element_stores.R` pin the edges.

## Computed subscripts stay in bytecode

**Status:** built.

- **Previous shape.** A compiled element store (`SubAssign`, `SubAssignMatrix`, `Replace`) evaluated its subscripts into registers, and when its in-place path declined it called `eval::complex_assign` on the whole call, which evaluated the subscripts again. The compiler therefore accepted only subscripts it could prove side-effect free (`side_effect_free_index`: locals, constants, arithmetic of those). `x[length(x) + 1L] <- k` failed that test and ran the tree walker's complex assignment, about 13,600 instructions per iteration against GNU R's 2,550.
- **Chosen shape.** `complex_assign` takes the forced arguments of a one-level target (`evaluated`, one promise per subscript holding its code and value), so the fallback reuses the registers' values and `substitute()` in an S3 `[<-` method still sees the code. The compiler accepts every subscript except `...`, `..N`, an empty argument and code that assigns the target (`Compiler::lowerable_subscript`). `bytecode::evaluated_arguments` builds the promises. A register holding a symbol counts as a formal-lookup marker only when it is the subscript's own code; otherwise the handler skips the in-place path.
- **Evaluation order.** GNU R evaluates the value, then reads the target variable, then evaluates the subscripts. When a subscript is not side-effect free, `Compiler::read_target_first` emits a read of the target into a scratch register before the subscripts, so an unbound target errors before a subscript's side effect runs, as in GNU R.
- **Accepted difference.** A subscript that calls a closure which rebinds the target with `<<-` makes GNU R assign into the value it read before the subscripts, while the in-place path reads the binding after them.
- **The stores the path now meets.** `assign_element` appends at `length(x) + 1` in place when the vector is unshared and has no attributes (tested inline, with the copy in `subset::append_copy` reserving the 5% headroom of `alloc_with_room`), and widens a store of an integer or logical into a double vector and of a logical into an integer vector without leaving the fast path. `element_access` reads a character element (`x[[i]]`, `x[i]`). `c()` of lists without attributes is one `copy_elements` per argument in `direct_c`.
- **Measured** (callgrind, instructions per loop iteration, tier 0, the difference between 40,000 and 20,000 iterations, main before and after, GNU R 4.6.2 in parentheses; an empty loop costs 216 and 99): `x[length(x) + 1L] <- k` on a double vector 8517 to 1557 (2550), `x[[length(x) + 1L]] <- k` on a list 7949 to 1622 (5445), `x[[k]] <- k` appending to a double vector 2938 to 1162 (1692) and to a list 1223 to 1207 (4710), `s <- x[w]` with a character `w` on a named list 2298 to 2255 (3326). `x <- c(x, list(k))` is quadratic in both engines; per iteration at these lengths it falls from 4.59M to 1.05M (GNU R 0.82M). Numeric reads stay within 2 instructions of main (`x[[i]]` 739 to 735, `x[i]` on integers 734 to 736): the character read sits behind a tail call to a cold function that also tests the subject's kind, because a kind test inline joins the numeric read's dispatch into a jump table that costs about 5 instructions.
- Oracle cases `assignment__computed_subscript_compiled.R`, `builtins__character_element_reads.R` and `builtins__combine_lists.R` pin evaluation order, call counts, S3 dispatch with `substitute`, coercion on append and list concatenation. `tier0_computed_subscript_stores_stay_near_gnu_r` in `crates/ry/tests/test_perf.rs` holds `x[length(x) + 1L] <- k` within 1.5 times GNU R's instructions.

## Compiled code stores list elements and kind-checked vector elements itself

**Status:** built.

- **Measured.** `rcp/vector-for` (`x <- vector(length = n)` filled by `x[[i]] <- i`) ran at 0.38x GNU R with the JIT on: the slot's type was unknown after the logical vector became an integer one, so every store stepped out to tier 0 and boxed three scalars per iteration (90M `scalar_integer` calls). `rcp/list-for` (`x[[length(x) + 1]] <- value`) ran more instructions with the JIT on than off (8.94G against 7.66G), because every list store stepped out.
- **Decision.** `analysis::sub_assign_store` picks one of three stores. `Typed` is the old path, for a slot whose vector type takes the scalar. `Checked` serves a slot of unknown or changing vector type: compiled code reads the vector's kind, attributes and flags at run time, stores an int, double or logical scalar in place when the kind takes it, and hands only a conversion or anything but a plain vector to tier 0 (`sub_assign_in_tier0`, on a cold block). `List` serves `x[[i]] <- value` on a list through `runtime::jit_store_list_element`, which stores in place or appends one element within the list's spare capacity and returns 0, changing nothing, for `NULL` (deletion), attributes (names to extend), a shared list, a list that must grow past its capacity, and a value that is the list itself (`x[[2]] <- x` must copy first).
- **Closures with native entries never tier on hot entry.** `isTRUE` and the other `NATIVE_ENTRIES` run their body only for the calls the native code declines; compiling that body bought nothing and its first compile read about 3.7 MB of the code generator's machine code into memory (`analysis::NATIVE_ENTRY`, final). On the testthat workload peak RSS is 77 MiB with the JIT on and off alike.
- **Impact.** vector-for 3.45 s to about 0.4 s with the JIT on (2.7 to 3.0 s off). list-for under callgrind: 7.00G instructions on against 8.84G off. `crates/oracle/cases/assignment__element_store_kinds.R` pins every store kind, including the declines, against GNU R in all three JIT modes.

## The road to 3x

**Status:** planned. Built so far: tier-0 quickening, the vector distribution kernels with batched samplers, and the `unlink()` rewrite; "No baseline tier, and code images hold bytecode only" supersedes the install-time machine code row.

- **Goal.** The 1.5x milestone holds on every bench-real suite within its 95% interval, so the next milestone is 3x. The 3x row of the roadmap table in `development/design/r-runtime.md` sets the factor each bucket needs: R code 5x, builtins 3x, ry's own ports 3x, GC and start-up 2x.
- **Why the R code bucket needs a new tier.** Tier 1 runs under 3% of R-level time, because workload closures are called a few thousand times and the tier-up threshold is about 3000 entries. Tuning tier 0 cannot reach 5x.
- **3x is a checkpoint, not the design target.** Every track designs for the 10x row (R code 30x, builtins and ry's ports 10x, GC and start-up 10x). A plan that can only reach 3x in its bucket is rewritten before it lands.
- **Owner and 10x end shape of each bucket.**

| Bucket | Owner | 10x end shape |
| --- | --- | --- |
| R code | JIT track (inlining tier, roadmap row (c)) | Almost all R-level time runs as machine code. Callees are inlined across closures and S3/S4 dispatch, and frames, promises and environments materialize only on deoptimization |
| R code: tier-0 interpreter (44% of testthat) | T0 | 10 to 20 instructions per simple bytecode op (about 120 today): pinned interpreter state, quickened slot-local typed ops, superinstructions, feedback recorded only until a site is quickened |
| R code | AOT track | 80% or more of package closures compiled at install, so package code is machine code from its first call |
| Builtins: call machinery, subassignment, `[`/`[[`, apply family | B5 | No allocation on any fast path, arguments passed without pairlists, results written in place when unshared; faster than GNU R's C |
| Builtins: element-wise kernels | B1 | Fused element-wise temporaries and SIMD kernels that beat GNU R's C |
| Builtins: strings and testthat hotspots | C | Interned strings with no temporaries; the testthat profile's top entries |
| ry's own ports | B3 | Vector d/p/q/r* paths, SIMD across lanes with bit-identical results, batched RNG with identical streams |
| GC | B2 | Young-survivor tracing at a fraction of today's 70 instructions per object; the GC bucket at a tenth of today's |
| Start-up | D | A mapped start-up image with no inflate and no per-object decode |
| Corpus I/O and waiting (0.7% and 9.5% of ry's corpus time) | B4, done | `unlink()` follows GNU R's `R_unlink`; children already run ry. Neither bucket bounds the corpus: most I/O is single-file `unlink`, which costs GNU R the same system call, and the waiting is the suites' own `Sys.sleep` ("unlink() follows GNU R's R_unlink", "The corpus's waiting is the suites' own sleeps") |

- **Inlining tier.**
  - Hot call sites inline their callees across closure calls, `UseMethod`/`NextMethod` and S4 dispatch, guarded by version-stamped bindings and the receiver's class.
  - Escape analysis and allocation sinking remove the inlined frames, promises, environments and scalar boxes.
  - Deoptimization rebuilds the frames, promises, `sys.calls` and `on.exit` that GNU R would show. An observation such as `sys.function`, `parent.frame`, `environment()`, `eval`, `substitute` or `missing` either deoptimizes or is answered from the deoptimization metadata.
  - Thresholds drop to the hundreds.
  - Target: the tier-1 share of R-level time goes over 60%. This follows the adopted method-JIT direction in `prior-art.md`; trace compilation stays rejected.
- **Progress tracking.** Each merge reports the bench-real suites with their intervals, and the bucket shares from `ry run --profile-summary`, against the 3x row.

## Tier 0 runs frameless fast handlers, quickened arithmetic and out-of-line loop fallbacks

**Status:** built, except the baseline tier it mentions, which "No baseline tier, and code images hold bytecode only" supersedes.

- **Previous shape.** Every tier-0 handler was one function that held the fast and the slow path, so each handler pushed and popped 4 to 6 callee-saved registers, and some reserved stack, on every instruction, even when it only read a variable. Every arithmetic or comparison ran the generic checks (ALTREP, attributes, the S4 bit, the result's allocation through the collector's entry point). An in-loop instruction with a cold fallback put the fallback inline in the loop body, so the loop's hot instructions were spread over more cache lines. The warmed loop `while (i < n) i <- i + 1L` with the JIT off ran 1,123 instructions per iteration (11 instructions, about 100 per instruction).
- **Frameless fast handlers.** Each hot instruction (`GetVar`, `SetVar`, `Guard`, `JumpUnless`, `Jump`, `Constant`, `SetSrcref`, `EndStatement`, the quickened arithmetic) has a fast handler that inlines an `#[inline(always)]` fast path making no calls at all, then either dispatches to the next instruction or tail-jumps (`become`) to the generic handler in `GENERIC_HANDLERS`. A fast path with any call in it, even on a branch that rarely runs, makes the compiler save registers in the prologue of the whole handler, which is why the heap gained `Space::allocate_from_run` and `Heap::try_alloc_scalar`: an allocation that bumps the current run and returns `None` instead of refilling or collecting. The generic handlers and `jump` are `#[inline(never)]`, or the compiler merges them back into the fast handler and its frame returns.
- **Quickening.** A generic `Arithmetic` site that computed a result on two plain scalars (one masked header-word compare, `object::is_plain_scalar`: kind, no OBJECT/ALTREP/ARRAY flags, no attributes, length 1) has its opcode byte rewritten to one of 19 specialized forms (`QUICK_ARITHMETIC`: integer and double `+ - * /` and the six comparisons). The specialized handler checks both operand kinds with one masked load each and writes the generic opcode back on a miss (`arithmetic_miss`), counting the undo in the word's top byte; at 4 undos the site stays generic. Only the opcode byte changes, so operands, pcs and the code vector's address are fixed, and `op_of` reports a specialized word as `Arithmetic`, so the verifier, code images (`serialize.rs` writes `op_word(op)`), the baseline and tier 1 never see a specialized opcode. Rejected: a separate quickened copy of the code, because the JIT and AOT tracks key feedback and deoptimization targets by pc.
- **Compare and branch.** A quickened comparison whose next instruction is a `JumpUnless` on its destination takes the branch itself and records the branch direction (`feedback::BRANCH_TAKEN`, `BRANCH_NOT_TAKEN`), saving one dispatch per loop test.
- **Feedback.** Value-kind feedback is a monotone OR of bits, read with `bytecode::feedback_bits(code, pc)`; feedback, entry and back-edge counters stay keyed by the compiled pc.
- **Loop layout.** The compiler queues the cold fallback blocks of instructions inside a loop and emits them after the loop's back edge (`back_edge` in the compiler), each ending with a `Resume` to the instruction after it, so the hot body is contiguous.
- **Step seam.** `bytecode::step(machine, pc)` runs exactly one instruction of an activation and returns the next pc or the returned value. `interp::testing::check_resume_at_every_pc` uses it with a test tier that leaves `f` after N steps through `TierExit::Resume` (the path the JIT's deoptimization exit takes), for every N, and compares against tier 0 alone; `fuzz_tier_zero_resumes_at_every_pc` runs it on fuzzed programs. Any pc is a valid entry, and no dispatch state lives in host locals across instructions.
- **Impact.** The warmed loop runs 406 instructions per iteration (2.77x fewer, about 37 per instruction). The remaining cost per iteration is three `Guard`s (about 34 each, a binding-cell and epoch check), three `GetVar`s (about 31 each) and `SetVar` (52, with the missing-flag clear). The 10x end shape needs: the guard folded into a per-body version stamp checked once per entry, pinned handler arguments (code, registers, pc) so handlers stop reloading them from the activation, and the visible flag set once per statement rather than per instruction.

## Tier 1 compile throughput: linear SSA construction and one compile per body

**Status:** built, except the weak reference from a body to its code, which "Closures over one body expression share its compiled code" supersedes.

- **Measured on the testthat workload** (`RY_JIT=force`, two warm repeats): 799 compiles and
  about 4.0 s of codegen before, 709 compiles and 3.1 s after. Bodies under 250 words cost about
  10 µs per word, almost all of it Cranelift's fixed cost per function; bodies over 3,000 words
  cost about 17 µs per word and are 12 compiles taking 1.8 s.
- **SSA construction was superlinear.** `ssa::Builder` resolves variables only when
  `seal_all_blocks` runs, and every block between a use and its definition got a parameter that
  trivial-parameter removal then deleted. For `[<-.data.frame` (14,819 words, 10k blocks) that
  was 420k parameters created for 3.9k kept. `read_variable` now walks single-predecessor chains
  once the predecessors are known, which creates 99k and keeps the same 3.9k; translation went
  from 95 to 54 ms.
- **The rest was quadratic in merges times slots, and in register allocation.** With the
  steps below, testthat's forced codegen fell from 3.1 to 1.4 s, and the 12 bodies over 3,000
  words went from 17 to 7.1 µs per word (5.3 for bodies under 250 words), so cost is linear in
  body size and no size cap or hotness threshold is needed. For `[<-.data.frame` (14,819 words)
  one compile went from about 300 to about 125 ms.
  - Each slot's run-time status was an SSA variable live across the whole body, so every merge
    got a parameter per slot: 47k parameters over 8k blocks, and a `PureUnlessObserved` step
    redefined all of them. Statuses now live in a stack array of 4-byte cells
    (`Translator::statuses`): 8.6k parameters, and seal time fell from 24 to 5 ms. A reset
    touches only the slots the state may have loaded, behind a cold branch.
  - A tier 1 body without a loop compiles with Cranelift's single-pass register allocator
    (`ssa::Allocator`), and one with a loop with the backtracking allocator. Measured after the
    status change, on testthat's forced codegen and the untyped `bench-why` kernels (best warm
    time, `TMPX` override, not committed):

    | Policy | testthat codegen | Loop kernels vs backtracking |
    |---|---|---|
    | Single-pass everywhere | 1.14 s | 1.5 to 2.4x slower (`loop_sum` 2.4, `array_filter` 2.0) |
    | Backtracking everywhere | 1.67 s | 1.0 |
    | By loop (chosen) | 1.44 s | 1.0 |

    Call-heavy kernels (`report`, `validate`, `orders`) ran the same under both. Both allocators
    are linear in body size after the status change (7.0 and 7.5 µs per word for small and big
    bodies under backtracking). Cranelift fixes the allocator per module, so each has its own
    pool. If big cold bodies with loops turn out to matter, the next step is compiling them
    single-pass and recompiling with backtracking once their loops get hot, not a size cap.
  - A deopt exit writes only the loops around its pc, since tier 0 reads a loop's words only
    inside that loop. Writing every loop made each exit grow with the body's loop count and kept
    every loop variable live everywhere.
  - A variable load inlines only the slot-frame hit; frame walks and the environment probe run
    in `runtime::jit_probe`, whose result tags the enclosing status in its low bit.
  - Trivial-parameter removal is a worklist over each parameter's users rather than repeated
    sweeps.
- **Why one body compiled up to four times**, each fixed at its source:
  - A failed speculation on an instruction compiled code steps through tier 0 was never recorded
    as feedback, so each recompile made the same bet. `interpret::<SINGLE>` records always.
  - `long_lived` counted any environment whose parent is the base namespace, which includes every
    call frame of a base function, so code speculating on `tryCatch`'s local functions was
    portable and failed its proof in every other frame. It now needs the `name` attribute an
    imports environment has.
  - The slot check of a closure a slot frame made compared the closure's environment with the
    frame's, which is nil until the frame reifies; the closure holds the frame's pending
    environment (`eval::pending_env_of`).
  - Code that speculates on a function local to its enclosure dies with that enclosure. The
    first time a body's code meets a second enclosure, the tier state gets `runtime::SHARED`, and
    its compiles keep only speculations that every enclosure resolves the same way, so one
    portable compile serves every call of `tryCatch` and friends. A body that only one long call
    runs keeps its per-enclosure speculations.
  - Closures the AST evaluator makes from one `function` expression share the body object, and
    `bytecode::State::bodies` (the per-body call counter) holds that body's code once compiled,
    through a weak reference keyed on the body, so the code lives exactly as long as its source.
    Formals and srcref must match too, since compiling reads them. Residual: rlang's argument
    capture (`expect_equal(f(vapply(..., function(r) ...)))`) hands back a copied expression, so
    such a lambda still compiles once per test block (73 small compiles, about 40 ms).

## A compiled element op costs no more than tier 0's handler

**Status:** built.

- Rule: a JIT-compiled element operation never costs more instructions per iteration than tier 0's handler for the same shape. Pinned by `element_ops_cost_no_more_than_tier_zero` in `crates/jit/tests/test_tiers.rs`, which runs each shape in a child of the test binary under cachegrind (no hardware counters in the cloud VMs) with the JIT off and forced; without valgrind it passes with a note.
- Previous shape: an element op the code generator did not inline (`x[[i]]` and `x[2L]` on named lists, `x$b`, `x[["b"]]`, every `SubAssign` it did not specialize) ran as an opaque step: write back every variable, run `step_one` with no fast paths, forget every variable and reload them on the next iteration. That cost 1.2 to 2.7 times tier 0's instructions.
- Chosen shape: such a step first calls `bytecode::fast_step` (through `runtime::jit_fast_step`), which runs tier 0's own fast handler (`call_builtin_fast`, `sub_assign_fast`, `field_fast`) and skips the write-back and reload when it applies. `Action::Step` carries `Fast::{No, Read, Store(slot)}`; a store writes back only its own variable first. One handler serves both tiers, so the two cannot drift.
- A fast store changes the frame's object in place, so a variable compiled code holds as that object stays loaded; only an unboxed copy is forgotten.
- Boxing an unboxed register for tier 0 reuses an object tier 0 already has instead of allocating: the constant pool's object for a constant, and the variable's frame cell for a clean variable copy (`Translator::box_for_tier0`). A dirty variable is written back once at its first boxing, so later iterations reuse it.
- Tier 0 gained `field_fast` for `x$name` on a plain named list, which also cuts tier 0's own cost of that shape by about a third.
- Where the fast handler declines, `bytecode::quiet_step` (through `runtime::jit_quiet_step`) runs the generic handler without any write-back when it cannot run R code: no calling handler is established (a warning would run one) and no operand is an object, an ALTREP vector, an environment or a promise. Only otherwise does the step write back every variable. This is what keeps a range subscript (`v[k:4L]`), an append that converts the vector (`w[[3L]] <- k`) and a partial `$` match under tier 0's cost.
- The two calls stay separate and out of line. Running the generic handler from `fast_step` hands it the fast handlers' activation, which then escapes, and every fast hit pays about 90 instructions of stores for fields it never reads; one combined helper behind a panic guard costs the hit path a stack frame.
- The fast handler is skipped where it can only decline: a `[` or `[[` read whose index is typed as a vector, and a store into a vector of known atomic type that the compiled store declined (the handler takes only what that store takes).
- `$` steps as pure unless observed, like `[[`: the observed check sends an object, an ALTREP vector or an environment (whose promises and active bindings run R code) the opaque way.
- Writing a variable back into a slot frame uses the slot's position in the code's slot table, fixed at compile time (`Compiled::frame`), not a search of the table's names, and a boxed value is stored the way `SetVar`'s fast handler stores it.
- **Code shared between bodies is found by the body's address, and the collector drops dead bodies' entries** (`Bodies::clear_dead` in `bytecode.rs`). A body is any language object, with no spare field for a dense index, so the table is keyed by address, and only an uncompiled body reaches it, because a compiled closure's body slot is its code. A table that held a weak pair from body to code once kept an entry while the pair's key was live, but the collector clears that key to `NULL` before `clear_dead` runs and `NULL` counts as live, so no dead entry was ever dropped. A new body the allocator placed at a dead body's address then ran the dead body's code (wrong results, or `NULL`; `fuzz_argument_expressions` failed only in some test orders). `clear_dead` drops an entry, and a call count, once its body dies; it runs before anything is freed, so no address is reused while a stale entry exists. `a_body_at_a_dead_bodys_address_runs_its_own_code` pins it.

## Tier 0 compiles whole bodies

**Status:** built.

- Tier 0 compiles a closure body whole, at its second call or first call if it has a loop (`bytecode::compile_before_call`). Deferring large `if` arms to a stub compiled at the arm's first run was built and measured, and it loses, so do not retry it without a new idea.
- Callgrind, `library(testthat)` with packages installed by GNU R (so no `.ryc` code): compile fell from 34.5M to 29.8M instructions, but the whole run rose from 229.0M to 230.0M. On `testthat_full`, compile fell 6M and the run rose 11M. Most compiled code in a hot body is on paths that run, and an arm that runs often pays a child `Machine` entry each time (`op_deferred`, `execute_code` and `grow_registers` together about 17M).
- Recompiling the body with hot arms inline at their eighth run fixes the entry cost but doubles compile on `testthat_full` (109M to 235M) and adds 96M of collection, since every recompile is a second full code object.
- Raising the call threshold from 2 to 4 also does not help (load 232.4M, `testthat_full` 5,164M): the bodies compiled during load are hot or loop.
- What remains of the compile share is the cost per compiled body (`Compiler::expression` is about 60% of it) and code images: a package ry installs carries compiled code in `.ryc`, so its load compiles nothing.

## Tier-0 call entry: raw pool reads, an out-of-line tier schedule, and a cleared register window

**Status:** built.

- **The entry reads the constant pool raw** (`pool_entry`, `pool_ints` in `bytecode.rs`). The compiler builds the pool and the vectors in its fixed entries (`REGISTERS`, `PROFILE`, `FEEDBACK`, `SLOTS`, `EPOCHS`, `HINTS`, `LOOPS`), so none is ALTREP and the element accessor's ALTREP test is dead weight on every call. Debug builds assert it. Code objects and `.ryc` images are unchanged.
- **The tiers' scheduled entry is out of line** (`run_scheduled_entry`), taken only when the entry count reaches the profile's `NEXT`, so `execute_code` inlines into `run_at` and the common call builds one frame instead of two.
- **`holding` counts references in place** over the held registers instead of copying them into a `SmallVec`; the held registers belong to the running activation, and the callee only uses registers above the file's length.
- **A global `GetVar` reads through the site's cached cell**, as `GetFun` does, and a cached cell holding a forced promise answers with its value.
- **The register window is cleared on every grow, and must stay so.** Growing without a fill (nil only above a high-water mark, nil `[len, high_water)` at each collection) was built and is sound on the non-moving heap, but a register a frame has not written yet then holds what a returned call left there, and the collector traces it. A `gc()` inside a later, wider window kept those objects alive, so finalizers ran later than in GNU R (`bytecode__finalizer_under_wide_window`, `bytecode__register_reuse_under_gc`). A frame's unwritten registers cannot be told from its written ones without liveness maps, so there is no cheap repair at collection time. The clear is about 16 of `grow_registers`' 57 instructions per call. The slot-frame binders also rely on it: the header register reads as "no environment yet" because the clear wrote nil there.
- **GNU R's compiled `for` keeps its last values on its node stack**, so a finalizer's timing after a `for` loop depends on whether GNU R's JIT is on (`R_ENABLE_JIT=0` and `3` disagree). Oracle cases about collection timing make their objects with `lapply` or a function call, where both GNU R modes agree.

## Tier-0 global guards read two bits and the base binding

**Status:** built.

- **Previous shape.** A tier-0 `Guard` on a base function proved the binding unchanged through its site's cache: the site's lookup stamp (symbol epoch plus heap enclosure epoch) against the stored one, then the cached enclosure and cell. Any reparenting moved the enclosure epoch for every site, and a miss walked the chain (`lookup_enclosing_function`).
- **Target shape.** The heap answers the question on the write side, and the guard reads the answer:
  - `gp::BOUND_ELSEWHERE` on a symbol is set the first time an attached frame that is not a symbol frame binds it. A detached frame marks nothing on insert, and `set_enclosure` marks every symbol of a frame when it goes from detached to attached. Marking detached frames eagerly defeated the guard: GNU R's `.ArgsEnv` and `.GenericArgsEnv` hang off the empty environment and bind every primitive, which marked 80% of guarded names. `Frame::insert`, `Frame::insert_formals` and `Frame::from_image` are the only code that adds a binding, and they are crate-private, so every write route (`<-`, `<<-`, `assign`, `delayedAssign`, `makeActiveBinding`, `list2env`, the environment forms of `$<-` and `[[<-` (`[<-` on an environment is an error in R), `attach`, `sys.source`, namespace loading and imports, and the C API's `Rf_defineVar`, `Rf_setVar`, `R_MakeActiveBinding`) passes through it. The test is a byte load and a branch before the store.
  - `gp::DETACHED` on an environment means its chain may not reach base. The empty environment has it, a new environment inherits it from its enclosure, `set_enclosure` sets or clears it from the new parent (a symbol frame is always attached), and `drop_enclosure` sets it.
  - When an environment that already has children (`ENCLOSING`) becomes detached, those children still claim to be attached. That case sets `Heap::everything_bound_elsewhere` once, which marks every symbol and every symbol interned later, and is persisted in the start image.
  - With neither bit set and no local probe, the only binding any frame on the chain can hold is base's, so the guard compares `symbol.cdr().cdr()` (the base binding cell's value) against the expected function, looking through a forced promise because lazy-loaded base binds most functions as one. Otherwise it falls back to the per-site stamp check and then the full lookup.
- **Monotonic by design.** Neither bit is ever cleared on a symbol, so there is no reset path to get wrong; a name once bound outside base keeps today's per-site check. `DETACHED` on an environment is cleared only when it is reparented onto an attached chain. Lexical probes (a frame that may bind the name) keep their own check.
- **Known gap.** A cyclic enclosure chain keeps whatever bit it had when the cycle closed. GNU R loops forever on a lookup through such a chain.
- **Measured.** Release build, callgrind against main 8dda3df. testthat_full over five repetitions, JIT off: 4,145.6M to 4,132.8M instructions (-0.31%), with `run_guard` 26.1M to 13.7M; JIT on -0.30%. The applications move between -0.20% and +0.13% (records, where most guarded names are bound outside base and the failed bit test adds about 1.6 instructions per guard). An environment write costs 940 instructions against 934. Wall clock over ten paired rounds: geometric mean -0.39% ± 1.74%, peak RSS unchanged. The ceiling was low from the start: guards are 2.8% of testthat_full, and a passing bit test does about the same loads as the stamp check it replaces, so only the slow path (at most 1.0%) can go.
- **Coverage.** `fuzz_shadowed_guards_agree_with_the_ast_evaluator` (`test_fuzz.rs`) rebinds a guarded name through every write route and every reparenting and compares compiled calls with the AST evaluator; removing either mark makes it fail. The oracle case `environments__guarded_base_calls_see_rebinding` pins each route against GNU R, including `rlang::env_bind` (the C API), a shadowing inside a function that already ran compiled, and the global reparenting trip.

## Tier-0 superinstructions do not pay

**Status:** built.

- **Built and measured, not landed.** Eight fused pairs (guard plus `GetFun`, `GetVar` plus `Element` and so on) covered 38.8% of dispatches on testthat_full. Instructions moved +0.08% and the dispatch group fell only from 11.68% to 11.50% of instructions. Paired wall time on one build, with fusion off as the baseline, was +2.86% ± 3.46% on testthat_full (12 pairs) and -0.57% ± 2.59% as the applications geomean (5 rounds).
- **Why.** Tier-0 dispatch is already threaded (each handler tail-calls the next), so fusing a pair saves one indirect jump while both fast paths still run in full. The lever is work per instruction (guards, lookups, call entry), not the number of dispatches.
- **Guard dedup is not worth building either.** Guard is about 19% of dispatches. A guard of a symbol already guarded in the same activation with no impure op in between is 1.5% of guards (3.9% if variable reads and writes count as pure), no guard is loop-invariant because every loop body makes a call, and even the same symbol guarded anywhere earlier in the frame is only 27.5%. Almost every repeat follows a call that could rebind the symbol, which is why the guard itself was made cheap instead ("Tier-0 global guards read two bits and the base binding").
- The design is kept on the branch `perf/tier0-superinstructions`: one `superinstructions!` table, a rewrite of only the first word of each pair so every pc and deopt point is unchanged, and a fuzz check against the unfused pairs.

## Closures over one body expression share its compiled code

**Status:** built, except "formals and srcref are the whole key", which "Compiled code binds GNU R's inlined base functions at compile time" supersedes.

- **Previous shape.** Tier 0 kept a map from each body to its call count and, once compiled, a weak pair from the body to its code with the formals and srcref it compiled with (`State::bodies`). The weak pair kept the code alive as long as the body, even once no closure ran it, and nothing noticed a body changed in place through the C API, so a new closure over it ran the old code. Before that map, tier 0 counted calls and kept compiled code per closure, so every closure the AST evaluator made from one `function` expression, and every copy `environment<-` made, compiled its body again; R6 copies each method into every object that way.
- **Shape.** `Bodies` in `crates/interp/src/bytecode.rs` keys the tier state by the body expression. It counts calls across all closures over the body and, once the body compiles, records the code, the formals and srcref it was compiled with, and a structural hash. A later closure over the body runs that code from its first call when its formals and srcref are the same objects and the hash matches. The entry holds all four objects untraced: `Bodies::clear_dead` (called from `RootSource::clear_dead`, beside `CallPlans`, with no heap traversal of its own) drops it in the collection that finds any of them dead. Code therefore lives exactly as long as a closure that runs it, as on a per-closure design, and peak RSS is no higher. A weak reference from body to code was tried first and kept every compiled body alive while its source lived, which cost about 1 MiB of peak RSS on each app; a quoted `function` expression or a package's body outlives its closures.
- **Why formals and srcref are the whole key.** Compiling reads the body, the formals (local symbols, slot layout, `tail_updates`, `effects::summarize`) and the srcref attribute. It reads the enclosure only through `enclosing_function`, whose results (base primitives, fusion heads, library leaves) are all emitted behind a run-time guard that falls back to the generic call. So code compiled for one environment is correct in every other, as `Op::Closure` already assumed. The same body under other formals compiles separately; the first entry stays, and the other closure keeps its own code.
- **In-place changes.** A body changed through the C API (`SETCAR` on the body or a nested call) keeps its address. The structural hash (`shape`: the tree of calls and pairlists, symbols by identity, scalar constants by value, everything else by address, the same choice as GNU R's `hashexpr1`) is checked when a closure takes shared code, so a new closure over a changed body compiles the change. A closure that already runs the code keeps it, as in GNU R, whose JIT cache (`JIT_cache` in `eval.c`) is keyed by the same kind of hash and also leaves compiled closures alone. The check costs nothing at the C boundary; a mutation hook in the setters was the alternative, and libr's `SETCAR` stays untouched.
- **Against GNU R and a slot on the call.** GNU R's JIT cache (`JIT_cache` in `eval.c`) is a fixed table of 1,024 entries indexed by a structural hash of the body and confirmed with `identical`; it shares code across fresh copies of an expression, holds each entry's code until a hash collision evicts it, and forgets a body once 1,024 others displace it. `Bodies` keys by the body's address, so it costs one hash lookup per call before tier-up and shares only one body object's closures, but needs no eviction and frees code with its last closure. A slot on the `LANGSXP` pointing at its code was rejected: it would grow every pairlist node by a fifth, and pairlists dominate the heap, which breaks the peak RSS rule. The structural hash runs when a body compiles and when a closure takes shared code, once per closure, iteratively so a deeply nested body cannot overflow the stack. The walk loops over each call's arguments and pushes only nested calls, so a long left-nested chain such as `a + b + c + ...` costs one stack slot per level; a recursive walk cost about 9K Ir per compiled body, and with this walk the strings and report apps, which recompile little, measure flat (plain release, two iterations: +0.016% and -0.02% Ir).
- **Code images.** The image writer takes each recorded closure's code through `bytecode::compiled_body`, so a closure whose body another closure compiled is written with that shared code. Images name any base closure a constant holds (`tag::BASE_FUNCTION` in `serialize.rs`): the guard of an inlined safe base internal keeps base's closure (`match`, `nchar`, `inherits`) as a constant.
- **Pinned by** `bytecode__shared_body_environments` (one quoted `function` evaluated in six environments, one rebinding `*`; one body under three formals lists; `environment<-` on a compiled closure), `bytecode__shared_body_r6` (non-portable classes sharing a method, one with a field `length` shadowing base's), and `bytecode__shared_body_mutation` (a SHLIB routine changing the body at the top level and at a nested node).
- **Measured** with plain release builds, two iterations per run (`RY_BENCH_REPEAT=2`, R_LIBS built by GNU R 4.6.2). Against the per-closure tier this replaced, the objects app (`tools/bench/applications/objects.R`: S3, S4 and R6 classes, whose R6 objects each copy their methods) went from 53.8 to 42.6 MiB anonymous peak (-21%) and from 8,075.6M to 7,734.3M Ir (-4.2%), and compiles fell from 6,605 to 246. Against the weak-pair map, which already shared code, objects goes from 7,598.7M to 7,584.3M Ir (-0.19%) and 42.86 to 42.59 MiB peak, and `testthat_full` (testthat 3.3.2) from 2,103.0M to 2,099.2M Ir (-0.18%) with 573 compiles instead of 585. Most compiles left on `testthat_full` are lambdas inside `expect_*()` arguments, whose quosure path evaluates a fresh copy of the expression each time, so the body is a new object. GNU R shares those too, because its cache compares bodies with `identical` (`backlog.md`, "Open: untyped R against GNU R").
- **Peak RSS and purge timing.** Against the weak-pair map, `testthat_full`'s anonymous peak rises from a median of 41.56 to 41.99 MiB under mimalloc's default policy, although the old generation is 0.3 to 0.4 MB smaller after every collection. Freeing code with its last closure returns its Rust-side buffers to mimalloc pages that are not yet purged at the peak: with `MIMALLOC_PURGE_DELAY=0` the medians are 40.52 on the old shape and 40.39 MiB here.

## Top-level loops compile per evaluation, without a call frame or a tier

**Status:** built.

- **Previous shape.** Only closure bodies compiled, so a loop the tree walker evaluated (top-level code, `source()`, `eval()`) ran about 6x slower than GNU R, which compiles every loop it evaluates in the global environment.
- **Target shape.** `bytecode::run_loop`, called from `do_for`, `do_while` and `do_repeat`, compiles the loop with no local symbols (every variable is an environment binding) and runs it in an activation with no call frame, before its first iteration (see "Compiled code binds GNU R's inlined base functions at compile time"). Only the global environment qualifies, as in GNU R. The design and the open broadening step are in `docs/.../development/design/top-level-loops.md`.
- **No code cache and no marker on the call.** The code is dropped after the evaluation, so a loop evaluated again compiles again, as in GNU R. A gp bit on the call would be serialized with it (`libr/src/serialize.rs` writes a language object's gp bits as its levels) and change `digest()` of a quoted loop.
- **No optimizing tier.** `profile::TIER_OFF` keeps entry scheduling and tier 1 away from this code, because tier 1's frames and resume points assume a call frame. The tier-1 planner already takes `analysis::Frame::Global { env }` (no locals, no slots, variables as tier-0 steps, lookups speculated from the global environment, which `Machine::enclosure()` skips); wiring the entry is the remaining step, listed in the design page.
- **One path per loop for compact sequences.** A global `for` over `1:n` runs compiled, and `ForInit` steps the compact sequence. Every loop that stays in the tree walker (other environments, a `return` in the body, the debugger) uses `do_for`'s own stepping, so `do_for` tries `run_loop` first and only then decides whether to expand the sequence.
- **Pinned by** the oracle case `base__top_level_loops_compile`.

## No baseline tier, and code images hold bytecode only

**Status:** built.

- **Previous shape.** A code image carried a native section after its streams: install-time machine code for scalar numeric closures (`crates/jit/src/ahead.rs`, linked position-independent by `ssa::section` through `cranelift-object`), with a link table for runtime helpers, a manifest of checks, and a check epoch moved by every binding unlock, `SET_ENVFLAGS`, `SETLEVELS` and debugger flag. The next step was to write each closure's baseline code there too, so package code would skip the run-time compile.
- **Why it went.** The native functions covered 5 to 11% of closures and measured neutral (the superseded entry above). Baseline code for every body then had to beat tier 0 on execution alone, and it does not. Forced baseline (`RY_JIT=baseline`, iterate build, callgrind on `testthat_full`) runs 4.280G instructions against tier 0's 3.982G (+7.5%); 175M of that is compiling 1,172 bodies (about 150k each), which an image would remove, leaving 4.105G (+3.1%) for execution. Per operation against tier 0: `CallBuiltin` about +40M (the slow step costs 44 instructions per call over 703k calls, 16 of them the step wrapper), `GetFun` +25M, `CallGeneric` about even (the shared step doubles to 85.8M but `run_call_generic` drops 60M), and the generated code itself about 46M.
- **The ceiling.** What baseline code can remove at most is tier 0's dispatch, so it was measured directly: callgrind `--dump-instr=yes` on tier 0, every executed instruction of the `interp::bytecode` handlers classified by its disassembly. Code word reads 49.3M (1.24%), loading the state's code and register pointers 20.2M (0.51%), the stop check 17.3M (0.44%), the dispatch table address 9.7M (0.24%) and the indirect jump 9.7M (0.24%, 9.65M instructions dispatched) total 106.1M, 2.66%; with the next-pc bookkeeping about 125M, 3.1%. A tier whose best case is 3% under its measured 3.1% execution loss is not worth an image section, an ABI of step indices and a link table.
- **The run-time baseline tier goes too.** The same ceiling binds it, and with its compile at run time it measured +6.7% instructions on `testthat_full`, so it is not kept even as an opt-in: an opt-in nobody should turn on is a maintenance cost (a second assembler for x86-64 and aarch64, a `step_*` copy of every handler, a parked-panic path, a constants slot and two threshold checks on every entry and back edge) with no user.
- **Chosen shape.** Images hold bytecode, profiles and streams only (`interp::code_image`, layout magic `RYCODE\0\3`). Two tiers remain: tier 0 and tier 1 (Cranelift through `crates/ssa`). Deleted, with git history keeping them: `ahead.rs`, `ssa::section`, `ssa::ObjectModule`, symbol imports in the SSA IR, the check epoch and `set_flags_from_c`, `ry install --explain` and `R CMD INSTALL --ry-explain`, `crates/jit/src/baseline.rs` and `assembler.rs`, `RY_JIT=baseline`, the `BASELINE` constants slot (so `SLOTS` is 10 and `FIRST_CONSTANT` 11), `BASELINE_STEPS` and every `step_*` function, `Tier::baseline`, and the third field of `RY_JIT_THRESHOLDS` (now `ENTRIES,BACK_EDGES`). Tier 1 keeps what it uses: `bytecode::interp_layout` (the `Interp` offsets it writes), `heap::object::header_layout` and `heap::env::layout`. Ahead-of-time machine code remains the plan for strict typed code only, where types are facts (`backlog.md`, "Parked: ahead-of-time compilation of typed ry"). Speed for untyped code comes from cheaper op bodies, not from removing dispatch.
- **Impact.** About 6,000 fewer lines; no executable mappings from package files; a binding unlock no longer bumps a global epoch; tier 0's entry schedule and back-edge check each lose a comparison.

## Compiled code calls an active binding at every read

**Status:** built.

- **Previous shape.** `jit_load` read a variable through `bytecode::load_variable`, which calls an active binding's function, and the analysis then marked the slot loaded, so later reads in the same activation reused that one value. A loop of 2000 reads of a binding called it 3 times in `RY_JIT=force`, against 2001 in tier 0.
- **Detection.** `eval::cell_value` counts calls of active bindings in `Interp::active_binding_reads`, a plain field because the interpreter is single-threaded. `jit_load` compares the count around the load. When it changed and the variable is not in the volatile bitset of the running code's `Compiled` record, it sets `feedback::ACTIVE_BINDING` on the `GetVar`'s feedback and returns `LOAD_INVALIDATED`, whose deoptimization resumes tier 0 after the load, so the binding is not called twice for that read. A count rather than a check of the cell keeps `load_variable` unchanged, and a binding read while forcing a promise only costs that variable its reuse.
- **Compiled shape.** `collect_slots` marks the slot of every `GetVar` whose feedback carries the bit as volatile. A volatile read always loads, as a boxed value, with `forcing` set, because the binding runs R code that may read and assign the body's variables. The load leaves the slot unloaded, both in the analysis state and in the runtime status codegen stores, so the next read calls the binding again. Feedback is never cleared on a recompile, which makes the first bailout the last one.
- **Cost.** A load that reaches the runtime pays two reads of that field and a compare. Inline probes and loads of already loaded slots are unchanged. The volatile set lives in `Compiled` rather than in a fourth `jit_load` argument: the argument changed nothing on the fast path, yet the backtracking allocator then left `array_dot`'s inner loop one instruction longer (+1% Ir).
- **Pinned by** `active_bindings_are_called_at_every_compiled_read` in `crates/jit/tests/test_tiers.rs`, which allows compiles and one deoptimization only in the first of five calls (it measures 3 and 1, then none), and the oracle case `environments__active_bindings_in_loops`, which also covers super-assignment through a binding. GNU R's byte code calls a binding more than once per read in a loop, so the case checks at least one fresh value per read rather than an exact count.

## Compiled code binds GNU R's inlined base functions at compile time

**Status:** built.

- **Previous shape.** Every compiled call of a base function GNU R inlines carried an `Op::Guard` (or a `GetFun` lookup) that checked at run time that the name still found base's function, with a cold fallback per guard. A function that masked `+` in the middle of a compiled loop therefore took effect at once, where GNU R, which binds `+` when it compiles, ignores it: `for (i in 1:40) { if (i == 10) attach(list(\`+\` = minus)); total <- total + 1 }` printed -22 on ry and 40 on GNU R.
- **Target shape.** ry binds exactly where GNU R 4.6.2 at its default level would, with GNU R's schedule (`bytecode::schedule`, mirroring `R_CheckJIT` with `JIT_score`; the gp bits `CLOSURE_MAYBE_JIT` and `CLOSURE_NO_JIT` sit at GNU R's positions) and GNU R's rules (`base_binding_at_compile`, `compiler_locals` for `findLocals`, `handler_binds` for the `::`, `local` and `browser` exceptions). The compile-time frames are `Compiler::binding`, innermost first; a nested function compiles eagerly with its outer one (`nested_body`). Bound code carries `gp::BINDING_CODE`. Top-level loops compile at their start, with the loop's own `findLocals` as their one frame, as GNU R's `compile` adds them. Code GNU R never compiles, and typed code, keeps ry's own counted schedule and its guards.
- **Sharing bound code across closures over one body.** `Bodies` (one compiled entry per body, shared by every closure over it, as for R6 methods) stores, for bound code, the closure's top environment and a sorted snapshot of the names its formals and local frames bound (`compile_environment`). A closure takes that code only under GNU R's `jit_env_match` (`binds_like`): same top environment, and every local name it binds is in the snapshot, because compiling depends on which names are bound, not their values. Guarded code serves only closures that guard too (`binds_at_compile`). A closure that would bind but fails the match compiles guarded code, matching GNU R, which marks it `NOJIT` and evaluates it uncompiled. Checking only the enclosure's identity would keep R6 objects' methods from sharing code; checking nothing would let a closure that masks `+` locally run a sibling's code that bound it.
- **One guard mechanism.** `Compiler::guard` is the one place that decides between binding and guarding; a bound site records `UNGUARDED`, which fusion and the cold-fallback path skip, so bound code has no guards and no cold blocks.
- **Known gaps.** Promise code compiles at its call site's first run, binding what the names find then against the function's own locals. GNU R's JIT cache shares code between structurally equal global bodies; ry does not.
- **Pinned by** the oracle case `base__compile_time_binding`; the rules are in `r-runtime.md`, "When code compiles and what it binds".

## Tier 1 leaves compiled code through frame states

**Status:** built, except the list under "Not built" below: frame states in the SSA IR with one shared exit path, closure calls inlined without a frame, exits that rebuild every inlined frame, and lazy arguments passed by name are built.

- **Previous shape.** `crates/jit/src/codegen.rs` wrote every deoptimization exit by hand: one cold block per exit that boxed the unboxed registers, flushed the dirty variable slots and stored the loop words with code emitted at that site, then called `jit_deopt`. An exit described one frame only, the compiled function's own, so nothing could describe a callee that had no interpreter frame. That is why tier 1 inlined only callees that provably need no frame (`effects::effect_free_lookups`) and reran the whole call in tier 0 when an inlined body failed.
- **Chosen shape (built).** A point where execution can leave compiled code is a `FrameState` of the SSA IR (`crates/ssa/src/ssa.rs`): a run of `Source` cells (an SSA variable, or a cell of a stack slot) in one flat `sources` array, and an optional parent, which is the state of the caller at the call the frame was inlined at. The front end only declares states (`Builder::frame_state`) and branches to `Builder::exit_block(state)`. `Builder::emit_exits` writes all exit code once: an exit saves its own cells into one stack buffer at the state's `base`, then jumps to its parent's chain block, which saves the parent's cells below, so the buffer holds the outermost frame first. Every exit ends in one shared block that calls `jit_deopt_exit(activation, buffer, state number)`.
- **The runtime decodes, the code only saves.** `Compiled.exits[number]` (`crates/jit/src/runtime.rs`, `Exit`) says what each saved cell is: which registers hold an unboxed integer, double or number, which variable slots may be dirty, and which loops are live. `jit_deopt_exit` boxes the registers, flushes each slot whose saved runtime status is dirty, writes the loop words, and hands the frame to tier 0 at the exit's pc. An exit costs a few stores in cold code instead of a call per boxed value, and the hot path carries no exit code at all.
- **Inlined closure calls run without a frame (built).** At a call whose callee is a known closure, the planner plans the callee's own bytecode as a nested `Plan` (`analysis.rs`, `frame`, `Action::Frame`), and the caller's code runs it in place (`codegen.rs`, `inlined_call`). The caller computes the arguments, calls `jit_enter_frame`, which reserves the callee's register window above the caller's in the register file and returns the call's activation, and translates the callee's instructions with their own SSA variables. Each `Return` of the callee jumps to one continuation block with the value, unboxed when every return has the same scalar type. While the call stays in compiled code it allocates no environment, no promise and no frame record. `jit_enter_frame` returns null when the call needs its frame from the start, as tier 0's own frame entry decides, or when a calling handler could look at the stack, and the caller then hands the call instruction to tier 0.
- **An inlined callee runs only what nothing can observe; everything else is an exit, not a refusal.** A callee instruction that could run R code, look at the call or capture its environment (`stop`, `warning`, `substitute`, `missing`, `sys.call`, a superassignment, an operator on an object, a `function` expression) compiles to an exit with the callee's frame state when its type feedback is empty, which means it has not run yet. Tier 0 records feedback at a `function` expression for this reason alone: whether its body constant is compiled says nothing, because a nested body is compiled with its outer function. Such an instruction is never run through a helper in an inlined call, since the helper would look for an interpreter frame the call does not have (`crates/oracle/cases/jit__inlined_frames_closures.R` pins a closure created on a branch that had not run). So a callee with an error path, a warning path or a rarely taken reflective branch is inlined, and the branch gets a real frame the first time it runs. The planner refuses the frame only for what the callee is known to do on every call: such an instruction with feedback, a loop, `...`, a call that itself needs a frame, or a call whose arguments tier 0's own call plan cannot bind without running code (`eval::direct_sources`). `RY_JIT_LOG=1` prints each refusal with its reason (`jit: no inlined frame for <call>: <why>`).
- **A step in a call without a frame signals nothing (built).** What an inlined callee runs through a tier-0 helper (`jit_step`, `jit_quiet_step`: a builtin call or an element read that runs no R code) can still fail or warn, and a signal is observable: the default error handler prints `Calls:` from the frame stack, and the `error` option's handler, a calling handler and the traceback read `sys.calls()`. So while such a helper runs for a call without a frame, `Interp.signalling` (`interp.rs`, `Signalling`) is `Deferred`. A builtin's error is then returned unsignalled (`eval::builtin_failure`), the exit rebuilds the frames, and `resume_slots` signals it there. A warning that only the default handler takes prints or queues as usual, since that runs no R code and reads no frame. A warning whose signalling runs R code (`.signalSimpleWarning`, taken under `options(warn = 2)`; with a calling handler established the call gets its frame up front) sets `Abandoned` and fails the step with nothing parked, so the exit re-executes the instruction in tier 0 under the rebuilt frames. Running it twice is safe because these instructions write nothing before they warn. `crates/oracle/cases/jit__inlined_frames_error_stack.R` pins the stacks against GNU R.
- **Laziness without promises.** The caller evaluates an argument before the call only when its expression is effect free, so the order of evaluation is unobservable. What R can still observe is whether a formal is forced, so each inlined frame keeps that in a mask with one bit per formal (`codegen.rs`, `Frame.forced`), which is why a callee with more than 64 formals gets no frame. The callee's own read of a formal sets its bit. When the argument was computed from formals of an inlined caller, the same read sets those formals' bits in the caller's mask (`Action::Frame.forces`), because forcing the argument is what would have read them. The caller's argument code sets no bit: copying a formal into the callee's window is not a read (`Translator.evaluating_arguments`). An exit saves the mask with the frame's cells, and `enter_inlined` binds a formal whose bit is set to a forced promise of its value and every other formal to an unforced promise of the argument expression over the caller's environment, which is what tier 0 binds, so `substitute`, `missing`, a rebinding through `parent.frame()` and a later first read behave as in tier 0. An argument that may have an effect is passed unevaluated: that is allowed when the callee has never read the formal (`analysis.rs`, `has_read`), and a read of it is an exit. In a callee that is itself inlined, an argument that mentions one of the callee's formals under a condition also stays unevaluated, because evaluating it before the call would force a formal that R forces only on one branch. `crates/oracle/cases/jit__inlined_frames_forced_arguments.R` pins the cases against GNU R.
- **A formal passed on is passed by name (built).** A body that passes one of its own formals to an inlined call before reading it must not force it, because the callee may never read it. Such an argument is `Passing::Variable`: nothing is read at the call, and the callee's load of its formal (`runtime.rs`, `jit_load_passed`) reads the binding in the outermost frame, through every inlined caller that forwarded it, because an inlined frame holds no binding. The load forces a promise only when that runs no R code (`eval::settled_without_code`: a promise already forced, a promise of a constant, or a promise of a bound name, followed for up to 16 links). Any other promise leaves compiled code at the load with `reason::UNFORCED`, and tier 0 forces it under the rebuilt frames. Those exits count against the root body, not the callee, because the callee did nothing unusual: once they are frequent the root's tier state gets `UNFORCED_FORMALS`, and its recompile neither reads an unloaded formal for an inlined call nor passes one on to a callee that reads it. `inlined_frames_keep_passed_formals_lazy` in `test_tiers.rs` pins that these shapes stop leaving compiled code once they settle, and `crates/oracle/cases/jit__inlined_frames_lazy_arguments.R` pins what a promise sees when it is forced under rebuilt frames.
- **A frame is planned only from evidence, and within a budget.** A callee whose type feedback is all zero is refused, because tier 0 runs a small closure without an activation (`eval::run_inlined`) and records nothing for it, so empty feedback says nothing about which instructions have run. A typed callee is refused, since the typed core compiles it. A callee longer than `MAX_FRAME_WORDS` (512 bytecode words) is refused, and one compilation plans at most `FRAME_BUDGET` (4,096) words of callees, refused ones included: planning costs time and arena memory per callee, up to `MAX_INLINE_DEPTH` (3) levels deep, and without the budget a body with many call sites raised peak RSS by megabytes.
- **Exits rebuild the frames outermost first.** Every exit in a callee has the caller's state after the call as parent. `jit_deopt_exit` (`runtime.rs`, `materialize`) walks that chain: it writes back the root frame, pushes each inlined call's interpreter frame with `eval::enter` over the cells already in the register file, resumes the innermost body in tier 0 at the exit's pc, and continues each caller after its call with the result, or raises the callee's error at the call instruction. A callee's free variable loads through `jit_load_free`, which walks the closure's enclosure without running code and declines on an active binding, an unforced promise or a missing variable.
- **A frame that keeps failing is given up, and the cause decides what is given up.** Every inlined frame counts its entries and the times it left compiled code (`runtime.rs`, `Inlined.entries`, `Inlined.handoffs`), and `frequent` compares the two: at least `MAX_FRAME_HANDOFFS` (16) exits, and more than one entry in eight. An absolute count would retire a frame that fails once in a thousand calls, and a rare exit costs less than the frame every other call saves. Three causes are kept apart, because the remedy differs.
  - The callee: an exit inside it, or an entry declined for a reason of its own (it is being debugged, its code changed). Its tier state is marked (`NO_ELISION`, or `NEVER_ELIDED` for a callee without compiled code) and the root's code is discarded, so the recompiled root calls it. A type mismatch in a callee records the new type before it exits, so the recompile plans from it.
  - The context: `jit_enter_frame` declines because a calling handler is established, the debugger is attached, object systems are off, or the stack is at its limit. No callee did anything unusual, so none is marked. Once such declines are frequent among all of the root's frame entries, the root's tier state gets `NO_FRAMES` and its recompile plans no frame at all, which leaves the callees free to be inlined into other roots.
  - The root's arguments: the `UNFORCED_FORMALS` case above, counted against the same total.
  `retire` discards a root's code once, however many counters reach their limit while it is still running. `inlined_frames_outlast_calling_handlers` in `test_tiers.rs` pins that a handler on one call in twenty costs no frame, and that a root always called under a handler keeps its compiled code, loses its frames and stops leaving compiled code.
- **Why a frame state and not re-running the call.** Re-running is only sound when nothing observable happened before the failure, which limited inlining to effect-free callees and made each new inlinable construct a proof obligation. With a frame state the callee may have written its variables, forced arguments and allocated, because the interpreter continues in the middle of the body instead of starting over. This is the shape of Ř's PIR framestates, V8 Maglev's eager deopt frames and JSC DFG's OSR exit descriptors (`prior-art.md`).
- **Not built.**
  - Entering an inlined call without a helper call. A start costs one call of the leaf `jit_enter_frame` ("An inlined call starts with one leaf helper call" below), where the older effect-free inliner (`Action::Inline`, still tried first) costs none. The end shape tests one word that every change of the watched state bumps, after which `Action::Inline` and `effects::effect_free_lookups` go away.
  - Materializing an inlined frame and staying in compiled code: an exit of an inlined frame finishes the whole root call in tier 0.
  - An inlined frame while a calling handler is established. `jit_enter_frame` declines and the call instruction becomes an exit of the whole root activation, so a root that mostly runs under a handler ends up compiled without frames (`NO_FRAMES`). The end shape runs the call through the direct-call path in compiled code when entry declines, so nothing exits and `NO_FRAMES` goes away. Until then code that mostly runs under `withCallingHandlers` or `suppressWarnings`, which includes every testthat expectation, calls for real.
  - A callee without feedback. Seeding its plan from the argument types at the call would cover the small closures tier 0 runs without an activation.
  - Callees with a loop, `...`, a default that is code and is read, or a call that needs its own frame, and recursive calls. Literal defaults, named and omitted arguments and assigned formals are built ("An inlined call starts with one leaf helper call" below).
  - Argument expressions over variables that have no type feedback. A variable read only inside promise code records none, which refuses `inner(x * k)` in a body that never reads `x` itself.
  - Seeding the callee's types from the argument types at the call instead of the callee's own feedback, which would specialize one callee per call site.
  - Resolving a callee's free variable at compile time under a guard instead of `jit_load_free`'s walk per load.
  - R-level value nodes in the SSA IR with value numbering, and scalar replacement of a vector or list that does not escape an inlined call.
- **Impact.** `codegen.rs` lost about 170 lines of per-exit emission, and all exits share one block. On `just bench applications` against the tree before the change, `orders` runs in 85 ms against 154 ms (1.82x, interval 1.79 to 1.85) and every other program is within its noise (0.97x to 1.03x), so the geometric mean of GNU R's time over ry's moves from 2.83x to 2.96x (2.86x with the JIT off). Only `orders` moves because tier 1 compiles 0 to 4 bodies per program (11 on `functional`), the same as before apart from one more body each in `conditions` and `timeseries`, and only three compiled bodies contain a call that qualifies for a frame. Peak memory is within 2 MiB per program and `libR.so` grows by 63 KB of code. `crates/jit/tests/test_tiers.rs`, the oracle on `ry-jit` and 400 seeds of `oracle fuzz` across `ry-tier0` and `ry-jit` agree with tier 0 and GNU R.

## An inlined call starts with one leaf helper call

**Status:** built. The single-word check under "Not built" is planned.

- **Previous shape.** Each start of an inlined closure call (`codegen.rs`, `inlined_call`) called `jit_enter_frame`, which reserved the callee's register window, wrote the call's activation record, saved the caller's srcref and the call marker, and took a handle mark, all behind a panic guard. Each return called `jit_leave_frame`, which restored them and cleared the window. A free variable of the callee went through `jit_load_free`, a third call. A compiled loop that called a one-line closure 2.5 million times spent more in those helpers than in its own code.
- **What a start has to do is decided by what can change between two starts.** Nothing about the window, the activation record or the handle mark can: an inlined frame's registers sit at a fixed place above the compiled body's own, so the body reserves and clears the whole extent once per entry, and the frame's record (`runtime.rs`, `Child`) is written at its first start in that entry (`first_start`, cold). Every helper leaves the handle stack as it found it, so the mark written then holds for the whole entry. What can change is the state that decides whether the call needs a real frame: the closure's debug bits, its body and environment, a pending frame on that environment, a calling handler, the debugger, the activity log, move tracking, the object-system switch and the stack depth. `jit_enter_frame` tests exactly those and returns the record, as a leaf function without a panic guard. When one of them says no, the cold `declined` counts the cause and returns null, and the caller hands the call instruction to tier 0.
- **The srcref is saved by the compiled code.** The start stores the current srcref into the `Child` record and makes the closure's own current, and each return restores it, as two loads and two stores on `Interp.srcref`.
- **The call marker is set by the helper that needs it.** `sys.call()`, an error's `Calls:` line and a warning's call read `Interp.assignment_call` for a call that has no frame. Only code that runs inside a helper can read it, so `guarded` sets it around the helper's body when the helper's activation is an inlined frame, and restores it after. A deoptimization gets the root activation, so resumed tier-0 code never runs under the marker.
- **The window is cleared inline.** A return stores `NULL` over the callee's registers and slots with one store per word for up to `CLEARED_INLINE` (16) words, and calls `jit_clear_window` above that. The words must be cleared because the collector scans the whole extent and a stale word would keep an object alive.
- **A frame's slots are marked unbound when a deoptimization builds the frame.** The calls inlined in one caller share one window (each starts at the same header, with its registers laid out after its own slot count), so a call with fewer slots keeps registers where a call with more slots keeps locals, and its return leaves `NULL` there. Tier 0 reads a slot as bound unless it holds the unbound marker, so `enter_inlined` (`runtime.rs`) fills the slots before it writes the bound ones; filling them at the first start of the call instead showed tier 0 a never-assigned local as `NULL` after a sibling call had returned. Compiled code never reads a slot of a frame that does not exist, so the start writes nothing there. Pinned by the oracle case `jit__inlined_sibling_frames`.
- **A free variable is found inline.** Compilation walks the callee's enclosing environments once (`heap::Probe`), and the machine code repeats that walk against the frames' own fields: the symbol filter of a frame that lacks the binding, the slot of the frame that has it, or the cell on the symbol for the base and global frames. A moved slot, an active binding, an unforced promise or the environment of a live slot frame (`gp::SLOT_FRAME`: its cells and filter do not show the variables still in slots) goes to `jit_load_free`.
- **Frames bind what tier 0 would bind.** The planner binds a call's arguments with tier 0's own call plan (`eval::direct_sources`), so named and omitted arguments and literal defaults get a frame, and the frame a deoptimization builds matches tier 0's. A formal whose default is code hands the call to tier 0 when the callee reads it. An assigned formal is a dirty slot like any local.
- **An entry of optimized code skips the schedule.** `bytecode::entry_event` asks the tier at every entry of a body that has optimized code and returns as long as the body keeps it, without recomputing `profile::NEXT`, which only changes when the code is discarded.
- **Not built.** One word that every change of the watched state bumps (a handler pushed or popped, a debug bit, `environment<-`, the debugger, the activity log, `options(expressions=)`) would make the start one compare in compiled code and remove the call. It touches every site that changes that state, and a missed site would be a wrong result, so it waits for a workload where the remaining call matters.
- **Impact.** One iteration of `spectralnorm_alt_3`'s inner loop, which calls `function(i, j) cache[[i, j]]`, runs about 345 instructions: 294 of compiled code and 52 in `jit_enter_frame`. The program reports about 0.10 to 0.12 s for `execute(250)`, against 0.25 s before, 0.70 s with the JIT off and 1.42 s for GNU R. `RY_JIT_CODE=<directory>` writes each compiled body's machine code to a file named after its address, which lines a per-address profile (callgrind with `--dump-instr=yes`) up with a disassembly.

## A body compiles only when its native work covers what compiled code costs

**Status:** built. The lean entry under "Not built" is planned.

- **Previous shape.** The compile policy ("A loop-free hot body compiles only when its call rate repays the compile") assumed every native instruction saves 100 machine instructions per call and charged nothing for entering compiled code. A hot one-line closure therefore compiled, and then ran slower than in tier 0: with the JIT on, seven of the fifteen application programs were 3 to 7% slower than with it off, and the three benchmark code bases about 3% slower.
- **Measured.** Closures called 400,000 times from `vapply`, in machine instructions per call (callgrind), compiled against tier 0. Entering compiled code from the dispatch loop goes through `count_entry`, `entry_event`, `jit::enter`, `compiled_for` and `jit::run`, grows the register file and boxes the result, which together cost about 570 instructions more than starting the same body in tier 0.

  | body | native instructions | saved per call when compiled |
  |---|---|---|
  | `x^2` | 1 | -325 |
  | `x * 2 + 1` | 2 | -309 |
  | `(x * 2 + y) * (x - y)` | 4 | -27 |
  | seven operations, two locals | 7 | 575 |
  | twelve operations, three locals | 12 | 1393 |

- **A step costs too.** An instruction compiled code cannot run natively is handed to tier 0 through `jit_step`, which costs about 300 instructions more than tier 0 running it in its own loop (the call out, the frame synchronised before and after it). A body that steps most of its instructions therefore loses in compiled code however hot it is.
- **Chosen shape.** `analysis.rs`, `Work` counts three kinds of work: native instructions, calls the plan removes or makes direct, and steps. `jit.rs`, `saving` values them at `SAVING_PER_NATIVE` (130), `SAVING_PER_CALL` (1000) and minus `STEP_COST` (300), and `saving_per_call` subtracts `ENTRY_COST` (570), so a step-free body breaks even at five native instructions or one call. `repays` multiplies that saving by the expected calls. `Plan::work` returns the counts three ways (`BodyWork`): for the whole body, for the instructions outside every loop, and for the instructions in loops, where an instruction nested `d` loops deep weighs `LOOP_WEIGHT` (8) to the power `d - 1`. The weight stands for the iterations an inner loop runs per outer iteration, so one stepped instruction in an outer loop does not refuse a native inner loop. The decision after planning, skipped under `RY_JIT=force`:

  | body | refused when | refusal | final |
  |---|---|---|---|
  | no loop, hot by entries | `saving_per_call` of the whole body is zero | `SMALL` | yes |
  | loops | `saving` of the weighted loop work is zero | `STEPPING_LOOP` | yes |
  | loops, entered at the top | the work outside the loops has steps and its `saving` is zero | `MOSTLY_TIER0` | no: a hot loop still compiles, and only the loop enters compiled code |

  `SMALL` is also decided once before planning, from the instruction counts of `analysis::shape` (`Shape::work`), which spares the plan.
- **A refused body stops recording.** `bytecode.rs`, `back_edge`: a body whose tier state is negative (refused for good, or off the tiers) sets its back-edge counter to a value that reaches no threshold. Its loops used to reopen the recording window at half the threshold with no compile left to close it, so every variable load and field access in the loop recorded its type for the rest of the run.
- **A cold body with an empty window is refused once.** `COLD` normally lets a body ask again up to `MAX_COLD_REFUSALS` times, each after planning the body. When the whole feedback vector is still zero after a window, a native prologue finishes every call before any instruction records, so another window cannot differ, and the refusal is final at once.
- **A refused body keeps its feedback.** `SMALL` is decided after the recording window, unlike `NEVER_NATIVE`, which is decided before it. The body stops recording and counting, and its feedback stays, so a compiled caller still inlines it as a frame. That is where a one-line closure pays: inlined, it costs no entry at all.
- **The allocator follows the body's size.** A loop-free body of at most `SINGLE_PASS_FROM` (2000) SSA instructions compiles with the backtracking register allocator (`codegen.rs`). Single-pass allocation spilled every value it defined, which made compiled `x^2` 100 instructions long. Above that size, and only for loop-free bodies, single-pass keeps the compile cheap.
- **Rejected: trimming the entry chain first.** Even an entry of 200 instructions leaves a one-operation body about even with tier 0, so the policy has to refuse it either way. A lean entry (below) lowers `ENTRY_COST` and with it the break-even size; it does not remove the need for the model.
- **Not built.** An entry from the dispatch loop that jumps to the compiled code without `jit::enter`, `compiled_for` and `jit::run` (the code pointer in the code header, the register window already sized), and a result returned unboxed to a caller that wants a scalar. Recalibrate the constants when either lands. Cheaper steps (a step that does not synchronise the whole frame) would lower `STEP_COST` and let more loops compile.
- **Pinned by** `a_body_that_could_run_nothing_natively_never_records`, `a_hot_body_that_runs_rarely_waits_to_compile` and `fully_typed_closure_compiles_at_its_first_call` in `crates/jit/tests/test_tiers.rs`. No test pins the constants: they are calibrated on instruction counts of the application programs and the numeric kernels (`array_sum` is the kernel a wrong loop weight refuses).
- **Impact.** Machine instructions with the JIT on minus with it off, per application program (callgrind, `tools/bench/applications`), before and after:

  | program | before | after |
  |---|---|---|
  | `expr_tree` | +261M | +1M |
  | `team_scores` | +249M | +33M |
  | `report` | +143M | +12M |
  | `objects` | +134M | +26M |
  | `records` | +126M | +21M |
  | `conditions` | +102M | +71M |
  | `validation` | +56M | +19M |
  | `orders` | -4956M | -4956M |
  | `bond_book` | -3426M | -3426M |

  After the change no program runs more than 0.55% more instructions with the JIT on. Of the kernels, every loop that compiled before still compiles except `vasicek`'s, which steps eleven of its instructions and runs as fast in tier 0.

## A compiled return writes locals back only when something can still read the frame

**Status:** built.

- **Previous shape.** Compiled code keeps assigned locals in SSA variables and marks their frame slots dirty. Every `Return` of the body flushed all dirty slots (`flush_all`): one `jit_flush` call per slot, which boxed an unboxed scalar and stored it through `Machine::assign`. A body with two assigned locals paid about 450 instructions per call for values nothing read again.
- **Who reads a frame after its body returns.** Tier 0 finishes a frame (`bytecode.rs`, `finish_frame`) only when it has an environment, registered exit code, or a pending environment, which is the name a promise or a closure created over a slot frame holds until the frame is reified. Exit code compiled for slots runs on the slot frame, and a reified environment is filled from the slots, so each of the three reads the slots after the return. Nothing else does: a slot frame without them is dropped with its registers.
- **Chosen shape.** At the body's own `Return`, `codegen.rs`, `finish_slots` calls the leaf `jit_frame_read_later` (`runtime.rs`) when some slot may be dirty. It answers from the interpreter frame: the slot frame has no environment, the top frame is this body's, and that frame has no environment, pending environment or exit code. When nothing reads the frame, the return skips the write-back and only releases the counted references the dirty boxed values hold. Otherwise it flushes as before. An inlined callee's return was already free of write-backs, because its frame exists only in an exit's frame state.
- **Why a run-time test and not a compile-time proof.** Whether exit code is registered or the frame was captured depends on what the body's callees did (`do.call(on.exit, ..., envir = parent.frame())`, `sys.function`, `environment()` in a default), which the planner cannot see. The test is four loads against a flush per local.
- **Pinned by** the oracle case `jit__locals_read_after_return`: exit code that reads a reassigned local, a closure returned before a local is reassigned, a promise an escaping closure holds, exit code a callee registers on the caller's frame, and `environment()` captured in the middle of a body.

## A call compiled code cannot inline enters the callee in tier 0's dispatch loop

**Status:** built.

- **Previous shape.** A closure call that tier 1 neither inlines nor calls directly ran through a tier-0 step (`jit_step`), and the step started a nested dispatch loop for the callee: a native stack frame per call, and no tier-0 fast call path.
- **Chosen shape.** The step enters the callee the way tier 0's own call does (`bytecode.rs`, `enter_in_place`): the callee runs in the loop the step started, and a `Resume` record on `State::resumes` says where its result goes. `return_fast` leaves a stepped call's record to `resume_caller`, which hands the result back to the compiled code, so a tier-0 callee never runs on into the compiled caller's tier-0 instructions. Compiled code entered in place this way grows the native stack as a closure call does, because a deep recursion through compiled bodies otherwise overflowed it (`recursion__and_recall` on `ry-jit`).
- **A formal passed on as a subscript is the `Argument` instruction.** `x[i]` and `x[[i, j]]` with a formal as subscript compiled to `DispatchSubject`, which only tier 0 runs, because a missing formal is an empty subscript. `Argument` yields the empty argument for a missing formal and is `GetVar` otherwise. It records `feedback::MISSING` when it sees one, and tier 1 compiles it as a variable load unless that bit is set, with a hand-off to tier 0 when a compiled load meets a missing formal. An accessor such as `function(i, j) cache[[i, j]]` runs natively and inlines.
- **A local a join needs as an object is forgotten, not written back.** When a control-flow join needed an unboxed local as an object, the local was boxed and its slot marked dirty, and the later write-back replaced the promise a formal was bound to and cleared its missing bit, so `substitute(x)` and `missing(x)` differed from tier 0. The join drops the local from the compiled state, and the next read loads the frame's own object.
- **Pinned by** the tier tests for stepped calls and the oracle cases `jit__stepped_calls_reach_the_caller` and `jit__inlined_frames_defaults`.

## A native recursive function's depth budget shrinks with the interpreter depth

**Status:** built. Bounding native recursion by the native stack is not built.

- **Previous shape.** A native scalar function (`crates/jit/src/native.rs`) counts a depth budget down per nested call and fails as a whole when it runs out, and tier 0 then reruns the call. The budget was the `expressions` limit minus the interpreter depth (`runtime.rs`, `jit_depth_budget`). When tier 0 reran a failed call, the callee's own recursive call tried the native function again one frame deeper, with almost the whole budget again. Under `options(expressions = 500000)` a recursion past the limit therefore ran the native function about `limit` times to a depth of about `limit` each: quadratic, more than two minutes where tier 0 alone reports the error in 1.6 s.
- **Chosen shape.** The budget is the smaller of the frames left under the limit and `RETRY_NESTING` (8,000,000) divided by the interpreter depth. A retry started `d` frames deep nests at most `8M / d` native calls, so the retries of one failed recursion sum to a bounded amount of work that grows with the logarithm of the depth instead of its square. At the default limit of 5000 the product of depth and remaining frames never exceeds 6.25M, so the limit is always the smaller bound and nothing changes there.
- **Rejected: marking the function as failed and never retrying.** A recursion that fails once at depth 400,000 still succeeds natively for every shallower call, and those are the common ones.
- **Not built.** The native stack does not bound native recursion: a native frame's size is not known to the budget, and only a 128 KiB red zone is guaranteed below the stack check. A raised limit with a deep native recursion relies on the budget alone.
- **Pinned by** `a_recursion_past_a_raised_depth_limit_fails_as_in_tier_zero` in `crates/jit/tests/test_tiers.rs`.

## Parked: cheaper global bindings in top-level loops (tier 0)

**Status:** planned, parked on its branch until a workload clears the bar.

- **What the branch `perf/global-loop-tier0` does.** Global reads and writes go through the site's cached binding cell, validated by `Heap::lookup_stamp` (the stamp tier 1 checks), every fast path reads the cell's flags because `lockBinding` and `makeActiveBinding` move no stamp, and `x <- left op right` on an unshared scalar global overwrites it in place. The oracle case `base__global_loop_bindings` pins every binding-change path. `s <- s + i` per iteration falls from 856 to 495 Ir (412 with an ungated draft; GNU R 249).
- **Why parked.** Top-level loops may be rare in the application workloads, and compile cost, RSS and the workload effect are unmeasured. Measure the workloads before building more; the bar is 3% on a named workload (2% on testthat).

## A compiled replacement probes the target before any subscript runs

**Status:** built.

- **Previous source of truth.** Tier 0 compiled `x[i, j] <- v` (`Op::SubAssign`, `Op::SubAssignMatrix`) and `f(x, i) <- v` (`Op::Replace`) by evaluating every subscript into a register first, and only the instruction learned whether `x` was an object or `f<-` a closure. It then handed `eval::complex_assign` the registers as forced promises. A function's first call runs through the AST evaluator, which passes the code, so only calls 2 and later were wrong: a method that never forces `i` saw it forced, a subscript naming an undefined variable failed with "object not found", and `substitute`-based methods (data.table's `[<-` evaluates `i` in the table's scope) broke.
- **What was weak.** Which function receives the arguments was decided after the arguments were evaluated, so two facts that GNU R keeps in order (the target's class, then the arguments) were inverted, and the forced promise made the inversion invisible to `substitute`.
- **Chosen shape.** When a subscript is not self-evaluating (a constant or the empty argument), the compiled code reads the target right after the value and branches before any subscript runs: `Op::BranchIfObject` for `[<-` and `[[<-` (and base's own setter, which a guard pins), `Op::GetFun` plus `Op::BranchUnlessBuiltin` for any other `f<-`. The taken branch goes to a cold block with `Op::ComplexAssign`, which runs the whole assignment as `<-` does, with the subscripts as code. Tier 1 treats `ComplexAssign` as an opaque step and speculates the branch away while it has never been taken. A subscript that is a constant keeps the eager path, because a promise of a constant cannot be told from the constant. In `[<-`, `[[<-` and the matrix form, so does a local variable the compiler knows is bound (`Compiler::assigned`: an enclosing `for` variable, or a `name <- value` statement earlier in an enclosing block, dropped when the block or loop body ends), because reading it runs nothing and cannot fail, and the `SubAssign` fallback hands a method the forced promise of its code, which `substitute` reads as the symbol. Any local that might be unbound still probes: `z[late] <- 1` before `late <- 1` must reach a method that never forces `late`. A body that calls `rm()` or `delayedAssign()` itself records no bound locals. The one known gap is a callee that unbinds the local in its caller's frame (`rm("k", envir = parent.frame())`) between the assignment and the subscript.
- **Rule.** A fast path may evaluate an argument eagerly only once it knows that a builtin receives it. `Recall` follows the same rule (it forwards the `Recall` closure's own promises), and so does the name of `$`, `$<-`, `@` and `@<-`, which reaches a method as the promise of a string.
- **Impact.** Correctness: the replacement forms agree with GNU R on every call and in every tier. Performance: a subassignment whose subscripts are constants or known-bound locals (`x[[k]] <- v`, `x[i] <- v` in a `for` over `i`) compiles exactly as before and costs the same in both tiers. One with a computed subscript pays one extra read of the target and one branch on a non-object, and the dispatch path is out of line: a local `x[i %% 64L + 1L] <- 9` loop costs 756 instructions per iteration against 714 at tier 0 and 979 against 927 under the JIT, and `just bench-mechanisms`'s computed-subscript row, whose target is a global promoted on assignment, costs 1,579 against 1,317. Incremental analysis is unaffected.
- **Pinned by** the oracle cases `replacement__index_promises_repeated_calls`, `replacement__index_promises_shapes`, `replacement__index_promises_local_subscripts` and `promises__recall_and_accessor_names`, and by the replacement block every `oracle::fuzz::program` carries, which runs each shape three times per context under GNU R, tier 0 and the JIT.
