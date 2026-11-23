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
compiled code costs 560 more than entering the body in tier 0 (next section), so a generic body
would need about 55 instructions per call before a baseline compile broke even, and a compiled
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
`ENTRY_COST`, `LOAD_COST`, `STEP_COST`, `WRITE_BACK_COST` and `SAVING_PER_FRAME` are measured, and
`SAVING_PER_NATIVE` and `SAVING_PER_CALL` are the model's estimates for work that varies with the
instruction.

| Mechanism | Against tier 0 | Constant |
|---|---|---|
| Entering compiled code instead of starting the body in the dispatch loop | About 560 more: the tier's entry (246), the scheduled entry's dispatch and return path (107), the register window (36 to 53), the caller's feedback (55) and the code's prologue. End to end, two bodies that differ by four native instructions (`a + a + a + a + a` and the same with nine operands, each called through a closure made on every call) differ by 550, and the smaller one runs 132 more compiled than in tier 0 | `ENTRY_COST` |
| A variable read from the frame, against one kept in a machine register | About 30 more when the inline probe answers, about 125 more when the runtime forces an argument's promise. A body that adds five arguments runs 130 more than one that adds one argument five times | `LOAD_COST` (60) |
| An instruction handed to tier 0 (a step) | 60 to 70 more for the helper call alone; about 180 with the registers reloaded after it | `STEP_COST` |
| A variable written back to the frame before a step that can run R code | About 170 to 210 more than tier 0's assignment, which stores into the frame as it goes. In a body of 20 lines whose plan has seven such write-backs, the write-back path (the flush, the boxed scalar, the assignment) runs 1,940 instructions a call against 435 for tier 0's assignments | `WRITE_BACK_COST` |
| A call inlined with a frame, no arguments | About 600 fewer. The functions involved account for 585: tier 0's call (368) and return (459), less the first start (152), the start (12), the register window (40) and the compiled code (38). The `f()` row of "Measured" with the entry added back gives about 640 | `SAVING_PER_FRAME` |
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
| The compiled body itself, through a pending unwind | `count_leave`, per body | The code is discarded and the body stays in tier 0 for good |
| One instruction of the body, through a hand-off or an abandoned signal | `count_exit`, per exit | The instruction is marked in the feedback (`feedback::HANDED_OFF`), and the body recompiles with it as a step |

The body's count covers a window: when 16 exits were not frequent among the entries since the
count began, the count starts again, so exits spread thinly over a long run never add up to a
retirement. A retirement is final, because the model has no evidence that a second compile of the
same body would leave less often. A body with a loop is exempt, because a loop entered by on-stack
replacement runs compiled without an entry being counted, so its entries do not say how much the
code ran.

An exit's count is measured against the work between its hand-offs: the body's entries plus the
passes of the innermost `for` loop around the instruction, so a read past the end on every pass
of a long loop is frequent and one at the end of each run of a loop is not. The remedy keeps the
body: the native form of `x[i]`, of integer arithmetic or of a builtin call has tier 0's step as
its general form, and the step handles in place what the native form left for. This is
JavaScriptCore's frequent exit site, with a step where the DFG would drop a speculation. An
instruction with no step to fall back on (a `for` over an empty sequence) refuses the body, and
so does a marked instruction that leaves again.

### The cost model counts the work saved

A loop-free body compiles when the work its plan saves per call, times the calls it expects, covers
the compile (`jit.rs`, `unpaid`). The saving is counted in tier-0 instructions the plan removes:

| The plan has | Saving per activation |
|---|---|
| An inlined frame | `SAVING_PER_FRAME`, plus what the callee's own plan saves |
| An inlined body, a native call, a traversal | `SAVING_PER_CALL` |
| A native instruction | `SAVING_PER_NATIVE` |
| A step | Minus `STEP_COST` |
| A variable that may differ from the frame before a step that can run R code | Minus `WRITE_BACK_COST` |
| The entry | Minus `ENTRY_COST` |
| A variable read from the frame outside the loops | Minus `LOAD_COST` |

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

A body with loops is asked the same question in backward jumps instead of calls. When one of its
loops asks, the model counts the work of one pass through that loop, leaving out the loops nested
in it, because the body counts the backward jumps of all its loops together (`Plan::work`,
`BodyWork.hot`). The body compiles when that saving, times the backward jumps it expects, covers
the compile (`jit.rs`, `unpaid_loop`). It expects `HORIZON_EDGES` backward jumps, or as many as
the body has taken once that is more:

| The loop's pass saves | The body compiles |
|---|---|
| Enough that `HORIZON_EDGES` passes cover the compile | At the back-edge threshold |
| Less | Once it has taken as many backward jumps as the compile costs in passes, and at least twice as many as at the refusal |
| Nothing | Never from this loop, and it asks again when the taken backward jumps have doubled |

The first row holds every numeric loop in a body of a few hundred instructions: one native
instruction per pass is enough there. The second row is the large body with a loop that mostly
steps, such as base R's `rbind.data.frame` or `apply`: compiling it costs as much as millions of
its passes save, and most sessions never run that many. Waiting until tier 0 has run them bounds
the loss, because the compile then costs at most what tier 0 has spent on the saving it would have
made.

A hot `for` loop also says how many passes one run of it takes, and entering compiled code in
the middle of it costs an entry. A loop over a call's two arguments turns hot because the call
does, and each activation enters compiled code for a pass or two. So the model prices one call
of the body from the running loop: the work around the loops once, plus the loop's run length in
passes, less the entry. The loop's request is refused while that saves nothing (`jit.rs`,
`short_loop`), and the loop asks again after as many backward jumps again as the body has taken,
which a loop that sometimes runs long reaches soon. The run length decides, not the passes left
at the request: a loop of ten passes reaches the threshold on the same pass of every call, often
its last, and it repays the entry on the next call all the same. A `while` or `repeat` loop does
not say how long it runs and is not asked, and neither is a loop nested in another, whose passes
the outer loop multiplies.

A body with loops can also turn hot by its calls. Its profile holds the backward jumps it has
taken and the entries it has counted, so the model prices one call by measurement: the work
around the loops once, plus the mean pass through a loop for each backward jump the body takes
per entry, less the entry (`jit.rs`, `saving_per_call_with_loops`). That saving is asked the
same question as a loop-free body's (`unpaid`). A body whose loops run two passes per call is
thereby priced as the short body it is, and a body whose work around the loops is steps that
save nothing is left to its loops (`MOSTLY_TIER0`), which enter compiled code past those steps.

### Native forms on boxed values (stage 2, partly built)

The planner's types describe numbers and numeric vectors, and every other value is `Boxed`. Generic
code asks a boxed value a few questions that the object's header answers, so these calls compile
to a test of the header and a few loads, under the guard on the builtin's binding that every
speculated function has:

| Call | Native form | State |
|---|---|---|
| `is.null(x)`, `is.list(x)`, `is.character(x)`, `is.function(x)` and the other type predicates | A test of the kind byte against a set of kinds | Built |
| `length(x)` on a vector that is neither an object nor ALTREP | A load of the length | Built |
| `for (v in x)` over a character vector, a vector with attributes or a factor | The loop's counter in a register; the start prepares the sequence as tier 0 does, and each pass reads one element through tier 0's reader | Built |
| `x$name`, `x[["name"]]` on a plain list | The position cached for the list's names, then a load | Planned |
| `a == b`, `a != b` on string scalars | A pointer compare of the interned strings, `NA` aside | Planned |
| `missing(x)` in an inlined call | The call's own passing record, a constant | Planned |
| `nchar(x)`, `identical(a, b)` on scalars, `inherits(x, "class")` | A helper that runs no R code | Planned |

A value the test cannot answer (an object, where a method or a class decides, or an ALTREP
vector) calls the builtin on a cold path of the same code, not an exit, so a call site that sees
both keeps its compiled code.

A `for` loop over a pairlist stays in tier 0, because only tier 0's loop state holds the
position in the list: the loop's start hands the instruction to tier 0, and an entry in the
middle of such a loop is declined.

Arithmetic and comparison on boxed operands, such as `a == b` on strings or `x + y` on vectors
with attributes, runs as tier 0's step. The step dispatches only when an operand is an object,
which it tests, so in a call inlined without its frame it needs no frame and the callee keeps its
inlined form. An operand whose type the planner does not know keeps the step opaque.

Every other builtin call from compiled code is one helper call with its signals deferred. The
helper tests the arguments itself for a value that could run R code, and a builtin that has a
condition to signal while a handler waits abandons the call, so the body leaves and tier 0 signals
under the real frame. The call therefore needs no handler test and no write-back of the variables
before it, with or without handlers established.

### Stretches (stage 3, planned)

Consecutive steps run as one stretch: compiled code writes back the variables once, tier 0's
dispatch loop runs from the first instruction to the last, and compiled code reloads once. A body
whose plan is one stretch is not compiled.

