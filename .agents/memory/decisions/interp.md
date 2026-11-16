# Decisions: Interpreter

The evaluator: calls, promises, frames, environments, lookups, dispatch and conditions.

## The interpreter and base R

**Status:** built. Where base R's R code comes from is settled by "ry ships its own base R, and GNU
R through `R_HOME` is the opt-in".

The code is `crates/interp` (`development/design/r-runtime.md`, step 3).

- **`interp` sits below `libr`.** `libr` installs it (`interp::install`) with host hooks (console,
  ALTREP materialization, finalizers) and replaces the foreign-function builtins
  (`interp::register_builtins`). `interp` never sees C, so it builds and tests without libR; the C
  API's `Rf_eval`, `R_tryEval` and `R_ToplevelExec` are thin calls into it.
- **Every non-local exit is a Rust `Err(Unwind)`.** `return`, `break`, `next`, exiting handlers and
  restarts are `Jump`s to a frame's environment; a builtin's error stays unsignalled
  (`Unwind::Error`) until the nearest `eval` or builtin boundary signals it, so calling handlers run
  before any frame unwinds. Only the libR boundary converts to and from `longjmp`, which keeps Rust
  destructors sound.
- **Frames mirror GNU R's function contexts exactly** (closure calls, `eval()` and internal
  dispatch push one), because `sys.call`, `sys.function`, `parent.frame` and `sys.nframe()`
  arithmetic observe them.
- **The default packages' native code is reimplemented in Rust** (`builtin_libraries.rs`): GNU R
  builds them against its internal headers, so they inline its object layout and cannot run on
  ry's heap. `dyn.load` of one returns a table with GNU R's routine names and arities, so
  `useDynLib` binds every `C_` symbol and an unimplemented routine fails naming itself. CRAN
  packages see an opaque `SEXP` since R 4.2, so they are unaffected.
- **Tier-0 code is a register machine stored in the closure** as a `BCODESXP`, compiled on a
  body's second call, so it is collected with the closure and needs no side table (`closure_body`
  is the one reader that must know). Inlined constructs run behind a guard that the symbol still
  names the base primitive, falling back to the AST evaluator, so redefining `if` or `+` keeps GNU
  R's semantics rather than GNU R's compiler's assumption.
- **Inline caches are validated per symbol** (`Heap::lookup_stamp`): a symbol epoch moves when a
  binding of it is created or removed outside a function frame, plus a heap-wide enclosure epoch. A
  single heap-wide counter was invalidated by every test body run in a fresh environment. Function
  frames (`FUNCTION_FRAME`) are excluded and probed directly, since bindings appear in them every
  call. A function lookup that skipped a non-function binding is not cached (that binding could
  later hold a function without moving the stamp). Global lookups use the symbol's search-path cell
  (`Heap::global_cell`), GNU R's global cache.
- **`*tmp*` does not count as a reference** (`Heap::untrack_references`, GNU R's
  `DISABLE_REFCNT`); counting it made every `x[i] <- v` copy `x`.
- **A replacement call is built as GNU R builds it**: each level's call names `*tmp*`
  (`` `[<-`(`*tmp*`, i, value = v) ``) and `apply_call` gets a separate list of forced promises
  (object code `*tmp*`, value code the right-hand side at the first level), so `sys.call()` and
  `substitute(value)` in a method match GNU R.
- **The call a replacement error names is chosen per message.** `Interp::assignment_call` maps to
  the whole assignment at the first level and `` names(x) <- `*vtmp*` `` later; `error`/`error_in`
  use it (GNU R's `error()`), while `unwind::errorcall` and `generated_call` keep the generated
  call for GNU R's `errorcall()` messages (out-of-bounds matrix `[<-`, vectorIndex, "no such index
  at level N"). A closure `` `f<-` `` reports its own frame call.
- **`*tmp*` is answered from a side table, not bound** (`Interp::tmp_values`, consulted by
  `find_var` only when a `*tmp*` lookup finds no binding). Binding it would bump the binding epoch
  twice per assignment, invalidating global caches in `for (i in s) x[i] <- i`, and count a
  reference to the target. So `exists("*tmp*")` and `ls()` do not see it.
- **"Not subsettable" and out-of-bounds errors are classed conditions**
  (`notSubsettableError`, `subscriptOutOfBoundsError` via `conditions::classed_error`) with GNU R's
  `object`, `subscript` and `index` fields (`subset.rs`).
- **Task callbacks live in libr's C list**, because C packages register through
  `Rf_addTaskCallback`; R's `addTaskCallback` reaches the same list through
  `Hooks::task_callback_routine`, and `session::run_toplevel` runs them after the task's warnings.
- **S4 objects get their S3 class vector from `Globals::s4_extends`** (GNU R's
  `R_S4_extends_table`, filled by `.cache_class` or `.extendsForS3`); without it S3 dispatch misses
  methods for contained classes.
- **`.External2` routines get GNU R's `(call, op, args, env)`**, which rlang's `ffi_eval` needs.

## `[`, `[[`, `[<-` and `[[<-` dispatch before evaluating their other arguments

**Status:** built.

- **Shape.** GNU R's DispatchOrEval: `BuiltinDef::dispatch_or_eval()` makes `eval::apply_call`
  call `dispatch::dispatch_or_eval`, which evaluates only the first argument and, for an object,
  passes the rest to the S4 or S3 method as promises; otherwise it evaluates them, so subset code
  never sees unevaluated arguments. data.table's `[.data.table` needs `j` and `by` unevaluated. A
  symbol bound to a missing argument evaluates to the empty argument (`f(m, , 2)` selects every
  row); `do.call` hands forced values.
- **Bytecode** (`subject_call`): evaluate the subject, `BranchIfObject` to `DispatchSubject`
  (`dispatch::dispatch_evaluated_subject`, so methods still get promises), else `CallBuiltin`,
  which the JIT turns into its element intrinsic. `BranchUnlessSame` sends a rebound `[` to the
  generic call. A formal in an index position takes the generic path, because a register cannot
  hold the empty argument.
- **NextMethod** reaching a primitive names the call `<generic>.default`, which a dispatching
  special reads as "do not dispatch again"; otherwise `[.simple.list` recursed forever.

## `object-systems = false` stops class definitions in project code at run time

**Status:** built.

- `ry run`, `ry load` and the console pass the option as `RUN_FLAG_OBJECT_SYSTEMS`. With it off,
  `run_closure` calls `refuse_object_system`, which raises the lint's message
  (`syntax::native_values::object_systems_disabled_message`, table `object_system_function`,
  shared with the lint and strict mode) when a call named after a class definition form (bare or
  after `::`) runs a closure owned by `methods` or `R6` from project code.
- **Project code** has the global environment as its top level, or runs in a namespace the loader
  built (marked `.__ry_loader__.` by `runtime.R`). Installed packages are exempt because they must
  keep loading (`stats4` defines S4 classes). Matching by call name costs one flag test, or one
  symbol-name lookup per closure call when off; an alias (`f <- setClass`) slips through, which is
  acceptable for a guardrail. GNU R has only the lint.

## Smaller runtime choices

**Status:** built.

- **PostScript reuses the PDF device's drawing operators** through a prolog, so both share one
  renderer. Placement and DSC headers follow GNU R (`%%Orientation`, `EPSF-3.0` only for
  `onefile = FALSE`, `paper = "special"`), pinned by `graphics__device_files`. JPEG uses
  `jpeg-encoder` on the PNG device's raster.
- **The native codeset follows `LC_CTYPE`** (`l10n_info`, `iconv("")`), so the C locale reports
  ASCII and escaping under `LC_ALL=C` matches GNU R.

## The interpreter's call path, lookups and allocation

**Status:** built.

- **A closure call's bookkeeping stays off the Rust heap.** Arguments and matched formals live in
  inline `SmallVec`s (`eval::Args` 10, `eval::Matched` 8), callers fill a vector they own
  (`promise_args_into`, `evaluate_args_into`). A formal whose default is atomic or `NULL` binds the
  value directly with `BINDING_MISSING` set (a promise of a constant costs an allocation and a
  force); `missing()` reads the bit. A typed closure turns such a binding back into a promise,
  because an inline annotation checks a default only when the body uses it.
- **Duplicate formals keep the first binding**, as GNU R's frame list does (`formals<-` can make
  them and `define_formals` skips the probe).
