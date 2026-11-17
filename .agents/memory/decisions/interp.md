# Decisions: Interpreter

The evaluator: calls, promises, frames, environments, lookups, dispatch and conditions.

## The interpreter and base R

**Status:** built, except loading base R's R code from the user's GNU R installation, which "ry ships its own base R, and GNU R through `R_HOME` is the opt-in" supersedes.

The code is `crates/interp` (`development/design/r-runtime.md`, step 3).

- **`interp` sits below `libr`.** `libr` installs it (`interp::install`) with host hooks (console,
  ALTREP materialization, finalizers) and replaces the foreign-function builtins
  (`interp::register_builtins`). `interp` never sees C, so it builds and tests without libR; the C
  API's `Rf_eval`, `R_tryEval` and `R_ToplevelExec` are thin calls into it.
- **Every non-local exit is a Rust `Err(Unwind)`.** `return`, `break`, `next`, exiting handlers and
  restarts are `Jump`s to a frame's environment; a builtin's error stays unsignalled
  (`Unwind::Error`) until the nearest `eval` or builtin boundary signals it, so calling handlers run
  before any frame unwinds. Only the libR boundary converts to and from `longjmp`, which keeps Rust
  destructors sound and needs no context stack beyond `Interp::frames`.
- **Frames mirror GNU R's function contexts exactly** (closure calls, `eval()` and internal
  dispatch push one), because `sys.call`, `sys.function`, `parent.frame` and `sys.nframe()`
  arithmetic observe them.
- **The default packages' native code is reimplemented in Rust** (`builtin_libraries.rs`): GNU R
  builds them against its internal headers, so they inline its object layout (`LENGTH`, `PROTECT`
  through `R_PPStack`) and cannot run on
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
  `DISABLE_REFCNT`); counting it made every `x[i] <- v` copy `x`, so fill loops went quadratic.
- **A replacement call is built as GNU R builds it**: each level's call names `*tmp*`
  (`` `[<-`(`*tmp*`, i, value = v) ``, getters `` `*tmp*`[["a"]] ``) and `apply_call` gets a
  separate list of forced promises (object code `*tmp*`, value code the right-hand side at the
  first level, the value later), so `sys.call()` and `substitute(value)` in a method match GNU R
  and `substitute` unwraps a promise chain. Known difference: GNU R's bytecode shows the
  right-hand side at every level inside compiled code (`value = vals[[v]]`); ry's AST form does
  not copy that.
- **The call a replacement error names is chosen per message.** `Interp::assignment_call` maps to
  the whole assignment at the first level and `` names(x) <- `*vtmp*` `` later; `error`/`error_in`
  use it (GNU R's `error()`), while `unwind::errorcall` and `generated_call` keep the generated
  call for GNU R's `errorcall()` messages (out-of-bounds matrix `[<-`, vectorIndex, "no such index
  at level N"). Only builtin replacements are mapped: a closure `` `f<-` `` reports its own frame
  call (`` `f<-`(`*tmp*`, value = 1) ``), as in GNU R.
- **`*tmp*` is answered from a side table, not bound** (`Interp::tmp_values`, consulted by
  `find_var` only when a `*tmp*` lookup finds no binding). Binding it would bump the binding epoch
  twice per assignment, invalidating global caches in `for (i in s) x[i] <- i`, and count a
  reference to the target. So `exists("*tmp*")` and `ls()` do not see it.
- **"Not subsettable" and out-of-bounds errors are classed conditions**
  (`notSubsettableError`, `subscriptOutOfBoundsError` via `conditions::classed_error`) with GNU R's
  `object`, `subscript` and `index` fields (`subset.rs`'s `not_subsettable`, `out_of_bounds`).
  `subscript` is the 1-based dimension, `NA` for a vector or numeric `[` subscript; `index` is the
  failing element for `[` and the whole vector of a recursive `[[`. Not-subsettable errors name the
  type as `typeof` does (`builtin`, `special`), and `$`/`$<-` on a function or symbol raise them.
- **Task callbacks live in libr's C list**, because C packages register through
  `Rf_addTaskCallback`; R's `addTaskCallback` reaches the same list through
  `Hooks::task_callback_routine` (no second list in `interp`), and `session::run_toplevel` runs them after the task's warnings.
- **S4 objects get their S3 class vector from `Globals::s4_extends`** (GNU R's
  `R_S4_extends_table`, filled by `.cache_class` or `.extendsForS3`); without it S3 dispatch misses
  methods for contained classes.
- **`.External2` routines get GNU R's `(call, op, args, env)`**, which rlang's `ffi_eval` needs to evaluate in the
  caller's environment.

## `[`, `[[`, `[<-` and `[[<-` dispatch before evaluating their other arguments

**Status:** built.

- **Shape.** GNU R's DispatchOrEval: `BuiltinDef::dispatch_or_eval()` makes `eval::apply_call`
  call `dispatch::dispatch_or_eval`, which evaluates only the first argument and, for an object,
  passes the rest to the S4 or S3 method as promises; otherwise it evaluates them, so subset code
  never sees unevaluated arguments. data.table's `[.data.table` needs `j` and `by` unevaluated. A
  symbol bound to a missing argument evaluates to the empty argument, as GNU R's `R_isMissing`
  check does (`f(m, , 2)` selects every row); `do.call` (`eval::apply`) hands forced values.
- **Bytecode** (`subject_call`): evaluate the subject, `BranchIfObject` to `DispatchSubject`
  (`dispatch::dispatch_evaluated_subject`, so methods still get promises), else `CallBuiltin`,
  which the JIT turns into its element intrinsic. `BranchUnlessSame` sends a rebound `[` to the
  generic call. A formal in an index position takes the generic path, because a register cannot
  hold the empty argument. Without this shape `x[i]` in a loop ran as a generic call and compiled
  loops fell to tier-0 speed.
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
  renderer. Placement and DSC headers follow GNU R (centred, turned a quarter with
  `%%Orientation: Landscape` for `horizontal = TRUE`; `EPSF-3.0` only for `onefile = FALSE`, one
  file per page, with no `%%DocumentMedia` or `%%Orientation`; `paper = "special"` is the device
  size and omits `%%Orientation`; the bounding box is rounded), pinned by `graphics__device_files`. JPEG uses
  `jpeg-encoder` on the PNG device's raster.
- **The native codeset follows `LC_CTYPE`** (`l10n_info`, `iconv("")`), so the C locale reports
  ASCII and escaping under `LC_ALL=C` matches GNU R.

## The interpreter's call path, lookups and allocation

**Status:** built, except `Decoded` and `combine_unnamed_atomic`, which the code does not have.

- **Goal.** Per-test time on the three benchmark code bases, without the JIT, at or below GNU R's.
  The cost was spread thin over the core, so the fixes are many small cuts, not one redesign.
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
- **A for loop reuses its scalar** when only the loop binding holds it (GNU R's `STEPFOR`), and
  the AST loop releases each iteration's handles instead of rooting them until the end.
- **Compiled regexes are cached by pattern and flags**, and fixed argument names (`PACKAGE`,
  `NAOK`, `.Environment`) are interned once in `values::symbols()`.
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
  indexes a table; a null pointer from serialization falls back to lookup by name. C code calling
  `R_ExternalPtrAddr` on such a symbol gets the entry's address, not a function (it got null
  before, which fails the same way); every libr foreign-call path tries `try_call` first.
- **The random generators skip GNU R's per-call rebuild and stay bit-identical.** `GetRNGstate`
  returns early when `.Random.seed` still holds the session's state (rebuilding the generator is
  the dominant cost of a short draw); `PutRNGstate` writes into an uninitialized vector. `rnorm`
  with scalar finite parameters under Mersenne-Twister and Inversion fills in two passes
  (uniforms, then `qnorm`), with GNU R's arithmetic and no FMA; other cases draw one at a time
  through a closure `random_vector` monomorphizes per generator. `m[i, ]` and `m[i, ] <- v` with a
  scalar row copy the strided row directly. Vasicek simulation (one `rnorm(200)` and one
  `m[i, ] <- ...` per step): 142.5k to 87.3k instructions per step, against GNU R's 117.7k.

## A primitive's S4 cache lookup matches arguments by name first

**Status:** built.

- `methods::signature_arguments` (used by `s4_dispatch` in `crates/interp/src/dispatch.rs`) matches supplied arguments to the generic's formals (exact names,
  then positions up to `...`) and orders them by `.SigArgs` before the cache lookup; a partial or
  unknown name skips the cache. Ordering by position, as GNU R's `R_quick_dispatch` does, keyed
  `x[i, drop = FALSE]` as `x#logical#logical#missing` and ran the `x[i, j]` method (Matrix's
  `indexing.R`: after `D[TRUE, TRUE]`, `D[L, drop = FALSE]` failed with "logical subscript too
  long").
- GNU R shows the quirk only for small top-level classes, because its quick table is usually stale
  and misses. ry deliberately differs; the Matrix suite pins it, an oracle case cannot.

## A hashed frame counts complex assignment's `*tmp*`

**Status:** built.

- GNU R binds `*tmp*` for the length of a complex assignment, which moves a hashed frame's chain
  count and can grow the table, changing `ls(sorted = FALSE)` order. ry never binds it, so
  `complex_assign` calls `Heap::count_transient_binding`, which applies the insert and removal to
  the table without a slot (`HashTable::count_transient` in `crates/heap/src/env.rs`). Pinned by
  `crates/oracle/cases/environments__hashed_order_after_removal.R`.

## Single-thread first, parallel-ready architecture (user directive)

**Status:** built, except the explicit context invariant: `interp()` and `heap()` are process
globals (`crates/interp/src/interp.rs`). The other invariants are not verified.

- **Scope.** Performance work targets one thread; the parallel runtime (thread pool, atomic
  counts, parallel GC) is deferred, "architecture-ready".
- **The effect-free fact** is one stored per-function fact in the function summary (from bytecode
  at compile and install time for untyped code, statically for strict and locked typed code). Promise elision reads it, and the parallel
  apply family will read the same fact; there is no second computation.
- **Invariants to keep parallelism possible:** state in an explicit context, not globals;
  allocation through a per-context bump buffer; GC safepoints at allocation and back edges; room in
  the reference count for a "shared across threads" mark (atomic counts only for marked objects,
  as in Lean 4); inline caches and feedback change monotonically; symbol and CHARSXP tables
  readable concurrently; warnings, conditions and the RNG stream collected per task and merged
  deterministically.
- **Package native code stays single-threaded by contract**, because packages assume R's C API
  is; a parallel task runs only ry-owned code (R code, builtins, ry's ports such as nmath and fft).
- Tier 0 caching the `Interp` pointer per activation is the first step toward a passed context.

## Argument-match caches live in the callee's code, not at the call site

**Status:** built.

- Matching produces a plan, one formal index per argument (`Plan` in `eval.rs`). The callee's
  compiled body keeps its last plan in the `MATCHES` constant with the formals and tags it was made
  for, and a call with identical tag symbols reuses it (`match_args_cached`). A failed match is
  never stored, so errors come from the full matcher. `validation` and `objects`
  (`tools/bench/real`) make 1.0 to 1.2 million named calls each; matching in
  `seq(1, 10, by = 2)` drops from about 3.6k to 1.0k instructions.
- **Why the callee.** Calls passing `...` (633k in `validation`) and S3 dispatch have no call site
  with fixed tags; keying on the callee covers them, and the check is one pointer compare per
  argument, so a miss costs only the full match plus three small allocations. Alternating name
  patterns thrash the one entry; revisit with two if a profile shows it.

## Cheap promises: measured, not redesigned; call setup and package loads trimmed instead

**Status:** built.

- **Promises keep their shape.** Closure calls make about 1.2 promises each (testthat: 1.23M
  calls, 1.51M promises). Lazy creation, inline promises in the frame slot and a per-call arena
  were weighed; none removes more than the nursery bump allocation (a few dozen instructions of a
  flat ~8,100 per call: `frame_environment` 325, `run_closure` 322, `bind_arguments` 276, defining
  formals 270, frame clear and release 250, `force_promise` 113, settling 87), and each changes
  what `PRCODE`, `PRENV`, `substitute`, `delayedAssign` identity and the C API see. Promises are
  1.4 of 7.4 objects freed per call; binding cells (2.8) and environments (1.0) are more. The cuts
  below together measured only 8,174 to 8,140 per call.
- **Formals bind in one pass** (`Frame::insert_formals`, `Heap::define_formals_uncollected`);
  settling a forced promise counts and barriers once (`Heap::settle_promise`).
- **The stack check** compares the stack pointer against a floor and ceiling kept on the
  interpreter, so `stacker`'s thread-local lookup runs only near the floor (`on_large_stack` in
  `eval.rs`); a segment another reader switched to (`libr`'s and `rdb`'s readers call R hooks on
  their own `stacker` segments) takes the slow path. A guard restores the
  caller's floor on unwind, since a stale floor from a freed segment would skip the check.
- **Namespace loads defer major collections.** `run_closure` recognises `loadNamespace` by identity
  (`Interp::namespace_loader`, recorded by `session::mark_base_closures` on every start), not a gp
  bit: closure gp bits are scarce, and one shared with `NATIVE_ENTRY` once counted every declined
  native entry as a load (`gp::CLOSURE_BITS` and `closure_gp_bits_do_not_overlap` in
  `crates/heap/tests/test_frames.rs` guard it). While a load runs, `escalate_if_full` doubles the
  small-object limit, because almost everything a load allocates survives; the outermost load
  rebases the limit on the bytes kept. Loading testthat, xts, data.table and a 40,744-object
  package: 2 major collections to 0, GC from 15.7% to about 5% of the time. Pinned by
  `a_namespace_load_defers_major_collections_until_it_ends` in `crates/heap/tests/test_gc.rs`.
- **Lazy-load records.** `makeLazy` builds GNU R's promise (key spliced into the expression, one
  shared environment) rather than an environment per binding. `registerS3methods` answers whether a
  generic is S4 from its record header without fetching it (`serialize::lazy_record_header`,
  inflating 64 bytes). `library()` of 11 packages: 914 records fetched to 651.
