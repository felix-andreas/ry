---
title: The big levers
description: "An independent review of where ry's largest wins are: five ranked levers with the evidence from other language implementations, what to stop, where the runtime tracks are aimed wrong, and the cheapest measurement that confirms or kills each lever"
---

This draft is a critical review of ry's plan, written from outside the tracks that carry it out. It
ranks the few changes that could move ry by an order of magnitude, names the work that cannot, and
says what to measure before committing to any of them. It is an opinion backed by measurements and
citations, not a settled design, and it disagrees with `decisions.md` and `r-runtime.md` in places.

## The short version

No system that keeps R's semantics and R's C API has made real R code more than about 2 times
faster than GNU R, and ry is already there (1.9x on package test suites, 2.0x on application
workloads). The roadmap's 10x target for untyped R has no precedent and does not survive its own
arithmetic. The order-of-magnitude factors that ry has actually measured are somewhere else:

| Where | Measured factor | Status in the plan |
| --- | --- | --- |
| Typed ry against GNU R | 15x on kernels, 20x on five applications | One track among several, scored against GNU R instead of Rust |
| Edit to first test result, `ry test --watch` against `devtools::test()` | about 30x to 60x (2 s against 62 s, 1 s against 68 s, one code base) | Shipped, unpublished, not a benchmark suite |
| A call the compiler removes entirely | 37x to 68x (probes below) | Reaches under 3% of R-level time |
| A base R closure replaced by a native entry | 3.4x to 6.9x per function (probes below) | Ported one function at a time |
| Everything that keeps a real R call | 0.9x to 1.5x (probes below) | Where most of the runtime effort goes |

The ranked levers follow from that table:

1. **Make declared types pay on whole programs**, compiled ahead of time, with "this function runs
   natively" as a property the checker reports. Expected: 3x to 10x on the typed share of a program.
2. **Make the edit-to-result loop the headline benchmark and the product.** Expected: 10x to 60x on
   the time a person or an agent waits per change, and 2x or more on CI.
3. **Give base R's data-frame and object layer native kernels**, chosen by measured share. Expected:
   10x or more on those calls; the effect on a suite depends on a share nobody has measured.
4. **Build one optimizing tier that removes calls, and judge it by what it removes.** Expected: 1.3x
   to 1.5x on package and application code, which would put untyped R near 3x GNU R. This is the
   right track with the wrong expectations.
5. **Decide whether ry-owned kernels may use all cores and plain IEEE arithmetic.** Expected: 2x to
   4x on large vector work on four cores, nothing on test suites. Both are user directives, so this
   is a question, not a plan.

## What this rests on

Three kinds of evidence, in decreasing order of trust:

- **Published results of other systems**, cited inline with links.
- **ry's own records**: the results files behind `benchmarks.mdx`, `decisions.md`, `backlog.md` and
  `MEMORY.md`.
- **Measurements made for this review** on the release binary `nightly.20261110.1955.a15ce0`
  (ry 0.4.3-beta, profile-guided and BOLT-laid-out) against GNU R 4.6.2 on a shared 4-core Xeon
  cloud VM. They are single sessions with best-of-three timing, so read differences under 10% as
  noise. The binary is two days older than the tree this draft sits in, so each number describes
  that binary. Anything not measured is marked as an estimate.

### Probes: where ry is fast and where it is level with GNU R

`tools/research/big_levers_probes.R` times 55 small constructs, each warmed and then timed as the
best of three. The table shows a selection; the ratio is GNU R's time over ry's, so above 1 means
ry is faster. Checksums agree on every probe.