- **Frames scan up to 16 cells before indexing** and hold nine bindings inline as symbol/cell pairs
  (`identical` has nine formals). Separate symbol and cell arrays cost 3% more, because inserts
  check two spills.
- **A call binds its formals in one batch** (`Heap::define_formals`): one collection check, the
  cells, one write barrier.
- **Compiled code inlines more of base**: `.Internal(f(...))` of a non-dispatching builtin becomes
  `CallBuiltin`; `local(expr)` a closure creation plus call behind a guard compared against the
  forced lazy-load value; a function expression compiles its body once per site.
- **Scalar `x[i]`/`x[[i]]` on a plain vector skip subscript resolution** in `subset.rs`, so both
  evaluators take it.
- **A compiled read of a promise-bound variable forces the promise it found**; only active,
  missing and unbound cells restart the lookup.
- **A for loop reuses its scalar** when only the loop binding holds it (GNU R's `STEPFOR`).
- **Minor collections mark only roots added since the last one.** The nursery stays 8 MB: 32 MB
  was slower because first touches of fresh pages cost more than the collections saved.
- **`inherits()` follows `class()`'s implicit class** (`matrix`/`array`, else the basic class), so
  `inherits(1L, "numeric")` is `FALSE` as in GNU R.
- **A hashed frame's table is flat head/next arrays**; set-only callers (`length(env)`, sorted
  `ls()`, `lockEnvironment`) use `Heap::bound_symbols`.
- **`GetFun` always looks in the local frame first.** Starting in the enclosure's cached cell when
  the compiler sees no local binding was rejected: frames gain bindings the compiler cannot see
  (`assign(..., envir = parent.frame())` as zeallot's `%<-%`, `<-` in a default, locals
  `UseMethod` copies), and those do not move the symbol epoch. `Guard` keeps that shortcut for
  operators only, as GNU R's compiler does.
- **S3 dispatch allocates no implicit class vector** (`Globals::implicit_classes`, one shared
  not-mutable vector per shape and type, except calls and records).
- **A `.Call` routine's external pointer holds its `Routine` entry's address**, so `try_call`
  indexes a table; a null pointer from serialization falls back to lookup by name.
- **The random generators skip GNU R's per-call rebuild and stay bit-identical.** `GetRNGstate`
  returns early when `.Random.seed` still holds the session's state; `rnorm` with scalar finite
  parameters under Mersenne-Twister and Inversion fills in two passes (uniforms, then `qnorm`),
  with GNU R's arithmetic and no FMA; `m[i, ]` and `m[i, ] <- v` with a scalar row copy the strided
  row directly.

## A primitive's S4 cache lookup matches arguments by name first

**Status:** built.

- `methods::signature_arguments` matches supplied arguments to the generic's formals (exact names,
  then positions up to `...`) and orders them by `.SigArgs` before the cache lookup; a partial or
  unknown name skips the cache. Ordering by position, as GNU R's `R_quick_dispatch` does, keyed
  `x[i, drop = FALSE]` as `x#logical#logical#missing` and ran the `x[i, j]` method (Matrix's
  `indexing.R` failed).
- GNU R shows the quirk only for small top-level classes, because its quick table is usually stale
  and misses. ry deliberately differs; the Matrix suite pins it, an oracle case cannot.

## A hashed frame counts complex assignment's `*tmp*`

**Status:** built.

- GNU R binds `*tmp*` for the length of a complex assignment, which moves a hashed frame's chain
  count and can grow the table, changing `ls(sorted = FALSE)` order. ry never binds it, so
  `complex_assign` calls `Heap::count_transient_binding` (`HashTable::count_transient`). Pinned by
  `environments__hashed_order_after_removal`.

