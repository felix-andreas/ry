---
title: Slot frames
description: "Closure calls that keep their variables in the interpreter's register window and create an environment only when something asks for one"
---

A closure call today allocates an environment object, one binding cell per formal, and a promise for each argument
that is not a constant. It releases them all when it returns. On the testthat workload
(`tools/bench/applications/testthat.R`, images on, JIT at its default), that work is the largest exclusive
cost under callgrind:

| Sink | Share of 8.03G instructions | Largest functions |
|---|---|---|
| Frames and argument matching | 23.8% | `run_closure` 3.8%, `bind_arguments` 3.2%, `define_formals_uncollected` 2.1%, `release_frame` 1.5%, `clear_frame` 1.5% |
| Binding cells and lookup | 14.3% | `lookup_variable` 2.9%, `lookup_function` 2.5%, `frame_cell` 2.1%, `run_get_var` 1.8% |

That is about 2.2k instructions of frame work per closure call. This page proposes the end shape. A
call keeps its formals and locals in slots of the register window, and an environment object exists
only once something needs one.

## Most frames never need an environment

An instrumented build counted, for each closure call on testthat, what asked for its environment
before it returned. The run had 600,000 calls.

| What touched the frame | Calls | Share |
|---|---|---|
| Nothing | 374,756 | 62.5% |
| A promise made in it, and nothing else | 200,171 | 33.4% |
| A promise and a closure made in it | 11,493 | 1.9% |
| A promise and a reflective read (`parent.frame`, `sys.frame`, `environment()`) | 8,767 | 1.5% |
| A reflective read alone | 3,669 | 0.6% |
| Other combinations | 1,144 | 0.2% |

53,288 frames (8.9%) were still referenced when their call returned, so they had escaped. The rest
were dead at return even when a promise had captured them.

A slot frame therefore saves the environment, its cells and their release on 62.5% of calls if a
promise reifies the frame. It saves them on up to 91% of calls if a promise can point at the slot frame
and reify it only when the promise outlives the call.

## The shape

The first step is built (`bind_slots` and `reify` in `crates/interp/src/eval.rs`). This section
describes it, and the open gaps are marked.

- **Which calls get a slot frame.** The compiler gives a body a slot table (`SLOTS` in its
  constants) when `local_symbols` succeeds (`collect_bindings` in `crates/interp/src/bytecode.rs`).
  It fails for a body that may bind a name its own text does not show or hand its frame to code
  that may: a bare `environment()`, `sys.frames()`, `attach` or `source`, an `eval`,
  `evalq`, `assign`, `delayedAssign` or `rm` without an explicit environment, an assignment to a
  string target, or a read of an S3 dispatch variable (`.Generic`, `.Class`, `.Method`, `.Group`,
  `.GenericCallEnv`, `.GenericDefEnv`). The same calls with an explicit environment, `local`,
  `with`, `on.exit`, `sys.function` and `do.call` of a named function are accepted, because every
  route by which a callee reaches the frame (`parent.frame`, `sys.frame`, `environment(fn)` of a
  closure made inside) reifies it on demand. The body also has at most 64 formals and does not
  open with one of the fast-path prologues that evaluate code in their frame or define variables
  there (`match.arg`, `Reduce` and rlang's `trace_back`, listed in
  `base_fast_paths::prologue_needs_environment`). The other prologues read their formals from the
  slots and return through the pending environment (`base_fast_paths::ClosureFrame`), so
  `stopifnot`, `structure`, `setdiff` and the withr helpers keep their slot frames. At the call, `bind_slots` also refuses a closure with tier-1 or baseline machine code
  (which reads variables through the environment), a closure under the debugger or a type signature,
  and a call where a missing formal's default is neither a literal, a symbol, nor code compiled
  with its own slot table. Those calls bind a heap frame as before.
- **Defaults.** A missing formal whose default is not a literal starts with the default's
  expression in its slot and its missing bit set. Once the frame is pushed, `default_promises`
  replaces it with a promise over the slots, compiled once per closure (`bytecode::slot_defaults`,
  cached in the slot table's third element against the closure's formals and never written to a
  code image). On return, default promises that were never forced and that nothing else holds do
  not count as escaping promises (`unforced_defaults`), so they do not force the environment.
