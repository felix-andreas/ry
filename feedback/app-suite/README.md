# ry on a large application test suite: findings, recommendations and reproducers

This package comes from a user of ry. The user runs the test suite of a private R application on
ry 0.5.0 and on GNU R 4.5.2. The suite has about 400 test files. The application uses S4 classes,
small data.table and xts objects, string keys, dates, condition handlers and many small functions.

All code in this package uses neutral names and generated data. It contains no code, names or
data of the application.

## The result

The suite needs 6801 s of CPU time on GNU R and 3918 s on ry 0.5.0 in CI. That is a factor of
1.74. On a developer machine, without ry's profile summary, a sample of 12 CPU-heavy test files
is 2.56 times as fast on ry.

## Where ry's time goes

The numbers are sums of ry's profile summaries over the whole suite, without the waits for the
database and for HTTP.

| What ry does | CPU s | Share |
|---|---:|---:|
| Tier 0 bytecode instructions | 1407 | 35.9% |
| Builtins | 1178 | 30.1% |
| Closure call setup | 496 | 12.7% |
| Native package code (`.Call`) | 407 | 10.4% |
| Garbage collection | 127 | 3.2% |
| S3 method lookup | 77 | 2.0% |
| AST evaluator | 72 | 1.8% |
| Variable lookup, S4 method selection, frame introspection | 126 | 3.2% |
| Profile summary bookkeeping | 23 | 0.6% |
| JIT-compiled code | 1 | 0.0% |

The suite makes 1297 million closure calls (383 ns of setup each) and 2637 million builtin calls
outside the inline fast paths (447 ns each). It forces 1481 million promises. 188 million calls
pass `...`. 199 million slot frames become environments, 23.7 million of them for
`standardGeneric`.

The JIT does not run, and the entry threshold is not the reason. Bodies with 3000 or more
entries take 70.1% of the time. The JIT refuses them: "the body runs too rarely to repay
compiling it" 28,942 times, "compiled code would run nothing natively" 2,680 times, "does too
little natively" 2,288 times. On one test file, `RY_JIT=off` costs 4%, `RY_JIT_THRESHOLDS=100,100`
gains nothing, and `RY_JIT=force` costs 46%.

## Recommendations, ranked by expected gain for this suite

The gains are seconds of the 3918 s. Changes 2 to 4 overlap.

| # | Change | Gain | Basis |
|---|---|---:|---|
| 1 | Profile summary that keeps the fast paths | 490 s (12.5%) | measured |
| 2 | Compiler tier for generic closure bodies | 420 to 700 s | assumed: 30% to 50% of tier 0 |
| 3 | Cheaper builtin calls on small values | 130 to 260 s | 264 s per 100 ns per call |
| 4 | Cheaper closure calls | 170 to 300 s | 130 s per 100 ns per call, plus garbage collection |
| 5 | Cheaper calls of native package code | 100 to 130 s | profile, uncertain |
| 6 | S4 generic call at the cost of one closure call | 50 to 70 s | profile |
| 7 | `tryCatch` and `try` as builtins | about 40 s | profile |
| 8 | Builtins that are slower than GNU R's | 20 to 40 s | reproducers |

### 1. A profile summary that keeps the fast paths

The user's CI collects a profile summary in every test process. With the summary on, ry needs
14% more CPU time on real test files (between 3% and 34% per file). The workload in `bench/`
needs 17% more. While `activity::enabled()` is true, a closure call runs through the general
path, and the inline fast paths for builtins and for scalar `x[i]` are off. One call costs 150
to 250 ns more:

| Pattern | ry ns, summary off | ry ns, summary on | Factor |
|---|---:|---:|---:|
| `f()` | 181 | 328 | 1.81 |
| `f(a, b = , c = )` by name | 126 | 330 | 2.62 |
| A function that returns early | 166 | 414 | 2.49 |
| 10 scalar assignments in a function | 209 | 455 | 2.18 |
| S4 generic with one argument | 998 | 1511 | 1.51 |
| `new()` with initialize and validity | 68,726 | 94,482 | 1.37 |

Reproduce it with any file in `repro/`: run it once with `RY_PROFILE_SUMMARY=/tmp/x.md` and once
without. `bench/run.sh` prints the cost per section of the workload.