## Single-thread first, parallel-ready architecture (user directive)

**Status:** built, except the explicit context invariant: `interp()` and `heap()` are process
globals (`crates/interp/src/interp.rs`). The other invariants are not verified.

- **Scope.** Performance work targets one thread; the parallel runtime is deferred,
  "architecture-ready".
- **The effect-free fact** is one stored per-function fact in the function summary (from bytecode
  for untyped code, statically for strict typed code). Promise elision reads it, and the parallel
  apply family will read the same fact; there is no second computation.
- **Invariants to keep parallelism possible:** state in an explicit context, not globals;
  allocation through a per-context bump buffer; GC safepoints at allocation and back edges; room in
  the reference count for a "shared across threads" mark (atomic counts only for marked objects,
  as in Lean 4); inline caches and feedback change monotonically; symbol and CHARSXP tables
  readable concurrently; warnings, conditions and the RNG stream collected per task and merged
  deterministically.
- **Package native code stays single-threaded by contract**, because packages assume R's C API
  is; a parallel task runs only ry-owned code.
- Tier 0 caching the `Interp` pointer per activation is the first step toward a passed context.

## Argument-match caches live in the callee's code, not at the call site

**Status:** built.

- Matching produces a plan, one formal index per argument (`Plan` in `eval.rs`). The callee's
  compiled body keeps its last plan in the `MATCHES` constant with the formals and tags it was made
  for, and a call with identical tag symbols reuses it (`match_args_cached`). A failed match is
  never stored, so errors come from the full matcher.
- **Why the callee.** Calls passing `...` and S3 dispatch have no call site with fixed tags; keying
  on the callee covers them, and the check is one pointer compare per argument. Alternating name
  patterns thrash the one entry; revisit with two if a profile shows it.

## Cheap promises: measured, not redesigned; call setup and package loads trimmed instead

**Status:** built.

- **Promises keep their shape.** Lazy creation, inline promises in the frame slot and a per-call
  arena were weighed; none removes more than the nursery bump allocation (a few dozen
  instructions of a flat ~8,000 per call), and each changes what `PRCODE`, `PRENV`, `substitute`,
  `delayedAssign` identity and the C API see.
- **Formals bind in one pass** (`Frame::insert_formals`, `Heap::define_formals_uncollected`);
  settling a forced promise counts and barriers once (`Heap::settle_promise`).
- **The stack check** compares the stack pointer against a floor and ceiling kept on the
  interpreter, so `stacker`'s thread-local lookup runs only near the floor (`on_large_stack` in
  `eval.rs`); a segment another reader switched to takes the slow path. A guard restores the
  caller's floor on unwind, since a stale floor from a freed segment would skip the check.
- **Namespace loads defer major collections.** `run_closure` recognises `loadNamespace` by identity
  (`Interp::namespace_loader`, recorded by `session::mark_base_closures` on every start), not a gp
  bit: closure gp bits are scarce, and a shared one once counted every declined native entry as a
  load (`closure_gp_bits_do_not_overlap` guards it). While a load runs, `escalate_if_full` doubles
  the small-object limit, because almost everything a load allocates survives; the outermost load
  rebases the limit on the bytes kept. Pinned by
  `a_namespace_load_defers_major_collections_until_it_ends` in `crates/heap/tests/test_gc.rs`.
- **Lazy-load records.** `makeLazy` builds GNU R's promise (key spliced into the expression, one
  shared environment) rather than an environment per binding. `registerS3methods` answers whether a
  generic is S4 from its record header without fetching it (`serialize::lazy_record_header`,
  inflating 64 bytes).
- Minor collections during a load cost tracing the young survivors themselves, not remembered
  frames (see "Card marking, frame watermarks and moved list growth").

## Promises per call: native base closures take arguments by value; the rest of the promises are observable

**Status:** built.

- **By-value native entries.** A call of a closure in `base_fast_paths::NATIVE_ENTRIES` first tries
  `native_entry_by_value` on the unevaluated arguments (`op_call_generic`, the closure branch of
  `eval::apply_call`), only when every argument is untagged and a constant or a variable bound to a
  value or forced promise (`effects::eager_argument`). Reading those runs no R code; when the
  native code declines nothing has changed and promises are built as usual, so `substitute`,
  `missing`, `sys.call` and `match.call` see what GNU R shows. Native-entry calls build no frame
  and are counted apart (`Counter::NativeEntries`).
- **Compiled sites force strict arguments of closures GNU R's compiler inlines** (`typeof`,
  `identical`, `inherits`, `is.vector`, `match`: `GETVAR` plus `CALLBUILTIN` in GNU R, so
  `sys.call(-1)` in a forced promise shows the caller's call). Marked by the fourth field of
  `NATIVE_ENTRIES`. Other entries and AST callers only settle promise chains whose links are
  variables or constants (`eval::settled_without_code`, at most 16 links), and only for arguments
  the entry consumes, so `getOption(x, default)` with the option set leaves `default` unsettled:
  settling up front is observable, since a later assignment to the variable must still reach the
  promise. Pinned by `builtins__native_entry_promise_chains`.
- **Rejected because GNU R code can observe it:** forcing a caller's promise before the call
  (handlers would see `sys.calls()` without the callee, and rlang traces record that); passing the
  caller's own promise for a forwarded formal (`substitute` would show the caller's expression);
  binding values in a frame that forwards formals to a builtin (a warning handler can `substitute`
  the formal via `sys.frame()`); pools for frames and argument lists (binding cells need stable
  addresses for compiled watches, and dead frames can be held by uncounted references).

## Slot frames: closure locals live in registers until something asks for the environment

**Status:** built. Tier-1 code also runs on slot frames. "A slot frame's environment binds a
variable only when a lookup asks for it" replaces the rule that the first trigger moves every
variable into the environment; the layout, plans and references below stand.

Design and measurements: `development/design/slot-frames.md`.

