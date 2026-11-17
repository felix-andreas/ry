---
title: Tier 1 on generic code
description: "How the optimizing tier pays off on closure bodies that call builtins and closures on lists, objects and strings: calls without a frame, native forms on boxed values, and a cost model that counts the work saved"
---

Generic code is a closure body that does no numeric work of its own: it checks its arguments,
reads fields of a list or slots of an object, compares strings and calls small helpers. An
application's test suite is almost all generic code. A test user's suite of about 400 files spent
36% of its time in tier-0 bytecode and none in compiled code, and turning the JIT off cost it
nothing.

Tier 1 pays off on generic code through three things, in this order of value:

1. **A call of a small closure runs without a frame**, in the caller's compiled code. This removes
   the largest cost tier 0 has there.
2. **A builtin that only inspects a value runs as a few machine instructions** on the boxed value
   (`is.null(x)`, `length(x)`, `x$name` on a plain list, `a == "b"` on strings).
3. **What compiled code cannot do itself costs what it costs in tier 0**, so that compiling a body
   with one call without a frame and ten other instructions is a gain and not a loss.

No third compiler is added. "Steps" at the end says which parts are built. The sections before it
give the evidence, then the design of each part.

## Where generic code spends its time

The table splits the instructions of the app-suite workload (`tools/bench/appsuite`) with the JIT
off by what the function that ran them does. The counts are callgrind's, each function's own
instructions, on an unoptimized-link build, so they order the costs and do not time them.

| What runs | Share of instructions |
|---|---|
| Closure calls: binding arguments, promises, the frame, the return | 16.7% |
| The heap: allocation, binding cells, string cache, releasing frames | 13.5% |
| Tier 0's instruction handlers, including their dispatch | 7.8% |
| Calling a builtin: argument vector, checks, the dispatch on its arguments | 5.9% |
| Everything else: the builtins themselves, native package code, start-up | 56.1% |

Two conclusions follow.