### 2. A compiler tier for generic closure bodies

Tier 0 takes 36% of the time and tier 1 takes 0.0%. The bodies call builtins and closures on
lists, S4 objects, strings and tables. Reproducers: `r15_plain_function_bodies.R`,
`r02_argument_checks.R`.

### 3. Cheaper builtin calls on small values

The builtins with the most time are `[` (83 s), `paste` (48 s), complex assignment such as
`x$a <- v` (39 s), `c` (32 s), `lapply` (30 s), `==` (22 s), `UseMethod` (20 s) and `[[` (19 s).
Reproducers: `r03_keys_and_sets.R`, `r11_functionals.R`, and in `r15` the rows "writes of `x$a`"
(1.19 times as fast as GNU R) and "`if` / `else if` chain on a string" (1.26).

### 4. Cheaper closure calls

Half of the objects that the garbage collector frees are parts of call frames: pairlists, binding
cells, promises and environments (2763 of 5451 million). Reproducers in `r15`: `f(a)` (1.20
times), `f()` (1.57), a chain of 5 calls (1.47), a function that passes `...` on (1.36). In
`r09_closure_objects.R`: a method that assigns with `<<-` (1.24).

### 5. Cheaper calls of native package code

The suite makes 166 million native calls. The builtin `.Call` alone takes 51 s. Functions of
data.table and fastmap are as fast as on GNU R or slower. Reproducers: `r05_data_table_small.R`,
`r09_closure_objects.R` (fastmap: 1.11 to 1.25 times), `r14_large_vectors.R` (`rbindlist`,
`setkey`, `unique` of a table, grouped sum: 0.92 to 1.42 times).

### 6. S4 generic calls

A call of an S4 generic costs 1041 ns, and a plain closure call costs 205 ns. Every generic call
makes an environment. `standardGeneric` takes 40 s and S4 method selection 43 s. Reproducer:
`r01_s4_objects.R` (generic 1.57 times, method with extra arguments 1.71, slot write 1.88, slot
read 0.97).

### 7. Conditions

`tryCatch` runs as four closures (`tryCatch`, `tryCatchList`, `tryCatchOne`, `doTryCatch`).
Reproducer: `r10_conditions.R` (`try` 1.27 times, `tryCatch` with two handlers 1.33, `on.exit`
1.35, `tryCatch(f(i), error = h)` 1.57).

### 8. Builtins that are slower than GNU R's

| Pattern | GNU R ns | ry ns | GNU R / ry |
|---|---:|---:|---:|
| `pmin()` and `pmax()` of 10,000 values | 50,781 | 118,896 | 0.43 |
| `cov()` of a 250 x 20 matrix | 121,826 | 270,020 | 0.45 |
| `format()` of 5,000 dates | 1,060,547 | 1,671,875 | 0.63 |
| `sort()` of 10,000 values | 87,402 | 129,639 | 0.67 |
| `unlist()` of a list of 5,000 numbers | 96,680 | 113,525 | 0.85 |
| `pt()` of 10,000 values | 1,175,781 | 1,125,000 | 1.05 |
| `qt()` of 10,000 probabilities | 5,109,375 | 4,781,250 | 1.07 |

Reproducers: `r12_native_math.R`, `r14_large_vectors.R`.

## An open point

In the user's CI, ry is about 1.3 times slower against GNU R than the developer machine predicts,
after the profile summary is taken into account. CI runs four test processes on a machine with
four cores, inside a container. The cause is not known. Four processes on two cores do not
explain it on the developer machine. ry's benchmarks could include one run under these
conditions.

## Two crashes

- ry 0.5.0 crashes when a function assigns two matrix elements (`m[1, 1] <- 1; m[2, 2] <- 1`) on
  its second call with a matrix of 64 or more elements.
- ry 0.5.0 crashed once with a segmentation fault (address 0x6) in the third test file of one
  process. This crash is not reduced.

## The reproducers (`repro/`)

Each file runs with `ry run file.R` and with `Rscript file.R`, and prints the nanoseconds per
call of each pattern. `run_all.sh` runs all files on both engines and prints GNU R's time over
ry's. `results.txt` holds the timings of all 236 patterns for GNU R 4.5.2 and ry 0.5.0. The
median is 2.31 times. 7 patterns are slower on ry, and 32 are below 1.5 times. The files need the
packages data.table, xts, zoo, zeallot, fastmap and lubridate.