The measurements that motivate stretches come from the "helper of 20 lines" body of
`r15_plain_function_bodies.R`: two inlined frames, ten native instructions, four steps that can
run R code and a loop over strings. Compiled, it runs within 1% of tier 0's instructions. What
the inlined frames and the native instructions save is spent on the entry, on the steps' helper
calls, and above all on writing the variables back before each step that can read the frame. The
model charges each of those write-backs, so the body stays in tier 0 in every mode but `stress`.
One write-back and one reload per stretch is the structural answer for such a body, and a direct
call from compiled code into a callee's compiled code (stage 4) removes the entry that every
compiled callee pays.

### Forced compilation costs no more than tier 0

`RY_JIT=force` asks for compiled code as early as possible: a body is asked about at its second
entry and a loop at its first backward jump. It skips the model's payback tests, which weigh the
compile against the calls a body expects, and keeps the steady-state tests: a body or loop whose
compiled activation the model prices above tier 0's is refused in this mode too (`SMALL` for a
saving per call of zero, `STEPPING_LOOP`, `MOSTLY_TIER0`, `SHORT_LOOP`). The rule forced
compilation meets is therefore per compiled body: in the steady state a compiled activation runs
at most the instructions tier 0 runs for it. The "Forced" columns below measure that. The
session's total time includes every compile, which is the cost the mode is asked to pay.

`RY_JIT=stress` compiles every body that compiles at all, whatever the model says. The
differential tests, the everything-compiled oracle engine and the fuzzers run it, because their
subject is whether compiled code agrees with tier 0, and the bodies the model refuses reach the
most exits.

## Measured

Each row is one pattern of `tools/bench/appsuite/patterns/r15_plain_function_bodies.R`: a closure
called from a loop, in callgrind instructions per call, the loop included. The counts cover 32,000
calls that start after a collection. "Default" is the tier as a session runs it, and "Forced" is
`RY_JIT=force`.

| Body | JIT off | Default | Forced |
|---|---|---|---|
| f() with no argument | 1,627 | 1,544 | 1,544 |
| a function that defines and calls a function | 5,422 | 5,437 | 5,437 |
| lapply over 3 with a function that reads a local | 18,566 | 18,578 | 18,579 |
| two argument guards with stop() | 3,961 | 2,379 | 2,379 |
| a function that passes ... on | 3,777 | 3,793 | 3,794 |
| field(x, name) as x[[name]] | 2,201 | 2,213 | 2,212 |
| 10 scalar assignments | 4,339 | 1,988 | 1,987 |
| if / else if chain on a string | 5,807 | 5,824 | 5,826 |
| switch() on a string | 2,218 | 2,230 | 2,230 |
| reads of x$a, x$a$b, x[['a']][['b']] | 5,047 | 4,204 | 4,204 |
| writes of x$a, x[['a']], x$a$b | 14,203 | 14,221 | 14,220 |
| f(a) with one argument | 1,776 | 1,735 | 1,736 |
| for loop that sums 10 elements | 9,543 | 3,967 | 3,967 |
| for loop that fills 10 elements | 9,066 | 5,010 | 5,010 |
| for loop that copies 5 list fields | 17,131 | 17,273 | 17,241 |
| list() of 4 named fields with paste0() | 9,129 | 9,143 | 9,143 |
| helper of 20 lines | 22,140 | 22,187 | 22,189 |
| f(a, b, c) by position | 1,896 | 1,758 | 1,758 |
| f(a, b = , c = ) by name | 1,899 | 1,758 | 1,758 |
| f(a) with two default arguments | 3,051 | 3,065 | 3,065 |
| a function that passes 3 arguments on | 2,852 | 1,985 | 1,985 |
| a chain of 5 calls | 5,016 | 3,784 | 3,784 |
| a function that tests missing(b) | 3,066 | 3,082 | 3,080 |
| a function that returns early | 1,915 | 1,904 | 1,903 |

The same loops inside `withCallingHandlers`, as a test suite runs them:

| Body | JIT off | Default | Forced |
|---|---|---|---|
| f() with no argument | 1,825 | 1,737 | 1,744 |
| a function that defines and calls a function | 5,478 | 5,495 | 5,495 |
| lapply over 3 with a function that reads a local | 18,632 | 18,649 | 18,649 |
| two argument guards with stop() | 3,961 | 2,380 | 2,380 |
| a function that passes ... on | 4,662 | 4,679 | 4,679 |
| field(x, name) as x[[name]] | 3,142 | 2,429 | 2,429 |
| 10 scalar assignments | 5,370 | 2,407 | 2,407 |
| if / else if chain on a string | 6,028 | 6,045 | 6,045 |
| switch() on a string | 2,218 | 2,235 | 2,235 |
| reads of x$a, x$a$b, x[['a']][['b']] | 5,047 | 4,204 | 4,204 |
| writes of x$a, x[['a']], x$a$b | 14,221 | 14,238 | 14,238 |
| f(a) with one argument | 2,746 | 2,154 | 2,154 |
| for loop that sums 10 elements | 10,536 | 5,456 | 5,456 |
| for loop that fills 10 elements | 9,070 | 5,015 | 5,015 |
| for loop that copies 5 list fields | 17,129 | 17,272 | 17,241 |
| list() of 4 named fields with paste0() | 9,125 | 9,143 | 9,143 |
| helper of 20 lines | 23,462 | 23,510 | 23,510 |
| f(a, b, c) by position | 2,867 | 2,177 | 2,177 |
| f(a, b = , c = ) by name | 2,867 | 2,177 | 2,177 |
| f(a) with two default arguments | 4,043 | 4,060 | 4,060 |
| a function that passes 3 arguments on | 4,636 | 2,404 | 2,404 |
| a chain of 5 calls | 7,746 | 4,963 | 4,963 |
| a function that tests missing(b) | 3,063 | 3,080 | 3,080 |
| a function that returns early | 2,942 | 2,323 | 2,323 |

The patterns of `tools/bench/appsuite/patterns/r02_argument_checks.R`:

| Body | JIT off | Default | Forced |
|---|---|---|---|
| function with an argument check | 145,301 | 145,403 | 145,377 |
| match.arg() | 9,102 | 9,134 | 9,115 |
| stopifnot() with two conditions | 6,379 | 6,419 | 6,396 |
| identical() of two short vectors | 3,987 | 4,000 | 4,000 |
| check_args(...) alone | 106,406 | 106,474 | 106,475 |
| list(...) of three values | 5,863 | 5,880 | 5,880 |
| ifelse() on a scalar | 4,129 | 4,144 | 4,144 |
| nested ifelse() with is.na() | 11,169 | 11,183 | 11,184 |
| isTRUE() | 4,092 | 4,104 | 4,104 |
| inherits(x, 'Date') | 1,873 | 1,889 | 1,889 |
| is(x, 'character') | 4,393 | 4,405 | 4,405 |
| missing() and default arguments | 4,899 | 4,914 | 4,917 |

With the tier on, tier 0 counts back edges and entries for it: 17 instructions per back edge, the
measuring loop's included. A count that is 17 above "JIT off" is therefore a body that runs in
tier 0 unchanged, and a body with loops of its own or with closures called from a builtin pays the
same counts again. No row of either mode is above "JIT off" by more than those counts, which stay
under 1% of every row.

The rows that gain are bodies with an inlined frame, native arithmetic, a loop with a native
counter or a field read, and they gain most under a handler, where tier 0's call makes promises:
a chain of five calls runs in under two thirds of tier 0's instructions there. Every body of
`r02_argument_checks.R` is steps around builtins and closures that keep their frame, so none
compiles and both modes match tier 0.

The two modes agree on every row, because forced compilation keeps the tests of what a compiled
activation costs. What it adds is the compiles. The `bodies` section of the app-suite workload
(`tools/bench/appsuite/app_workload.R`) shows both, in instructions for the whole process at two
scales:

| Scale | JIT off | Default | Forced |
|---|---|---|---|
| 0.1 | 1,377,828,236 | 1,384,168,284 | 3,089,804,953 |
| 0.2 | 2,337,249,486 | 2,348,551,206 | 4,050,783,519 |
| Difference | 959,421,250 | 964,382,922 | 960,978,566 |

The difference between the scales is the section's steady state, where the three modes are
within 1% of each other. The rest of the forced column is compiling every body that passes the
steady-state tests at its second entry.

On the app-suite workload the default mode compiles nothing. Its hot bodies are steps around
calls that keep their frame (they pass `...`, run an instruction only tier 0 has, or call a
closure that needs its own frame), with a few native instructions between them, and the model
prices their write-backs above what the native instructions save. Stretches (stage 3), the
remaining native forms of stage 2 and direct calls into compiled code (stage 4) are what bring
such bodies into compiled code.

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
2. **Partly built.** Type predicates and `length` on the value's header, builtin calls with
   deferred signals, loops over boxed sequences, boxed arithmetic inside inlined frames, the exit
   profile that turns an instruction that keeps leaving into a step, a body with loops priced by
   its measured passes per call, write-backs charged in the model, and forced compilation held
   to tier 0's cost per body. Planned:
   the other native forms in the table.
3. Planned. Stretches of tier-0 instructions.
4. Planned. Calls with a frame made from an inlined frame, a direct call from compiled code
   into a callee's compiled code, `...` passed through, and argument expressions that are
   effect-free builtin calls.