- **Layout.** A body the compiler can number (`local_symbols` succeeds, at most 64 formals; a `...`
  slot holds the `DOTSXP`) gets a slot table (`SLOTS` constant). `bind_slots` binds a call into a
  header register (the environment once there is one) plus one slot per symbol, formals first. A
  slot counts one reference; `Frame.missing` keeps one bit per formal. A variable is in its slot or
  in a cell of the environment, never both, so nothing is copied back.
- **Frame identity.** `Frame.env` is nil while code runs on slots, `Frame.pending` holds the
  environment once made, and a nil `Frame.sys_parent` means a slot-frame caller named by position
  (`envir::caller_index`). `eval::frame_env` and `eval::frame_sys_parent` are the only ways to get
  either as an `SEXP` and make the environment once, so every route returns the identical object.
  Lookups by environment go through `Frame::owns`; property-only builtins take `Call::frame_index`.
  Routes that need the environment are tabled in `slot-frames.md`.
- **The C API** exports no context walker (`R_GlobalContext` is opaque; `R_sysframe` and friends
  are not exported); `R_GetCurrentEnv` reifies through `frame_env`.
- **Direct calls.** A planned site runs `eval::call_with_plan`, writing arguments straight into
  slots; the plan is built on a body's second call because allocating on the first pushed start-up
  over a collection threshold. A site stops after eight plans. `run_frame` moves handler and
  restart stacks back on return rather than cloning.
- **Compile before binding.** `bytecode::compile_before_call` compiles where `run_body` would, so
  that call already runs on slots; the first call of a looping function is often its only one
  (bench drivers call `execute` once).
- **Accepted cost in heap frames.** Variable instructions test the header register
  (`Activation::in_slots`) first. Making the heap path cheaper again would need a second dispatch
  loop; inlining removes these frames from hot loops. `lookup_outside` is `#[inline(always)]`
  because as a call it cost ~50 instructions per iteration in saved registers.
- **Forwarding.** `slot_dots_into` appends the slot's promises for `...`; `slot_builtin_call` forces
  constant and slotted arguments from slots.
- **No heap-frame fallback for bodies that keep reifying.** A per-body reify counter cut reifies
  15-fold yet ran more instructions: making the environment late costs about what binding a heap
  frame costs, and the heap path loses the direct plan. Fix the trigger instead (give the builtin
  a slot path).
- **Argument references.** A variable passed from a slot frame to a planned callee is a pooled
  promise with `gp::ARGUMENT_REFERENCE` naming the caller's frame and slot
  (`eval::argument_reference`). It stays a promise, not a new kind or tagged register, because
  every slot reader already handles promises and the GC traces registers. A shared per-symbol
  reference was rejected because a stray one would silently read the wrong frame; the pooled one
  carries its frame, so a stale one panics. Nothing may keep a reference past the call:
  `materialize_references` converts them where one could escape (reify, S3/S4 dispatch, the
  debugger), and `op_call_generic` releases the pool slice on return. Variants share one pool,
  told apart by the environment field: `eval::env_reference` (caller with an environment),
  `eval::code_reference` and `eval::env_code_reference` (compiled argument code); the AST evaluator
  passes constants as references too, because lazyeval-style native code reads a promise's
  environment. Each reference's attribute vector lives in `Interp.reference_targets`. The AST
  evaluator plans through `direct_sources`/`call_with_plan`, so planned calls have one binding
  path. A `...` callee plans with `DOTS_SOURCE`; `detach_reference` converts in place so a shared
  reference is not forced twice. A call passing `...` has the single plan `MATCH_PER_CALL`
  (`call_matching`). Tier 1 lowers a direct call's argument code in the caller's frame, so
  `lower_block` refuses an assignment there (`Lowering.inlined`), which would drop the caller's
  write.
- **No separate call path for builtin wrappers.** A closure whose body is one builtin call is an
  ordinary callee of `bytecode::call_fast` (`PLAN_FAST_BY_NAME` covers callers with an
  environment). A dedicated wrapper path was deleted: it was a second source of truth for argument
  order, laziness and visible frames, needed its own fixes, and was slower. `do.call` keeps
  `eval::run_inlined`. Pinned by `functions__builtin_wrappers`.
- **Closures close over the frame's environment, guarded in the heap** (`eval::slot_closure`).
  Making every variable a cell per function definition cost thousands of calls for handlers that
  never escape. No single accessor in `interp` covers every reader (`CLOENV` in C, lexical lookup,
  `effects.rs`), so every binding access in `heap.rs` goes through private `bindings`/`bindings_for`,
  which call the interpreter's resolver; `frame_ptr` is `pub(crate)`. The resolver runs with
  collection inhibited and panics if the owner returned, which `finish_frame` rules out: a frame
  returns with tracked closures (at most 16, `Interp.pending_closures`) only when each is provably
  dead, else its environment takes every variable.
- **Subassignment without an environment.** `x[i] <- v`, `x[[i]] <- v`, `x$name <- v` and one-level
  builtin replacements on a non-object slot value call the primitive and store into the slot;
  `Op::SubAssign`'s seventh operand keeps the generated call for error blame. Copy semantics rest on
  `slot_owns`; objects reify, since a method may read the frame.
- **`parent.frame()` hands out the caller's environment without its variables; exit code runs
  there.** `on.exit` from a slot frame registers compiled code on `Frame.on_exit` (`slot_on_exit`),
  run on the slots by `run_on_exit`. A native `withr::defer` keyed by frame was rejected: every
  other `do.call(on.exit)` user would still need this.
- **Call plans live in a side table** (`bytecode::CallPlans`, columns by plan id, sources in one
  pooled `Vec<i32>`); a site's state sits in its call instruction's unused feedback word, so
  planless sites allocate nothing. Heap-object plans grew the permanent heap enough to cross one
  more 2 MB huge page. Keys are weak via `heap::RootSource::clear_dead`, and `CallPlans::owned`
  checks owner and position, so a stale id never matches.