```sh
RY=ry RSCRIPT=Rscript repro/run_all.sh                 # all files
RY=ry RSCRIPT=Rscript repro/run_all.sh repro/r15_plain_function_bodies.R
```

| File | Patterns | For change |
|---|---|---|
| `r15_plain_function_bodies.R` | Calls, defaults, `...`, guards, list fields, small loops | 1, 2, 4 |
| `r02_argument_checks.R` | Type checks, `ifelse` on a scalar, `match.arg`, `stopifnot` | 2, 3 |
| `r03_keys_and_sets.R` | `paste`, `%in%`, `match`, `unique` on short vectors | 3 |
| `r11_functionals.R` | `lapply`, `sapply`, `Map`, `Reduce`, `do.call` | 3, 4 |
| `r09_closure_objects.R` | Objects made of closures, fastmap, `<<-` | 4, 5 |
| `r01_s4_objects.R` | `new`, validity, generics, slots, `is`, `callNextMethod` | 6 |
| `r13_s3_dispatch.R` | `UseMethod`, `NextMethod`, operators | 4 |
| `r10_conditions.R` | `tryCatch`, `try`, handlers, `on.exit`, `sys.calls` | 7 |
| `r05_data_table_small.R`, `r06_xts_small.R`, `r08_data_frame.R` | Small tables and series | 5 |
| `r12_native_math.R`, `r14_large_vectors.R` | Large inputs | 5, 8 |
| `r04_destructuring.R`, `r07_dates.R` | zeallot `%<-%`, dates | 3 |

## A proposed benchmark (`bench/`)

ry's kernels that were derived from this suite (`tools/bench/internal/ci/kernels.R`) are 4 to 6
times as fast on ry, and the suite is 1.74 times as fast. A kernel is one builtin in a loop, and
the score is a geometric mean. In a test suite, time adds up.

`bench/app_workload.R` is one workload with 13 sections. The rounds per section are set so that
each section has the share of GNU R's time that this kind of code has in the suite. Score it by
its total time. It needs the packages data.table, xts, fastmap and testthat.

```sh
Rscript bench/app_workload.R      # or: ry run bench/app_workload.R
RY=ry RSCRIPT=Rscript bench/run.sh
```

| Section | GNU R s | ry s | ry + summary s | GNU R / ry |
|---|---:|---:|---:|---:|
| bodies (plain functions and checks) | 3.23 | 1.00 | 1.33 | 3.23 |
| objects (S4) | 2.36 | 0.73 | 1.01 | 3.24 |
| functionals | 2.09 | 0.74 | 1.00 | 2.82 |
| native (large inputs, native package code) | 2.02 | 2.10 | 1.82 | 0.96 |
| keys (strings) | 1.88 | 0.53 | 0.56 | 3.59 |
| tables (small data.table objects) | 1.77 | 1.06 | 1.25 | 1.67 |
| series (small xts objects) | 1.65 | 0.68 | 0.89 | 2.43 |
| dates | 1.39 | 0.45 | 0.60 | 3.11 |
| conditions | 1.23 | 0.74 | 0.92 | 1.66 |
| closures | 1.12 | 0.41 | 0.61 | 2.76 |
| expectations (testthat) | 1.03 | 0.41 | 0.36 | 2.49 |
| frames (data.frame) | 0.71 | 0.31 | 0.43 | 2.29 |
| storage (serialize) | 0.40 | 0.15 | 0.15 | 2.73 |
| **total** | **20.88** | **9.31** | **10.93** | **2.24** |

With `RY_JIT=off` the total is 8.89 s. ry's time split for the workload is close to its split for
the suite: tier 0 37.5% (suite 35.9%), builtins 29.6% (30.1%), call setup 13.9% (12.7%), native
code 8.5% (10.4%), garbage collection 3.2% (3.2%).

Three more numbers to track:

- The cost of the profile summary on the workload (`bench/run.sh`). It is 17% now.
- The number of patterns in `repro/` below 1.5 times (32 of 236) and below 1.0 times (7).
- The share of the workload's time in tier 1. It is 0.0% now.

The timings were taken on a shared machine. Single timings vary by up to 1.5 times between runs.