- **S3 methods.** A method called by `UseMethod` or `NextMethod` gets a slot frame too. Its
  dispatch variables live beside the frame (`Interp::method_vars`, `Frame::method_vars`), and
  `reify` binds them, so `NextMethod` and `environment()` in the method find them.
  `NextMethod` from such a method reads them without reifying (`eval::pending_name`), passes
  the next method promises over the frame's pending environment, and lets the method return in
  slots when the next method neither forced nor kept them (`MadePromises` in `dispatch.rs`).
- **Layout.** The activation reserves `1 + n` registers above its own: a header register, then one
  slot per symbol the table numbers, formals first in order. The header holds the frame's
  environment once it has one, and `R_NilValue` before. A slot holds a value and counts one
  reference, as a binding cell does, so copy-on-modify is unchanged. An unassigned local holds the
  unbound marker, and the frame record keeps one missing bit per formal (`Frame.missing`, a `u64`).
  The slots are GC roots because the whole register file is. A `...` formal's slot holds the
  `DOTSXP` the call collected, or the empty argument when it collected nothing.
- **Reads and writes.** `GetVar`/`SetVar` on a numbered symbol read and write the slot while the
  header is nil, and the environment after. Builtins that only read a frame property take the
  frame's index (`Call::frame_index`) instead of its environment. A fused element-wise pass
  (`fused_pass`) finds a leaf's slot by name (`Machine::slot_of`), because its leaves carry no
  slot number, and `<<-` starts from the closure's environment, so neither reifies.
- **Reification.** `reify` builds the environment from the slots exactly as `bind_arguments` and
  the assignments since would have: formals first with their missing flags, then each bound local.
  The slots then drop their references, the header and `Frame.env` point at the environment, and
  from then on the environment is the source of truth. Nothing is copied back.
- **Frame identity.** `Frame.env` is filled lazily, and `Frame.sys_parent` is nil while the caller
  is an unreified slot frame, which then is the frame just below (`envir::caller_index`).
  `frame_env(index)` and `frame_sys_parent(index)` are the only ways to get either as an `SEXP`, and
  both reify on demand.
- **Release.** A frame that was never reified drops its slots' references on return instead of
  `release_frame` and `clear_frame`.