- **Left open.** Minor collections during a load cost about 30M instructions each: tracing the
  young survivors (about 360k objects at about 70 instructions), not remembered frames (a frame
  watermark removed 1.3M of 90M on `library(testthat)`; see "Card marking, frame watermarks and
  moved list growth"). `unserializeFromConn` reads through the connection's buffer.

## Promises per call: native base closures take arguments by value; the rest of the promises are observable

**Status:** built.

- **Where the promises come from** (testthat, an instrumented build): symbol arguments bound in
  the caller to a value 421k, a forced promise 267k, an unforced promise 423k; language arguments
  154k; compiled arguments 307k; non-literal defaults 171k. The receivers are mostly small base
  and rlang predicates. The effect-free eager path (`eager_args_into`) applied to almost none:
  the callee is rarely provably effect-free, and testthat always has a calling handler.
- **By-value native entries.** A call of a closure in `base_fast_paths::NATIVE_ENTRIES` first tries
  `native_entry_by_value` on the unevaluated arguments (`op_call_generic`, the closure branch of
  `eval::apply_call`), only when every argument is untagged and a constant or a variable bound to a
  value or forced promise (`effects::eager_argument`). Reading those runs no R code; when the
  native code declines nothing has changed and promises are built as usual, so `substitute`,
  `missing`, `sys.call` and `match.call` see what GNU R shows. Native-entry calls build no frame
  and are counted apart (`Counter::NativeEntries`). Entries added for this: `typeof`, `inherits`
  (not S4, character `what`), `is.factor`, `is.data.frame`, `is.vector` (one-string `mode`) and
  `isNamespace`. Measured: 1.23 to 1.13 promises per R-level call, objects freed 7.42 to 7.17.
- **Compiled sites force strict arguments of closures GNU R's compiler inlines** (`typeof`,
  `identical`, `inherits`, `is.vector`, `match`: `GETVAR` plus `CALLBUILTIN` in GNU R's
  `cmpSimpleInternal`, so
  `sys.call(-1)` in a forced promise shows the caller's call). Marked by the fourth field of
  `NATIVE_ENTRIES`. Other entries and AST callers only settle promise chains whose links are
  variables or constants (`eval::settled_without_code`, at most 16 links), and only for arguments
  the entry consumes, so `getOption(x, default)` with the option set leaves `default` unsettled:
  settling up front is observable, since a later assignment to the variable must still reach the
  promise. A language argument at the site declines to the closure call. Pinned by
  `builtins__native_entry_promise_chains`.
- **Rejected because GNU R code can observe it:** forcing a caller's promise before the call
  (handlers would see `sys.calls()` without the callee, and rlang traces record that); passing the
  caller's own promise for a forwarded formal (`substitute` would show the caller's expression);
  binding values in a frame that forwards formals to a builtin (a warning handler can `substitute`
  the formal via `sys.frame()`); pools for frames and argument lists (binding cells need stable
  addresses for compiled watches, and dead frames can be held by uncounted references such as
  handles and the C protect stack).
- **Left.** Binding cells (2.7 per framed call), the promises above and environments; cutting
  cells needs frames whose slots compiled code can watch without a cell object.

## Slot frames: closure locals live in registers until something asks for the environment

**Status:** built. Tier-1 code also runs on slot frames. "A slot frame's environment binds a
variable only when a lookup asks for it" replaces the rule that the first trigger moves every
variable into the environment; the layout, plans and references below stand.

Design and measurements: `development/design/slot-frames.md`.

- **Why.** On testthat, frames and argument matching were the largest exclusive cost (23.8% of
  instructions), and 62.5% of calls never needed the environment as an object.

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
  Routes that need the environment are tabled in `slot-frames.md`; `substitute(sym)`
  (`slot_substitute`), `missing`, `nargs`, `sys.call`, `return` and the dots accessors need none.
