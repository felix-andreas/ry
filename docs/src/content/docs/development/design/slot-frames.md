---
title: Slot frames
description: "Closure calls that keep their variables in the interpreter's register window and create an environment only when something asks for one"
---

A closure call today allocates an environment object, one binding cell per formal, and a promise for each argument
that is not a constant. It releases them all when it returns. On the testthat workload
(`tools/bench/real/testthat.R`, images on, JIT at its default), that work is the largest exclusive
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
  constants) when `local_symbols` succeeds, which means the body has no `assign`, `eval`, `local`,
  `environment`, `sys.function`, `on.exit`, `Recall` or similar (`collect_bindings` in
  `crates/interp/src/bytecode.rs`), it has no `...` formal and at most 64 formals, and it does not
  open with a base fast-path prologue (`base_fast_paths::has_prologue`), which reads its frame as an
  environment. At the call, `bind_slots` also refuses a closure with tier-1 or baseline machine code
  (which reads variables through the environment), a closure under the debugger or a type signature,
  and a call where a missing formal has a default that is not a literal. Those calls bind a heap
  frame as before.
- **Layout.** The activation reserves `1 + n` registers above its own: a header register, then one
  slot per symbol the table numbers, formals first in order. The header holds the frame's
  environment once it has one, and `R_NilValue` before. A slot holds a value and counts one
  reference, as a binding cell does, so copy-on-modify is unchanged. An unassigned local holds the
  unbound marker, and the frame record keeps one missing bit per formal (`Frame.missing`, a `u64`).
  The slots are GC roots because the whole register file is.
- **Reads and writes.** `GetVar`/`SetVar` on a numbered symbol read and write the slot while the
  header is nil, and the environment after. Builtins that only read a frame property take the
  frame's index (`Call::frame_index`) instead of its environment.
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
- **Not built.** Binding by a per-site plan (`bind_slots` matches by name through the shared match
  cache), promises for non-literal defaults without an environment, and slot access from the JIT's
  tiers.

## Reification triggers

These are the reads that need the environment as an object. Each one reifies the frame first.

| Trigger | Where |
|---|---|
| A promise made in the frame (argument promises, default promises, `delayedAssign`) | `make_promise`, `compiled_promise`, `formal_binding` in `eval.rs`; `NextMethod`'s promises in `dispatch.rs` |
| A closure created in the frame | `make_closure` in `eval.rs`; `Op::Closure`; `~` (the formula's environment) |
| Reflection that returns the environment | `environment()`, `sys.frame(s)`, `parent.frame`, `as.environment(-1)`, `sys.function` with its environment, `eval`'s numeric `envir` |
| Code that the compiler did not translate and that runs in the frame | `Op::Ast` (`<<-` targets, `...` forms), `BoundEval`, the debugger and `browser()` |
| S3 and S4 dispatch reading the generic's frame | `UseMethod` and `NextMethod` (`dispatch.rs`); `standardGeneric`, which reads the signature arguments from the generic's frame and evaluates the method in a frame derived from it (`do_standard_generic` passes `call.env` to `dispatch_generic`), and `exec_method` and `callNextMethod` (`methods.rs`) |
| Condition handlers and restarts that record a frame | `.addCondHands`, `.addRestart` (`conditions.rs`) |
| The C API | `R_GetCurrentEnv`, `PRENV` on a promise over the frame, `.External` and `.External2` (which pass the environment) |

A builtin that receives `call.env` and only reads a frame property (`missing`, `nargs`, `on.exit`,
`sys.call`, `match.call`) gets the frame reference instead and does not reify.

`parent.frame()` and `sys.frame()` called from a callee reify the caller's frame, not their own. That
makes `match.arg` (which evaluates the caller's formals through `sys.frame(sys.parent())`) and
`eval(expr, parent.frame())` the most common triggers in package code. The instrumented build counts
them apart from other reflection, so the second step can tell whether they need a cheaper route.

### Reads that must not reify

These are hot in package code and work from the slots and the frame reference alone:

- `substitute(x)` and `deparse(substitute(x))`, which read the promise's expression, not its
  environment.
- `missing(x)`, including on an argument forwarded from the caller, which follows the promise into
  the caller's frame and reads its slot's missing bit.
- `...length()`, `..1` and `...elt(n)`, which read the `...` slot.
- `nargs()`, `sys.call()`, `match.call()` and `on.exit()`, which read the frame record.

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

The second step addresses the 33% of calls that only promises capture. A promise for a variable
argument holds a reference to the slot frame (index and generation) instead of an environment. Forcing
it during the call reads the slot. If the frame returns while such a promise is still unforced and
referenced, the return reifies the frame and points the promise at the new environment. `PRENV`,
`substitute` and native code see only reified environments, because reading a promise's
environment reifies it. This step is planned after the first step's numbers are in.

## Checking it

Behind `syntax::testing::enable_invariants()`, reification cross-checks itself: it compares the
environment built from the slots with what the heap-frame path would have bound for the same call
(the same symbols, values and missing flags). A scenario in `crates/ry/tests/scenarios/` exercises
each trigger above and each read that must not reify.

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