- **Prologues run on slot frames.** The prologue helpers take a `ClosureFrame` (environment or slot
  index) and read `eval::slot_formal`; only `match.arg`, `Reduce` and `trace_back` need an
  environment (`prologue_needs_environment`). `install_fast_paths` replaces a stale saved prologue
  (`calls_fast_path`), so a prologue edit needs no base image rebuild.
- **Checking.** Under `syntax::testing::enable_invariants()`, `check_lifted` verifies a lifted
  frame; the bytecode fuzzer runs each program with the environment in each state
  (`check_bytecode_agreement`); `crates/ry/tests/scenarios/slot-frames.scenario` covers each route.

## A slot frame's environment binds a variable only when a lookup asks for it

**Status:** built.

- **Shape.** The environment has three states (`gp::PENDING_FRAME`, `gp::PARTIAL_FRAME`): binds
  nothing; binds the variables lookups asked for; binds everything (ordinary). A variable is in its
  slot or a cell, never both. The heap asks for one variable (`bindings_for`,
  `eval::box_variable`) or all (`bindings`, `eval::lift_frame`), and passes other names through
  (see the next entry). Lookups from a closure or promise over the frame read the slot in place
  (`eval::slot_binding`, `dots_binding`, `slot_is_missing`). Arguments are pooled references on
  every slot path: closure calls, S3 methods (`dispatch::use_method_in_slots`) and the frame's own
  defaults (`eval::default_promises`). A reference becomes a real promise only where something
  outliving the call can hold it (`detach_reference`). `.GenericCallEnv` is read through
  `eval::method_var_value` when the caller has no environment.
- **Why.** One request for the frame's identity used to pay for every variable, while most
  requesters need only the object or one or two variables.
- **Why not slots only, with a view environment.** `R_findVarLoc`, active bindings, `lockBinding`
  and hashed frames hand out real `SEXP` cells, and a returned frame whose environment is held
  needs cells anyway. Moving one variable at a time keeps the C API on real cells and makes escape
  and lookup the same code.
- **Once a variable has a cell, the frame's code reads by name** (`leave_slots`); staying on slots
  would need a per-variable test in every slot instruction and in tier 1.
- **The environment stores its frame's position** (`Heap::slot_owner`) in the field that counts
  removed bindings in an ordinary environment; the uses never meet, because a slot frame's
  environment becomes ordinary before a binding can be removed. Scanning the stack instead costs
  per depth.
- **Pending closures are ordered by frame index** (`pending_closures_of`,
  `forget_pending_closures`), so `bytecode::return_fast` tests only the returning frame; an
  unordered list sent every return down the general path.
- **Impact.** About a quarter fewer objects and less collector time at unchanged run time, so the
  remaining sources (`backlog.md`, "The call path after slot frames bind on demand") are memory
  churn, not speed. Escape rules are in one function (`outliving_cause`).

## A slot frame's environment asks its frame only for names the frame binds

**Status:** built.

- The frame record keeps the frame's slot names (`Frame::expected`, set by `set_slot_owner`), and
  S3 dispatch variables' symbols carry `gp::DISPATCH_VARIABLE` (`expect_dispatch_variables`).
  `bindings_for` calls the resolver only when the name passes the 64-bit filter, has no cell, and
  `Frame::expects` it. The filter alone let a colliding name call the resolver on every read,
  depending on symbol addresses. The field fits because the spilled-frame index lives in
  `Spilled::index`.
- **`Heap::may_bind` stays filter-only**, because its callers search the slot names next anyway;
  an exact test there scanned twice per lookup.
- Pinned by `slot_frame_environment_asks_only_for_slot_names` in
  `crates/heap/tests/test_frames.rs`, which forces a collision.

## Single-core speed first; per-process costs count once per worker

**Status:** built. How package code avoids run-time compilation is settled by "No baseline tier,
and code images hold bytecode only".

- **Decision.** Target users run test suites in parallel processes and are CPU bound, so
  single-core speed is the goal; automatic parallelism inside a worker is deferred because threads
  would compete with the other workers.
- **Metrics.** Primary: the testthat workload (`tools/bench/applications/testthat.R`) under
  callgrind; then `just bench applications`; native execution work also reports `just bench public`.
- **Per-process costs** (JIT warm-up, compile time, package loading, resident memory) are paid once
  per worker, so per-worker resident memory must not regress.

## Closure call setup: the 10x end shape

**Status:** planned. Built: `eval::enter` and `eval::leave`, tier-0 calls to a compiled body run
in the dispatch loop, the planned fast path (`bytecode::call_fast`, `return_fast`), `CallPlans`
rows with `CallShape` and default sources, and the reference pool. Not built: depth-indexed frame
columns, the packed code header, pooled default promises with `u32` reference columns, `...`
without cells, promise forcing in the loop, and one observer word on the general path.

- **Goal.** On the common path (planned site, slot-table callee, constant/variable/code arguments,
  any defaults, `...`, no `on.exit`, no handler in the callee) a call allocates nothing and runs in
  about 200 instructions from call op to callee's first op and back. What GNU R code can observe
  stays as "Promises per call" settles it.
- **Frame layout: stacks indexed by depth, struct of arrays.** A 40-byte hot record per depth
  (`function`, `call`, `slots` with flags in its top byte, `missing` u64, `caller` u32, the prior
  `slot_owner` u32); cold columns (`env`, `sys_parent`, `pending`, `unforced`, starts of `args`,
  `method_vars`, `closures`, and the caller's `srcref`) written only when set. `on.exit` moves to
  one stack with a start per depth. Handler and restart stacks become arenas with parent indices,
  replacing the `Rc` lists (a calling handler runs with the index below it). The `srcref` is read
  once at compile time. The register window is already a bump region.
- **Formals-dependent facts live in the call plan (settled, built).** A closure whose formals were
  replaced keeps its body, so formal count and defaults are facts of formals and body together,
  the key of a `CallPlans` row. `eval::CallShape` (formal, slot, register counts, 6 bytes) is
  filled in `plannable`; literal vs code defaults are in the row's sources (`LITERAL_DEFAULT`,
  `PROMISED_DEFAULT`). Separate default bitsets were redundant with the sources and cost memory.
  The counts cannot drift because the row is keyed by the immutable body. A table generation lets a
  site check the row without copying sources.