- **The C API** exports no context walker (`R_GlobalContext` is opaque; `R_sysframe` and friends
  are not exported); `R_GetCurrentEnv` reifies through `frame_env`.
- **Direct calls.** A planned site runs `eval::call_with_plan`, writing arguments straight into
  slots; the plan is built on a body's second call because allocating on the first pushed start-up
  over a collection threshold (+33M instructions on an empty script). A site stops after eight
  plans. Per `h -> f` pair (JIT off): 3,065 instructions on heap frames, 2,605 on slot frames with
  matching, about 2,330 with the plan. `run_frame` moves handler and
  restart stacks back on return rather than cloning.
- **Compile before binding.** `bytecode::compile_before_call` compiles where `run_body` would, so
  that call already runs on slots; the first call of a looping function is often its only one
  (bench drivers call `execute` once). On rcp/bytecodes (25 locals assigned per iteration) slot
  frames took 18% of instructions off tier 0 and 16% off JIT-on runs.
- **Accepted cost in heap frames.** Variable instructions test the header register
  (`Activation::in_slots`) first: rcp super-while (a closure made in `execute` reifies it) runs
  3.6% more tier-0 instructions, matrix-for (a variable promise in `matrix(nrow = n, ncol = n)`)
  8% more, 2.5% with the JIT on. Making the heap path cheaper again would need a second dispatch
  loop; inlining removes these frames from hot loops. `lookup_outside` is `#[inline(always)]`
  because as a call it cost ~50 instructions per iteration in saved registers.
- **Forwarding.** `slot_dots_into` appends the slot's promises for `...`; `slot_builtin_call` forces
  constant and slotted arguments from slots.
- **No heap-frame fallback for bodies that keep reifying.** A per-body reify counter cut testthat's
  reifies from about 100,000 to 6,700 yet ran 0.2 to 0.7% more instructions: making the
  environment late costs about what binding a heap frame costs, and the heap path loses the direct
  plan. Fix the trigger instead (give the builtin a slot path; `UseMethod` generics, `.Internal`
  wrappers that receive the environment, `paste0`).
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
  write (`RY_JIT=force` showed `n <- 0; twice({ n <- n + 1; n }); n` giving 0).
- **No separate call path for builtin wrappers.** A closure whose body is one builtin call is an
  ordinary callee of `bytecode::call_fast` (`PLAN_FAST_BY_NAME` covers callers with an
  environment). A dedicated wrapper path was deleted: it was a second source of truth for argument
  order, laziness and visible frames, needed its own fixes, and was slower (`k1(i)` 2,419 against
  903 instructions per call on the fast path, `just bench-mechanisms --filter call:`). `do.call` keeps
  `eval::run_inlined`. Pinned by `functions__builtin_wrappers`.
- **Closures close over the frame's environment, guarded in the heap** (`eval::slot_closure`).
  Making every variable a cell per function definition cost 8,926 testthat package calls, mostly
  `tryCatch` handlers and callbacks that never escape. No single accessor in `interp` covers every reader (`CLOENV` in C, lexical lookup,
  `effects.rs`), so every binding access in `heap.rs` goes through private `bindings`/`bindings_for`,
  which call the interpreter's resolver; `frame_ptr` is `pub(crate)`. The resolver runs with
  collection inhibited and panics if the owner returned, which `finish_frame` rules out: a frame
  returns with tracked closures (at most 16, `Interp.pending_closures`) only when each is provably
  dead (reference count equal to the slots and cells holding it, not the result, not preserved,
  not a global calling handler), else its environment takes every variable.
- **Subassignment without an environment.** `x[i] <- v`, `x[[i]] <- v`, `x$name <- v` and one-level
  builtin replacements on a non-object slot value call the primitive and store into the slot;
  `Op::SubAssign`'s seventh operand keeps the generated call for error blame. Copy semantics rest on
  `slot_owns` (reference count one and no expression register holding the value; anything else
  copies first); objects reify, since a method may read the frame.
- **`parent.frame()` hands out the caller's environment without its variables; exit code runs
  there.** `on.exit` from a slot frame registers compiled code on `Frame.on_exit` (`slot_on_exit`),
  run on the slots by `run_on_exit`. Most `parent.frame()` calls come from callers that only pass
  the frame on (`do.call`'s `envir` default, `tryCatch`'s handler frame, `withr::defer(envir =
  parent.frame())`). A native `withr::defer` keyed by frame was rejected: every other
  `do.call(on.exit)` user would still need this.
- **Call plans live in a side table** (`bytecode::CallPlans`, columns by plan id, sources in one
  pooled `Vec<i32>`); a site's state sits in its call instruction's unused feedback word, so
  planless sites allocate nothing. Heap-object plans grew the permanent heap about 0.5 MB, which
  crossed one more 2 MB huge page (peak RssAnon 55 against 53 MB); with the table testthat's peak
  RSS is 76 MB with the JIT off and on. Keys are weak via `heap::RootSource::clear_dead`, and `CallPlans::owned`
  checks owner and position, so a stale id never matches.
- **Prologues run on slot frames.** The prologue helpers take a `ClosureFrame` (environment or slot
  index) and read `eval::slot_formal`; only `match.arg`, `Reduce` and `trace_back` need an
  environment (`prologue_needs_environment`). Denying prologue closures a slot table made "frame
  with an environment" about 18% of testthat's package-call reifies; `methods::is` is a one-line
  prologue in `methods_core.R` for the same reason. `install_fast_paths` replaces a stale saved prologue
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
  outliving the call can hold it (`detach_reference`: a heap-frame callee, a variable that gets a
  cell, native typed entries, non-closure methods). `.GenericCallEnv` is read through
  `eval::method_var_value` when the caller has no environment.
- **Why.** One request for the frame's identity used to pay for every variable, while most
  requesters need only the object or one or two variables. The finance tests made 10.2M whole
  environments in 53M closure calls and freed 348M objects.
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
  unordered list sent every return down the general path (`eval::leave` returns on a 225-test
  finance slice: 0.90M before on-demand binding, 1.54M with it unordered, 0.60M ordered).
- **Measured** (finance tests, JIT on): objects freed 348.2M to 264.0M (cells 65.9 to 22.8M,
  promises 41.1 to 21.1M, environments 19.5 to 14.1M); 81.5% of package calls with no argument
  promise and no environment (59.7% before); GC 2.41 to 1.86 s; call setup 16.9 vs 17.3 s and
  1.55x vs 1.57x GNU R, equal within noise.
- **Impact.** About a quarter fewer objects and less collector time at unchanged run time, so the
  remaining sources (`backlog.md`, "The call path after slot frames bind on demand") are memory
  churn, not speed. Escape rules are in one function (`outliving_cause`).

## A slot frame's environment asks its frame only for names the frame binds

**Status:** built.

- The frame record keeps the frame's slot names (`Frame::expected`, set by `set_slot_owner`), and
  S3 dispatch variables' symbols carry `gp::DISPATCH_VARIABLE` (`expect_dispatch_variables`).
  `bindings_for` calls the resolver only when the name passes the 64-bit filter, has no cell, and
  `Frame::expects` it. The filter alone let a colliding name call the resolver on every read,
  depending on symbol addresses (`spectralnorm_alt_3` made 840k resolver calls on one build, none
  on another). The field fits because the spilled-frame index lives in `Spilled::index`.
- **`Heap::may_bind` stays filter-only**, because its callers search the slot names next anyway;
  an exact test there scanned twice per lookup (about 0.5B instructions on `spectralnorm_alt_3`).
- Pinned by `slot_frame_environment_asks_only_for_slot_names` in
  `crates/heap/tests/test_frames.rs`, which forces a collision.

## Single-core speed first; per-process costs count once per worker

**Status:** built, except package code running from install-time compiled code, which "No baseline tier, and code images hold bytecode only" supersedes.

- **Decision.** Target users run test suites in parallel processes and are CPU bound, so
  single-core speed is the goal; automatic parallelism inside a worker is deferred because threads
  would compete with the other workers.
- **Metrics.** Primary: the testthat workload (`tools/bench/applications/testthat.R`) under
  callgrind; then `just bench applications`; native execution work also reports `just bench public`,
  because scalar loops are where FastR and Ř are ahead.
- **Per-process costs** (JIT warm-up, compile time, package loading, resident memory and cache
  pressure) are paid once per worker, so per-worker resident memory must not regress.

## Closure call setup: the 10x end shape

**Status:** planned. Built: `eval::enter` and `eval::leave`, tier-0 calls to a compiled body run
in the dispatch loop, the planned fast path (`bytecode::call_fast`, `return_fast`), `CallPlans`
rows with `CallShape` and default sources, and the reference pool. Not built: depth-indexed frame
columns, the packed code header, pooled default promises with `u32` reference columns, `...`
without cells, promise forcing in the loop, and one observer word on the general path.