- **Direct calls.** A call site keeps a plan for the closure it calls (`direct_plan` in
  `bytecode.rs`): which argument fills each formal's slot. The site marks itself seen on its first
  call, builds the plan on the second, and from then on `eval::call_with_plan` writes the arguments
  into the slots without matching. A site stops planning after eight misses (a closure with other
  formals or body than its plan's, or one it cannot plan for). Plans live outside the heap, in
  `CallPlans`, columns indexed by plan id; the site keeps the id in the unused feedback word of its
  call instruction's first operand, so a site without a plan costs nothing. The table holds its
  keys (the site's code, the closure's formals and body) weakly: after marking, the collector calls
  `RootSource::clear_dead`, which frees every plan with a dead key before its address can be
  reused. A closure without formals gets no plan. A call
  that passes `...` has a plan of one entry, `MATCH_PER_CALL`, because what `...` holds differs
  per call: `call_matching` matches the expanded arguments each time (`matched_sources`) and then
  writes them like any plan. The plan is refused for a `..N` argument or a left-out formal whose
  default is not a literal, and those calls bind through `bind_slots`. `call_with_plan` checks per call what can change after planning (machine code for the
  body, the debugger, move tracking), and takes the generic path when any holds.
- **Call shape.** Each plan row also holds the callee's `CallShape`: its formal count, slot and
  register counts, and two bitsets of the formals that have a default and of those whose default
  is code. Binding reads the shape instead of walking the formals and the slot table, writes a
  left-out formal's default by walking the formals only as far as the last one left out, and
  makes default promises only for the code bits that are also missing. The shape lives in the
  plan, not in the compiled body, because a closure whose formals were replaced keeps its body:
  the defaults are a fact of the formals and body together, which is the plan's key. The site
  reads its row after collecting the arguments, which runs no R code; a generation counter that
  every store and free moves confirms the row is unchanged with one compare.
- **Forwarding `...`.** A call that passes `...` appends the promises the frame's `...` slot holds,
  to a closure (`slot_dots_into`) or, forced, to a builtin (`slot_builtin_call`, when every other
  argument is a constant or a slotted local), so `f(x, ...)`, `list(...)` and `c(x, ...)` leave the
  frame without an environment.
- **Not built.** Slot access from the JIT's tiers ([the native execution model](/development/design/r-runtime/#the-native-execution-model)).

## Reification triggers

These are the reads that need the environment as an object. Each one reifies the frame first.

| Trigger | Where |
|---|---|
| A promise made in the frame that the slot-promise path does not cover (see below), default promises, `delayedAssign` | `make_promise`, `compiled_promise`, `formal_binding` in `eval.rs`; `NextMethod`'s promises in `dispatch.rs` when the next method keeps one past its return |
| A closure created in the frame, past the frame's 16 tracked closures (see below) | `slot_closure` in `eval.rs`; `make_closure` for an uncompiled `function`; `~` (the formula's environment) |
| A closure made in the frame that may outlive it | `finish_frame` in `eval.rs` |
| Reflection that returns the environment | `environment()`, `sys.frame(s)`, `as.environment(-1)`, `sys.function` with its environment, `eval`'s numeric `envir` |
| A read of the bindings of the pending environment `parent.frame()` handed out, or that environment kept past the frame's return | `eval::resolve_pending_read`, `finish_frame` in `eval.rs` |
| Code that the compiler did not translate and that runs in the frame | `Op::Ast` (`<<-` targets, `...` forms), `BoundEval`, the debugger and `browser()` |
| S3 and S4 dispatch reading the generic's frame | `UseMethod` (`dispatch.rs`), and `NextMethod` only from a function that was not dispatched to (no `.Class` or `.GenericCallEnv` in its frame); `standardGeneric`, which reads the signature arguments from the generic's frame and evaluates the method in a frame derived from it (`do_standard_generic` passes `call.env` to `dispatch_generic`), and `exec_method` and `callNextMethod` (`methods.rs`) |
| Condition handlers and restarts that record a frame | `.addCondHands`, `.addRestart` (`conditions.rs`) |
| The C API | `R_GetCurrentEnv`, `PRENV` on a promise over the frame, `.External` and `.External2` (which pass the environment) |
| A promise over the frame still unforced when the frame returns | `run_frame` in `eval.rs` |
| Subassignment, a replacement call or `x$name` on an object, which may dispatch to a method | `op_sub_assign`, `op_replace`, `op_field` in `bytecode.rs` |
| A method `UseMethod` dispatches to while the generic has locals to forward (the R < 4.4 behavior) | `do_usemethod` in `dispatch.rs` runs the method in a heap frame, because a slot frame shows the forwarded locals only once it has an environment |

A builtin that receives `call.env` and only reads a frame property (`missing`, `nargs`, `on.exit`,
`sys.call`, `match.call`) gets the frame reference instead and does not reify.

