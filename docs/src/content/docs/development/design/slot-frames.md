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

- **Slots.** The compiler numbers every symbol a body binds when `local_symbols` succeeds, which
  means the body has no `assign`, `eval`, `local`, `sys.function` or similar (`collect_bindings` in
  `crates/interp/src/bytecode.rs`). Formals come first, in order. The activation reserves that
  many slots after its registers, and `GetSlot`/`SetSlot` replace `GetVar`/`SetVar` for them. A
  body whose symbols cannot be numbered keeps today's heap frame from the start.
- **Values, counts and flags.** A slot holds the value and counts one reference, exactly as a
  binding cell does, so copy-on-modify is unchanged. A per-frame bitset holds what a cell keeps in
  its flags: missing, and assigned since the call began (which `missing()` needs).
- **Binding by plan.** Each call site and callee pair keeps a matching plan in the site's cache. A
  positional call copies argument `i` into slot `i`, and a named call applies the plan's
  permutation. Defaults that are not literals become promises only when the slot is read while
  missing.
- **Reification.** On the first trigger below, the frame gets an environment object. One binding
  cell is created per bound slot, the slot then holds that cell instead of a value, and a mode bit
  on the activation switches `GetSlot`/`SetSlot` to read and write through the cell. From then on
  the environment is the source of truth, as today, and nothing is copied back.
- **Frame identity.** `Frame.env` becomes a lazily filled field. Everything that today compares
  environments to find a frame (`return`, `break`, `parent.frame`, `sys.parent`, `on.exit`,
  condition targets) compares a frame index and generation instead, and reifies only when it hands an
  environment to R code. `Frame.sys_parent` becomes a reference to the calling frame for the same
  reason. Otherwise every call would reify its caller.
- **Release.** A frame that was never reified drops its slots' references on return, which
  replaces `release_frame` and `clear_frame` for those calls. A reified frame is released as
  today.

## Reification triggers

These are the reads that need the environment as an object. Each one reifies the frame first.

| Trigger | Where |
|---|---|
| A promise made in the frame (argument promises, default promises, `delayedAssign`) | `make_promise`, `compiled_promise`, `formal_binding` in `eval.rs`; `NextMethod`'s promises in `dispatch.rs` |
| A closure created in the frame | `make_closure` in `eval.rs`; `Op::Closure`; `~` (the formula's environment) |
| Reflection that returns the environment | `environment()`, `sys.frame(s)`, `parent.frame`, `as.environment(-1)`, `sys.function` with its environment, `eval`'s numeric `envir` |
| Code that the compiler did not translate and that runs in the frame | `Op::Ast` (`<<-` targets, `...` forms), `BoundEval`, the debugger and `browser()` |
| S3 and S4 dispatch reading the generic's frame | `UseMethod` and `NextMethod` (`dispatch.rs`), `exec_method` and `callNextMethod` (`methods.rs`) |
| Condition handlers and restarts that record a frame | `.addCondHands`, `.addRestart` (`conditions.rs`) |
| The C API | `R_GetCurrentEnv`, `Rf_eval` and `R_tryEval` with a frame, `.External` and `.External2` (which pass the environment), `R_findVarLoc*`, `FRAME`, `ENCLOS` and every function that takes an `SEXP rho` from R |

A builtin that receives `call.env` and only reads a frame property (`missing`, `nargs`, `on.exit`,
`sys.call`, `match.call`) gets the frame reference instead and does not reify.

The C API is the riskiest row. Native code can reach a frame only through an `SEXP` that R code or
another API call handed it, and every route that hands one out is in the table. So a reified
environment is the only kind native code ever sees, and binding cells keep their stable addresses
for `R_findVarLoc`.

## Promises over a slot frame

The second step addresses the 33% of calls that only promises capture. A promise for a variable
argument holds a reference to the slot frame (index and generation) instead of an environment. Forcing
it during the call reads the slot. If the frame returns while such a promise is still unforced and
referenced, the return reifies the frame and points the promise at the new environment. `PRENV`,
`substitute` and native code see only reified environments, because reading a promise's
environment reifies it. This step is planned after the first step's numbers are in.

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