- **Question.** Closure call setup is 22.4% of corpus time (frames, promises, argument matching,
  entry and exit); the 10x row asks for a tenth of that. A planned `h -> f` pair (one positional
  argument, a literal default) measured about 3,300 instructions, about 2,100 of them call setup.
  Structural causes: a 150-byte `Frame` with an `on_exit` `Vec`, two `Rc` clones and a `srcref`
  attribute walk per call; an `Arg` buffer copied into `Interp.frame_args`; each observer tested
  separately; a non-literal default allocating a heap promise plus pending environment; one
  `DOTSXP` cell per dotted argument; reference targets in heap integer vectors; and tier 0
  entering callees through Rust recursion (about 700 instructions of entry, exit and `scope`
  bookkeeping per call).
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
  `PROMISED_DEFAULT`). Two `u64` default bitsets in the shape (32 bytes a row) were redundant
  with the sources and raised testthat_full's peak RssAnon by about 0.09 MiB. `bind_slots`, which
  machine code binds through, records the same `promised` mask in `CallFrame::Slots`.
  The counts cannot drift because the row is keyed by the immutable body. A table generation lets a
  site check the row without copying sources.
- **The code header** packs slot, register and formal counts, default bitsets, the `...` slot,
  srcref, effect-free proof and prologue flag. It is part of the immutable code object, not a cache.
