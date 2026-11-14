---
title: Why the code bases run at 1.5x
description: "A profile of the three 300,000-line benchmark code bases against GNU R, function by function: where ry is no faster than GNU R, why the other suites look faster, an Amdahl table of the causes, and the ranked work that follows"
---

The three benchmark code bases (finbench, dsbench, biobench; `just bench codebases`) run 1.4x to
1.6x faster than GNU R. Package code itself is the reason, not something peculiar to the code
bases. Six tenths of ry's time is spent in package closures running in tier 0, and that code runs
only 1.4x faster than GNU R's bytecode. In tier 0 a closure call, an S3 dispatch, a `tryCatch` and an
`eval` each cost about as much as they do in GNU R, and package code is made of exactly those. Small
builtins run 2.4x to 3x faster than GNU R, but the closures that spend their time in builtins run at
1.2x to 1.8x as a whole, and they hold a quarter of the time.

The other suites only look faster because of how they are averaged. Their headline numbers are
geometric means over programs, and the code bases sum time over 9,000 tests. Summed the same way,
the packages suite runs at 1.60x, the same as the code bases:

| Suite (`tools/bench/*/results.json`) | geometric mean over programs | sum of GNU R's time over sum of ry's |
| --- | ---: | ---: |
| public R JIT benchmarks | 2.66x | 2.07x (55.3 s against 26.7 s) |
| package test suites | 1.91x | 1.60x (229.5 s against 143.6 s) |
| application workloads | 1.95x | 1.77x (6.8 s against 3.9 s) |
| code bases, finbench | | 1.62x (233.7 s against 144.6 s) |

The packages suite's large packages run at the code bases' ratio: rlang 1.73x, purrr 1.57x, cli
1.54x, xts 1.30x, data.table 1.31x, foreach 1.10x. Its geometric mean is lifted by small packages
such as abind at 3.2x, whose tests are a few hundred milliseconds of builtin calls.

## How it was measured

`tools/bench/compare_profiles.py run` runs the benchmark driver (`crates/oracle/src/bench.R`) on a
package copy that `oracle bench --keep` prepared, once on GNU R under `Rprof()` and once on ry with
`RY_PROFILE_STACKS`, which writes every sampled stack with its activity. `report` keeps the samples
inside test blocks, scales each engine's samples to its own block time, and compares the time per
closure (self and inclusive), per builtin, per package and per kind of work. Every number on this
page comes from one run of each code base on a 4-core machine with GNU R 4.6.2 and a profiling
build of ry, so treat differences under about 10% as noise.

The comparison has three limits. Rprof samples no faster than every 10 ms, against ry's 1 ms. Rprof
gives a frame only to true builtins, so `[`, `+` and `==`, which GNU R's bytecode inlines, and the
`.Internal` builtins count toward their caller there. ry's self time for a closure therefore
includes the builtins it calls, so the two sides match. Rprof also names no package, so
closures with the same name in different packages are merged on both sides (`FUN`,
`(anonymous)`).

## Where the time goes

Each closure is classified by where ry spends its self time in it: half or more in builtins makes
it builtin-bound, half or more in native code native-bound, half or more in the interpreter (tier-0
instructions, call setup, dispatch, the AST evaluator) interpreter-bound. The classes partition the
time on both engines, so each has a measured ratio:

| Kind of closure | finbench share, ratio | dsbench share, ratio | biobench share, ratio |
| --- | ---: | ---: | ---: |
| interpreter-bound | 60.3%, 1.48x | 56.8%, 1.43x | 60.8%, 1.41x |
| builtin-bound | 25.3%, 1.83x | 27.2%, 1.50x | 26.9%, 1.24x |
| native-bound | 6.0%, 1.51x | 8.5%, 1.11x | 8.0%, 0.86x |
| mixed | 8.4%, 1.20x | 7.4%, 1.11x | 4.2%, 1.31x |
| run only on GNU R | 10.0 s of GNU R's | 5.6 s | 5.9 s |
| suite | 1.62x | 1.48x | 1.39x |

The last data row is GNU R closures that never run as closures on ry, mostly `.deparseOpts`, which
ry's native `deparse` replaces. Counted together with it, `deparse` runs 2x faster than GNU R.

Inside the interpreter-bound closures, ry spends half its time on tier-0 instructions, 18% in
builtins, 16% in call setup and 5% in S3 method lookup. Across the whole suite the split is the same
in all three code bases:

| ry's activity | finbench | dsbench | biobench | per event |
| --- | ---: | ---: | ---: | --- |
| tier-0 instructions | 36.7% | 37.0% | 33.3% | |
| builtins | 30.6% | 30.6% | 33.2% | about 2 calls per closure call |
| closure call setup | 12.0% | 13.0% | 12.6% | 355 to 404 ns per closure call |
| native code | 9.2% | 9.9% | 9.1% | |
| S3 method lookup | 3.3% | 2.1% | 2.6% | 1.2 to 1.7 µs per dispatch |
| AST evaluator | 2.4% | 2.9% | 3.6% | |
| garbage collection | 1.3% | 1.4% | 2.1% | GNU R's is 5.8% to 5.9% |
| JIT-compiled code | 1.3% | 0.1% | 0.1% | |