- **The code header** packs slot, register and formal counts, default bitsets, the `...` slot,
  srcref, effect-free proof and prologue flag. It is part of the immutable code object, not a cache.
- **Every promise a slot-frame call creates comes from the reference pool**, the per-depth promise
  arena. Kinds: caller variable, caller-with-environment variable, compiled argument code (built),
  and a non-literal default run on the callee's slots (planned, so `default_promises` stops
  allocating). Targets move to three `u32` columns. A constant argument goes into the slot as the
  value; a variable does only when the slot table proves the formal's first read precedes any op
  that can run arbitrary code, even for a forced promise: `x <- 1; f <- function(a) { x <<- 2; a };
  f(x)` returns 2 in GNU R (oracle-pinned).
- **`...` without cells.** A received `...` is a dots view (start, count) over `Interp.frame_args`;
  a `DOTSXP` chain is built only when a reader needs the pairlist.
- **Arguments are written once** onto `Interp.frame_args`; one observer bitset is tested once per
  call; the depth limit compares against reserved capacity.
- **The contract T0 and the JIT call through.** `enter(plan, function, call, args_start,
  sys_parent) -> R<Entered>` (16 bytes, returned in registers; `header == NO_SLOTS` means the
  general path ran) and `leave(depth, result)`, the only thing that pops a depth, which runs
  `return` targeting and `on.exit`. Tier 0 continues its loop at the callee's first op and keeps
  resume state on its own shadow stack (`Resume`), not in a frame column, since machine frames
  never use it; on error it calls `leave` for each depth above its base. Machine code writes slots
  per the frame-state map and calls `enter` with the plan checked by its guard. `call_target(code,
  pc)` gives the JIT the closure a site last ran; there is no map.
- **Built: tier-0 calls run in the loop.** `eval::apply_closure_entering` shares setup with
  `apply_closure_owning`, so the paths cannot drift; `bytecode::enter_in_place` pushes a `Resume`
  and `bytecode::finished` pops it through `eval::leave` on every exit. Calls from builtins,
  `Rf_eval`, `UseMethod` or the general binding path still nest through Rust.
- **Built: the planned fast path.** `call_fast` takes sites with a prepared row (made while
  `heap::activity::hooks()` is zero) from `PLAN_FAST` or `PLAN_FAST_BY_NAME` callers;
  `slot-frames.md`, "The fast call path", lists what qualifies. The observer word is `activity`'s
  `HOOKS` byte, asserted against `interp::observer_bits()` under `enable_invariants()` so fuzzing
  catches a stale bit. The general path (`op_call_generic`, `eval::enter`) still copies an `Arg`
  buffer and tests observers one by one.
- **No single part of a framed call is large**, so the depth-indexed columns alone move little;
  the goal needs the hot record, a smaller resume record, a window that needs no clearing, and
  guards folded into the header and the plan row together.
- **Forcing a promise in the loop does not pay before promises are pooled** (measured as a net loss
  everywhere): its forcing record, two roots and heap target reads cost about what the Rust frame
  did. It belongs with pooled targets in `u32` columns and was removed rather than kept disabled.
- **S3 dispatch has no per-site state**; methods are found through their name's row.
- **Checking.** The bytecode fuzzer's reify agreement and `slot-frames.scenario` cover new
  reference kinds; the oracle runs forced in all modes.

## Tier-0 lookups through binding cells

**Status:** built.

- **Sites keep (start environment, cell) and a lookup stamp** (symbol epoch plus enclosure epoch),
  revalidated with one compare. Replacing an enclosure moves only the stamps of symbols bound
  between old and new enclosure (`bump_symbols_between`), and only for environments marked
  `ENCLOSING` (named by a closure or environment, or used as a cache key via
  `Heap::observe_environment`); an unnamed key would otherwise go stale unseen.
- **`pkg::name` with literal names is `Op::Namespace`**, keeping the cell `namespace_binding` found
  and checking the summed stamps of package, name, `.__NAMESPACE__.` and `exports` (stamps only
  grow, so the sum moves when one does). The bindings read on the way are `WATCHED`. Lazy data and
  renamed exports stay on the builtin. The JIT treats the op as opaque because the slow path can
  load a namespace.
- **Large frames index with one allocation** (`SlotIndex` in `env.rs`, linear probing at load
  ≤ 1/2, backward-shift deletion), cross-checked by `fuzz_indexed_lookups_match_the_bindings`.

## Native entries read promises over a pending frame without forcing them

**Status:** built.

- **Shape.** `peek_variable(symbol, env)` resolves a pending environment through `pending_name`
  (slot, else enclosure) without forcing or reifying; `settle_unforced(promise, value)` settles and
  decrements the owner frame's `unforced`, only after the settle succeeds (a checked promise's
  check can fail). The typed core's `gather` and `settled_without_code` use both;
  `reference_variable` reads what an argument reference names. Refusing such promises sent every
  typed call from a slot-frame caller to tier 0 (up to 500x slower).
- **Call heads.** A slot-bound head is refused (`native::pure_value`); any other resolves from the
  enclosure, never the pending environment, because every lookup there reifies the frame, which a
  declined typed call would pay every call. `effects::resolve_function` uses `heap.global_cell` for
  the search path, since walking attached frames costs ~2,000 instructions per call.
- **Counting.** `Frame.unforced` counts every unforced promise over the pending environment, taken
  wherever a promise is made (including code evaluated there from outside, as
  `do.call(f, list(quote(x)), envir = parent.frame())`) and when a reference detaches
  (`promises_over_slot_frames_are_counted`). `R_MakeDelayedBinding` and `SET_PRENV` give a promise
  the real environment, since native code may hold a pending one.
