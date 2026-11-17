---
title: Tier 1 on generic code
description: "How the optimizing tier pays off on closure bodies that call builtins and closures on lists, objects and strings: calls without a frame, native forms on boxed values, and a cost model that counts the work saved"
---

Generic code is a closure body that does no numeric work of its own: it checks its arguments,
reads fields of a list or slots of an object, compares strings and calls small helpers. An
application's test suite is almost all generic code. The profile summary of a test user's suite of
about 400 files attributed 36% of its time to tier-0 bytecode and none to compiled code, and
turning the JIT off changed the time of one of its files by 4%.

The design has tier 1 pay off on generic code through three things, in this order of value:

1. **A call of a small closure runs in the caller's compiled code**, on a window of registers and
   with no frame in the interpreter. This removes the largest cost tier 0 has there. It is built.
2. **A builtin that only inspects a value runs as a few machine instructions** on the boxed value
   (`is.null(x)`, `length(x)`, `x$name` on a plain list, `a == "b"` on strings). It is planned.
3. **What compiled code cannot do itself costs about what it costs in tier 0**, so that compiling
   a body with one inlined call and ten other instructions is a gain and not a loss. The call
   that cannot be inlined is built this way, and runs of other instructions are planned.

No third compiler is added. "Stages" at the end says which parts exist. The sections before it
give the evidence, then the design of each part.

Three terms recur. A **step** is one instruction that compiled code hands to tier 0. An **inlined
frame** (`Action::Frame`) is a callee's plan run on a window of registers above the caller's, with
exits that build the interpreter's frames when something needs them. An **inlined body**
(`Action::Inline`) is a callee whose instructions have no effect, compiled into the caller as if
written there.

## Where generic code spends its time

The table splits the instructions of the app-suite workload (`tools/bench/appsuite`) with the JIT
off by what the function that ran them does. The counts are callgrind's, each function's own
instructions, on an unoptimized-link build, so they order the costs and do not time them. The
profile summary's tier-0 share is larger than the handlers' row here because it is a share of
time, and it counts as tier 0 everything a running body does that no other category of the summary
claims, allocation included.

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
tier, and code images hold bytecode only"). The baseline tiers of JavaScriptCore and V8 remove
their interpreters' dispatch and operand decoding and reuse the interpreters' inline caches. Tier 0
has quickened handlers and inline caches for variable reads, function lookups, fields and calls,
so the dispatch share is all a baseline tier could remove here.

**The call is the cost to remove.** A closure call and its return take about 830 instructions in
tier 0 for a callee without arguments. An established calling handler adds about 210 to that call
and about 970 to a call with three arguments, because the arguments then need promises. A test
suite runs every expectation under a calling handler. The
helpers of generic code are short (`function(x, name) x[[name]]`, a guard, a default), so the call
often costs more than the callee's body.

## What compiled code costs against tier 0

Each number is a difference in callgrind instructions between the compiled path and tier 0's path
for the same work. The cost model's constants (`crates/jit/src/jit.rs`) follow these numbers:
`ENTRY_COST`, `STEP_COST` and `SAVING_PER_FRAME` are measured, and `SAVING_PER_NATIVE` and
`SAVING_PER_CALL` are the model's estimates for work that varies with the instruction.

| Mechanism | Against tier 0 | Constant |
|---|---|---|
| Entering compiled code instead of starting the body in the dispatch loop | 380 more: the tier's entry (276), the scheduled entry's own return path, the jump to the code | `ENTRY_COST` |
| An instruction handed to tier 0 (a step) | 60 to 70 more for the helper call alone; about 180 with the variables written back before it and reloaded after it | `STEP_COST` |
| A call inlined with a frame, no arguments | 450 fewer, read from the `f()` row of "Measured" with the entry added back. The functions involved account for 585: tier 0's call (368) and return (459), less the first start (152), the start (12), the register window (40) and the compiled code (38). The constant is the smaller, end-to-end number | `SAVING_PER_FRAME` |
| A call inlined with a frame, three arguments | 510 to 630 fewer | |
| The first start of an inlined call in an entry | 152, once per entry of the compiled body for each call | |
| A native arithmetic instruction with its loads and stores | about 130 fewer | `SAVING_PER_NATIVE` |
| An inlined body, a native call or a traversal | The call and its boxing, estimated at 1,000 fewer | `SAVING_PER_CALL` |