- **Every promise a slot-frame call creates comes from the reference pool**, the per-depth promise
  arena. Kinds: caller variable, caller-with-environment variable, compiled argument code (built),
  and a non-literal default run on the callee's slots (planned, so `default_promises` stops
  allocating). Targets move to three `u32` columns. A constant argument goes into the slot as the
  value; a variable does only when the slot table proves the formal's first read precedes any op
  that can run arbitrary code (any call, `<<-`, `eval`) on every path, even for a forced promise,
  because what is fixed is the promise, not the caller's binding: `x <- 1; f <- function(a) {
  x <<- 2; a }; f(x)` returns 2 in GNU R, as do the same shapes with `assign("x", 2, envir =
  parent.frame())` and with a callee that calls a function writing `x` (oracle-pinned).
- **`...` without cells.** A received `...` is a dots view (start, count) over `Interp.frame_args`;
  `slot_dots_into`, `...length()`, `..N` and `list(...)` read the view directly, and a `DOTSXP`
  chain is built only when a reader needs the pairlist (reify, `substitute(list(...))`,
  `match.call`, C code) and then replaces the view in the slot.
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
  `Rf_eval`, `UseMethod` or the general binding path still nest through Rust. Measured
  (`RY_JIT=off`): expr_tree -10.4%, validation -1.5%, the planned `h -> f` pair 4,127 to 2,725
  instructions. With the JIT on, functional costs 1.3% more: half its 320K closure calls come from
  builtins such as `lapply` and nest through Rust, where the shared setup costs up to 70
  instructions more, and 124K of its 168K `CallGeneric`s fall back after the fast path's tests.
- **Built: the planned fast path.** `call_fast` takes sites with a prepared row (made while
  `heap::activity::hooks()` is zero) from `PLAN_FAST` or `PLAN_FAST_BY_NAME` callers;
  `slot-frames.md`, "The fast call path", lists what qualifies. The observer word is `activity`'s
  `HOOKS` byte, asserted against `interp::observer_bits()` under `enable_invariants()` so fuzzing
  catches a stale bit. `return_fast` exits a frame still on its slots with no `on.exit` and
  nothing observing, discarding an environment nothing references any more. The general path
  (`op_call_generic`, `eval::enter`) still copies an `Arg` buffer, reads counts through
  `pool_ints` and tests observers one by one; the fast path still writes the whole `Frame`, clones
  the handler and restart `Rc`s, reads `srcref` from attributes, and allocates in
  `eval::default_promises` for a code default.
- **No single part of a framed call is large** (`f <- function(a, b = 1) { r <- a + b; r }` from a
  loop: 1,172 instructions per call; the callee's own ops and caller's `GetFun` 342, the call op
  535, the return 225 plus `Heap::release_slots` 69; the `Frame` write is 47 of 829 instructions
  of call machinery), so the depth-indexed columns alone move little;
  the goal needs the hot record, a smaller resume record, a window that needs no clearing, and
  guards folded into the header and the plan row together.
- **Why validation does not move.** Of its 284K `CallGeneric`s, 115K take the fast path (49% of
  closure calls); the rest: 55K from a caller frame with an environment, 48K to a builtin, 42K at
  a site whose plan word is still zero because a body compiled per closure creation never gives
  its sites a second call, 12K at an unprepared site, 11K to a native entry. Even with all of them
  on it, the call op cannot gain 8%: its 772K promise forces (2.7 per call instruction) dominate.
- **Forcing a promise in the loop does not pay before promises are pooled** (net loss everywhere:
  validation 2.544G to 2.551G, expr_tree 4.827G to 4.872G, `h -> f` 2,725 to 2,779): its forcing
  record, resume record, two roots, live-register walk and heap target read cost about what the
  Rust frame did. The pool removes three of the five (roots and target read), so it belongs with
  pooled targets in `u32` columns and was removed rather than kept disabled.
- **Expected impact.** Call setup about 2,100 to 200 instructions on the planned pair, objects per
  framed call about 7 to 0 on the common path.
- **S3 dispatch has no per-site state**; methods are found through their name's row.
- **Checking.** The bytecode fuzzer's reify agreement and `slot-frames.scenario` cover new
  reference kinds; the oracle runs forced in all modes. Allocations per call are compared with GNU
  R's from the allocation audit.

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
- **Large frames index with one allocation** (`SlotIndex` in `env.rs`, one block `[capacity,
  count, entries]` with entries `hash << 32 | position + 1`, linear probing at load at most 1/2,
  backward-shift deletion; it replaced an `FxHashMap`), cross-checked by
  `fuzz_indexed_lookups_match_the_bindings` in `crates/heap/tests/test_frames.rs`.
- **Measured** (testthat_full, JIT off, `RY_CODE_IMAGE=0`): 841.0M to 821.5M instructions per
  repetition, the first repetition 2,137.9M to 2,077.3M.

## Native entries read promises over a pending frame without forcing them

**Status:** built.

- **Shape.** `peek_variable(symbol, env)` resolves a pending environment through `pending_name`
  (slot, else enclosure) without forcing or reifying; `settle_unforced(promise, value)` settles and
  decrements the owner frame's `unforced`, only after the settle succeeds (a checked promise's
  check can fail). The typed core's `gather` and `settled_without_code` use both;
  `reference_variable` reads what an argument reference names (its slot, else the enclosure, else
  the reified environment), and both readers continue through an unforced promise found there, so
  a wrapper's computed argument (`doubled(k + 0.5)` passing `v` on) reaches native code. An
  environment reference reads as its promise, and a compiled promise's argument from its source.
  Refusing such promises sent every typed call from a slot-frame caller to tier 0
  (`tools/bench/why/typed` `array_dot` 0.004 s to 2.2 s).
- **Call heads.** A slot-bound head is refused (`native::pure_value`); any other resolves from the
  enclosure, never the pending environment, because every lookup there (`frame_cell`, `bindings`,
  `resolve_function`) reifies the frame, which a declined typed call would pay every call (20,083
  frames reified in 20,000 calls of a record-returning closure). `effects::resolve_function` uses `heap.global_cell` for
  the search path, since walking attached frames costs ~2,000 instructions per call.
- **Counting.** `Frame.unforced` counts every unforced promise over the pending environment, taken
  wherever a promise is made (`make_promise`, `compiled_promise`, including
  code evaluated there from outside, as `do.call(f, list(quote(x)), envir = parent.frame())`) and
  when a reference detaches (`promises_over_slot_frames_are_counted`); `finish_frame` skips the
  escape check when the count is zero. `R_MakeDelayedBinding` and `SET_PRENV` give a promise
  the real environment, since native code may hold a pending one.
- **Refusal stays per (closure, argument types).** A refused strict closure re-reads its
  arguments every call (about 3% of typed `orders`' instructions; `backlog.md` records it), but
  marking the closure refused is wrong: `log_rank`'s first call declines while its callee compiles
  (2x slower when marked), and an untyped caller can pass one annotation in several
  representations, so a `List<I64>` closure refused for doubles compiles for `List.range`
  (`test_tiers::typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic`).
- **Guard.** `RY_TYPED_LOG` prints when a typed call declines native code
  (`annotations::log_interpreted`); `test_scenarios::typed_benchmarks_run_natively` fails on any
  unlisted name. `typed-calls-stay-native.scenario`, `native_entries_hold_in_slot_frames`,
  `declined_typed_calls_keep_slot_frames` and `native_typed_calls_keep_slot_frames` bound reifies;
  the last fails when a native read leaves a settled promise counted as unforced. Untyped native
  entries are unaffected, because tier 0 binds effect-free closures' arguments eagerly first.

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
  `Interp::handler_pool` (at most 64; `conditions::new_handler` reuses an unshared entry,
  `conditions::restore_handlers` returns them; restarts still allocate one `Rc` each); the regex
  table is keyed by borrowed text with one slot per flag combination, and a fixed pattern caches
  its `memmem::Finder`.
- **Left in place:** fancy_regex's per-match capture set (its API cannot reuse one), non-`.`
  `OutDec` and general `format`/`print` padding, and the call protocol's own allocations
  (`build_call`, `Bindings::grow`, `run_at`, `bind_slots`, the bytecode compiler).
- **Measured** (warm, JIT on): Rust allocations 218k to 55k in testthat, 763k to 117k in strings,
  573k to 95k in timeseries; instructions down 1.5% to 14% except simulation; peak RSS unchanged.
- **Enforced** by `crates/interp/tests/test_rust_allocations.rs` (per-iteration budgets under a
  counting allocator, zero except `gsub` with groups and `mapply`).

## Closure frames live in fixed slots that are never moved

**Status:** built.

- `FrameStack` (`crates/interp/src/interp.rs`) builds a frame in the slot above the top
  (`push_with`), overwriting a dead slot without a drop; `pop` takes out the handler and restart
  stacks and clears `on.exit`, so a slot above the depth owns nothing, which makes the overwrite
  sound; `truncate` releases every slot it pops the same way. Readers see a slice through `Deref`.
  Why: pushing and popping a `Vec<Frame>` cost two `memcpy` calls per call (about 60
  instructions). A plain in-place pop without slot reuse saved only 33 per call, trading the
  `memcpy` for drop glue. Measured: `f(i)` from a loop 2,927 to 2,844 instructions; testthat_full
  -1.16% JIT off, -1.17% on.
- `on_exit` is a `ThinVec` (one word, no allocation while empty, `Frame` at 120 bytes); a `const`
  assertion keeps `Frame` at or under 128 bytes, LLVM's inline copy limit, so a by-value push is
  never a `memcpy` call. `restore_handlers` inlines its common case (the same stack, compared by
  pointer) and keeps the release loop out of line.
- The arguments are still copied into `Interp::frame_args`; writing them there directly belongs to
  the call path.

## Arguments that name a local stay lazy

**Status:** built.

- **Rejected: binding a caller's local to the parameter by value.** A promise reads `x` when
  forced, and code can write `x` first: `f <- function(a, b) { b; a }; g <- function() { x <- 1;
  f(x, x <- 2) }` returns 2, as does a callee that `assign`s into `parent.frame()`. The early read
  needs a proof that nothing writes the local before the force; `effects::effect_free` is that
  proof for the callees it accepts, which is why tier 0 binds their arguments eagerly. The proof
  needs that no other argument or outstanding promise over the frame assigns the local and the
  frame is not reified during the call. A promise created already forced keeps `substitute()` and
  `match.call()` right but not the early read, and still costs the promise.
- **Measured ceiling** (callgrind, warm iteration): all promise work / full ceiling (create and
  force every variable argument's promise) / force only: testthat_full 7.9% / 2.7% / 1.7%,
  validation 6.4% / 0.53% / 0.4%, objects 5.5% / 0.37% / 0.25%, functional 4.6% / 0.62% / 0.4%.
  Constants already pass as themselves; the rest is code promises and each promise's fixed cost.
- **Revisit** only with a caller-side proof that locals cannot be written during the call and a
  workload where such arguments to non-effect-free callees exceed 2% of instructions.

## The fixed cost of an argument reference

**Status:** built.

- `slot_promises_into` builds in one pass and, in the rare case an argument does not fit, undoes
  it (restores `unforced` from a snapshot, releases the references). `release_references` indexes
  the pool directly (the pool keeps `references_used` within both vectors) and writes each field
  only when it differs from its reset value. testthat_full -0.32% warm instructions, RSS flat.
- **Trap.** A forced reference's tag is nil whatever its kind (`settle_promise` clears it), so the
  attribute must always be compared with the stored target; skipping that when the tag is nil lets
  a reused environment reference run a forced code reference's stale code and breaks base R's
  start-up (`rm -> match.call`).
- **Guard.** `fuzz_argument_references_agree_with_the_ast_evaluator`
  (`testing::check_argument_reference_agreement`) fails on that shortcut;
  `environments__argument_reference_reuse` pins reuse across writes, recursion, `...`,
  `sys.function`, `substitute`, `match.call` and a reified caller.
- **Rejected:** a slot-reference fast path in `force_promise` (0.16% for about 30 lines and a
  second guarded-forcing helper); a side table of reset state (a pooled promise has no free field
  for an index, and the barriered resets stay either way; revisit if references gain one); lazy
  defaults (at most ~0.6% of testthat_full, since `default_promises`, `alloc_compiled_promise` and
  `compiled_promise` are 1.1% together, for a new slot value kind every reader, tier 1 included,
  must handle).

## S3 methods are found through one row per method symbol

**Status:** built.

- **Shape.** `MethodCache` in `crates/interp/src/dispatch.rs` keeps a struct-of-arrays table with
  one row per method or generic symbol; the symbol holds its row index (`Heap::method_row`, sharing
  a word with its stamp, so a symbol stays 64 bytes). A row holds the name as a shared one-string
  vector (`.Generic`, `.Group` or `.Method` without allocating) and the binding found from a top
  environment on, with the top environment, methods table and lookup stamp. Validity rules are in
  the `MethodCache` doc comment. It replaced per-site inline caches plus a binding map: the same
  fact in two places with two invalidation schemes, a low site hit rate (testthat 15%, objects 25%,
  validation 40% of 10k, 116k and 45k resolutions, since most sites run a few times and `$`
  dispatch from a builtin has no site), and GC roots walked on full collections (about 0.4% of
  testthat in `trace_method_cache`).
- Calling frames below the top environment are walked on every dispatch, as GNU R does. One row
  holds one scope, so dispatching a name from two top environments alternately repeats the walk.
- **`.Class` matches GNU R**: the class vector itself (not mutable) when the first class has the
  method, `NULL` for the default, otherwise the remaining classes with the whole vector as
  `previous` unless a group generic dispatched (`dispatch__dot_class_values`).
- **The (generic, class) name map stays a hash**: a dense key needs an id per `CHARSXP`, and
  interning the class as a symbol costs about 81 instructions (`install_charsxp` 26 plus
  `install_char` 55, 0.8% of objects at its 274k name lookups), more than the 69-instruction hash
  lookup. A dense `CHARSXP` id is a strings follow-up (`backlog.md`).
- **Measured** (warm, against the site caches): testthat -1.54%, validation +0.58%, objects
  -0.08%, functional +0.18%; RSS flat, libR `.text` 19 KB smaller. Validation's regression is its
  45k resolutions (mostly `format()` on doubles in `vapply`, each walking two function frames and
  three candidate names), which the old site entry skipped on its hits. That walk is what slot
  frames make cheap, so it is a closure-frame follow-up, not a reason for a site cache.
- **Checking.** `dispatch__method_lookup_order` (lookup order, define/mask/`rm`/`registerS3method`
  sequences, two top environments, `$`/`$<-` methods defined after the first call),
  `dispatch__r6_and_dollar_methods` (R6 `self$x`, `super$`),
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
  install, so it reads no engine state. (A few kernels changed results between GNU R releases, and
  the release comes from the loaded base R.)
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
  builtins and leaves (`backlog.md`, "Open: the evaluator's edit loop"). After the split an
  evaluator edit still rebuilds the engine's 126,000 lines (76 s of the 82 s `iterate` loop). The
  evaluator core (`eval`, `bytecode`, dispatch and closure application, about 30,000 lines) would
  sit above a lower crate of values, coercion, unwinding types, environments and builtins, which
  reaches `eval` through `fn` pointers; only that direction needs pointers, because tier 0 calls
  the builtins' kernels directly. It is also where passing interpreter state as an argument would
  start (about 1,500 reads of the global).
- **Rejected: incremental compilation for `iterate`.** It cuts the loop to 41 s (29 s, or 17 s
  with 256 codegen units) but its code runs tier 0 2x to 3.6x slower (records 2.0x, validation
  2.3x, `fib`/`loop_sum`/`vector_loop` with the JIT off 2.5x to 3.6x; JIT code unchanged), which
  defeats a profile meant for performance work; the fast correctness loop is `dev`.
- **Measured** (edit to `eval()`'s body, four cores): `dev` 11.7 s to 11.3 s, `iterate` 95.3 s to
  81.9 s. A leaf edit rebuilds that leaf and `libr` only, except `cargo test -p interp`, whose
  viewer tests install `frames` and so build `table` once.

## A watchdog thread raises the poll flag at a time limit's deadline, and the interrupt check raises the error

**Status:** built.

- **Constraint.** The evaluator and compiled back edges poll one atomic
  (`unwind::INTERRUPT_REQUESTED`); a clock read there, or keeping the flag raised while a limit is
  armed, would cost every loop (compiled loops would deoptimize at every back edge).
- **Choice.** `crates/interp/src/time_limits.rs` keeps GNU R's limit state on `Interp`. A watchdog
  thread, started when a limit is first armed, sleeps to the elapsed deadline (or polls CPU time
  every 20 ms) and raises `time_limits::DUE` and the poll flag; `check_user_interrupt` (GNU R's
  `R_CheckUserInterrupt`, reached every 1000 polls once the flag is up, after each `Sys.sleep`
  wait, and from native code) reads the clocks and raises the error, so the thread never decides
  an outcome. Deadlines are atomics only
  the evaluator thread writes, so a fork copies no held lock; a forked child restarts the thread
  (`time_limits::after_fork`). `session::run_toplevel` calls `time_limits::reset` per top-level
  expression, as GNU R's `resetTimeLimits`.
- **Rejected:** `setitimer` (interrupts system calls in packages, none on Windows); a clock read per
  poll.
- **Difference.** GNU R 4.6 skips five of every six checks and reads the clock at most every
  50 ms, so a long `Sys.sleep()` under a
  shorter limit often returns normally; ry fires at the end of that sleep.
  `crates/oracle/cases/system__time_limits.R` pins only shared outcomes.

## Measured and not taken: O(1) `Sys.getenv` and `options`

**Status:** built (not taken; the measurement stands).

- Together they cost about 12.5M Ir, 2.1% of testthat_full (callgrind, `RY_JIT=off`; the
  `environ` scan in `find_variable`/`do_sys_getenv` rejects most entries after one byte, the
  `.Options` walks are `set_option`, `option_value`, `do_options`), and a probe per lookup costs
  some back, so the net is 1.5 to 1.9%.
- If a profile puts them higher: for options, an index from symbol to pairlist cell beside
  `.Options`, rebuilt when the binding's head changes (about 1.1% net); for getenv, a slot index
  validated against the `environ` base and entry pointers that re-reads the entry's bytes so C
  `setenv` stays visible (about 0.8% net; derived state, so it needs a stronger case).

## Measured and not taken: a leaner `format()` on the validation workload

**Status:** built (not taken; the measurement stands).

- About 2.5% of validation is within reach, under the 3% bar, and ry is already about 7x faster than
  GNU R per `format(round(v, 3), nsmall = 3)` (about 16k Ir against 111k; `.Internal(format())`
  3.76k against 8.25k). The cost: S3 dispatch to `format.default` about 6.6k (call-path work);
  `do_format_default` 7.3k, of which per-formal lookups and default forcing 2.47k (about 1.0% from
  one pass), `real_format_candidates` on the emulated long double 1.13k and the text buffer 1.19k
  (about 1.3% together).

## Base fast paths read formals one at a time, except `formatC()`

**Status:** built (one pass taken for `formatC()` only).

- The helpers `argument`, `is_default`, `is_supplied` and `dots` (`base_fast_paths.rs`) each walk
  the formals; their self share of warm Ir is 5.15% on report, 1.88% on validation and under 0.6%
  elsewhere, and one pass saves only about 40% (it still walks and re-resolves the frame per
  lookup, since forcing can give a slot frame an environment), so it is under the bar except on
  report. `formatC()` reads its 13 formals in one walk (`eval::slot_formals`), takes a defaulted
  formal as its known constant without forcing it, and reads `OutDec` only when `decimal.mark` is
  defaulted: report runs 4.5% fewer warm instructions. `format.default` keeps per-formal lookups.

## Parked: cheaper restart setup in `withRestarts`

**Status:** planned, parked on its branch until a workload clears the bar.

- Branch `perf/restart-setup`: the native `withRestarts` prologue shares one `names` and `"restart"`
  class vector, looks up `makeRestart` and `$` methods through pre-installed symbols, builds
  handler closures without `eval`, and calls `withOneRestart` directly for one restart
  (`conditions__restart_objects` passes on four engines). team_scores warm Ir fell 1.98%
  (`make_restart_for` 55M to 22M).
- **Parked because** the native muffle in `warning_call` (`conditions.rs`) skips
  `.signalSimpleWarning`, `withRestarts` and the handler when `suppressWarnings`'s own handler
  applies, which removed this path from team_scores; what is left matters only for the conditions
  workload, estimated at 1.5 to 2%, below the bar. To revive, merge main and re-measure the
  conditions workload.

## Replacement calls and `lapply`/`vapply` element calls run on a slot frame's slots

**Status:** built.

- **Previous source of truth.** A variable of a slot frame lives in a register, but four operations
  could only reach it through the frame's environment, so each made the environment and moved the
  frame to reading by name: `f(x) <- v` and `x[i, j] <- v` on anything but a plain vector with a
  builtin setter (`eval::complex_assign` builds `*tmp*`, the getter call and the setter call in the
  environment), `x@name <- v` (a special), and the element calls of `lapply` and `vapply`, whose
  `.Internal` evaluated `FUN(X[[i]], ...)` in the calling frame after binding `i` there.
- **What was weak.** The environment was a means of addressing the variable, not something the
  operation needed: a builtin setter reads its arguments and nothing else, and `FUN` needs the
  element and the frame's `...`. On the finbench suite these four reasons were 1.47M of the 2.91M
  slot frames that moved to reading by name (`vapply` 471,633, replacement call 375,260, `lapply`
  320,598, subassignment 298,534), and each such frame then reads every variable by name for the
  rest of the call.
- **Chosen shape for replacement.** `replace_variable` (`crates/interp/src/bytecode.rs`) addresses
  the variable by its home, a binding cell or a slot (`Home`), and runs the same update for both. On
  a slot it reads an unforced argument or an enclosing variable first, as `complex_assign` does, and
  gives up (`false`, so `complex_assign` runs) whenever that or the setter lookup gave the frame its
  environment. The setter is a builtin (when the target has no method for it, asked through
  `dispatch::has_internal_method` with `Caller::SlotFrame`), a special with a direct entry (`@<-` on
  an S4 object, `at_assign_values`), or a closure whose other arguments are constants
  (`replace_with_closure`). The closure gets the target and the value in forced promises that count
  them, unlike the uncounted `*tmp*` and value promises `complex_assign` hands a builtin: tier 0
  passes a builtin its arguments without the argument list whose cells count in GNU R, so a value
  only an uncounted promise holds reads as unreferenced, and the closure's `unclass(value)` would
  strip the class in place. A frame that dies with the call releases its promises
  (`Heap::release_slots`), which leaves the target unshared for the next update, and a frame that
  outlives the call keeps counting, so the caller's next update copies. `assign_slot_element` does
  the same for `[<-`, `[[<-` and `$<-`, including on an object without a method, and
  `assign_slot_matrix_element` for an element of a plain matrix. A target with a method, a closure
  setter with computed arguments and a nested target still take `complex_assign`.
- **Chosen shape for element calls.** A body with `.Internal(lapply(...))` or
  `.Internal(vapply(...))` gets a slot for `i` (`binds_element_index`), and `ElementCalls`
  (`crates/interp/src/envir.rs`) stores each index there while the frame has no environment. A
  closure `FUN` is called with the element as a forced argument reference and the frame's `...`
  items appended (`eval::force_and_call_from_slots`), so it sees the call `FUN(X[[i]], ...)` and the
  promises a call evaluated in the environment would give it. A builtin `FUN` gets the element and
  the forced values of the `...` items (`eval::call_builtin_from_slots`). `ElementCalls` switches to
  the environment for the rest of the loop as soon as the frame has one (`parent.frame()` in `FUN`),
  and evaluates the call there from the start when `X[[i]]` may dispatch, `FUN` is a special, a
  generic builtin `FUN` would see an object, or the closure has no `...` formal (the `...` in the
  call is then an enclosing function's, or an error).
- **A direct entry runs under a profile.** `eval::call_direct` declined every call while a profile
  summary, `Rprof` or a capture was on, so a special reachable only through its direct entry (`@<-`)
  fell back to `complex_assign` exactly when someone was measuring. It publishes the builtin's
  activity, counts the call and takes the profile sample itself (`call_direct_observed`), so an
  observed run takes the path an unobserved one takes.
- **Rule.** An operation on a slot frame's variable addresses the slot. The environment is made only
  when something asks for the frame itself, and code that may trigger that (forcing a promise, a
  lookup) rechecks the frame afterwards before it writes a slot.
- **Impact.** Correctness: the element call, the promises and the copy-on-shared behavior are what
  the environment path gives (`y <- x; names(x) <- v` leaves `y` alone because a slot's value
  changes in place only when the slot alone holds it). Performance, on the finbench suite (both
  tiers): slot frames given an environment 8.94M to 6.61M, with `vapply` 471,633 to 2,731, `lapply`
  320,598 to 7,047, replacement call 375,260 to 38,391 and subassignment 298,534 to 75,333, and per
  suite run 7.9M fewer pairlists (68.0M to 60.1M), 3.9M fewer promises (19.7M to 15.9M) and 2.3M
  fewer environments (12.9M to 10.6M). The hot suite's weighted instruction count falls 3.0% on
  finbench, 2.9% on dsbench and 2.3% on biobench. The nested forms the compiler leaves to the `<-`
  special are 113,492 frames on finbench. Simplicity: one update routine for both homes instead of a
  slot-only fast path beside the general one. Incremental analysis is unaffected.
- **An empty `...` item reaching a builtin** is "argument is missing, with no default" from the
  frame that passes `...` on, as in GNU R, where it was "argument N is empty" from the builtin's
  call (`collect_evaluated_args`, `slot_builtin_args`).
- **Pinned by** the oracle cases `replacement__slot_frame_targets` and
  `apply__element_calls_from_slot_frames`, each run on GNU R, tier 0 and the JIT.

## A complex assignment calls a primitive replacement with its values

**Status:** built.

- **Previous source of truth.** `eval::complex_assign` ran every level of `f(g(x, i), j) <- v` the
  way GNU R's AST evaluator describes it: it built the call `` `f<-`(`*tmp*`, j, value = v) ``, an
  argument list for it, a forced promise for `*tmp*` and one for the value, and applied the
  replacement function to that list. The call existed so that a closure or a method could read it
  (`sys.call()`, `substitute(value)`, `match.call()`) and so that an error could name it.
- **What was weak.** A primitive replacement (`[<-`, `[[<-`, `$<-`, `names<-`, `attr<-`, `dim<-`,
  `levels<-`, `class<-` and the like) on an object without a method reads its arguments' values and
  nothing else, so the call, the list and both promises were built to be unwrapped again: about
  nine objects a level. Sampled allocation paths on the finbench suite put `complex_assign` at 11%
  of the pairlist cells, 8% of the language objects and 17% of the promises. Tier 0 runs a
  one-level replacement on a slot frame's variable without it (the entry "Replacement calls and
  `lapply`/`vapply` element calls run on a slot frame's slots"), so what reached `complex_assign`
  were targets that code does not serve: a variable in a binding cell, `<<-`, a nested target, a
  frame that reads by name.
- **Chosen shape.** A level whose replacement function is a primitive goes to
  `replace_through_primitive` (`crates/interp/src/eval.rs`), which pushes the object, the level's
  other arguments evaluated once, and the value into the primitive's argument vector and calls it
  (`call_builtin`, or `subset::dollar_assign` for `$<-`, wrapped as a builtin call is: its error is
  signalled while the assignment is the call in progress, and a profile counts it). It declines,
  and the level builds its call as before, when the object has a method for the replacement (the
  method receives the promises), when the primitive is a group generic (`` `add<-` <- `+` ``
  dispatches on the value's class too), when it is a special that reads its arguments' code, when
  the name of a `$<-` is neither a symbol nor a string, and when an argument compiled code already
  evaluated is `...` or code. The last test scans every argument before one is read, so a declined
  level reads no variable twice. A closure replacement always gets the built call. Getter levels
  (`g(`*tmp*`, i)` for the inner levels of a nested target) are built and evaluated as before.
- **The call an error names.** `Interp::assignment_call` holds the replacement call of the
  assignment in progress and the assignment it reports as, so `unwind::resolve_call` turns one into
  the other. On the direct path no replacement call exists, so the level's target (`f(x, j)`, a
  constant of the code) stands for it and is the call the primitive runs with. GNU R reports three
  messages against the generated call itself ("attempt to select more than one element in
  vectorIndex", "no such index at level", a matrix "subscript out of bounds");
  `unwind::generated_call` builds that call from the target and the assignment
  (`eval::generated_replacement_call`) only when one of them is raised.
- **`lapply` and `vapply` share one element call.** `FUN(X[[i]], ...)` is the same call for every
  loop, so `ElementCalls::new` (`crates/interp/src/envir.rs`) reads it from `Globals::element_call`
  instead of building five cells per `lapply`. Nothing may change it: `sys.call()` hands a frame's
  call out as a copy (`envir::call_with_srcref`).
- **GNU R behavior fixed alongside** (each pinned by `replacement__primitive_levels`): `exact` and
  `drop` have no meaning to `[<-` and `[[<-` and count as subscripts; `attr<-` with a name vector
  longer than one is an error from R 4.6.0; an unbound target is reported against the call of the
  frame that owns the enclosure of the assigning environment, and against no call when no frame
  owns it (an assignment in a promise that top-level code made); a nested assignment whose value
  is the variable or one of its levels (`x$a$b <- x`, `z$a$b$c <- z$a`) copies that level first,
  because the value on the operand stack counts no reference and the write would otherwise close
  a cycle.
- **Left unmatched.** GNU R's compiled code drops the names of subscripts from the assignment it
  reports (`x[["a", exact = TRUE]] <- "s"` reports as `x[["a", TRUE]] <- "s"`), and its AST
  evaluator keeps them. ry keeps them in every tier, so the oracle case prints only the message
  for such rows.
- **Measured** (finbench suite, profiling build, JIT on, before and after on one base). Complex
  assignments that build a call: 1,029,547 to 256,523. Objects freed: 232.3M to 217.0M (-6.6%),
  pairlist cells 56.5M to 47.0M, language objects 16.7M to 13.9M, promises 15.7M to 13.4M.
  `just bench hot`, weighted instructions per second of GNU R work: finbench -0.4%, dsbench -0.3%,
  biobench -0.8%. `lapply` over four elements: 16,550 to 15,897 instructions. Suite wall time
  moves within its noise.
- **What still builds calls.** 256,523 assignments on that suite, by profile row ("Complex
  assignments that built their calls" in a profile summary): a nested target's getter levels
  (`attr(attr(x, a), b) <- v` on an xts object is 38% of them, `names(l$a) <- v` 13%), an object
  with a method for the replacement (`dimnames<-`, `[<-` on xts, `[[<-` and `$<-` on a data frame;
  about 21%), and a closure replacement (about 5%). A direct getter for a primitive getter on an
  object without a method is the next step (`backlog.md`, "Replacement calls that still build
  their calls").
- **Pinned by** the oracle cases `replacement__primitive_levels` (classed lists with and without
  methods, calls and pairlists, environments, `<<-`, nested targets, a value that is the target,
  evaluation order, empty and named subscripts, a locked binding and a locked environment, the
  reported calls, a group generic as the replacement, S4 slots, data frames and factors) and
  `apply__element_call_is_never_changed`.

## An empty named argument leaves its formal to a positional argument

**Status:** built

- GNU R's positional pass tells a formal that a name matched from the value it holds, and an empty
  argument leaves none. So `f(a = , 1)` for `function(a, b)` binds `a` to 1, and
  `ifelse(test = , 1, 2)` is 2.
- `eval::match_plan` records the empty argument that matched each formal by name. A positional
  argument takes such a formal, and the empty argument's plan entry becomes `BINDS_NOTHING`, which
  every reader of a plan skips because it indexes no formal.
- A plan therefore depends on which named arguments are empty, so `match_args_cached`, whose key
  is the names alone, matches a call with an empty argument afresh. A call site's own plan is
  built from that site's arguments, whose empties never change.
- `match_builtin_args` follows the same rule for a builtin that matches by name. `log`, `rep` and
  `NextMethod` differ from GNU R with an empty named argument, which `backlog.md` describes.
- An "unused argument" message deparses the argument with no option, as GNU R does, so an integer
  loses its `L` and a vector its names (`deparse::deparse_unused_argument`).
- Pinned by the oracle case `calls__empty_named_argument`.

## `[[<-` reports a wrong subscript count in GNU R's order

**Status:** built

- With a subscript count that fits neither a vector nor the target's dimensions, GNU R's
  `do_subassign2_dflt` reports the first of: a target that is no vector ("object of type ... is
  not subsettable"), for an atomic target a value of the wrong length ("replacement has length
  zero", "more elements supplied than there are to replace", or the "long vectors not supported
  yet" message for a value with no length), and only then "[[ ]] improper number of subscripts".
  `subset::subassign2` checks in that order, so a subscript that is itself a call, as in
  `y[g(), , 1] <- 2`, reaches the same message as a constant one.
- `islistfactor` refuses an `NA` for `recursive` from R 4.5 on, which earlier releases took as true.
- Pinned by the oracle case `replacement__subscript_count_errors`.

## A call site that passes `...` keeps a plan for what `...` held

**Status:** built.

- **Previous source of truth.** A call site's plan says which argument fills which of the callee's
  slots, and it is made once from the site's own arguments. A site that passes `...` had the plan
  `MATCH_PER_CALL` instead: every call collected its arguments with `...` spread out, matched them
  against the formals (`matched_sources`), and entered the callee through the generic handler.
- **What was weak.** Matching reads only the formals and the arguments' count and names, and a
  wrapper is called with the same shape of `...` again and again, so each call derived the same
  sources as the one before. The mechanism row "call: forwards `...`" cost 3,531 instructions
  against 1,213 for a call with three arguments, and 4.7% of the hot suite's closure calls
  matched per call.
- **Chosen shape.** A site that passes `...` once keeps a spread plan (`PLAN_SPREAD`,
  `spread_plan` in `bytecode.rs`): the sources matched for one call, and beside them how many
  arguments `...` held and their names. A call for which that holds binds like any planned call.
  From a frame on its slots it takes `call_fast::<false, true>`, which compares the chain in the
  frame's `...` slot with the plan and writes the chain's items where the site has `...`. Those
  items are the caller's promises and values and pass on unchanged.
- **When the plan does not hold.** A call whose `...` holds another count or other names replans
  the site, which counts as a miss on the count that a callee with other formals or body also
  uses, and after `PLAN_MISSES` misses the site keeps its last plan and matches every other call
  per call. A call with an empty argument, in `...` or beside it, is never planned for and never
  uses a plan, because matching gives an empty argument's formal its default: `call_fast` keeps
  an eager argument whose value is the empty argument as a reference for the same reason. A site
  that passes `...` twice, or whose `...` holds a name that is no symbol, matches per call.
- **An unset `...` slot is not an empty `...`.** A function without the formal that assigns
  `...` somewhere has a `...` slot, and while it is unset the function's `...` is an enclosing
  function's. The slot paths (`forwardable_dots`, `slot_builtin_call`, `call_fast`) leave such a
  call to the generic path, which looks `...` up. Taking the unset slot as empty passed no
  arguments where GNU R passes the enclosing function's.
- **Why a plan per site and not a table per shape.** A site with several shapes of `...` could
  keep one plan for each. How many sites alternate between shapes is not measured. The single
  plan with a miss count is the smaller design, and a site with alternating shapes replans up
  to the miss limit and then matches per call whatever its last plan does not hold for.
- **One representation for both tiers.** Tier 1 does not inline a call that passes `...`. The
  row is what such inlining is to read: the count, the names and the sources, copied at compile
  time and guarded by the chain's shape and the callee, because a plan can be replaced or freed
  while compiled code lives.
- **Kept apart from the common call.** `call_fast` takes `SPREAD` as a constant parameter, so the
  path of a site without `...` carries none of its tests (`crates/interp/AGENTS.md`, the trap
  about rare branches in the call handlers). `call_generic_by_name` tries the spread path for a
  frame on its slots after the plain one declined.
- **Checked by.** With invariants on, every use of a spread plan is compared with matching the
  call afresh, in both the generic handler and `call_fast`. The oracle case
  `calls__dots_spread_plans` pins the shapes that change a plan: a count or a name that differs,
  an empty argument, `..N`, `...` twice, an empty `...`, a callee that reads `...length()`,
  `match.call()` and `sys.call()`, a site past its miss limit, partial names, a frame that
  leaves its slots between two calls, an unset and an assigned `...`, changed formals, `do.call`
  and S3 dispatch behind two wrappers. `check_argument_reference_agreement` generates forwarding
  calls of varying counts and names with invariants on.
- **Measured** (`profiling` builds, like for like). "call: forwards `...`" 3,531 to 2,036
  instructions; no other `call:` row moved by more than 0.8%. `just bench hot` weighted
  instructions: finbench -2.1%, dsbench -2.6%, biobench -2.3%. The code-base suites' seconds
  did not move beyond their run-to-run noise (finbench 107.9 and 108.0).

## A lazily signalled error carries its condition class

**Status:** settled.

- **Previous shape.** `RError` held a message and a call, so every error that left `eval` was signalled as a `simpleError`. GNU R classes a missing argument's error since 4.5 (`c(<reader>Error, "missingArgError", "error", "condition")`), and a classed handler (`tryCatch(f(), missingArgError = ...)`) never ran. `classed_error` signals eagerly, which fits a subscript error raised inside a builtin but not a variable read, whose error the optimizing tier defers past its frameless inlined calls.
- **Target shape.** `RError` has a `class: ErrorClass` (`unwind.rs`): `Simple`, or `MissingArgument(ArgumentReader)` where the reader is GNU R's subclass (`evalError`, `getvarError`, `getMissingError`, `tailcallError`, `tailcallRecError`, `MathMissingError`, `log1Error`, `log2Error`). `conditions::signal_error` builds the classed condition where the error leaves the activation, natively, so no `stop()` frame shows in `sys.calls()`; `unwind::condition_classes` is the one place that maps a class to its condition classes, for R handlers and for the C API's `R_tryCatchError`. `Unwind` fits in 40 bytes (asserted in `unwind.rs`).
- **The class is gated on the emulated release** (`r_at_least(4, 5)` in `unwind::missing_argument`): an older base R gets a `simpleError` whose call is the innermost closure call, as that release gives.
- **The call is the lexical call**, the frame whose code named the argument (`eval::lexical_call`), as GNU R's `getLexicalCall(rho)`: `f <- function(a) g(a)` reports `f()`, not `g(a)`. A complex assignment `x[i] <- v` reads `x` in the frame when the frame binds it and in the enclosure otherwise (GNU R's `EnsureLocal`), so from a nested function the error names the enclosure's call.
- **`evalError` against `getvarError` depends on GNU R's tier** (its AST evaluator against its compiled code), so a test prints one name for both (`conditions__missing_argument_classes`).
- **Which builtins keep a missing argument** follows GNU R's `evalListKeepMissing` users, among them `log`, `round`, `signif`, `rep` (after its first argument, which `DispatchOrEval` evaluates: a name there bound to no argument is the caller's error, and any other missing `x` reaches the type check) and `Exec`. `.subset` and `.subset2` are plain builtins there, so an empty argument is an error.

## `coerce_vector` applies GNU R's attribute rule in one place

**Status:** settled.

- **Previous shape.** `coerce_vector_quiet` copied every attribute to every result, and callers cleared what GNU R drops case by case (`as_vector` stripped after the fact), so `as.list` of a matrix kept `dim`, `x[1, 2] <- list(v)` on an atomic matrix kept a `dim` GNU R drops, and `as.expression` of a named vector kept names.
- **Target shape.** The tail of `coerce_vector_quiet` (`coerce.rs`) applies `coerceVector`'s rule: atomic to atomic keeps all attributes, list to and from expression keeps all, atomic to list keeps names only, atomic to expression keeps none, list or expression to atomic keeps names only. `coerce_atomic` sets no attribute itself. `storage.mode<-` and `mode<-` copy all attributes themselves, as GNU R's `do_storage_mode` (`SHALLOW_DUPLICATE_ATTRIB` after `coerceVector`) and the R code of `mode<-` (`attributes(x) <- atr`) do. libr's own `Rf_coerceVector` (`libr/src/vectors.rs`) follows the same rule.
- **A subassignment reads its subscripts against the object as it was** (`subset.rs`): `dim` and `dimnames` are taken before the coercion that a list value forces, as GNU R's `MatrixAssign` reads them before `SubassignTypeFix`, so `m[1, 2] <- list(v)` assigns the cell and returns a plain list. `m[[1, 2]] <- list(v)` hands the atomic matrix to the same assignment with the value wrapped in a list.
- Oracle case: `coerce__list_coercion_attributes`.

## A promise whose evaluation was left warns when it is forced again

**Status:** settled.

- **Rule.** GNU R's `forcePromise` sets `PRSEEN` to 2 when a jump leaves a promise's code, and the next force warns "restarting interrupted promise evaluation". ry keeps the mark in the promise's gp bits (`PROMISE_INTERRUPTED` beside `PROMISE_SEEN`, `eval.rs`): set when a force fails, read and cleared at the next force.
- **Each promise on a forwarded chain warns**, because an argument passed on is a promise of its own in GNU R; ry's forwarded reference carries the mark the same way.
- **A read that settles a promise without forcing it leaves a marked promise to forcing** (`eval::settled_without_code`, the JIT's `pure_value`), so `typeof(x)` on such an argument warns as a force does.
- **An argument reference is pooled**, so `pooled_reference` clears the mark when it hands a reference to the next call. Without that, an argument that failed in one call made an unrelated argument of a later call warn.
- Oracle case: `promises__restarting_interrupted_evaluation`.

## A direct call of an attribute replacement wraps its target

**Status:** built.

- **Rule.** A direct call of a replacement primitive (`` `names<-`(x, v) ``) copies a bound target first, because ry's arguments hold no reference and the builtin would otherwise change the caller's binding. `Checks::replacement` says how: `Replacement::Attributes` (`attr<-`, `attributes<-`, `class<-`, `dimnames<-`, `names<-`, `oldClass<-`) copies through `values::replacement_copy`, which puts a long vector, list included, under a wrapper that shares its data, as GNU R's `R_shallow_duplicate_attr` does; `Replacement::Contents` makes a shallow copy.
- **Why.** A plain shallow copy of a 1,000-element list costs 2 to 3 µs against GNU R's 0.45 µs wrapper.
- The bytecode fast path `direct_set_names` copies through `handles::attributes_unshared` for the same reason.