`sys.frame()` called from a callee reifies the caller's frame, not the callee's own, which makes
`match.arg` (it evaluates the caller's formals through `sys.frame(sys.parent())`) a common trigger
in package code. `parent.frame()` instead hands out the caller's pending environment
(`eval::frame_sys_parent_pending`), because most package code only passes that frame on or
compares it: `do.call(f, args, envir = parent.frame())`, `tryCatch`'s handler frame, and
`withr::defer(expr, envir = parent.frame())`, which registers exit code on the caller. The caller
reifies only when code reads or writes its bindings through that environment (counted as
`parent.frame`, an observing cause), or when the environment outlives the frame (see "Exit code
and returns" below). Every lookup of a frame by its environment goes through `Frame::owns`, which
also matches the pending environment, so `on.exit`, `sys.function`, `Recall`, `UseMethod` and the
`parent.frame` chain find a frame that has handed out its pending environment.

### Reads that must not reify

These are hot in package code and work from the slots and the frame reference alone:

- `substitute(x)` and `deparse(substitute(x))`, which read the promise's expression, not its
  environment.
- `missing(x)`, including on an argument forwarded from the caller, which follows the promise into
  the caller's frame and reads its slot's missing bit.
- `...length()`, `..1` and `...elt(n)`, which read the `...` slot.
- `nargs()`, `sys.call()`, `match.call()` and `on.exit()`, which read the frame record.
- `UseMethod(generic)` and `UseMethod(generic, object)`, `.Internal(...)`, `quote(...)`,
  `x <<- value` and `switch(selector, ...)` over the frame's `...`, which `slot_special_call` in
  `bytecode.rs` runs on the slots.
- `parent.frame()` that is only passed on or compared (see above).
- `x[i]` and `x[[i]]` whose other arguments are constants, empty or locals, such as `s[[field]]`
  with `field` a formal (`slot_subject_call` in `bytecode.rs`). The subject evaluates first. An
  object subject then looks for its method before anything else runs (GNU R's `DispatchOrEval`
  order), starting from the enclosure, which answers as the frame would because the check refuses
  a frame with a local named like a method of the generic or named `.packageName`. Only a method
  found reifies the frame, so that the method's promises (a fresh promise of `field` in the frame,
  so `substitute(i)` in the method returns `field`) have an environment to point at. Without a
  method, the other arguments evaluate from the slots after the lookup, and a formal that is
  missing, also deeply through the caller's promise of a missing argument, passes on as an empty
  argument. An S4 subject and a frame subject of `[` take the environment's path.

### The C API

`crates/libr` exports no function that walks the context stack. `R_GlobalContext` and
`R_ToplevelContext` point at one opaque zeroed block that reads as "at top level" (`native.c`), and
`R_sysframe`, `R_sysfunction` and `Rf_sysparent` are not exported. The routes that hand native code a
frame's environment are `R_GetCurrentEnv` (`eval.rs`, through `current_env`), `PRENV` on a promise,
and the `rho` argument of `.External` and `.External2`; each reifies first. Every other function that
takes an environment (`Rf_eval`, `R_tryEval`, `Rf_findVar`, `R_findVarLoc`, `R_getVar`,
`R_existsVarInFrame`, `R_ParentEnv`, `FRAME`, `ENCLOS`) receives an `SEXP` that one of those routes or
R code already handed out, so it only ever sees a reified environment, and binding cells keep their
stable addresses for `R_findVarLoc`. `R_GetCurrentSrcref` reads the frame record and does not reify.

### Identity

Reification happens once per frame. The frame record keeps the environment it created, so a second
`environment()`, `sys.frame()` of the same frame, and the environment a promise or closure captured
are the identical `SEXP`, and `identical()` and `environmentName()` behave as for a heap frame.

## Promises over a slot frame

The second step is built (`slot_promise`, `force_slot_promise` and `resolved_env` in `eval.rs`,
`slot_promises_into` in `bytecode.rs`). It covers the 33% of calls that only promises capture.

- **Which calls.** A call from an unreified slot frame passes its arguments as promises over the
  frame when every argument is a constant, the missing marker, a symbol, or code the compiler
  compiled against the caller's slot table, and `...` passes on what the frame collected. A
  `..N` symbol, a call that is not compiled, or a compiled promise that reads a local the table does not number falls back to the
  heap-frame path, which reifies the caller.
- **The pending environment.** The first such promise allocates an empty environment for the frame
  (`Frame.pending`), flagged `PENDING_FRAME`, and the promises name it as their environment. If the
  frame reifies, `reify` fills that environment and clears the flag, so a promise forced later reads
  the same object every other reader sees.
- **Forcing.** Forcing a promise over a pending environment finds the owner frame (the frame whose
  `pending` it is) and reads its slots: a symbol reads its slot and falls back to the enclosure when
  the slot is unbound, and compiled code runs on the owner's registers with `Interp.slot_owner` set
  to the owner, so `slot_owner_env` and `Call::frame_index` name the owner and not the frame forcing
  it.
- **Return.** `Frame.unforced` counts the frame's promises that have not been forced yet. A frame
  that returns with any reifies first, so an escaped promise keeps reading the values the slots held
  at return, as it would from a heap frame.
- **Frame identity.** A callee whose caller is an unreified slot frame records the caller's index
  (`Frame.caller`), which `envir::caller_index` and `frame_sys_parent` follow. A promise forced
  further down the stack therefore still reports its own frame's caller.
- **Readers of a promise's environment** go through `eval::promise_env`, which reifies the owner:
  `PRENV` in the C API, serialization and the table engine's `eval` expansion. `missing()`
  through a promise of a name reads the owner's slot instead (`pending_slot_is_missing`), as
  the binding in the reified environment would answer. Identity checks against a promise's environment compare the pending
  environment, which is the frame's environment once it reifies.

## Closures over a slot frame

A `function` expression compiled in an unreified slot frame makes its closure on the frame's
pending environment (`slot_closure`) instead of reifying, so a `tryCatch` handler or an `lapply`
callback that never escapes costs no environment.

- **Reads resolve through the heap.** Every binding read or write of an environment goes through
  `bindings` in `heap.rs`, which checks `PENDING_FRAME` and, when set, calls the resolver the
  interpreter installs (`eval::resolve_pending_read`). The resolver reifies the live owner frame,
  with collection inhibited, and the read proceeds on the filled environment. No reader can see
  the empty pending frame: `CLOENV` followed by `findVarInFrame` in C, `environment(f)` and
  `get`, lexical lookup from the closure's body, and the analysis passes all reach bindings this
  way. Readers that use only the environment's identity or its enclosure need no resolution,
  because reifying keeps the object.
- **Tracking.** `Interp.pending_closures` lists each closure with its owner frame, at most 16 per
  frame, and `Frame.unforced` counts them with the frame's promises. A 17th closure reifies the
  frame.
- **Return.** A closure dies with its frame when its reference count equals the number of the
  frame's slots that hold it, it is not the frame's result, `R_PreserveObject` does not hold it
  and no global calling handler is it. A frame that returns with any other closure reifies first
  (the profile reason "closure outliving the frame"), so the escaped closure sees the values its
  variables held at return. A frame that returns
  through an error keeps none of its closures as dead.

## Exit code and returns

- **Registering.** `on.exit(expr, add, after)` with constant `add` and `after` registers on the
  frame's exit list (`Frame.on_exit`) without an environment (`slot_on_exit` in `bytecode.rs`).
  The entry is `expr` compiled as promise code against the frame's slot table, or the expression
  itself when it does not compile.
- **Running.** `run_on_exit` in `eval.rs` runs a compiled entry on the slots
  (`run_promise_on_slots`) and evaluates any other entry in the frame's pending environment, so
  exit code registered through `do.call(on.exit, ..., envir = parent.frame())` reifies the frame
  only if it reads a binding. `sys.on.exit()` shows the source of a compiled entry.
- **Return.** A `return()` evaluated in the pending environment targets that environment, which
  `finish_frame` catches for an unreified frame.
- **Escape.** A frame that returns while anything but its dying closures and its unforced default
  promises holds its pending environment reifies first, with the profile reason "environment
  outliving the frame" (`pending_env_kept`). The check reads the environment's reference count,
  so an environment that is the result, is held by `R_PreserveObject`, or is stored in a binding,
  a list or another environment's enclosure is kept.

## Subassignment and replacement in slots

`x[i] <- v`, `x[[i]] <- v`, `x$name <- v` and one-level replacement calls with a builtin setter,
such as `names(x) <- v`, update a slot without an environment when `x` is not an object
(`assign_slot_element`, `replace_variable` in `bytecode.rs`). The primitive's own code runs with
the replacement call `` `[<-`(`*tmp*`, i, value = v) `` that the compiler stores beside the
instruction, so errors name the same call as the general assignment's. The value changes in place
only when the slot alone holds it (`slot_owns`: a reference count of one and no expression
register holding it); otherwise the slot gets a copy, so `y <- x; x[1] <- 0` leaves `y` alone.
An object reifies the frame, because a replacement method may look at it. `x$name` reads on a
non-object likewise skip the environment.

## Argument references

A variable passed to a callee that has a direct-call plan needs neither a promise allocation nor
the caller's pending environment, because the callee's frame is above the caller's for as long
as the callee runs. A slot frame passes such a variable as an argument reference to its slot
(`eval::argument_reference`, from `slot_promises_into`), and a caller that has an environment
passes a reference to the name in that environment (`eval::env_reference`, from
`referenced_args_into`), so calls from test code and from reified frames allocate no promise
either. Compiled argument code is passed the same way, as a code reference (`eval::code_reference`
from a slot frame, `eval::env_code_reference` from a caller with an environment). The AST evaluator plans its
calls too: `apply_call` asks `direct_sources` for the callee's plan at each call and binds the
references it builds with `call_with_plan`, as the bytecode call instruction does. It passes
constants as references as well, because lazyeval-style native code reads a promise's
environment and the AST evaluator is what runs uncompiled package code.

- **Shape.** A reference is a promise with `gp::ARGUMENT_REFERENCE`: its expression is the
  variable's name. A reference into an environment keeps that environment in its environment
  field. A code reference from a slot frame keeps the compiled code there and the code's
  source as its expression, and one from a caller with an environment keeps the environment
  there, the source as its expression and the code in its attribute. A reference to a slot frame's variable has nil there, and its attribute is an integer vector
  holding the caller's frame position, the variable's slot (`NO_REFERENCE_SLOT` when the caller
  keeps the variable in no slot) and the caller's header register, which `reference_target`
  checks so a reference that outlived its frame panics. References come from `Interp.references`, a pool
  that the calling instruction releases when the call returns, so a call allocates none once
  the pool has grown. Each pooled reference owns its attribute vector, held in
  `Interp.reference_targets` beside it, and release puts that vector back in place when a code
  reference borrowed the attribute.
- **Forcing.** `force_promise` forces a reference once, reading the caller's slot at that moment:
  a value as it is, a promise (or another reference) by forcing it, an unbound slot from the
  caller's enclosure, and anything once the caller has an environment from that environment.
  A reference into an environment forces as a promise of the name in it would, and a code
  reference runs its code on the caller's slots (`run_promise_on_slots`) or in the caller's
  environment (`run_promise`), as a promise over that code would. A constant forces to itself.
  A write the caller's frame takes before the force is therefore visible, and one after it is
  not, exactly as for a promise of the name.
- **Readers.** `missing()` follows a reference into the caller (`reference_is_missing`) and is
  false for a code reference, `substitute`, `match.call` and `sys.call` read the name or source
  from the expression, and
  `promise_env` reifies the caller. The package-call counter does not count a reference as a
  promise.
- **Dots.** A callee with a `...` formal gets a plan too: the formal's source is
  `DOTS_SOURCE`, and the positions of the arguments `...` collects follow the formals' sources,
  so `call_with_plan` builds the `...` list from them. Forwarding `...` from a slot frame passes
  its items unchanged, which hands the same reference to a second frame.
- **Escape.** Before anything could keep a reference past the call, `materialize_references`
  turns the callee's references into ordinary promises over the caller (`detach_reference`): a
  forced one into a forced promise of its name, an unforced one into a promise over the
  caller's environment or pending environment. The conversion happens in place, and a fresh
  object takes the reference's place in the pool, so every frame that holds it, through its
  slots or a forwarded `...`, holds the same promise, which forces once. `reify` runs it, as do
  S3 and S4 dispatch, which hand the arguments to a method, and `call_with_plan` when a debugger
  or move tracking sends the call down the generic path. Forwarding `...` to a callee without a
  plan detaches the references among its items first.
- **Scope.** Every argument to a planned callee becomes a reference except a `..N` symbol and a
  value that is already a promise, and compiled code passes constants as they are. The
  element call that `lapply`, `vapply` and `Map` make, `FUN(X[[i]])`, passes its first argument
  as a reference into the builtin's environment whose value is already the element, so the
  callee neither allocates a promise nor reifies on it. A call
  that passes `..N` gets no plan, so its other arguments become promises. When per-call matching
  of a call that passes `...` fails (an unused argument, a default that is not a literal), the
  call detaches its references and takes the generic path.

`crates/oracle/cases/environments__argument_references.R`,
`environments__code_argument_references.R` and `environments__dots_argument_references.R` pin
these rules.

## Builtin wrappers

Package code is full of closures whose body is one builtin call, such as rlang's
`node_car <- function(x) { .Call(ffi_node_car, x) }`. A planned call of such a wrapper runs the
builtin directly (`call_wrapper` in `bytecode.rs`, `eval::run_wrapper`), with no body to
execute, no promises and no environment.

- **Which bodies.** A slot-table body that is `f(...)` or `{ f(...) }`, where `f` is a builtin
  found from the closure's environment, and every argument of `f` is a formal, a literal or a
  variable bound outside any function frame. The closure has no `...` formal, and every default
  is missing, `NULL` or a constant.
- **Pin.** The site's plan keeps a pin (`CallPlans::pins`, the one heap object a plan holds
  strongly, and dropped at every major collection because the enclosure it holds can reach the
  plan's own keys): the enclosure, each
  argument's source, and the cell and lookup stamp of each looked-up symbol, `f` first and `{`
  when the body has braces. A rebinding of any of them changes its stamp, so the next call
  re-pins. A body that does not qualify is pinned as `NULL` and never analyzed again.
- **Which calls.** Each argument the call passes must read without running code: a literal, a
  slot or variable holding a value, a forced promise, or an argument reference that resolves
  without code. A call argument (`-1` included) or an unforced promise sends the call down the
  generic path, so laziness is unchanged. A formal the call does not supply takes its constant
  default. Debugged, stepped or traced closures, the debugger and move tracking also take the
  generic path.
- **Frame.** The call still pushes a slot frame for the wrapper, holding the argument values,
  so `sys.call`, `sys.calls`, `sys.function`, `conditionCall` and native code that calls back
  into R see the wrapper exactly as they would through the generic path. The builtin gets the
  inner call (`log(x)`, not the `{` around it) as its call.
- **Reify.** The slots hold values, so a reify of the wrapper's frame (a handler calling
  `sys.frames()`, say) binds each formal the caller passed a variable to as a forced promise
  whose code is that variable, as the generic path would have. `substitute(x, frame)` then
  answers the variable's name, and building those promises costs nothing on calls that never
  reify.

