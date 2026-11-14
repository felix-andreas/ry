---
title: Slot frames
description: "Closure calls that keep their variables in the interpreter's register window and create an environment only when something asks for one"
---

A closure call that binds a heap frame allocates an environment object, one binding cell per formal, and a
promise for each argument that is not a constant. It releases them all when it returns. On the testthat workload
(`tools/bench/applications/testthat.R`, images on, JIT at its default), that work was the largest exclusive
cost under callgrind before slot frames:

| Sink | Share of 8.03G instructions | Largest functions |
|---|---|---|
| Frames and argument matching | 23.8% | `run_closure` 3.8%, `bind_arguments` 3.2%, `define_formals_uncollected` 2.1%, `release_frame` 1.5%, `clear_frame` 1.5% |
| Binding cells and lookup | 14.3% | `lookup_variable` 2.9%, `lookup_function` 2.5%, `frame_cell` 2.1%, `run_get_var` 1.8% |

That is about 2.2k instructions of frame work per closure call. A slot frame avoids it. The call
keeps its formals and locals in slots of the register window, takes its arguments as pooled
references that allocate nothing, and gets an environment object only when something asks for one.
That environment binds nothing at first: a variable moves into a binding cell only when a lookup
through the environment asks for that variable.

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
promise forces the frame into an environment that binds every variable. It saves the cells on up to
91% of calls if a promise can point at an environment that binds nothing and takes the variables
only when the promise outlives the call.

## The shape

`bind_slots`, `pending_env`, `box_variable` and `lift_frame` in `crates/interp/src/eval.rs` implement
it.

- **Which calls get a slot frame.** The compiler gives a body a slot table (`SLOTS` in its
  constants) when `local_symbols` succeeds (`collect_bindings` in `crates/interp/src/bytecode.rs`).
  It fails for a body that may bind a name its own text does not show or hand its frame to code
  that may: a bare `environment()`, `sys.frames()`, `attach` or `source`, an `eval`,
  `evalq`, `assign`, `delayedAssign` or `rm` without an explicit environment, an assignment to a
  string target, or a read of an S3 dispatch variable (`.Generic`, `.Class`, `.Method`, `.Group`,
  `.GenericCallEnv`, `.GenericDefEnv`). The same calls with an explicit environment, `local`,
  `with`, `on.exit`, `sys.function` and `do.call` of a named function are accepted, because every
  route by which a callee reaches the frame (`parent.frame`, `sys.frame`, `environment(fn)` of a
  closure made inside) gets the frame's environment on demand. The body also has at most 64 formals and does not
  open with one of the fast-path prologues that evaluate code in their frame or define variables
  there (`match.arg`, `Reduce` and rlang's `trace_back`, listed in
  `base_fast_paths::prologue_needs_environment`). The other prologues read their formals from the
  slots and return through the pending environment (`base_fast_paths::ClosureFrame`), so
  `stopifnot`, `structure`, `setdiff` and the withr helpers keep their slot frames. At the call, `bind_slots` also refuses a closure under the debugger, a closure with a type
  signature, any call made while a caller's collection is moved into a parameter (`moves.rs`), and a call where a missing formal's default
  is neither a literal, a symbol, nor code compiled with its own slot table. Those calls bind a
  heap frame.
