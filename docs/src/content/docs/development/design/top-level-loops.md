---
title: Compiled top-level loops
description: "Compiling the for, while and repeat loops the tree walker evaluates in the global environment, and what broadening that to other environments would take"
---

`s <- 0; for (i in 1:1e7) s <- s + i` written at top level runs in ry's tree walker, because only
closure bodies compile. GNU R compiles every loop it evaluates in the global environment before
running it, so the same line took 2.71 s on ry against 0.47 s on GNU R (release build, 1e7, one
busy machine; relative only). The same loop inside a closure compiles and takes 0.09 s.

## The rule

A `for`, `while` or `repeat` that the tree walker evaluates compiles when all of these hold:

- the loop's environment is the global environment, which is GNU R's own condition
  (`R_GlobalEnv == rho` in `eval.c`'s `do_for`, `do_while` and `do_repeat`);
- no debugger session is active;
- the loop does not call `return` outside a function it defines;
- a `for` loop's sequence is a vector (not a record or `I64`).

Otherwise the tree walker runs it. The loop compiles before its first iteration, as GNU R's does,
because the compiled code binds the base functions it inlines when it compiles (see "When code
compiles and what it binds" in `r-runtime.md`): a loop compiled later would bind at another
moment than GNU R's.

## How the compiled loop runs

- The entry point is `bytecode::run_loop(call, sequence, env)`, called from `do_for`, `do_while` and
  `do_repeat`.
- `for` compiles after evaluating its sequence, with the evaluated value in place of the sequence
  expression, so the sequence is evaluated once. `ForInit` coerces a factor and steps a compact
  sequence without expanding it, as in a closure.
- A `for` loop that stays in the tree walker steps through a compact sequence the same way, in
  `do_for` after `run_loop` declines it, so no loop expands `1:n`.
- The loop compiles with no local symbols, so every variable read and write goes to the
  environment's bindings. Code called from the loop, `get()`, `exists()` and active bindings see
  every assignment at once, and the loop variable keeps its last value afterwards.
- The code runs through `run` like a closure body, with a frame-less closure as `function` (no
  formals, the loop as its body, the environment as its enclosure), and no call frame is pushed.
  `parent.frame()`, `sys.call()` and the calls that conditions report are therefore those of the
  tree walker.
- A closure's frame restores `interp().srcref` on leaving. This activation has none, so
  `run_loop` saves and restores it.
- Ops that index `interp().frames[slot_owner]` (`on.exit`, `missing` and the argument shortcuts of
  slot frames) all run only in slot frames, so environment-backed code never reaches them, and
  `on.exit` or `missing()` in a top-level loop behave as in the tree walker.
- The `for`, `while` or `repeat` special's `LoopContext` stays pushed while the compiled loop runs,
  so a `break` from `tryCatch(break)` or `eval(quote(break))` is found as before, and the compiled
  loop catches it at its own loop target.
- The code lives for one evaluation of the loop and is dropped after it, so a loop evaluated again
  compiles again, as in GNU R. There is no table from calls to code and no marker on the call: a
  language object's gp bits are serialized, so a marker would change `serialize()` and `digest()`
  of a quoted loop.
- The optimizing tier never sees this code. Its `TIER_STATE` is `profile::TIER_OFF`, which entry
  scheduling skips and `back_edge` turns into a reset of the back-edge counter, so no tier-1 code
  is compiled and no feedback is recorded. The tier's frames and resume points assume a
  call frame, and in the global environment every closure call, active binding and `<<-` from a
  callee writes the loop's variables, so a compiled global could not stay in a register across any
  call.

## Open questions

- **Other environments.** GNU R compiles no loop in an `eval()`, `local()` or `test_that`
  environment, and two differences show why broadening is a separate, measured step. In
  `f <- function() { eval(quote(for (i in 1:200) if (i == 200) on.exit(cat("exit f\n")))); cat("body f\n") }`
  the `on.exit` attaches to `eval`'s frame, so GNU R prints `exit f` before `body f`; and
  `g <- function(x) { local({ for (i in 1:200) r <- missing(x) }); r }` raises
  "'missing(x)' did not find an argument". Each needs an oracle case before the rule widens. The
  testthat suite's gain would come from that step.
- **The optimizing tier.** Deoptimization and OSR need no call frame, only the bytecode `Machine`,
  whose header register must hold the global environment. With that header, `Machine::enclosure()`
  returns the global environment's parent (the first search-path environment), not the global
  environment: it is the lookup start for names the frame does not bind, and the tier's reuse and
  epoch checks key on it. The planner takes `analysis::Frame::Global { env }` for this frame: no
  locals and no slots, every `GetVar`, `SetVar` and element assignment a tier-0 step on the
  environment's bindings (any callee can write them through `<<-`, `assign()` or an active
  binding), a refusal at `ForStep`, which keeps the loop variable in a slot, and function lookups
  speculated from the global environment itself, since its bindings may shadow the search path.
  Because the planner's lookup start then differs from `Machine::enclosure()`, wiring the entry
  also needs the reuse check and the activation's enclosure to use the frame's lookup start, a
  thorough validity check at loop entry, the body rooted for the loop's lifetime, pending unwinds
  pushed on `Interp::held_unwinds` when the entry bypasses `jit::run`, and `frame_safe` only under
  the no-locals rule. Until then nothing constructs `Frame::Global`.
- **What the global frame's planner leaves on the table.** Three gaps are known, each
  a follow-up once the entry is wired:
  - `ForStep` refuses, so only `while` and `repeat` loops would reach the tier, while top-level loops
    are mostly `for (i in 1:n)`. Keeping the counter and sequence in registers and writing the
    variable each iteration through a binding write on the environment costs what tier 0's write
    costs.
  - A direct call whose argument is a variable refuses, so `s <- f(s, x)` runs as a tier-0 call.
    An argument kind that loads the binding cell, falling back like a failed load, would cover it.
  - Every `GetVar` is a tier-0 step, so arithmetic on globals stays boxed. The larger gain needs
    globals pinned to their binding cells (speculated, and invalidated on rebinding) or promoted
    to registers for the loop's duration.
- **`return` in a top-level loop.** GNU R's compiled loop treats `return(1)` as leaving the loop,
  so `for (i in 1:100) if (i == 3) return(1); cat(i)` prints `3`, where ry's tree walker raises "no
  function to return from". Such loops stay in the tree walker, which keeps today's behavior.