finbench makes 52.6 million closure calls, 115 million builtin calls outside the inline fast paths,
23 million promises and 4.5 million S3 dispatches, and turns 10.2 million slot frames into
environments in 145 seconds.

## What each mechanism costs against GNU R

Micro-benchmarks in tier 0 (`RY_JIT=off`) price the mechanisms package code is made of. Each runs
3 million times inside one loop, best of three, in nanoseconds per operation:

| Operation | GNU R | ry | GNU R over ry |
| --- | ---: | ---: | ---: |
| `f0()`, no arguments | 96 | 126 | 0.76x |
| `f1(i)`, one argument | 231 | 249 | 0.93x |
| `function(x) x` called in place | 258 | 246 | 1.05x |
| `f2(i)` with a default `y = 2` | 372 | 197 | 1.89x |
| `f2(x = i, y = 3)` by name | 344 | 212 | 1.62x |
| `f3(1, i)` forwarding `...` | 591 | 414 | 1.43x |
| `f(x, n = length(x), na.rm = FALSE, ...)` called with `na.rm = TRUE` | 2,096 | 1,301 | 1.61x |
| a body with `on.exit` | 176 | 185 | 0.95x |
| `UseMethod`, default method | 1,450 | 936 | 1.55x |
| `UseMethod`, class method | 1,252 | 947 | 1.32x |
| `tryCatch(i, error = ...)` | 4,727 | 4,865 | 0.97x |
| `withCallingHandlers(i, warning = ...)` | 1,746 | 786 | 2.22x |
| `eval(quote(i + 1))` | 1,215 | 1,079 | 1.13x |
| `sum(i)` | 189 | 67 | 2.82x |
| `c(i, 1)` | 149 | 59 | 2.53x |
| `inherits(x, "foo")` | 217 | 73 | 2.97x |
| `list(a = i)` | 127 | 84 | 1.51x |

The pattern is the diagnosis. A builtin call costs a third of GNU R's, but a closure call costs about
what it does in GNU R, an S3 dispatch costs five closure calls on both engines, and `tryCatch` costs
4.8 µs on both. Package code makes about two builtin calls for every closure call, and the builtins
it calls with real data (`c`, `[[`, `names`, `match`, `strptime`) gain less than the tiny ones here,
so its ratio settles near the closure call's. The suites where ry reaches 2x to 3x are the ones where builtins and loops
dominate: `strings` (5.5x), `report` (6.4x) and `spectralnorm` (14x).

The testthat harness is not a special cost. `expect_equal` holds 16% to 39% of ry's time inclusive
of the code under test and runs at 1.4x to 1.9x, and waldo's `compare`, the pure harness part, holds
8% to 13% at 1.5x to 1.9x, the same ratios as the rest. Expectations measured alone run at 1.7x to
2.5x (`expect_true` 429 µs against 205 µs, `expect_equal` on a vector 489 µs against 194 µs), and
`expect_error` at 9x.

## Rows where ry is slower than GNU R

The profile is flat: the largest self-time row in finbench holds 3.3% of ry's time. These rows run
at or below 1x in all three code bases or are large in one, and each has an identified cause:

| Row | share of ry's time | GNU R over ry | Cause |
| --- | ---: | ---: | --- |
| `compiler::compile` from listcomp's `gen_list` (ompr models) | finbench 4.0% | 1.27x | ry's `mkCode` keeps only the source expression, so the whole R-level compiler runs to return its argument. Replacing `compile` by the identity (in `compiler` and listcomp's imports) takes the 574 tests that build models from 16.3 s to 11.9 s, with every test passing. `gen_list` with `.compile = TRUE` costs 3.1 ms against 0.2 ms without it. |
| `unlink`, `dir.create`, openxlsx's `write_file` | biobench 6.8%, finbench 1.8%, dsbench 1.7% | `unlink` 0.19x to 0.32x; `dir.create` 0.50x to 0.64x in biobench and dsbench, 1.28x in finbench; `write_file` 0.48x in biobench | In isolation ry is at parity or faster (`dir.create` 28 µs against 63 µs), but inside openxlsx's `saveWorkbook` ry issues 3.5 times GNU R's `mkdir` system calls, 71% of them failing with `EEXIST`, because a recursive `dir.create` calls `mkdir` on every ancestor from the root where GNU R checks with `stat` first. Why `unlink` of a test's temporary directory costs 1 ms in the suite and not in isolation is unmeasured. |
| `tryCatchOne`, `withRestarts`, `withCallingHandlers`, `doTryCatch` | 2.2% to 5.8% | 0.18x to 0.84x | Self time in `tryCatchOne` is 59% to 70% call setup, and `withCallingHandlers` and `withRestarts` spend 41% to 53% in the AST evaluator, evaluating the expression promise of a caller that was never compiled (the test block). |
| `block` (the test body) and rlang's `eval_bare` | 2.0% to 3.4% | 0.15x to 0.31x | About half of it is the loader forcing a lazily bound definition on first use (`ryLoaderForce`, 1.5 to 1.7 s per code base, plus `parse`); the rest is the AST evaluator, because every test body is a fresh closure that runs once. GNU R compiles each test body on its first call. |
| R6 methods (`(anonymous)`) | 3.3% to 3.6% | 0.79x to 0.86x | Small methods called through `self$f()`: one closure call and environment lookups each. `R6Class(...)$new()$f()` costs 30.0 µs against 24.7 µs. |
| `apply`, `as.character.POSIXt`, `[[.data.frame`, `lm.fit`, `glm.fit`, `.POSIXct`, `[.xts`, `.subset_xts` | 0.5% to 1.9% each | 0.54x to 0.97x | Base R closures that are mostly argument handling and attribute work around a builtin. They are interpreter-bound, so they share the closure call's cost. |

## The Amdahl table

Each row is one cause, its share of ry's suite time, its ratio against GNU R today, and the suite's
ratio if that cause alone reached the target. The interpreter-bound row is the measured class from
above; the specific rows overlap it.

| Cause | share of ry's time | GNU R over ry today | target | suite if only this is fixed (fin, ds, bio) |
| --- | ---: | ---: | --- | --- |
| package closures in tier 0 (calls, dispatch, conditions, instructions) | 57% to 61% | 1.41x to 1.48x | 3x | 2.33x, 2.11x, 2.05x |
| the same at 5x | | | 5x | 2.81x, 2.50x, 2.46x |
| builtin-bound closures | 25% to 27% | 1.24x to 1.83x | 3x | 1.79x, 1.72x, 1.65x |
| closure call setup alone | 12% to 13% | about 1x to 1.9x | 4x cheaper | about +10% to +11% |
| S3 method lookup alone | 2.1% to 3.3% | 1.3x to 1.55x | 10x cheaper | about +2% to +3% |
| explicit `compiler::compile` calls | finbench 4.0% | 1.27x | removed | finbench 1.62x to 1.67x (measured: 4.4 s saved) |
| file-system builtins in context | 1.7% to 6.8% | 0.19x to 1.28x | parity with GNU R | about +0.6% (fin), +1% (ds), +4% (bio) |
| native code | 6% to 8.5% | 0.86x to 1.51x | none possible | none |

The ceiling follows from native code, which ry calls through the same C API at the same speed.
With every kind of closure except the native-bound ones at the target, the suites reach:

| Target for all R-level work | finbench | dsbench | biobench |
| --- | ---: | ---: | ---: |
| 2x | 1.96x | 1.90x | 1.88x |
| 3x | 2.84x | 2.71x | 2.67x |
| 5x | 4.42x | 4.09x | 4.03x |
| 10x | 7.59x | 6.62x | 6.54x |

So 3x on the code bases requires R-level package code to run at more than 3x, and no single fix
moves the suite by more than its share. The interpreter-bound class is the only one large enough
to reach 2x alone.

## What to do, in order

1. **Make the closure call cheaper than GNU R's.** Every interpreter-bound row is calls: 52.6 million
   in finbench at 355 to 404 ns of call setup each, plus the instructions around them. A no-argument
   call costs 126 ns against GNU R's 96 ns, and a call with promise arguments about 200 to 400 ns.
   The frame and promise representation redesign is this lever. Judge it by the per-call costs in the
   table above, and aim below 50 ns for a call that needs no environment.
2. **Make S3 dispatch cost one closure call.** A dispatch costs 0.94 µs in a micro-benchmark and 1.2
   to 1.7 µs of lookup alone in the suites. Each dispatch searches every candidate name (one per
   class, then the default) through the caller's frames, the top environment, the generic's methods
   table and the search path (`dispatch.rs`, `MethodScope`). A per-generic table from class to
   method, invalidated when a method is defined or a frame shadows one, would bring it near a call.
3. **Make `compiler::compile` and `cmpfun` native on ry.** They run GNU R's compiler in R only for
   `mkCode` to keep the source expression. A native `compile` that returns its argument (and a
   `cmpfun` that compiles through ry's own path) saves 3% of finbench and is a small change.
4. **Fix the file-system builtins.** Check ancestors before making them in a recursive `dir.create`,
   as GNU R does, and find why `unlink` of a temporary directory costs 1 ms inside the suite. This is
   4% of biobench.
5. **Conditions.** `tryCatch` costs 4.8 µs on both engines, and the condition helpers run below 1x in
   context. A native `tryCatch` that establishes its handlers without the R-level `tryCatchList`,
   `tryCatchOne` and `doTryCatch` closures removes three closure calls and their promises per call.
6. **Compile the test bodies and promise code that run in the AST evaluator** (2.4% to 3.6%), or
   make the AST evaluator's calls as cheap as tier 0's. This is the `block` and `eval_bare` rows.

A tier-1 JIT that compiles more package bodies is not first on this list. Tier-1 code runs 0.1% to
1.3% of the time, and the bodies it would compile are full of the calls and dispatches in items 1
and 2, which it would hand back to the same call path.