- **Defaults.** A missing formal whose default is not a literal starts with the default's
  expression in its slot and its missing bit set. Once the frame is pushed, `default_promises`
  replaces the expression with an argument reference of the frame to itself (see "Argument
  references"): a code reference that runs the default's code on the frame's slots, compiled
  once per closure (`bytecode::slot_defaults`, cached in the slot table's third element against
  the closure's formals and never written to a code image), or a reference to the variable a
  default names. The reference comes from the pool and goes back when the frame returns
  (`Frame.references`, `release_frame_references`), so a default costs the call no promise and no
  environment.
- **S3 methods.** A method called by `UseMethod` or `NextMethod` gets a slot frame too, and
  `UseMethod` from a slot frame (`dispatch::use_method_in_slots`) hands the method the argument
  references the generic was called with, unchanged. The method's dispatch variables live beside
  the frame (`Interp::method_vars`, `Frame::method_vars`) and get a binding cell when something
  reads one by name (`box_variable`) or the environment takes every variable (`lift_frame`).
  `.GenericCallEnv` is recorded as unbound when the generic's caller is a slot frame without an
  environment, and reading it asks that caller for its environment (`eval::method_var_value`).
  `NextMethod` from such a method reads the dispatch variables without moving them
  (`eval::pending_name`), passes the next method promises over the method's environment, and
  lets the method return in slots when the next method neither forced nor kept them
  (`MadePromises` in `dispatch.rs`).
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
  slot number, and `<<-` starts from the closure's environment, so neither asks for the frame's
  environment.
- **The environment.** A slot frame has no environment object until something asks for one, and
  the object it then gets binds nothing. "The environment of a slot frame" below gives its states.
- **Frame identity.** `Frame.env` is nil while the frame's own code runs on its slots,
  `Frame.pending` holds the environment once it exists, and `Frame.sys_parent` is nil while the
  caller is a slot frame, which `Frame.caller` then names by position (`envir::caller_index`).
  `frame_env(index)` and `frame_sys_parent(index)` are the only ways to get either as an `SEXP`,
  and both make the environment on demand.
- **Release.** A frame whose environment took no variable drops its slots' references on return
  instead of `release_frame` and `clear_frame`, and an environment that nothing outside the frame
  holds is discarded with it (`finish_frame`, `outliving_cause`).
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
- **Call shape.** Each plan row also holds the callee's `CallShape`: its formal count and its
  slot and register counts, 6 bytes. A formal the site leaves out gets `LITERAL_DEFAULT` or
  `PROMISED_DEFAULT` as its source when it has a default, so the sources say which defaults the
  call takes. Binding reads the shape instead of walking the formals and the slot table, writes a
  left-out formal's default by walking the formals only as far as the last one left out, and
  makes default promises only for the `PROMISED_DEFAULT` formals. The shape lives in the
  plan, not in the compiled body, because a closure whose formals were replaced keeps its body:
  the defaults are a fact of the formals and body together, which is the plan's key. The site
  reads its row after collecting the arguments, which runs no R code; a generation counter that
  every store and free moves confirms the row is unchanged with one compare.
- **Forwarding `...`.** A call that passes `...` appends the items the frame's `...` slot holds,
  to a closure (`slot_dots_into`) or, forced, to a builtin (`slot_builtin_call`, when every other
  argument is a constant or a slotted local), so `f(x, ...)`, `list(...)` and `c(x, ...)` leave the
  frame without an environment. A call evaluated in the frame's environment that passes `...` on,
  such as the element call `FUN(X[[i]], ...)` of `lapply`, `vapply` and `Map`, reads the slot in
  place (`eval::dots_binding`).
- **Not built.** Slot access from the JIT's tiers ([the native execution model](/development/design/r-runtime/#the-native-execution-model)).

## The environment of a slot frame

The environment goes through up to three states, and most frames stop before the second.

| State | Flag on the environment | Where a variable is | What the frame's own code reads |
|---|---|---|---|
| No environment | (no object) | its slot | slots |
| Empty | `gp::PENDING_FRAME` | its slot | slots |
| Partial | `gp::PARTIAL_FRAME` | its slot or a cell, never both | the environment, by name |
| Complete | none | a cell | the environment, by name |

- **Empty.** `pending_env` allocates one environment that binds nothing, enclosed by the
  closure's environment. Everything that only names the frame is served by that object: a promise
  or a closure made in the frame, `parent.frame()`, `sys.function`, a builtin that dispatches, a
  condition handler, exit code registered from a callee. The heap knows which names the frame may
  bind (`expect_bindings`: the slot table's names and a method's dispatch variables), so a lookup
  of any other name passes on to the enclosure without asking the frame.
- **Partial.** A lookup of a name the frame may bind, for which the environment has no cell, calls
  the resolver the interpreter installs in the heap (`eval::resolve_pending_access`, from
  `bindings_for` in `heap.rs`). `box_variable` moves that one variable from its slot into a cell
  (`box_slots`), and `leave_slots` switches the frame's own code to reading and writing by name:
  the header register and `Frame.env` hold the environment from then on. Every other variable
  stays in its slot until a lookup asks for it. A binding made by name (`assign`, `delayedAssign`)
  makes the same switch without moving anything.
- **Complete.** `lift_frame` moves every variable still in a slot into a cell, binds a method's
  dispatch variables, orders the bindings as a heap frame lists them (formals first) and clears
  the flags, which leaves an ordinary environment. It runs when something reads the bindings as a
  whole (`bindings` in `heap.rs`: `ls`, `as.list`, `mget`, serialization) and when the frame
  returns while something that outlives it holds the environment (see "Exit code and returns").

A variable is in its slot or in a cell, never in both, so there is one source of truth for each
variable in every state and nothing is copied back.

A lookup that reaches the environment has to find the frame it stands for. The environment's
frame record stores the frame's position on the interpreter's frame stack (`Heap::slot_owner`,
set by `pending_env` and cleared by `lift_frame`), in the field that counts removed bindings in
an ordinary environment. The two uses cannot meet, because a slot frame's environment takes every
variable and stops being one before a binding is removed from it. `eval::pending_owner` checks
that the frame at that position still names the environment, so a use after the frame returned
without it is a panic, not a read of another frame.

Code that reads an environment's frame record inline, without the heap's lookup, sees only the
cells: the symbol filter of an empty or partial environment does not show the variables still in
slots. Tier 1 reads the enclosing environments of an inlined closure that way, so its
compile-time probe (`Heap::probe`), the walk it emits (`codegen.rs`) and `jit_enter_frame` test
`gp::SLOT_FRAME` first and send such an environment to the lookup that asks the frame.

### What asks for the environment

| What | State it needs | Where |
|---|---|---|
| A promise the argument-reference path does not cover (see "Promises over a slot frame"), `delayedAssign` | empty | `slot_promise`, `slot_promises_into` |
| A closure created in the frame | empty | `slot_closure`; `make_closure` for an uncompiled `function`; `~` (the formula's environment) |
| `parent.frame()`, `sys.function`, `sys.frame(s)`, `environment()`, `as.environment(-1)`, `eval`'s numeric `envir` | empty, then whatever the code that receives it reads | `frame_env`, `frame_sys_parent` |
| Code that the compiler did not translate and that runs in the frame | partial, for the variables it reads | `Op::Ast` (`<<-` targets, `...` forms), `BoundEval`, the debugger and `browser()` |
| S3 dispatch from a builtin on an object argument | empty | `bytecode::call_builtin` |
| S4 dispatch reading the generic's frame | partial | `standardGeneric`, which reads the signature arguments from the generic's frame (`do_standard_generic`), `exec_method` and `callNextMethod` (`methods.rs`) |
| Condition handlers and restarts that record a frame | empty | `.addCondHands`, `.addRestart` (`conditions.rs`) |
| The C API | empty, then whatever the native code reads | `R_GetCurrentEnv`, `PRENV` on a promise over the frame, `.External` and `.External2` (which pass the environment) |
| Subassignment, a replacement call or `x$name` on an object, which may dispatch to a method | partial, for the variable assigned | `op_sub_assign`, `op_replace`, `op_field` in `bytecode.rs` |
| A method `UseMethod` dispatches to while the generic has locals to forward (the R < 4.4 behavior) | complete | `do_usemethod` in `dispatch.rs` runs the method in a heap frame |
| A promise, a closure or the environment itself held past the frame's return | complete | `finish_frame`, `outliving_cause` |

A builtin that receives `call.env` and only reads a frame property (`missing`, `nargs`, `on.exit`,
`sys.call`, `match.call`) gets the frame reference instead and asks for no environment.

`sys.frame()` called from a callee asks for the caller's environment, not the callee's own, which
makes `match.arg` (it evaluates the caller's formals through `sys.frame(sys.parent())`) a common
reason in package code. `parent.frame()` is served the same way, and most package code only passes
that frame on or compares it: `do.call(f, args, envir = parent.frame())`, `tryCatch`'s handler
frame, and `withr::defer(expr, envir = parent.frame())`, which registers exit code on the caller.
The caller's variables move only when code reads or writes them through that environment. Every
lookup of a frame by its environment goes through `Frame::owns`, which also matches
`Frame.pending`, so `on.exit`, `sys.function`, `Recall`, `UseMethod` and the `parent.frame` chain
find a frame whose own code still runs on its slots.

### Reads that move nothing

These are hot in package code and work from the slots and the frame record alone:

- A variable or function lookup that reaches the frame's environment from a closure or a promise
  made in it. `find_var`, `find_fun` and the byte code's `lookup_variable` and `lookup_function`
  take a value or a promise from the slot where it sits (`eval::slot_binding`), so a callback
  reading its defining frame's locals leaves them in their slots.
- `substitute(x)` and `deparse(substitute(x))`, which read the promise's expression, not its
  environment.
- `missing(x)`, by name through the environment or on an argument forwarded from the caller. It
  follows the promise or reference into the frame it names and reads the slot and its missing
  bit (`slot_is_missing`, `reference_is_missing`).
- `...length()`, `..1` and `...elt(n)`, which read the `...` slot.
- `nargs()`, `sys.call()`, `match.call()` and `on.exit()`, which read the frame record.
- `UseMethod(generic)` and `UseMethod(generic, object)`, `.Internal(...)`, `quote(...)`,
  `x <<- value` and `switch(selector, ...)` over the frame's `...`, which `slot_special_call` in
  `bytecode.rs` runs on the slots.
- `x[i]` and `x[[i]]` whose other arguments are constants, empty or locals, such as `s[[field]]`
  with `field` a formal (`slot_subject_call` in `bytecode.rs`). The subject evaluates first. An
  object subject then looks for its method before anything else runs (GNU R's `DispatchOrEval`
  order), starting from the enclosure, which answers as the frame would because the check refuses
  a frame with a local named like a method of the generic or named `.packageName`. Only a method
  found asks for the environment, so that the method's promises (a fresh promise of `field` in
  the frame, so `substitute(i)` in the method returns `field`) have an environment to point at.
  Without a method, the other arguments evaluate from the slots after the lookup, and a formal
  that is missing, also deeply through the caller's promise of a missing argument, passes on as an
  empty argument. An S4 subject and a frame subject of `[` take the environment's path.

### The C API

`crates/libr` exports no function that walks the context stack. `R_GlobalContext` and
`R_ToplevelContext` point at one opaque zeroed block that reads as "at top level" (`native.c`), and
`R_sysframe`, `R_sysfunction` and `Rf_sysparent` are not exported. The routes that hand native code a
frame's environment are `R_GetCurrentEnv` (`eval.rs`, through `current_env`), `PRENV` on a promise,
and the `rho` argument of `.External` and `.External2`; each makes the environment first. Every other function that
takes an environment (`Rf_eval`, `R_tryEval`, `Rf_findVar`, `R_findVarLoc`, `R_getVar`,
`R_existsVarInFrame`, `R_ParentEnv`, `FRAME`, `ENCLOS`) receives an `SEXP` that one of those routes or
R code already handed out, so every binding it reads or writes goes through `bindings` or `bindings_for` in `heap.rs`, which
moves the variable into a cell first. A cell keeps its address once it exists, which
`R_findVarLoc` relies on. `R_GetCurrentSrcref` reads the frame record and asks for no environment.

### Identity

A frame makes its environment once and keeps it in the frame record, so a second `environment()`,
`sys.frame()` of the same frame, and the environment a promise or closure captured are the
identical `SEXP` through every state, and `identical()` and `environmentName()` behave as for a
heap frame.

## Promises over a slot frame

Most arguments are argument references (see below) and need no promise. A call from a slot frame
makes real promises when the callee has no direct-call plan or takes a heap frame (`slot_promise`
in `eval.rs`, `slot_promises_into` in `bytecode.rs`).

- **Which calls.** Every argument is a constant, the missing marker, a symbol, or code the compiler
  compiled against the caller's slot table, and `...` passes on what the frame collected. A `..N`
  symbol, a call that is not compiled, or a compiled promise that reads a local the table does not
  number is evaluated by name in the environment, which moves the variables it reads.
- **The environment.** The promise names the frame's environment, which the first such promise
  makes in the empty state.
- **Forcing.** Forcing a promise over a slot frame's environment finds the owner frame and, while
  every variable is in its slot, reads the slots (`slot_promise_value`): a symbol reads its slot
  and falls back to the enclosure when the slot is unbound, and compiled code runs on the owner's
  registers with `Interp.slot_owner` set to the owner, so `slot_owner_env` and `Call::frame_index`
  name the owner and not the frame forcing it. Once the environment binds a variable, the promise
  evaluates in the environment.
- **Return.** A promise the frame made that is still unforced when the frame returns, and that
  something outside the frame holds, makes the environment complete (`outliving_cause`), so the
  escaped promise reads the values the slots held at return, as it would from a heap frame.
- **Frame identity.** A callee whose caller is a slot frame records the caller's index
  (`Frame.caller`), which `envir::caller_index` and `frame_sys_parent` follow. A promise forced
  further down the stack therefore still reports its own frame's caller.
- **Readers of a promise's environment** go through `eval::promise_env`: `PRENV` in the C API,
  serialization and the table engine's `eval` expansion. Identity checks against a promise's
  environment compare the frame's environment, which is one object through every state.

## Closures over a slot frame

A `function` expression compiled in a slot frame makes its closure on the frame's environment
(`slot_closure`), so a `tryCatch` handler or an `lapply` callback that never escapes costs one
empty environment and no binding cell.

- **Reads.** The closure's body finds a variable of the defining frame in its slot
  (`slot_binding`). Any other reader of the environment's bindings goes through `bindings` or
  `bindings_for` in `heap.rs`, which move what is read into cells: `CLOENV` followed by
  `findVarInFrame` in C, `environment(f)$x` and `get`. Readers that use only the environment's
  identity or its enclosure need nothing moved.
- **Tracking.** `Interp.pending_closures` lists each closure with its owner frame, at most 16 per
  frame (`FRAME_CLOSURES`). A closure past the limit is not tracked and keeps the environment as
  any other holder does. The list is ordered by frame, so a frame's closures are one run found
  from the end (`pending_closures_of`), and the frames that pop give theirs up from the end
  (`forget_pending_closures`). A return therefore never searches the list: a frame without an
  environment has no entry, and `bytecode::return_fast` takes every such return whatever the
  frames below it hold. A wrapper frame that keeps a handler closure for a whole test would
  otherwise send every return above it down the general path.
- **Return.** A closure dies with its frame when its reference count equals the number of the
  frame's slots and cells that hold it, it is not the frame's result, `R_PreserveObject` does not
  hold it and no global calling handler is it. A frame that returns with any other closure makes
  its environment complete first (the profile reason "closure outliving the frame"), so the
  escaped closure sees the values its variables held at return. A frame that returns through an
  error keeps its environment.

## Exit code and returns

- **Registering.** `on.exit(expr, add, after)` with constant `add` and `after` registers on the
  frame's exit list (`Frame.on_exit`) without an environment (`slot_on_exit` in `bytecode.rs`).
  The entry is `expr` compiled as promise code against the frame's slot table, or the expression
  itself when it does not compile.
- **Running.** `run_on_exit` in `eval.rs` runs a compiled entry on the slots
  (`run_promise_on_slots`) and evaluates any other entry in the frame's environment, so exit code
  registered through `do.call(on.exit, ..., envir = parent.frame())` moves only the variables it
  reads. `sys.on.exit()` shows the source of a compiled entry.
- **Return.** A `return()` evaluated in the frame's environment targets that environment, which
  `finish_frame` catches whatever state it is in.
- **Escape.** `outliving_cause` decides at return whether anything that outlives the frame holds
  its environment, from the environment's counted references. Every promise and closure over it
  counts one, as does a binding, a list element or an enclosure that holds it. The holders that
  die with the frame are known: an unforced promise that only a slot or a cell of the frame holds,
  and a closure only the frame holds. Any count beyond those keeps the environment, and so do the
  three holders that are not counted: the value returned, `R_PreserveObject` and a global calling
  handler. A promise or closure that one of the three holds does not die with the frame either,
  so native code that preserved a promise it read from the frame can force it after the call
  returned. A kept environment becomes complete (the profile reasons "promise or environment
  outliving the frame" and "closure outliving the frame"); any other is discarded with the frame
  (`Heap::discard_frame`).

## Subassignment and replacement in slots

`x[i] <- v`, `x[[i]] <- v`, `x$name <- v` and one-level replacement calls with a builtin setter,
such as `names(x) <- v`, update a slot without an environment when `x` is not an object
(`assign_slot_element`, `replace_variable` in `bytecode.rs`). The primitive's own code runs with
the replacement call `` `[<-`(`*tmp*`, i, value = v) `` that the compiler stores beside the
instruction, so errors name the same call as the general assignment's. The value changes in place
only when the slot alone holds it (`slot_owns`: a reference count of one and no expression
register holding it); otherwise the slot gets a copy, so `y <- x; x[1] <- 0` leaves `y` alone.
An object target goes through `eval::complex_assign` in the frame's environment, because a
replacement method may look at the frame, and that moves the assigned variable into a cell.
`x$name` reads on a non-object likewise skip the environment.

## Argument references

A variable passed to a callee that takes its arguments straight into slots needs neither a promise
allocation nor the caller's environment, because the callee's frame is above the caller's for as
long as the callee runs. A slot frame passes such a variable as an argument reference to its slot
(`eval::argument_reference`, from `slot_promises_into`), and a caller that has an environment
passes a reference to the name in that environment (`eval::env_reference`, from
`referenced_args_into`), so calls from test code and from frames with an environment allocate no
promise either. Compiled argument code is passed the same way, as a code reference
(`eval::code_reference` from a slot frame, `eval::env_code_reference` from a caller with an
environment). The AST evaluator plans its calls too: `apply_call` asks `direct_sources` for the
callee's plan at each call and binds the references it builds with `call_with_plan`, as the
bytecode call instruction does. It passes constants as references as well, because lazyeval-style
native code reads a promise's environment and the AST evaluator is what runs uncompiled package
code.

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
  `promise_env` asks the caller for its environment. The package-call counter does not count a reference as a
  promise.
- **Dots.** A callee with a `...` formal gets a plan too: the formal's source is
  `DOTS_SOURCE`, and the positions of the arguments `...` collects follow the formals' sources,
  so `call_with_plan` builds the `...` list from them. Forwarding `...` from a slot frame passes
  its items unchanged, which hands the same reference to a second frame.
- **Who may hold one.** A reference is reused once the call it was made for returns, so only a
  slot of a frame above the caller may hold it. Each path that puts an argument anywhere else
  turns the reference into the ordinary promise it stands for first (`detach_reference`): a
  forced one into a forced promise of its name, an unforced one into a promise over the caller's
  environment. The conversion happens in place, and a fresh object takes the reference's place in
  the pool, so every frame that holds it, through its slots or a forwarded `...`, holds the same
  promise, which forces once. The paths are: a callee that binds a heap frame
  (`run_on_heap_frame`), a variable that moves into a cell (`box_slots`), a native typed entry
  (`try_native_typed`), a method that is not a closure (`apply_method`), `NextMethod` and S4
  dispatch (`materialize_references`), and a `...` item forwarded to a callee without a plan.
- **Dispatch.** An S3 method that runs on slots takes the generic's references as they are
  (`use_method_in_slots`), because the calling instruction releases them only after the generic's
  call returns.
- **Scope.** Every argument to a planned callee becomes a reference except a `..N` symbol and a
  value that is already a promise, and compiled code passes constants as they are. A frame's own
  defaults are references of the frame to itself (see "Defaults" above). The element call that
  `lapply`, `vapply` and `Map` make, `FUN(X[[i]])`, passes its first argument as a reference into
  the builtin's environment whose value is already the element, so the callee allocates no
  promise for it. A call that passes `..N` gets no plan, so its other arguments become promises.
  When per-call matching of a call that passes `...` fails (an unused argument, a default that is
  not a literal), the call takes the generic path, which detaches its references.

`crates/oracle/cases/environments__argument_references.R`,
`environments__code_argument_references.R`, `environments__dots_argument_references.R` and
`dispatch__argument_references_dots_and_missing.R` pin these rules.

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
- **Environment.** The slots hold values, so a variable of the wrapper's frame that moves into a
  cell (a handler calling `sys.frames()` and reading it, say) is bound as a forced promise whose
  code is the variable the caller passed, as the generic path would have bound it.
  `substitute(x, frame)` then answers the variable's name, and building those promises costs
  nothing on calls where nothing reads the frame.

`crates/oracle/cases/functions__builtin_wrappers.R` pins these rules with the JIT off, on and
forced. Measured on a `node_car` loop: 7,751 instructions per call through the generic path,
6,258 inlined.

## Checking it

Behind `syntax::testing::enable_invariants()`, `lift_frame` cross-checks itself (`check_lifted`):
once the environment has taken every variable, no slot binds one, the frame's own code reads the
environment, and each of a method's dispatch variables is bound. Six oracle cases pin what R
code can observe, on every engine including the collector's torture modes:
`environments__slot_frames_observed.R` (every route that asks for a frame's environment),
`environments__slot_frames_escaping.R` (promises, closures and environments that outlive their
frame), `promises__default_arguments_on_slot_frames.R`,
`promises__by_name_promises_over_slot_frames.R`,
`dispatch__argument_references_dots_and_missing.R` and
`dispatch__generic_call_env_from_slot_frames.R`.

The bytecode fuzzer (`interp::testing::check_bytecode_agreement`) runs each generated body in a
slot frame whose environment is driven into every state, before the body and after it: an
observer that only names the caller's environment, one that looks a single variable up in it,
and one that lists its bindings. Each run must agree with the AST evaluator. The user journeys
are pinned by `crates/ry/tests/scenarios/slot-frames.scenario` and `slot-promises.scenario`.

## Why a frame has an environment

`--profile-summary` reports, under "Package calls", how many calls into package code skipped the
generic call protocol and why each of the rest did not. `frame_env` records what asked
(`eval::Reify`: the builtin, or a named site such as `"function definition"` or
`"method dispatch"`) in `Frame.reified_by`, and the frame's pop classifies the call:

| Class | Meaning |
|---|---|
| direct | the frame's own code ran on its slots to the end and was passed no promise |
| a named reason | the frame switched to reading by name for that reason; `eval::OBSERVING_REASONS` marks the reasons where code observed the environment itself (`eval`, `environment()`, `sys.frame`, `parent.frame`, `assign`, `get`, native code, `.External2`, `promise or environment outliving the frame`) |
| `promise argument: variable`, `call`, `other` | the frame stayed in its slots but was passed a real promise of that kind |
| `frame with an environment` | the callee has no slot table and always gets a heap frame |

A second section, "Slot frames turned into environments", counts every switch to reading by name
by its reason, whatever the frame's function, so it also covers base R and script closures. The
events table counts the three state changes separately: slot frames that made an environment,
frames that switched to reading by name, and frames whose environment took every variable. The
counting runs only while a profile summary is collected.

## Measured

One run of the finance code base's test suite (53 million closure calls), counted by
`--profile-summary` with the JIT on. The first column is the design this page replaced, where the
first promise, closure or reflective read built the whole environment.

| | Replaced design | This design |
|---|---:|---:|
| objects freed | 348.2 million | 264.0 million |
| binding cells | 65.9 million | 22.8 million |
| promises | 41.1 million | 21.1 million |
| environments | 19.5 million | 14.1 million |
| pairlist cells | 63.1 million | 63.4 million |
| language objects | 18.6 million | 18.7 million |
| promises created for arguments | 23.0 million | 8.5 million |
| package calls with no argument promise and no environment | 59.7% | 81.5% |
| slot frames whose own code reads by name | 10.2 million | 3.4 million |
| slot frames whose environment holds every variable | 10.2 million | 1.3 million |
| collections (minor + major) | 128 + 1 | 110 + 2 |
| time in the collector | 2.41 s | 1.86 s |
| time in closure call setup | 16.9 s (11.2%) | 17.3 s (10.9%) |

The two runs were taken back to back on one machine. Wall time differs by more than 10% between
runs of one binary there, so the two call setup times are equal within the noise.

The objects went away, and the time did not follow. Three measurements agree on that:

- **The three code bases** (`just bench codebases`, one round, JIT on): the geometric mean of
  GNU R's time over ry's is 1.55x with this design and 1.57x with the replaced one in the same
  round, with every test passing on both. One round is good to about 5%.
- **Instructions** (callgrind, a 225-test slice of the finance suite): 27.46 billion against
  27.88 billion, 1.5% fewer. Outside the collector the count is 2.5% lower; the slice happens to
  run one more major collection.
- **The time split** above: call setup takes the same seconds, and the collector, which was 1.6%
  of the run, takes a quarter less.

The reason is that the objects were cheap. A binding cell, a promise or an environment is a
small allocation, and the collector was 1.6% of the run, so 43 million fewer cells and 20 million
fewer promises save little. What a call still pays for is matching its arguments, pushing and
popping the frame record, and the bookkeeping this design adds in the objects' place (the
reference pool, the escape check at return). The sources of the objects
that are left are in `backlog.md`, "The call path after slot frames bind on demand".

## Prior decision

`decisions.md` ("Step 2 of the untyped roadmap") rejected slot frames when a plain call cost 3,968
instructions and binding cells about 420 of them. That estimate covered values held inline with
cells on demand, not frames without an environment, and it predates the testthat profile above,
where frame work is about 2.2k instructions per call.

## Open questions

- Whether the JIT's tier 1 should read slots directly. It already keeps its own slots for the same
  symbols (`crates/jit/src/analysis.rs`) and writes them back to the heap frame. With slot frames,
  tier 1 could share the interpreter's slots, which would remove its write-back.
- Whether a partial frame's own code should keep running on its slots. Today the first variable
  that moves into a cell switches all of the frame's code to reading by name, and each variable it
  then reads moves too. Keeping the code on slots would need every slot instruction, and the JIT's
  code, to test per variable where it lives.
- Nested replacement and replacement on an object (`eval::complex_assign`) build a forced promise
  for the object and one for the value at each level, plus the call and its argument list. GNU R's
  byte code keeps the call as a constant with `*vtmp*` for the value, which would remove most of
  them.