- **Refusal stays per (closure, argument types).** Marking the closure refused is wrong: a first
  call can decline while its callee compiles, and an untyped caller can pass one annotation in
  several representations
  (`test_tiers::typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic`).
- **Guard.** `RY_TYPED_LOG` prints when a typed call declines native code
  (`annotations::log_interpreted`); `test_scenarios::typed_benchmarks_run_natively` fails on any
  unlisted name. `typed-calls-stay-native.scenario`, `native_entries_hold_in_slot_frames`,
  `declined_typed_calls_keep_slot_frames` and `native_typed_calls_keep_slot_frames` bound reifies.

## Interpreter hot paths make no Rust allocations

**Status:** built.

- The rules are stated in `r-runtime.md` ("Interpreter hot paths make no Rust allocations"):
  short lists on the stack, existing data borrowed, unbounded text in one interpreter-owned scratch
  buffer (`Interp::text_buffer`, `strings::with_text_buffer`). The buffer is an explicit
  allocator, not a thread-local cache: each use takes it and puts it back, so a nested use gets a
  fresh one.
- **Pairlists link as they are built** (`values::PairlistBuilder`): only the head is rooted; later
  cells come from `Heap::alloc_triple`, which keeps its arguments alive, and link before anything
  else allocates. `hot_paths_survive_gc_stress` hangs if the head is unrooted.
- `Unwind::Jump` holds the jump inline (size-asserted); handlers come from
  `Interp::handler_pool` (at most 64); the regex table is keyed by borrowed text with one slot per
  flag combination.
- **Left in place:** fancy_regex's per-match capture set, non-`.` `OutDec` and general
  `format`/`print` padding, and the call protocol's own allocations.
- **Enforced** by `crates/interp/tests/test_rust_allocations.rs` (per-iteration budgets under a
  counting allocator, zero except `gsub` with groups and `mapply`).

## Closure frames live in fixed slots that are never moved

**Status:** built.

- `FrameStack` (`crates/interp/src/interp.rs`) builds a frame in the slot above the top
  (`push_with`), overwriting a dead slot without a drop; `pop` takes out the handler and restart
  stacks and clears `on.exit`, so a slot above the depth owns nothing, which makes the overwrite
  sound. Readers see a slice through `Deref`. Why: pushing and popping a `Vec<Frame>` cost two
  `memcpy` calls per call.
- `on_exit` is a `ThinVec`; a `const` assertion keeps `Frame` at or under 128 bytes, LLVM's inline
  copy limit, so a by-value push is never a `memcpy` call.
- The arguments are still copied into `Interp::frame_args`; writing them there directly belongs to
  the call path.

## Arguments that name a local stay lazy

**Status:** built.

- **Rejected: binding a caller's local to the parameter by value.** A promise reads `x` when
  forced, and code can write `x` first: `f <- function(a, b) { b; a }; g <- function() { x <- 1;
  f(x, x <- 2) }` returns 2, as does a callee that `assign`s into `parent.frame()`. The early read
  needs a proof that nothing writes the local before the force; `effects::effect_free` is that
  proof for the callees it accepts, which is why tier 0 binds their arguments eagerly. A promise
  created already forced keeps `substitute()` right but still costs the promise.
- **Measured ceiling** (callgrind, warm iteration): creating and forcing the promise of every
  variable argument is at most 2.7% of testthat_full and under 0.7% of validation, objects and
  functional.
- **Revisit** only with a caller-side proof that locals cannot be written during the call and a
  workload where such arguments to non-effect-free callees exceed 2% of instructions.

## The fixed cost of an argument reference

**Status:** built.

- `slot_promises_into` builds in one pass and, in the rare case an argument does not fit, undoes
  it (restores `unforced` from a snapshot, releases the references). `release_references` indexes
  the pool directly and writes each field only when it differs from its reset value.
- **Trap.** A forced reference's tag is nil whatever its kind (`settle_promise` clears it), so the
  attribute must always be compared with the stored target; skipping that when the tag is nil lets
  a reused environment reference run stale code and breaks base R's start-up.
- **Guard.** `fuzz_argument_references_agree_with_the_ast_evaluator`
  (`testing::check_argument_reference_agreement`) fails on that shortcut;
  `environments__argument_reference_reuse` pins reuse.
- **Rejected:** a slot-reference fast path in `force_promise` (0.16% for a second guarded-forcing
  helper); a side table of reset state (a pooled promise has no free field for an index; revisit
  if references gain one); lazy defaults (at most ~0.6% of testthat_full, for a new slot value kind
  every reader, tier 1 included, must handle).

## S3 methods are found through one row per method symbol

**Status:** built.

- **Shape.** `MethodCache` in `crates/interp/src/dispatch.rs` keeps a struct-of-arrays table with
  one row per method or generic symbol; the symbol holds its row index (`Heap::method_row`, sharing
  a word with its stamp, so a symbol stays 64 bytes). A row holds the name as a shared one-string
  vector (`.Generic`, `.Group` or `.Method` without allocating) and the binding found from a top
  environment on, with the top environment, methods table and lookup stamp. Validity rules are in
  the `MethodCache` doc comment. It replaced per-site inline caches plus a binding map: the same
  fact in two places with two invalidation schemes, a low site hit rate, and GC roots walked on
  full collections.
- Calling frames below the top environment are walked on every dispatch, as GNU R does. One row
  holds one scope, so dispatching a name from two top environments alternately repeats the walk.
- **`.Class` matches GNU R**: the class vector itself (not mutable) when the first class has the
  method, `NULL` for the default, otherwise the remaining classes with the whole vector as
  `previous` unless a group generic dispatched (`dispatch__dot_class_values`).
- **The (generic, class) name map stays a hash**: a dense key needs an id per `CHARSXP`, and
  interning the class as a symbol costs more than the hash lookup. A dense `CHARSXP` id is a
  strings follow-up (`backlog.md`).