Two facts in the table shape the design. A step can never be free, so a body that is one step
after another loses in compiled code whatever the entry costs, and such a body is not compiled.
And one inlined call covers the entry with 70 instructions to spare, so a thin wrapper around one
small callee is worth compiling only narrowly, and a body with two inlined calls clearly.

## The design

### A call compiles one of four ways

| The callee | Compiles to | Cost against tier 0's call |
|---|---|---|
| Has no effect and reads only its arguments | An inlined body (`Action::Inline`) | Saves the call |
| Has a plan that runs on registers until something needs its frame | An inlined frame (`Action::Frame`) | Saves the call |
| Is a typed or numeric closure with a native function | A native call (`Action::NativeCall`) | Saves the call and the boxing |
| Anything else | Tier 0's own call instruction, entered in place | A step |

Compiled code has no call path of its own for the last row. A call path of its own was measured
and rejected: it evaluated the arguments in compiled code and pushed the callee's frame through
the interpreter's general entry, and a body that made one such call ran about 940 instructions
more than in tier 0, the entry included, while the cost model credited the call as a saving
("Before" in the tables below). Tier 0's call instruction skips the promises at a site whose
arguments are effect free, so it is the cheaper call.

### Inlined frames for generic callees

An inlined frame runs the callee's plan on a window of registers above the caller's, with no
environment, no promises and no frame record. An instruction of the callee that could run R code or
look at the call is an exit that builds the frames first (`decisions/jit-ssa.md`, "Tier 1 leaves
compiled code through frame states"). Generic callees qualify under these rules:

- **A callee without type feedback is planned from its instructions alone.** Tier 0 runs a
  one-expression closure without an activation and records nothing about it. Every call of such a
  body runs the instructions before its first branch, so those count as run, and whatever follows
  the branch is planned as never run: an instruction there that needs the frame is an exit. Its
  values are boxed. A callee whose exits turn out frequent is no longer inlined (see "Code that
  keeps leaving is retired"), so a wrong guess costs a bounded number of exits and one recompile.
- **An argument may be any constant.** A string, `NULL` or a constant vector is passed as the
  object the call's syntax tree holds, which is what tier 0 binds. The callee treats it like any
  value a caller passes: its first element store copies.
- **A calling handler does not stop an inlined call.** While a call has no frame, an instruction
  run for it signals nothing. A plain error comes back unsignalled. A condition that is signalled
  where it is raised (a warning, or an error with a class of its own such as a subscript out of
  bounds) abandons the instruction instead, because its handler is R code that would see a stack
  without the call. Either way the exit repeats the instruction in tier 0 under the rebuilt
  frames, where it signals. That mechanism does not depend on whether a handler is established, so
  neither the call's start nor the quiet steps inside it test for one. A handler's R code runs
  only after an exit, where it finds every frame and every promise as tier 0 would have left them.
- **A profile summary does not stop an inlined call.** A profile that changes which path runs
  measures another program. The summary attributes an inlined callee's time to compiled code and
  to its caller's row. An exit under the summary pushes the frames it rebuilds, where a call
  that starts under the summary runs whole on the general path (`eval::Entering`).

These callees keep their frame, and their call is a step:

| The callee or the call | Why |
|---|---|
| The callee loops | A loop needs a counter and an entry of its own |
| The callee takes `...`, or the call passes or collects it | The arguments are not known at the site |
| A formal the call leaves out has a default that is code | The default runs in the callee's frame |
| An argument the callee reads may have an effect | The effect must happen where a promise would be forced |
| The callee is longer than 512 instruction words, the nesting is deeper than 3, or the call is recursive | The compile is bounded |
| The callee is typed, or has more than 64 formals | A typed callee has its native function |
| The callee has a native entry (a base closure that tier 0 runs in native code) | Tier 0's call runs neither its body nor a frame |
| The callee's environment may not outlive the compiled code | The code holds the callee by its address |
| The callee runs an instruction only tier 0 has, or a call that needs its own frame, on every path | Every call would exit |

On the app-suite workload these are most callees, which is the limit stage 4 lifts.

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
unwinds through a step.

So every such exit counts, and "frequent" has one meaning throughout: at least 16 exits, and more
than one for every eight entries (`runtime::frequent`).

| What keeps leaving | Counted by | When frequent |
|---|---|---|
| An inlined frame that needs its real frame | `count_handoff`, per callee | The callee is marked to keep its frame, and the caller recompiles with the call as a step |
| The compiled body itself, through a pending unwind or a hand-off | `count_leave`, per body | The code is discarded and the body stays in tier 0 for good |

The body's count covers a window: when 16 exits were not frequent among the entries since the
count began, the count starts again, so exits spread thinly over a long run never add up to a
retirement. A retirement is final, because the model has no evidence that a second compile of the
same body would leave less often. A body with a loop is exempt, because a loop entered by on-stack
replacement runs compiled without an entry being counted, so its entries do not say how much the
code ran.

### The cost model counts the work saved

A loop-free body compiles when the work its plan saves per call, times the calls it expects, covers
the compile (`jit.rs`, `unpaid`). The saving is counted in tier-0 instructions the plan removes:

| The plan has | Saving per activation |
|---|---|
| An inlined frame | `SAVING_PER_FRAME`, plus what the callee's own plan saves |
| An inlined body, a native call, a traversal | `SAVING_PER_CALL` |
| A native instruction | `SAVING_PER_NATIVE` |
| A step | Minus `STEP_COST` |
| The entry | Minus `ENTRY_COST` |

Only the instructions on paths the body has taken count (`Plan::ran`). Tier 0's `JumpUnless`
records each direction it takes, and the model follows the recorded directions from the entry. A
body that returns early on every call, such as `match.fun` given a function, has native work and
calls on a path it never runs, and a model that counts them compiles it for savings it never
makes.

Three kinds of step count as zero in the model, because compiled code runs them at about tier 0's
cost:

- a call that never ran while tier 0 recorded, such as the `stop()` under an argument check, and
  the function lookup that feeds it;
- an element read or store that compiled code runs through tier 0's fast handler;
- a builtin call that runs no R code and so needs no variables written back.

A body is refused for good when its saving per call would not cover the compile even if it were
called `HORIZON_CALLS` times, which includes every body whose saving is zero or less. A body that
passes that test and is called too rarely waits and is asked again, and each refusal doubles the
wait. A body with an inlined frame is planned before the model is asked, because only the plan
knows what its callees save.

### Native forms on boxed values (stage 2, planned)

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

### Stretches (stage 3, planned)

Consecutive steps run as one stretch: compiled code writes back the variables once, tier 0's
dispatch loop runs from the first instruction to the last, and compiled code reloads once. A body
whose plan is one stretch is not compiled.

`RY_JIT=force` compiles every body at its second entry, so its total time includes every compile.
The rule it must meet is per compiled body: in the steady state a compiled activation runs at most
the instructions tier 0 runs for it. The "Forced" columns below measure that directly. The rows
above tier 0 there are bodies of steps, and stretches are designed to bring them to tier 0's cost.

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
| JavaScriptCore DFG, V8 Maglev | Inline small callees by feedback, replace property reads by a shape check and a load, keep values in registers | Inlined frames and registers are built. Kind checks and the position cached for a list's names are stage 2 |
| Ř | Elides environments and promises, and specializes a callee per call context | An inlined frame is both elisions at once. A callee's plan is built once per compilation of a caller, with the arguments of each site checked against it |
| JavaScriptCore Baseline, V8 Sparkplug | A template per bytecode, reusing the interpreter's inline caches | Rejected after measurement: dispatch and decoding are a small share, and tier 0 already quickens and caches |
| YJIT | A block version per type context | Not adopted: generic R values need a kind test per use, which the guard on entry to a native form already is |
| LuaJIT | Traces through calls, each exit with a snapshot of the interpreter's state | Not adopted: a trace through an R call needs the same snapshots as an inlined frame's exits, and method inlining has them without a trace recorder |

## Stages

Each stage is one change, measured with the app-suite workload's total time and compiled share,
the per-pattern instruction counts above, and the kernels, public and code-bases suites.

1. **Built.** Inlined frames for generic callees (no feedback, constant arguments, calling
   handlers, the profile summary), tier 0's own call in place of compiled code's, steps on a kept
   view, the leaner entry, retiring code that keeps leaving, and the cost model in saved work.
2. Planned. Boxed kinds in the planner and the native forms in the table.
3. Planned. Stretches of tier-0 instructions, and forced compilation checked per body.
4. Planned. Calls with a frame made from an inlined frame, `...` passed through, and argument
   expressions that are effect-free builtin calls.