`crates/oracle/cases/functions__builtin_wrappers.R` pins these rules with the JIT off, on and
forced. Measured on a `node_car` loop: 7,751 instructions per call through the generic path,
6,258 inlined.

## Checking it

Behind `syntax::testing::enable_invariants()`, reification cross-checks itself: it compares the
environment built from the slots with what the heap-frame path would have bound for the same call
(the same symbols, values and missing flags). A scenario in `crates/ry/tests/scenarios/` exercises
each trigger above and each read that must not reify.

## Why frames reify

`--profile-summary` reports, under "Package calls", how many calls into package code ran without
an environment and why each of the rest has one. `reify` records its cause (`eval::Reify`: the
builtin that asked, or a named site such as `"function definition"` or `"method dispatch"`) in
`Frame.reified_by`, and the frame's pop classifies the call:

| Class | Meaning |
|---|---|
| direct | the frame never reified and was passed no promise |
| a reify cause | the frame reified for that reason; `eval::OBSERVING_REASONS` marks the causes where code observed the environment itself (`eval`, `environment()`, `sys.frame`, `parent.frame`, `assign`, `get`, native code, `.External2`, `environment outliving the frame`) |
| `promise argument: variable`, `call`, `other` | the frame stayed unreified but was passed a real promise of that kind |
| `frame with an environment` | the callee has no slot table and always gets a heap frame |

The section prints the direct share, the direct share of calls that did not observe their
environment, and the table of classes. A second section, "Slot frames turned into environments",
counts every reification by its cause, whatever the frame's function, so it also covers base R
and script closures. The counting runs only while a profile summary is collected.

## Prior decision

`decisions.md` ("Step 2 of the untyped roadmap") rejected slot frames when a plain call cost 3,968
instructions and binding cells about 420 of them. That estimate covered values held inline with
cells on demand, not frames without an environment, and it predates the testthat profile above,
where frame work is about 2.2k instructions per call.

## Open questions

- Whether the JIT's tier 1 should read slots directly. It already keeps its own slots for the same
  symbols (`crates/jit/src/analysis.rs`) and writes them back to the heap frame. With slot frames,
  tier 1 could share the interpreter's slots, which would remove its write-back.
- The baseline tier probes the heap frame inline at fixed offsets (`heap::env::layout`). It would
  read slots by index instead.