- The per-resolution frame walk (validation's `format()` in `vapply`) is a closure-frame follow-up,
  not a reason for a site cache.
- **Checking.** `dispatch__method_lookup_order`, `dispatch__r6_and_dollar_methods`,
  `dispatch__method_cache`, `dispatch__site_cache_invalidation`; the fuzzer's S3 block changes
  methods between calls of one site.

## The interpreter is an engine crate between a numeric crate and four leaf crates

**Status:** built.

- **Shape.** `numeric` (nmath `rmath`, `long_double`, `vector_math`, `fft`, `linpack`, number
  formats) sits below `interp` (the evaluator, tier 0, conditions, dispatch, base builtins, the
  standard library); the leaves `stats`, `graphics`, `frames` (`DataFrame` over `table`, the
  viewer) and `services` (HTTP) sit above it. Only a host (`libr`, or a test) depends on a leaf.
  Each leaf's `install()` runs after `interp::install`, registers builtins and routine tables, and
  fills its part of `Interp::extensions` (`crates/interp/src/extensions.rs`), plain `fn` pointers
  whose defaults behave as a session without that leaf. Why: the engine carried Polars, reqwest,
  rustls and tiny-skia for a handful of cold calls, so every crate needing the evaluator built after
  them.
- **Why `fn` pointers.** One host and one implementation per leaf, so a trait or generic adds a
  second way to say the same thing; the calls are cold. Cargo features would keep the code in the
  crate, where an evaluator edit still recompiles it.
- **`numeric` reads the emulated release through `numeric::set_emulated_r_provider`**, set at
  install, so it reads no engine state.
- **Stays in the engine:** `distributions` and `random` (base fast paths call their `.Internal`s),
  `linalg` (`%*%`), the console, help server and debugger (they read the frame stack), regex and
  compression (base builtins).
- **Rejected: an API crate with the engine behind a vtable.** Leaves call several hundred engine
  functions, so the vtable would be R's C API again in Rust; leaves already compile beside the
  engine's codegen and are off the critical path.
- **The loader backend for ry's runtime is `crates/libr/src/loader_backend.rs`**, because a
  `runtime` feature on `loader` was unified on by every build and linked the interpreter into the
  checker. `cargo tree -p ry-lang -i interp` finds nothing.
- **Not taken yet: the evaluator above base's builtins**, so an evaluator edit stops rebuilding the
  builtins and leaves (`backlog.md`, "Open: the evaluator's edit loop"). Only evaluator→builtins
  calls would need pointers. It is also where passing interpreter state as an argument would start.
- **Rejected: incremental compilation for `iterate`.** It halves the edit loop but its code runs
  tier 0 2x to 3.6x slower, which defeats a profile meant for performance work; the fast correctness
  loop is `dev`.

## A watchdog thread raises the poll flag at a time limit's deadline, and the interrupt check raises the error

**Status:** built.

- **Constraint.** The evaluator and compiled back edges poll one atomic
  (`unwind::INTERRUPT_REQUESTED`); a clock read there, or keeping the flag raised while a limit is
  armed, would cost every loop (compiled loops would deoptimize at every back edge).
- **Choice.** `crates/interp/src/time_limits.rs` keeps GNU R's limit state on `Interp`. A watchdog
  thread, started when a limit is first armed, sleeps to the elapsed deadline (or polls CPU time
  every 20 ms) and raises `time_limits::DUE` and the poll flag; `check_user_interrupt` reads the
  clocks and raises the error, so the thread never decides an outcome. Deadlines are atomics only
  the evaluator thread writes, so a fork copies no held lock; a forked child restarts the thread
  (`time_limits::after_fork`). `session::run_toplevel` calls `time_limits::reset` per top-level
  expression, as GNU R's `resetTimeLimits`.
- **Rejected:** `setitimer` (interrupts system calls in packages, none on Windows); a clock read per
  poll.
- **Difference.** GNU R reads the clock at most every 50 ms, so a long `Sys.sleep()` under a
  shorter limit often returns normally; ry fires at the end of that sleep.
  `crates/oracle/cases/system__time_limits.R` pins only shared outcomes.

## Measured and not taken: O(1) `Sys.getenv` and `options`

**Status:** built (not taken; the measurement stands).

- Together they cost about 2.1% of testthat_full (the `environ` scan rejects most entries after one
  byte), and a probe per lookup costs some back, so the net is under 2%.
- If a profile puts them higher: for options, an index from symbol to pairlist cell beside
  `.Options`; for getenv, a slot index validated against the `environ` base and entry pointers that
  re-reads the entry's bytes so C `setenv` stays visible (derived state, so it needs a stronger
  case).

## Measured and not taken: a leaner `format()` on the validation workload

**Status:** built (not taken; the measurement stands).

- About 2.5% of validation is within reach, under the 3% bar, and ry is already about 7x faster than
  GNU R per `format(round(v, 3), nsmall = 3)`. The cost is S3 dispatch to `format.default`
  (call-path work), per-formal lookups and default forcing, and `real_format_candidates` on the
  emulated long double.

## Base fast paths read formals one at a time, except `formatC()`

**Status:** built (one pass taken for `formatC()` only).

- The helpers `argument`, `is_default`, `is_supplied` and `dots` (`base_fast_paths.rs`) each walk
  the formals; one pass is under the bar except on report. `formatC()` reads its 13 formals in one
  walk (`eval::slot_formals`), takes a defaulted formal as its known constant, and reads `OutDec`
  only when `decimal.mark` is defaulted. `format.default` keeps per-formal lookups.

## Parked: cheaper restart setup in `withRestarts`

**Status:** planned, parked on its branch until a workload clears the bar.

- Branch `perf/restart-setup`: the native `withRestarts` prologue shares one `names` and `"restart"`
  class vector, looks up `makeRestart` and `$` methods through pre-installed symbols, builds
  handler closures without `eval`, and calls `withOneRestart` directly for one restart
  (`conditions__restart_objects` passes on four engines).
- **Parked because** the native muffle in `warning_call` (`conditions.rs`) removed this path from
  the workload it helped; what is left is below the bar. To revive, merge main and re-measure the
  conditions workload.
