---
title: Why the code bases run at 1.5x
description: "A profile of the three 300,000-line benchmark code bases against GNU R, function by function: where ry is no faster than GNU R, why the other suites look faster, an Amdahl table of the causes, and the ranked work that follows"
---

The three benchmark code bases (finbench, dsbench, biobench; `just bench codebases`) run 1.4x to
1.6x faster than GNU R. Package code itself is the reason, not something peculiar to the code
bases. Six tenths of ry's time is spent in package closures running in tier 0, and that code runs
only 1.4x to 1.5x faster than GNU R's bytecode. In tier 0 a closure call, an S3 dispatch, a `tryCatch` and an
`eval` each cost about as much as they do in GNU R, and package code is made of exactly those. Small
builtins run 2.4x to 3x faster than GNU R, but the closures that spend their time in builtins run at
1.5x to 1.9x as a whole, and they hold a quarter of the time. With everything but native code at 3x,
the code bases would reach 2.7x to 2.8x.

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

Each code base was run twice on a 4-core machine with GNU R 4.6.2 and a profiling build of ry. The
first run is unprofiled and gives each engine's block time, the sum of its test blocks' times
(`tools/bench/compare_profiles.py run --no-profile`). The second run runs the benchmark driver
(`crates/oracle/src/bench.R`) on the same package copy, once on GNU R under `Rprof()` and once on
ry with `RY_PROFILE_STACKS`, which writes every sampled stack with its activity
(`compare_profiles.py run`). `compare_profiles.py report --times` keeps the samples inside test
blocks, scales each engine's shares to its unprofiled block time, and compares the time per closure
(self and inclusive), per builtin, per package and per kind of work. The second run ran two code
bases at a time, which shifts both engines alike and cancels in the scaling.

The comparison has four limits:

- Both engines sample wall time, but Rprof samples every 10 ms against ry's 1 ms. A row with 100
  GNU R samples (1 s) carries about 10% sampling error, and the report prints each row's count.
- Rprof gives a frame only to true builtins, so `[`, `+` and `==`, which GNU R's bytecode inlines,
  and the `.Internal` builtins count toward their caller there. ry's self time for a closure
  therefore includes the builtins it calls, so the two sides match.
- Rprof names a package only for a `pkg::f()` call, so other names take the package ry attributes
  them to most often, and closures with the same name in different packages merge on both sides
  (`FUN`, `(anonymous)`). A method called as `obj$f()` is `f` on both.
- The benchmark's prepared package loads lazily on both engines, but GNU R's lazy loader is the R
  closure `.ry_force`, a row of its own, while ry's is the builtin `ryLoaderForce`, whose time counts
  toward the closure that forced the binding. ry also defers S4 definitions from load to first use
  (`.ry_force_s4`), so 1.1 to 1.5 s of work that GNU R does while loading falls inside ry's test
  blocks. That costs ry 1% to 2% of its block time and is not a gap in speed.

## Where the time goes

Each closure is classified by where ry spends its self time in it: half or more in builtins makes
it builtin-bound, half or more in native code native-bound, half or more in the interpreter (tier-0
instructions, call setup, dispatch, the AST evaluator) interpreter-bound. The classes partition the
time on both engines, so each has a measured ratio:

| Kind of closure | finbench share, ratio | dsbench share, ratio | biobench share, ratio |
| --- | ---: | ---: | ---: |
| interpreter-bound | 63.7%, 1.47x | 57.9%, 1.47x | 56.7%, 1.38x |
| builtin-bound | 24.5%, 1.91x | 26.4%, 1.47x | 24.8%, 1.48x |
| native-bound | 6.0%, 1.44x | 8.1%, 1.05x | 7.6%, 1.00x |
| mixed | 5.8%, 1.14x | 7.5%, 1.15x | 10.9%, 0.94x |
| run only on GNU R | 8.7 s of GNU R's | 4.7 s | 4.7 s |
| suite | 1.62x | 1.48x | 1.39x |

The last data row is GNU R closures that never run as closures on ry. The largest is `.deparseOpts`
(3.9 s, 1.9 s and 1.5 s), which ry replaces with a builtin fast path; counted together with it,
`deparse` runs 2x to 3.4x faster than GNU R. Next come `makeRestartList` (0.5 to 0.75 s), which
ry's native `withRestarts` prologue skips, and GNU R's lazy loader `.ry_force`.

Inside the interpreter-bound closures, ry spends half its time on tier-0 instructions, 18% in
builtins, 15% to 17% in call setup and 3% to 5% in S3 method lookup. Across the whole profiled run
the split is the same in all three code bases:

| ry's activity | finbench | dsbench | biobench | per event |
| --- | ---: | ---: | ---: | --- |
| tier-0 instructions | 37.3% | 37.7% | 34.4% | |
| builtins | 30.2% | 30.6% | 33.3% | about 2 calls per closure call |
| closure call setup | 11.9% | 12.2% | 11.9% | 310 to 330 ns per closure call |
| native code | 9.0% | 10.0% | 8.5% | |
| S3 method lookup | 3.3% | 2.1% | 2.5% | 1.1 to 1.6 µs per dispatch |
| AST evaluator | 2.3% | 2.8% | 3.8% | |
| garbage collection | 1.3% | 1.3% | 1.9% | GNU R's is 5.8% to 5.9% |
| JIT-compiled code | 1.3% | 0.1% | 0.4% | |

A per-event cost is the activity's share times the unprofiled block time, over the event count.
finbench makes 52.6 million closure calls, 115 million builtin calls outside the inline fast paths,
23 million promises and 4.5 million S3 dispatches, and turns 10.2 million slot frames into
environments.

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
so its ratio settles near the closure call's. The programs where ry reaches 5x or more are the ones
where builtins and loops dominate: `strings` (5.5x), `report` (6.4x) and `spectralnorm` (14x).

The testthat harness is not a special cost. `expect_equal` holds 16% to 39% of ry's time inclusive
of the code under test and runs at 1.4x to 1.9x, and waldo's `compare`, the pure harness part, holds
8% to 13% at 1.5x to 1.9x, the same ratios as the rest. Expectations measured alone run at 1.7x to
2.5x (`expect_true` 429 µs against 205 µs, `expect_equal` on a vector 489 µs against 194 µs), and
`expect_error` at 9x.

## Rows where ry is slower than GNU R

The profile is flat: the largest self-time row holds 2.7% of ry's time in finbench and 3.4% in
biobench. These rows run below 1x in two or more code bases or are large in one, and each has an
identified cause. Shares are of ry's block time, and the ratios are GNU R's time over ry's:

| Row | share of ry's time | GNU R over ry | Cause |
| --- | ---: | ---: | --- |
| `compiler::compile` from listcomp's `gen_list` (ompr models), inclusive | finbench 3.9% | 1.54x | ry's `mkCode` keeps only the source expression, so the whole R-level compiler runs to return its argument. Replacing `compile` by the identity (in `compiler` and listcomp's imports) takes the 574 tests that build models from 16.3 s to 11.9 s, with every test passing. `gen_list` with `.compile = TRUE` costs 3.1 ms against 0.2 ms without it. |
| `block` (the test body) and rlang's `eval_bare`, self | 1.9% to 3.6% | 0.16x to 0.33x | About half is ry's lazy loader (`ryLoaderForce`, 1.5 to 1.7 s per code base), which GNU R reports as `.ry_force` (2.3 to 2.4 s); together the two engines spend about the same on it. The rest is the AST evaluator on both engines, because GNU R compiles a closure outside the global environment only on its second call and a test body runs once. That rest runs at 0.3x to 0.7x. |
| `tryCatchOne`, `withCallingHandlers`, `withRestarts`, self | 1.3% to 2.8% | 0.22x to 0.94x | Self time in `tryCatchOne` is mostly call setup for R-level helper closures, and `withCallingHandlers` and `withRestarts` spend much of theirs in the AST evaluator, evaluating the expression promise of a caller that was never compiled (the test block). |
| `dir.create`, openxlsx's `write_file` | dsbench 1.7%, biobench 4.0% | `dir.create` 0.35x and 0.80x, 1.24x in finbench | Inside openxlsx's `saveWorkbook` ry issues 3.5 times GNU R's `mkdir` system calls, 71% of them failing with `EEXIST`, because a recursive `dir.create` calls `mkdir` on every ancestor from the root where GNU R checks with `stat` first. In isolation ry's `dir.create` is faster (28 µs against 63 µs). |
| `apply`, `as.character.POSIXt`, `[[.data.frame`, `lm.fit`, `.POSIXct`, `.subset_xts`, `as.double.xts`, `glue_data` | 0.4% to 2.1% each | 0.51x to 0.94x | Closures that are mostly argument handling and attribute work around a builtin. They are interpreter-bound, so they share the closure call's cost. |

## The Amdahl table

Each row is one cause, its share of ry's block time, its ratio against GNU R today, and the suite's
ratio if that cause alone reached the target. The interpreter-bound row is the measured class from
above; the specific rows overlap it.

| Cause | share of ry's time | GNU R over ry today | target | suite if only this is fixed (fin, ds, bio) |
| --- | ---: | ---: | --- | --- |
| package closures in tier 0 (calls, dispatch, conditions, instructions) | 57% to 64% | 1.38x to 1.47x | 3x | 2.39x, 2.11x, 2.00x |
| the same at 5x | | | 5x | 2.94x, 2.51x, 2.35x |
| builtin-bound closures | 25% to 26% | 1.47x to 1.91x | 3x | 1.77x, 1.71x, 1.58x |
| closure call setup alone | 12% | about 1x to 1.9x | 4x cheaper | about +10% |
| S3 method lookup alone | 2.1% to 3.3% | 1.3x to 1.55x | 10x cheaper | about +2% to +3% |
| explicit `compiler::compile` calls | finbench 3.9% | 1.54x | removed | finbench 1.62x to 1.67x (measured: 4.4 s saved) |
| condition helpers (`tryCatchOne`, `withCallingHandlers`, `withRestarts`) | 1.3% to 2.8% | 0.22x to 0.94x | 3x | about +1% to +2.4% |
| test bodies in the AST evaluator, without lazy loading | 0.9% to 1.8% | 0.31x to 0.69x | parity with GNU R | about +0.4% to +1.2% |
| `dir.create` in context | 0.8% to 2.5% | 0.35x to 1.24x | parity with GNU R | about +0.7% (ds), +0.5% (bio) |
| native code | 6% to 8.1% | 1.00x to 1.44x | none possible | none |

The ceiling follows from native code, which both engines run as the same compiled code, so ry gains
nothing there beyond cheaper calls into it. With every kind of closure except the native-bound ones
at the target, the suites reach:

| Target for all R-level work | finbench | dsbench | biobench |
| --- | ---: | ---: | ---: |
| 2x | 1.96x | 1.90x | 1.90x |
| 3x | 2.84x | 2.71x | 2.70x |
| 5x | 4.41x | 4.11x | 4.10x |
| 10x | 7.58x | 6.71x | 6.69x |

So 3x on the code bases requires R-level package code to run at more than 3x, and no single fix
moves the suite by more than its share. The interpreter-bound class is the only one large enough
to reach 2x alone.

## What to do, in order

1. **Make the closure call cheaper than GNU R's.** Every interpreter-bound row is calls: 52.6 million
   in finbench at 310 to 330 ns of call setup each, plus the instructions around them. A no-argument
   call costs 126 ns against GNU R's 96 ns, and a call with promise arguments about 200 to 400 ns.
   The frame and promise representation redesign is this lever. Judge it by the per-call costs in the
   table above, and aim below 50 ns for a call that needs no environment.
2. **Make S3 dispatch cost one closure call.** A dispatch costs 0.94 µs in a micro-benchmark and
   1.1 to 1.6 µs of lookup in the suites. `resolve_s3` (`dispatch.rs`) builds the method scope once
   per dispatch, then for each candidate name (one per class, then the default) reads every frame
   between the caller and its top environment before it consults the name's cached row for the part
   from the top environment on. The parked branch `perf/s3-resolution` (`backlog.md`) replaces that
   frame walk with one frame read in the common case. Land it, then profile where the rest of the
   time goes (building method names, the `.Class` vector, the method's frame).
3. **Make `compiler::compile` and `cmpfun` native on ry.** They run GNU R's compiler in R only for
   `mkCode` to keep the source expression. A native `compile` that returns its argument (and a
   `cmpfun` that compiles through ry's own path) saves 3% of finbench and is a small change.
4. **Conditions.** `tryCatch` costs 4.9 µs against GNU R's 4.7 µs, and the condition helpers run
   below 1x in context. A native `tryCatch` that establishes its handlers without the R-level
   `tryCatchList`, `tryCatchOne` and `doTryCatch` closures removes three closure calls and their
   promises per call.
5. **Run-once code.** Test bodies and the promises they pass down run in the AST evaluator on both
   engines, and ry's is 1.5x to 3x slower there. Make the AST evaluator's calls as cheap as tier 0's,
   or compile a body on its first call when it is large.
6. **`dir.create`.** Check ancestors before making them in a recursive `dir.create`, as GNU R does.

A tier-1 JIT that compiles more package bodies is not first on this list. Tier-1 code runs 0.1% to
1.3% of the time, and the bodies it would compile are full of the calls and dispatches in items 1
and 2, which it would hand back to the same call path.