| Construct | GNU R | ry | ry, `RY_JIT=off` | Ratio (JIT on) |
| --- | ---: | ---: | ---: | ---: |
| **The compiler removes the call** | | | | |
| `f(x)` on a tiny leaf function | 292 ns | 4.3 ns | 298 ns | 68 |
| recursive `fib(24)`, per call | 308 ns | 8.3 ns | 283 ns | 37 |
| `x[i] <- i` in a loop | 33 ns | 7.4 ns | 83 ns | 4.5 |
| **A native entry replaces a base R closure** | | | | |
| `structure(list(...), class =)` | 4.3 µs | 0.62 µs | 0.63 µs | 6.9 |
| `Map(f, a, b)`, per item | 1.22 µs | 0.25 µs | 0.28 µs | 4.8 |
| `match.arg()` | 5.8 µs | 1.65 µs | 1.34 µs | 3.5 |
| `stopifnot()` with two conditions | 3.4 µs | 1.0 µs | 0.84 µs | 3.4 |
| **The call keeps R's semantics** | | | | |
| call with named and default arguments | 461 ns | 222 ns | 205 ns | 2.1 |
| call that forwards `...` | 772 ns | 861 ns | 693 ns | 0.90 |
| call that tests `missing()` | 352 ns | 378 ns | 316 ns | 0.93 |
| method on an environment object, `obj$add(1)` | 622 ns | 657 ns | 676 ns | 0.95 |
| `l[[length(l) + 1]] <- i` | 615 ns | 625 ns | 585 ns | 0.98 |
| `on.exit()` | 386 ns | 365 ns | 391 ns | 1.06 |
| `tryCatch(expr, error =)` with no error | 5.3 µs | 4.9 µs | 4.8 µs | 1.09 |
| S4 generic call with slot reads | 1.23 µs | 0.90 µs | 0.89 µs | 1.37 |
| **Base R closures over data frames** | | | | |
| `aggregate(x ~ g)` on 1,000 rows | 677 µs | 643 µs | 580 µs | 1.05 |
| `merge(df, df)` on 100 rows | 259 µs | 231 µs | 253 µs | 1.12 |
| `rbind(df, df)` | 64 µs | 56 µs | 56 µs | 1.15 |
| `df$col <- value` | 4.9 µs | 4.1 µs | 4.2 µs | 1.19 |
| `df[df$a > 5, ]` on 10 rows | 25.6 µs | 19.8 µs | 19.3 µs | 1.29 |
| `data.frame()` with 3 columns, 10 rows | 151 µs | 105 µs | 99 µs | 1.44 |
| **Vector kernels on 1e7 elements, per element** | | | | |
| `sum(x * y + x)` | 8.1 ns | 3.2 ns | 2.9 ns | 2.5 |
| `match()` plus `unique()` on integers | 23.3 ns | 10.1 ns | 9.7 ns | 2.3 |
| `cumsum()` plus `diff()` | 25.5 ns | 16.9 ns | 17.4 ns | 1.5 |
| `sort(x)` on doubles | 80 ns | 66 ns | 61 ns | 1.2 |
| `ifelse(x > y, x, y)` | 51 ns | 42 ns | 42 ns | 1.2 |
| `exp(x) + log(y)` | 22.3 ns | 19.4 ns | 18.9 ns | 1.15 |
| `mean`, `var` and `sd` | 4.9 ns | 4.6 ns | 4.5 ns | 1.07 |
| `rowsum(x, g)` | 22.7 ns | 23.4 ns | 24.2 ns | 0.97 |
| `order(g)` on integers | 6.8 ns | 8.8 ns | 9.2 ns | 0.77 |

Three readings matter for the rest of this page.

The geometric mean over all 55 probes is 1.93x, the same figure the published application and
package suites give, so the probes are a fair miniature. Twenty-seven of the 55 are under 1.5x.