**A baseline compiler has nothing to win.** A compiler that emits one call per instruction removes
dispatch and operand decoding, which are a part of the 7.8%. Measured per instruction, tier 0
spends about 10 instructions on dispatch and decoding for a plain instruction, and entering
compiled code costs 380 more than entering the body in tier 0 (next section), so a generic body
would need about 40 instructions per call before a baseline compile broke even, and a compiled
body's builtin and closure calls are handed back to tier 0 at a cost above that saving. ry built
that tier once and it ran more instructions than tier 0 (`decisions/jit-ssa.md`, "No baseline
tier, and code images hold bytecode only"). Tier 0 already has what a baseline tier gives
JavaScriptCore and V8: quickened handlers and inline caches for variable reads, function lookups,
fields and calls.

**The call is the cost to remove.** A closure call in tier 0 takes about 900 instructions for a
callee that does nothing, and about 1,900 while a calling handler is established, because the
arguments then need promises. A test suite runs every expectation under a calling handler. The
helpers of generic code are short (`function(x, name) x[[name]]`, a guard, a default), so the call
often costs more than the callee's body.

## What compiled code costs against tier 0

Each number is a difference in callgrind instructions between the compiled path and tier 0's path
for the same work, read from the functions involved. The cost model's constants
(`crates/jit/src/jit.rs`) are these numbers.

| Mechanism | Against tier 0 | Constant |
|---|---|---|
| Entering compiled code instead of starting the body in the dispatch loop | 380 more: the tier's entry (276), the scheduled entry's own return path, the jump to the code | `ENTRY_COST` |
| An instruction handed to tier 0 (a step) | 60 to 70 more for the helper call alone; about 180 with the variables written back before it and reloaded after it | `STEP_COST` |
| A call inlined with a frame, no arguments | 450 fewer: tier 0's call (368) and return (459), less the first start (152), the start (12), the register window (40) and the compiled code (38) | `SAVING_PER_FRAME` |
| A call inlined with a frame, three arguments | 510 to 630 fewer | |
| The first start of an inlined call in an entry | 152, once per entry of the compiled body for each call | |
| A native arithmetic instruction with its loads and stores | about 130 fewer | `SAVING_PER_NATIVE` |

Two facts in the table shape the design. A step can never be free, so a body that is one step
after another loses in compiled code whatever the entry costs, and such a body is not compiled.
And one inlined call covers the entry with 70 instructions to spare, so a thin wrapper around one
small callee is worth compiling only narrowly, and a body with two inlined calls clearly.

## The design

### A call compiles one of three ways

| The callee | Compiles to | Cost against tier 0's call |
|---|---|---|
| Has a plan that runs without a frame | An inlined frame (`Action::Frame`) | Saves the call |
| Is a typed or numeric closure with a native function | A native call (`Action::NativeCall`) | Saves the call and the boxing |
| Anything else | Tier 0's own call instruction, entered in place | A step |

Compiled code has no call path of its own for the third row. An earlier design evaluated the
arguments in compiled code and pushed the callee's frame through the interpreter's general entry,
which cost about 940 instructions more than tier 0's call while the cost model credited it as a
saving. Tier 0's call instruction already skips the promises at a site whose arguments are effect
free.

### Inlined frames for generic callees

An inlined frame runs the callee's plan on a window of registers above the caller's, with no
environment, no promises and no frame record. An instruction of the callee that could run R code or
look at the call is an exit that builds the frames first (`decisions/jit-ssa.md`, "Tier 1 leaves
compiled code through frame states"). Generic callees qualify under these rules:

- **A callee without type feedback is planned from its instructions alone.** Tier 0 runs a
  one-expression closure without an activation and records nothing about it. Its values are boxed
  and its unobserved instructions are exits, as in a callee with feedback. The count of exits per
  inlined call gives up on a callee whose exits are frequent, so a wrong guess costs a bounded
  number of exits and one recompile.
- **An argument may be any constant.** A string, `NULL` or a constant vector is passed as the
  object the call's syntax tree holds, which is what tier 0 binds. The callee treats it like any
  value a caller passes: its first element store copies.
- **A calling handler does not stop an inlined call.** While a call has no frame, an instruction
  run for it signals nothing: an error comes back unsignalled and a warning that would run R code
  abandons the instruction, and the exit then repeats the instruction in tier 0 under the rebuilt
  frames. That mechanism does not depend on whether a handler is established, so neither the
  call's start nor the quiet steps inside it test for one. A handler's R code runs only after an
  exit, where it finds every frame and every promise as tier 0 would have left them.
- **A profile summary does not stop an inlined call.** A profile that changes which path runs
  measures another program. The summary attributes an inlined callee's time to compiled code and
  to its caller's row.

Callees with a loop, `...`, a default that is code and is read, an instruction only tier 0 has, or
a call that needs its own frame keep their frame. On the app-suite workload these are most
callees, which is the limit step 4 lifts.

### Steps run on a view of the activation that is kept

A step is an instruction compiled code hands to tier 0: a builtin with no native form, a call it
cannot inline, an assignment into a list. Tier 0 runs an instruction against its view of the
activation (the code, the constants, the registers, the frame header, the inline caches), and
building that view costs about as much as a simple instruction.

- **The view is built at an activation's first step and kept** (`bytecode::Stepper`, one per
  compiled activation and one per inlined call's activation). The addresses it holds do not move
  while the activation runs compiled. A stepped closure call that enters its callee in place points
  the view at the callee's code, so the step after it builds the view again.
- **A step's end is one byte**, not a result that carries a value or an unwind
  (`bytecode::StepEnd`). A failed step leaves its unwind in the view, and the helper takes it from
  there on the cold path.
- **A closure call is recognized by its opcode byte**, because tier 0 quickens no closure call's
  word, and every other instruction goes straight to its handler.

### The entry does only what an entry needs

- **A scheduled entry of a body with compiled code goes to the code**, without the tier schedule's
  cases for bodies that are still counting or recording.
- **The first start of an inlined call in an entry cannot panic**, so it runs without the panic
  guard and writes the call's activation record in place. A body that calls its callee once per
  entry starts every call this way, so the first start is the common start there.

### Code that keeps leaving is retired

Compiled code leaves for tier 0 at an exit. A failed speculation discards the code, and the body
recompiles without the speculation. An exit for another reason says nothing against the code: an
instruction failed, or an inline fast path of the body's own frame did not apply. Such an exit
costs more than several entries save, and some bodies take one on most calls: `tryCatchOne` leaves
whenever its expression signals, and `is()` returns through `return()` inside `switch`, which
unwinds through a step. On the app-suite workload these two bodies took 95,000 exits.

So every such exit counts against the code (`runtime::count_leave`). Code whose exits are frequent
among the body's entries since it was compiled is discarded, and the body is marked to stay in
tier 0. A body with a loop is exempt, because a loop entered by on-stack replacement runs compiled
without an entry being counted, so its entries do not say how much the code ran.

### The cost model counts the work saved

A loop-free body compiles when the work its plan saves per call, times the calls it expects, covers
the compile (`jit.rs`, `unpaid`). The saving is counted in tier-0 instructions the plan removes:

| The plan has | Saving per activation |
|---|---|
| An inlined frame | `SAVING_PER_FRAME`, plus what the callee's own plan saves |
| A call inlined without a frame, a native call, a traversal | `SAVING_PER_CALL` |
| A native instruction | `SAVING_PER_NATIVE` |
| A step | Minus `STEP_COST` |
| The entry | Minus `ENTRY_COST` |

Three kinds of step cost nothing in the model, because they cost nothing when the body runs:

- a call that never ran while tier 0 recorded, such as the `stop()` under an argument check, and
  the function lookup that feeds it;
- an element read or store that compiled code runs through tier 0's fast handler;
- a builtin call that runs no R code and so needs no variables written back.

A body whose saving per call is zero or less is refused for good. A body with a positive saving
that runs too rarely waits and is asked again. A body with an inlined frame is planned before the
model is asked, because only the plan knows what its callees save.

### Native forms on boxed values (step 2)

The planner's types describe numbers and numeric vectors, and every other value is `Boxed`. Generic
code needs three more facts about a boxed value, each of which tier 0's feedback already records
as a value kind: it is a string scalar, it is `NULL`, or it is a plain list (no class attribute,
not ALTREP). With them these calls compile to a kind test on the object's header and a few loads,
under the guard on the builtin's binding that every speculated function has:

| Call | Native form |
|---|---|
| `is.null(x)`, `is.list(x)`, `is.character(x)`, `is.function(x)` | A compare of the kind byte |
| `length(x)` on a plain vector or list | A load of the length |
| `x$name`, `x[["name"]]` on a plain list | The position cached for the list's names, then a load |
| `a == b`, `a != b` on string scalars | A pointer compare of the interned strings, `NA` aside |
| `missing(x)` in an inlined call | The call's own passing record, a constant |
| `nchar(x)`, `identical(a, b)` on scalars, `inherits(x, "class")` | A helper that runs no R code |

A value of an unexpected kind takes the instruction's tier-0 step, not an exit, so a call site
that sees two kinds keeps its compiled code.

### Stretches (step 3)

Consecutive steps run as one stretch: compiled code writes back the variables once, tier 0's
dispatch loop runs from the first instruction to the last, and compiled code reloads once. A body
whose plan is one stretch is not compiled.

`RY_JIT=force` compiles every body at its second entry, so its total time includes every compile.
The rule it must meet is per compiled body: in the steady state a compiled activation runs at most
the instructions tier 0 runs for it. The "Forced" columns below measure that directly. The rows
above tier 0 there are bodies of steps, which stretches bring to tier 0's cost.

## Measured

Each row is one pattern of `tools/bench/appsuite/patterns/r15_plain_function_bodies.R`: a closure
called from a loop, in callgrind instructions per call, the loop included. "Before" is tier 1
without the changes of step 1. A difference of 48 from "JIT off" is the loop's own, and means the
body runs in tier 0. The handler columns run the same loop inside `withCallingHandlers`.

| Body | JIT off | Before | Step 1 | Forced, before | Forced, step 1 |
|---|---|---|---|---|---|
| `f()` | 1,674 | 2,611 | 1,603 | 3,321 | 1,952 |
| `f(a)` | 1,813 | 3,085 | 1,782 | 2,519 | 2,276 |
| `f(a, b, c)` | 1,934 | 3,545 | 1,805 | 2,541 | 2,299 |
| passes 3 arguments on to `f` | 2,909 | 5,926 | 2,027 | 2,835 | 2,521 |
| a chain of 5 calls | 5,128 | 10,756 | 3,860 | 6,918 | 5,749 |
| two argument guards with `stop()` | 3,994 | 3,502 | 3,417 | 3,974 | 3,775 |
| a function that returns early | 1,952 | 2,073 | 1,946 | 2,549 | 2,306 |
| `x[[name]]` | 2,240 | 2,288 | 2,257 | 4,158 | 2,616 |
| `switch()` on a string | 2,246 | 2,294 | 2,294 | 4,203 | 2,668 |
| `if`/`else if` chain on a string | 5,847 | 5,895 | 5,895 | 8,431 | 7,735 |
| reads of `x$a`, `x$a$b` | 5,080 | 5,128 | 5,128 | 7,413 | 7,111 |
| `missing(b)` | 3,096 | 3,144 | 3,144 | 5,399 | 5,085 |
| passes `...` on | 3,815 | 5,537 | 3,863 | 7,374 | 6,292 |
| a helper of 20 lines | 23,627 | 23,786 | 23,772 | 25,443 | 25,754 |
| 10 scalar assignments | 4,355 | 2,157 | 2,030 | 2,633 | 2,390 |
| a loop that sums 10 elements | 10,449 | 6,066 | 4,970 | 5,591 | 5,934 |

| Body, under a calling handler | JIT off | Before | Step 1 |
|---|---|---|---|
| `f()` | 1,884 | 2,580 | 1,806 |
| `f(a, b, c)` | 2,904 | 3,782 | 2,227 |
| passes 3 arguments on to `f` | 4,640 | 6,305 | 2,449 |
| a chain of 5 calls | 7,846 | 11,893 | 5,042 |
| `x[[name]]` | 3,109 | 3,157 | 2,452 |
| a function that returns early | 2,978 | 3,861 | 2,368 |
| 10 scalar assignments | 5,385 | 4,744 | 2,452 |
| a loop that sums 10 elements | 11,291 | 7,216 | 6,282 |

With step 1 no row runs more instructions with the JIT on than off beyond the loop's 48, where
before the call rows ran up to twice tier 0's. The rows that gain are the ones with an inlined
frame, and they gain most under a handler, where tier 0's call makes promises. The patterns of
`r02_argument_checks.R` are unchanged: every body there is steps, and none compiles.

On the app-suite workload step 1 removes the loss and adds little gain. Four bodies compile, and
the exits fall from 95,000 to 16. The compiled share of the time stays near zero, because most of
its hot callees keep their frame: they pass `...`, run an instruction only tier 0 has, or call a
closure that needs its own frame. Steps 2 and 4 are what move that share.

## How it compares

| System | What it does for generic code | ry |
|---|---|---|
| JavaScriptCore DFG, V8 Maglev | Inline small callees by feedback, replace property reads by a shape check and a load, keep values in registers | The same three, on R's values: calls without a frame, kind checks, the list-names position cache |
| Ř | Elides environments and promises, and specializes a callee per call context | A call without a frame is both elisions at once. A context is the call site itself, since the callee's plan is built per site |
| JavaScriptCore Baseline, V8 Sparkplug | A template per bytecode, with inline caches | Rejected after measurement: tier 0 already quickens and caches, and dispatch is a small share |
| YJIT | A block version per type context | Not adopted: generic R values need a kind test per use, which the guard on entry to a native form already is |
| LuaJIT | Traces through calls | Not adopted: R's calls are lazy and observable, so a call needs the frame-state exits that method inlining already has |

## Steps

Each step is one change, measured with the app-suite workload's total time and compiled share, the
per-pattern instruction counts above, and the kernels, public and code-bases suites.

1. **Built.** Frames for generic callees (no feedback, constant arguments, calling handlers, the
   profile summary), tier 0's own call in place of compiled code's, steps on a kept view, the
   leaner entry, retiring code that keeps leaving, and the cost model in saved work.
2. Boxed kinds in the planner and the native forms in the table.
3. Stretches of tier-0 instructions, and forced compilation checked per body.
4. Calls with a frame made from a call without one, `...` passed through, and argument expressions
   that are effect-free builtin calls.