The JIT decides three probes and nothing else. With those three removed, the geometric mean is
1.67x with the JIT on and 1.74x with it off, and the interpreter alone is faster on 23 probes
against 13. The backlog records the same thing on the application suite ("JIT on runs about 1% more
instructions than JIT off"). ry's 2x over GNU R comes from tier 0, the native entries and the
kernels, not from compiled code.

The factor tracks what was removed, not what was compiled. Where a whole call disappears, ry is 37
to 68 times faster. Where a native entry replaces a few dozen interpreted calls, it is 3 to 7 times
faster. Where the call survives with its environment, promises and argument matching, ry is level
with GNU R whether or not the body is machine code.

### A code base's test suite

`tools/bench/tooling/generate.py --lines 30000 --files 50` wrote a finance code base of 30,800
lines, and its 953 `test_that()` blocks ran under both engines through `ry load --file` with the
driver `oracle bench` uses. The package library in the measuring container held 119 of the
benchmark closure's packages, so 391 blocks fail on GNU R for a missing package and 82 more fail on
ry because `quadprog`'s shared object did not find `libRblas.so` there. The comparison below covers
the 480 blocks that pass on both, and it is a partial run of a small code base, not the benchmark.

| | GNU R 4.6.2 | ry | ry, `RY_JIT=off` |
| --- | ---: | ---: | ---: |
| Time in the 480 blocks (two or three runs) | 4.74 s, 4.80 s | 2.84 s, 2.90 s, 3.02 s | 2.90 s |
| Median per-block ratio | | 1.5x | |

ry runs those blocks 1.67 times faster, every test family lies between 1.4x and 2.1x, and the JIT
changes nothing. In 24 of the 480 blocks the two engines return different values, which this review
did not investigate. `ry load --profile-summary` splits ry's whole run as follows:

| Activity | Share |
| --- | ---: |
| Tier-0 bytecode instructions | 45.0% |
| Builtins | 27.0% |
| Closure call setup | 13.2% |
| Native code (`.Call`, `.External`, `.C`) | 5.0% |
| AST evaluator | 3.0% |
| Garbage collection | 1.9% |
| S3 method lookup | 1.9% |
| Compiled code | not listed, so under 0.1% |

`MEMORY.md` records the full-size code bases as 1.21x to 1.35x slower than GNU R per test, and
`backlog.md` records a private 300,000-line code base as about 3% slower. This run disagrees with
both, and nothing in the repository says which is current ("Blind spots" below).

### Typed ry against Rust

The typed runtime's bar is 1.5 to 2 times the time of equivalent Rust (`typed-runtime.md`), and
`tools/climb/run.py` computes the score, but `tools/climb/log.tsv` has never recorded one. Two
estimates:

- Dividing the published typed times (`tools/bench/kernels/results.json` and
  `tools/bench/applications/results.json`) by the Rust twins' times measured here gives a geometric
  mean of about 4.7x over 17 programs. Five programs are within 2x (`loop_sum`, `closures`,
  `drawdown`, `array_dot`, `report`), and five are 10x or more away (`vasicek`, `expr_tree`,
  `orders`, `sales` at about 31x, `log_rank` at about 60x). The two sets of times come from
  different sessions, and the typed times are a few milliseconds on a clock with 1 ms resolution,
  so this is an estimate.
- Running `tools/climb/run.py --only speed` directly on the nightly binary reports 36.75x. On that
  binary the warm repeats of several typed programs leave the native path: typed `array_dot` takes
  0.006 s on its first run and a median of 2.06 s on the five repeats, against 0.031 s untyped.
  The binary is older than the harness, so this may be version skew and is unverified on the
  current tree. It still shows the failure mode: a typed program's speed depends on whether each
  call site reaches the native entry, and nothing reports when one does not.

## The ceiling on untyped R

`r-runtime.md` sets the target: "Untyped R with the JIT on runs at 10 times GNU R's speed on one
thread", with every track designing for a 10x row (R code 30x, builtins 10x). The published record
of every comparable system says the ceiling on real code is about 2x.

| System | What it does | Result on real code | Source |
| --- | --- | --- | --- |
| Ř | Speculative inlining, deoptimization, environment and promise elision, contextual dispatch | 1.80x over GNU R 4.1 on real programs (0.74x to 18.3x), 3.23x on are-we-fast-yet, 1.70x on shootout; the first in-process iteration runs at 0.37x to 0.50x | [Flückiger's thesis, 2022](https://janvitek.org/pubs/fluckiger-phd-22.pdf) |
| Ř | The same, measured earlier | "ranges from a 0.7× slowdown to a 46× speedup, with the overall speedup being 1.7×" over 59 programs | [OOPSLA 2020](https://janvitek.org/pubs/oopsla20-cd.pdf) |
| rcp | Copy-and-patch compilation of GNU R's bytecode | 1.26x geometric mean, 1.23x on its real programs | [VMIL 2025](https://ch-rigid.github.io/publications/kocourek_copyandpatch_2025.pdf) |
| GNU R's own compiler | Bytecode over the AST interpreter | 5.7x to 7.1x on scalar loops, but "Function calls use the (slow) interpreter mechanism" | [Tierney, 2019](https://stat.uiowa.edu/~luke/talks/Riot-2019.pdf) |
| Purdue FastR | A Java AST interpreter with specialization | 8.5x on shootout, 1.7x on Benchmark 25, "explained by the preponderance of native calls" | [VEE 2014](https://janvitek.org/pubs/vee14.pdf) |
| pqR | Deferred evaluation and merged vector tasks | 1.5x to 2.0x on merged tasks, 0.50x to 1.39x from helper threads | [Neal, 2013](https://radfordneal.wordpress.com/2013/06/22/announcing-pqr-a-faster-version-of-r/) |
| YJIT | A production JIT for Ruby, which also keeps a C extension API | 15% on Shopify's storefront in production, against about 65% on railsbench for the same release | [Rails at Scale, 2023](https://railsatscale.com/2023-09-18-ruby-3-3-s-yjit-runs-shopify-s-production-code-15-faster/) |
| CPython 3.11 | A specializing interpreter, no JIT | "an average of 25% faster" | [What's new in 3.11](https://docs.python.org/3/whatsnew/3.11.html) |
| CPython's JIT | Copy-and-patch above that interpreter | "about as fast as the existing specializing interpreter" | [PEP 744](https://peps.python.org/pep-0744/) |
| PyPy | A tracing JIT that emulates the C API | About 4x on pure Python, but C extensions run "2.5x to 10x slower" through the emulation | [pypy.org](https://pypy.org/), [cpyext post](https://www.pypy.org/posts/2018/09/inside-cpyext-why-emulating-cpython-c-8083064623681286567.html) |

Ř is the closest precedent for ry's inlining tier: the same ideas, built by a research group over
about six years, with no obligation to keep every package's C code working. It reached 1.8x on real
programs. The rcp authors state the reason in one sentence: "significant performance improvements
require high-level optimizations: inlining, type speculation, and operation elision. Removing
explicit callee environments, skipping promise creation, and avoiding copies provide the largest
performance gains."

The roadmap's own shares give the same answer. Its time-weighted corpus profile is R code 53%,
builtins 23%, package native code 5%, I/O 0.7% and waiting 9.5%, and the last three are fixed by
its own account (package C code is single-threaded by contract, and the waiting is the suites' own
`Sys.sleep`). Take GNU R's time as 100. At the published 1.91x, ry's time is 52, of which about 8
is fixed. Reaching 3x means the other 44 shrink to 25, a factor of 1.75 on R code, builtins, GC and
start-up together. Reaching 10x means they shrink to 2, a factor of 22 on all of them together.
That is an estimate, because the corpus profile and the published package suite cover different
package sets, but no reading of it makes 10x reachable by speeding up the buckets ry owns.

So this review recommends replacing the target, not chasing it:

- **Untyped R: 3x GNU R is the stretch goal, and 2.5x is success.** Drop the rule in "The road to
  3x" that "a plan that can only reach 3x in its bucket is rewritten before it lands". It forces
  every bucket to be designed for a result that the whole cannot reach.
- **Order-of-magnitude goals move to the places that have shown one**: typed code and the
  development loop.

This also answers what "a JIT on the level of Bun" should mean. Bun's JIT is JavaScriptCore, which
Bun did not write. What Bun added is start-up ("Bun processes start 4x faster than Node.js",
[bun.com/docs](https://bun.com/docs)), builtins in a systems language, and one integrated tool for
install, test and build. `r-runtime.md` already says so in "The performance bar". ry's equivalents
of Bun's actual contribution are the lazy loader, `ry test`, the start-up image and the native
base, and they are where ry already leads by the widest margin.

## The levers

### 1. Declared types that pay on whole programs

**What it is.** Typed ry is the only place where ry has measured an order of magnitude on user
code: 15.2x over GNU R on kernels and 20.5x on five applications, which is 3x to 10x over untyped
ry on the same programs. Yet on the benchmark code bases, whose templates carry inline annotations,
`backlog.md` records typed and untyped running the same number of instructions (71.5G against
72.1G on a finbench slice), because their functions take data frames, vectors and lists, and the
typed core compiles closures over ry's own types and scalars. The feature the mandate is built
around buys nothing on the mandate's benchmark. Closing that gap takes four changes:

- **Compile typed code ahead of time, from the checker's types.** `typed-runtime.md` designs it,
  and `backlog.md` files it as "mid priority". Today the typed core lowers R syntax at a closure's
  first call. Ahead-of-time compilation at `ry load`, `ry install` and `ry build --exe` removes
  warm-up and makes the next item possible.
- **Report native compilation statically.** A typed function either compiles or `ry check` says
  why not, at the construct that blocks it, in the editor. Today a refusal is a line in
  `RY_TYPED_LOG` at run time, one refusal spreads to its callees, and a call from untyped code with
  an unforced argument silently takes the interpreter ("Open: the typed core"). mypyc works this
  way: a module compiles or the build fails. No JIT can offer this, and ry has the checker to do it.
- **Let typed numerics leave R's.** `List.sum` over `F64` uses the long-double fold "to match the
  interpreter". The user directive makes typed ry "independent of R's semantics", so typed sums can
  be plain IEEE doubles, vectorized.
- **Score against Rust, on every step.** The climb's fitness function is typed ry over Rust, and
  its log holds no value for it. The estimate above is about 4.7x, with five of 17 programs within
  the 2x bar.

**Who shows it works.** [mypyc](https://mypyc.readthedocs.io/en/latest/introduction.html) compiles
type-annotated Python ahead of time for 1.5x to 5x, and mypy compiled with it is 4x faster. Cinder's
Static Python with its JIT is 18x faster on a typed Richards. The same Static Python gave
Instagram's web tier [3.7% more requests per second](https://programming-journal.org/2023/7/2/),
because production code is partly typed and crosses the boundary constantly, and sound gradual
typing has measured boundary overheads of up to
[105x](https://www2.ccs.neu.edu/racket/pubs/popl16-tfgnvf.pdf). The spread between 18x and 3.7% is
the lesson: the factor arrives when whole call graphs are typed and compiled together, and
disappears when typed islands sit in untyped code. Julia shows the end state for a dynamic-looking
numeric language: 1x to 3.5x of C on its
[microbenchmarks](https://julialang.github.io/Microbenchmarks/dev/benchmarks/), where R is 16x to
466x.

**Expected effect.** 3x to 10x on the typed share of a program, by ry's own measurements. On a
whole suite the share decides: if half of a run's time is in typed code that gets 10 times faster,
the run is 1.8 times faster, and at 90% it is 5.3 times faster. The order of magnitude therefore
needs whole modules typed, which makes the migration tooling (`ry migrate`, and the checker
reporting what blocks compilation) part of the lever.

**Cost.** Large: the ahead-of-time pipeline, the static refusal report, and typed versions of the
code bases' hot modules written in ry's own types.

**Depends on.** The typed core's open items (`sales` and `vasicek` do not run natively, boxes
convert deeply at the boundary: typed `expr_tree` spends 46 of 48 ms converting).

**Biggest risk.** Adoption. A team must rewrite modules into ry's types to see the gain, and the
boundary can eat it. If agents write most of the code, as in this repository, the rewrite is cheap
and the risk shrinks.

### 2. The edit-to-result loop as the headline

**What it is.** `ry test --watch` reruns the test files an edit can affect, against a lazily loaded
package. `MEMORY.md` records it on one code base: about 2 s to the first test file against 62 s for
`devtools::test()`, and about 1 s per edit against 68 s. Lazy `ry load` takes 0.33 to 0.41 s on an
S4-heavy package against about 6.5 s for `load_all`. These are the largest factors ry has, they
need the loader, the dependency graph and the runtime in one tool, and no other R implementation
can copy them by writing a better JIT. They are also not a published suite, not on the landing
numbers, and not a track. Making them the headline means:

- **Publish it as a fifth suite** in `benchmarks.mdx`: time from a one-line edit to the first
  affected test result, and to the full affected set, on the three code bases, against
  `devtools::test()`.
- **Select by `test_that()` block, not by file**, since `Loader::callers` already knows which
  functions a block reaches.
- **Offer the same selection to CI**: `ry test --changed-since REF` runs the blocks a diff can
  affect. The dependency graph makes this selection exact for static calls, where the published
  alternative is statistical.
- **Count `ry check` in the same loop.** A type error reported in 0.1 s on a 300,000-line package
  replaces a test run entirely.

**Who shows it works.** Bun, as argued above. Meta's
[predictive test selection](https://arxiv.org/abs/1810.05286) "reduces the total infrastructure
cost of testing code changes by a factor of two" while reporting over 95% of individual test
failures. On the other side, the systems that bet on a warm JIT lose this loop: Ř's first iteration
runs at 0.37x to 0.50x of GNU R, and on an RMarkdown workload
"[Ř take 405 seconds](https://janvitek.org/pubs/oopsla23.pdf) ... and the interpreter 85 seconds".
HHVM needed [Jump-Start](https://research.facebook.com/publications/hhvm-jump-start-boosting-both-warmup-and-steady-state-performance-at-scale/)
to cut the capacity lost to warm-up by 54.9%. Test suites are cold code, which the code-base
profile above confirms: compiled code is under 0.1% of the run.

**Expected effect.** 10x to 60x on the wait per edit (measured once, on one code base), and an
estimated 2x to 10x on CI time per change, depending on how local a typical change is.

**Cost.** Small to medium. The mechanism exists; the work is block-level selection, the CI entry
point, the benchmark and the reporting.

**Depends on.** Nothing in the runtime tracks.

**Biggest risk.** Unsound selection. A test that reaches a changed function through `do.call`,
`get`, S4 dispatch or a data file must still run, so the selection needs a conservative fallback
and a periodic full run, and its miss rate must be measured.

### 3. Native kernels for base R's data-frame and object layer

**What it is.** The probes show where untyped ry has the most room: base R's own closures over data
frames and objects cost tens to hundreds of microseconds per call on GNU R, and ry runs them 1.05x
to 1.5x faster, because it interprets the same R code. A 10-row filter through `[.data.frame` takes
20 µs, `merge` on 100 rows 231 µs, `data.frame()` 105 µs, `S4 new()` 55 µs. Where ry replaced a
base closure with a native entry, the factor is 3.4x to 6.9x (`structure`, `match.arg`,
`stopifnot`, `Map`).

ry ships its own base R, so these functions are ry's code. The lever is to treat the data-frame
family (`data.frame`, `[.data.frame`, `[[<-.data.frame`, `rbind`, `merge`, `aggregate`, `order`,
`split`, `tapply`, `format`) and the object layer (`new`, `initialize`, `validObject`, `tryCatch`,
`withCallingHandlers`) as a kernel layer with one design, the way "One kernel layer for R's vector
builtins" treats arithmetic: a native implementation behind the existing hash guard, taken only for
a plain `data.frame` with atomic columns, with the R code as the fallback for every subclass.

This differs from the current one-function prologue ports in how the functions are chosen. Today a
port follows a hotspot in one workload and lands a percent-level win. The kernel layer starts from
a ranked list of base closures by inclusive time across the code bases and the corpus, and takes
the top of it as one project.

**Who shows it works.** ry's own native entries, at 3x to 7x per function. Morandat and others
found GNU R spending 38% of its time in builtins and 29% in memory management
([ECOOP 2012](https://janvitek.org/pubs/ecoop12.pdf)), and Purdue FastR's 8.5x on shootout fell to
1.7x on a suite dominated by library calls. Bun's second leg is the same idea: hot builtins in a
systems language instead of in the guest language.

**Expected effect.** An estimated 10x to 50x on the calls themselves (a 10-row filter should cost
under 1 µs). The effect on a suite is unknown, because `--profile-summary` reports tier-0 time
without saying whose code it ran. At a 10% share the suite gains 1.1x, at 30% it gains 1.4x.

**Cost.** Medium: twenty to thirty functions with exact fallbacks, pinned by the oracle.

**Depends on.** A profile split by owning namespace ("What to measure first").

**Biggest risk.** Fidelity. `[.data.frame` has many corners (`drop`, row names, duplicate names,
matrix columns), and a subclass such as a tibble or a data.table must never take the native path.
The directive that data.table is a normal package bounds this lever: it covers base data frames
only.

### 4. One optimizing tier that removes calls

**What it is.** Track A (slot frames, a native calling convention between locked-namespace
closures, unboxed scalars, whole-package install-time compilation) and the inlining tier in "The
road to 3x" are one project, and it is the right one. The probes, Ř and rcp all say the same: the
gain is in the calls, environments, promises and dispatches that stop existing.

Two parts of the current plan are aimed wrong.

The success metric is "at least 80% of closures native in rlang, cli and testthat". Native is not
the same as fast. ry's own install-time baseline code ran 8.06G instructions against 7.96G without
it, V8's Sparkplug gains [5% to 15%](https://v8.dev/blog/sparkplug), rcp gains 1.26x, and CPython's
JIT gains nothing over its interpreter. A closure compiled to machine code that still builds an
environment and promises runs at the interpreter's speed, as every row in the third group of the
probe table shows. Judge the track by what a call no longer does, measured on the corpus profile:
objects allocated per call (about 8 today on the testthat workload), promises per call (1.30),
instructions per call (about 2,100 for call setup against a 200 target), and the share of dynamic
calls that run inlined.

The inlining tier itself, with a deoptimization exit that rebuilds the callee's frame, is "not
queued; starts only on the orchestrator's go" in `backlog.md`, behind the contract work. It is the
only part of the JIT plan with a precedent for more than a few percent on call-heavy code, so it
should be the track's goal, not its sequel.

**Who shows it works, and how far.** Ř, at 1.8x on real code with all of this built.
JavaScriptCore's own ladder shows the tapering: baseline about 2x the interpreter, DFG about 5x
baseline, FTL about 1.5x DFG on long-running code and
[1.1x on JetStream 2](https://webkit.org/blog/10308/speculation-in-javascriptcore/). V8's
optimizing compiler is 4.35x over its interpreter on JetStream and
["over 1.5x" on Speedometer](https://v8.dev/blog/maglev), the suite that resembles application
code.

**Expected effect.** 1.3x to 1.5x on package and application suites, taking untyped R from about 2x
to between 2.5x and 3x GNU R. On cold test code the gain comes only from the install-time part,
because nothing warms up: in the code-base run above, the JIT contributes nothing. Track A's own
target (testthat 30% faster) agrees with this estimate. The 5x and 30x figures in the roadmap's
R-code rows do not.

**Cost.** Very large; it is the bulk of the runtime effort for months.

**Depends on.** Slot frames and the enter and leave contract, which are in progress.

**Biggest risk.** Deoptimization correctness under R's reflection (`sys.call`, `parent.frame`,
`substitute`, `on.exit`), and spending the effort on a result bounded at 1.5x while the two larger
levers wait.

### 5. All cores and plain IEEE arithmetic for ry-owned kernels

**What it is.** Two settled decisions cap the vector kernels. "Single-thread first" defers
parallelism, and results are bit-identical to GNU R's, which forces `sum` through an 80-bit
long-double fold and forbids fused multiply-add and reassociation. The vector probes show the cap:
the geometric mean over twelve kernels is 1.56x, and five of them are within 15% of GNU R.

The reasoning for single-thread first is that users run test suites with one process per core, so
the cores are busy. That is true of test suites and false of the other thing these code bases do: a
risk batch, a simulation or a model fit runs as one process with idle cores. Element-wise maps
(`exp`, `log`, the distribution functions, comparisons, `ifelse`) can be split across threads with
bit-identical results, because no element depends on another. `DataFrame` already runs on Polars'
thread pool. A separate opt-in numeric mode could allow vectorized sums for code that accepts a
last-digit difference.

**Who shows it works.** [Riposte](https://tableau.com/sites/default/files/2023-01/pact2012-talbot-riposte.pdf)
ran vector R code a mean of 14.4x faster on one thread through fusion and 55x to 670x faster on
32 threads, with no C API to preserve. The counter-evidence is pqR, whose automatic helper threads
gave 0.50x to 1.39x. Polars and DuckDB show that the table engines that win
[db-benchmark](https://duckdblabs.github.io/db-benchmark/) are the parallel ones.

**Expected effect.** An estimated 2x to 4x on four cores for element-wise kernels above about 1e5
elements, and nothing on test suites.

**Cost.** Medium for parallel element-wise kernels behind a size threshold. The numeric mode is
small in code and large in testing.

**Depends on.** A user decision, since both constraints are user directives or rest on one.

**Biggest risk.** Oversubscription when a user already runs one process per core, which a
threshold and a worker-count check must prevent, and a second numeric mode that doubles the oracle
matrix.

## What to stop or demote

- **`backlog.md`, "Open: performance and memory": "Tier-0 call entry and dispatch", "Builtin call
  shape" and "Queued with measured ceilings"**, and the rows of "Open: data-oriented layouts and
  explicit allocators". Each has a measured ceiling of 3.5% or less. Close them as not worth
  doing, instead of holding them.
- **Sub-tracks B2 to B6 as separate efforts** (S3 dispatch, `match.arg`, top-level `for` loops, xts
  and zoo). Each lands a percent-level instruction win on one workload. Fold what remains into
  lever 3, chosen from a ranked list. The top-level loop is the exception: 2.55 s against 0.30 s
  is a visible defect in a first script, and compiling top-level code fixes it once.
- **The baseline tier** ("Open: the baseline tier and JIT on against JIT off"). It is opt-in, it
  runs 1% to 2% more instructions than the interpreter, and each execution path multiplies the
  oracle's configurations (six gate the release). Delete it, and keep three paths: tier 0, one
  optimizing tier with install-time images, and the typed core.
- **Fitting the 1e8-row db-benchmark joins into 15 GB** ("Open: DataFrame", top priority). The
  mandate is to outperform data.table, and `MEMORY.md` records that: 5.2 s against 27.3 s at 1e7
  rows and 22.4 s against 69.2 s on the 1e8 group-by. data.table does not fit the 1e8 joins either.
  If the size matters, the structural answer is Polars' streaming engine
  ("[up to 3-7x faster](https://pola.rs/posts/benchmarks/)" and out of core), not a custom
  allocator for the reader's transients. DuckDB is the only engine that completes the 50 GB joins
  on the public benchmark, and it does so on disk.
- **Typed ports of the public JIT programs as a goal.** Those programs were chosen by R compiler
  researchers to stress untyped R. They found real gaps, listed in "Gaps the typed public ports
  found", and that list is worth finishing. The typed column on that suite is not a measure of
  typed ry; the distance to Rust on programs written for typed ry is.
- **The 10x rows of the roadmap table** in `r-runtime.md` and the rule that every plan must reach
  them.
- **Tuning the JIT's thresholds and feedback recording.** JIT on against JIT off is within 2% on
  every call-heavy measurement in the repository and in this review. Nothing in that range changes
  the picture until the tier removes calls.

The backlog's proportions are worth a look as well. By lines, about 35% is open checker, editor,
documentation and CLI items, 15% is fuzzing and test architecture, 15% is runtime performance, 6%
is typed ry and 3% is `DataFrame`. The two areas with measured order-of-magnitude factors hold
under a tenth of the planning.

## Where the current tracks are right

- **The ABI-compatible `libR`.** PyPy and FastR lost their ecosystems at the native boundary. ry's
  code-base run spends 5% of its time in package native code across 293,000 `.Call`s. This was the
  decision that mattered most, and it was made first.
- **Track A's design**: slot frames, the enter and leave contract, frame-state maps and
  reification on demand are the prerequisites that Ř and rcp identify. Only its metric and its
  expectations need changing.
- **The vector kernel layer** as one module with slice kernels and a reuse rule. Builtins are 27% of
  the code-base run.
- **Lazy loading and `ry test`**, which produced the largest factor in the project.
- **`DataFrame` on Polars**, which met its mandate.
- **The testthat workload as the runtime's primary metric.** Its profile (tier 0 33%, builtins 26%,
  call setup 25%, compiled code 0.7%) is close to the code-base run's (45%, 27%, 13%, under 0.1%),
  so it is a fair stand-in for shares, though the code bases are colder.
- **The differential oracle as the release gate.** Compatibility is the product. Its cost is the
  number of engine configurations, which deleting the baseline tier reduces.

## What to measure first

Each lever has one cheap measurement that could kill it.

| Lever | Measurement | Kill or reshape if |
| --- | --- | --- |
| 1. Typed programs | Build the current tree and run `just climb --log`, so the typed-over-Rust score exists. Then run the code bases' tests with `RY_TYPED_LOG` and count typed closures compiled against refused, by reason | Fewer than 20% of the code bases' annotated closures can compile without being rewritten into ry's types. Then lever 1 is a migration project before it is a compiler project |
| 2. Edit loop | Replay 200 random one-function edits on each code base and record the share of test blocks selected and the time to the first result, against `devtools::test()` | The median edit selects more than half the blocks, which would mean the call graph is too connected for selection to pay |
| 3. Base kernels | Split tier-0 and call-setup time in `--profile-summary` by owning namespace (base and recommended, testthat with rlang, cli and withr, other packages, the user's package), and list the 50 base closures with the most inclusive time | The data-frame and object families are under 5% of every code base and of the corpus |
| 4. Call removal | Flatten the five hottest call chains of the testthat workload by hand in R source, and time the workload. This gives the inlining tier's upper bound without building it | The flattened workload gains less than 1.3x |
| 5. Cores | Time `exp`, `log` and `pnorm` kernels on one and four threads at 1e5 to 1e7 elements with a bit-identity check, and take the kernel share of one production-shaped batch run | The share of large element-wise kernels in a batch run is under 10% |

One more measurement comes before all of them: run the three full-size code bases on a release
build and publish the result as a suite. It is the benchmark `AGENTS.md` defines, and the
repository has no current number for it.

## Blind spots

- **The mandate's benchmark has no scoreboard.** `benchmarks.mdx` publishes four suites, and none
  is the three code bases. The recorded numbers (1.21x to 1.35x slower per test, and about 3%
  slower on a private code base) and this review's partial run (1.67x faster on a 30,000-line
  code base) cannot both describe the current tree.
- **The benchmarks are ry's own writing.** The code bases are generated from templates, and the
  kernels and applications are written in-house. One user's profile summary is the only outside
  data point in `r-runtime.md`. The targets should lean on more of those, because a generated code
  base rewards whatever its templates happen to exercise.
- **The reader is increasingly an agent.** An agent runs the check and the tests hundreds of times
  per task, so its throughput is bounded by the loop in lever 2, and it can do the rewrite lever 1
  asks for. Neither the plan nor the benchmarks model that user.
- **Typed speed is not observable.** Nothing tells a user, or the climb, when a typed call fell
  back to the interpreter. The nightly run above lost a factor of 300 on one program without any
  message.
- **Bit-identical numerics is a standing cost that nobody totals.** It decides the sum kernels, the
  fused loops and the software long-double path on targets without x87. Its total over the kernel
  suite is unmeasured, and so is the number of users who need the last digit.
- **Platforms.** `backlog.md` says the base build has run on Linux only. Every number on this page
  and in `benchmarks.mdx` is Linux x86-64.
- **Twenty-four differing values.** In the code-base run, 24 of 480 passing blocks returned
  different values on the two engines. The cause may be the incomplete library or the older binary,
  and it deserves one look, because `MEMORY.md` records a 100% match at full size.
