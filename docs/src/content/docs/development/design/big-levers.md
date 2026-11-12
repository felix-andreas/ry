---
title: The big levers
description: "An independent review of where ry's largest wins are: five ranked levers with the evidence from other language implementations, what to stop, where the runtime tracks are aimed wrong, and the cheapest measurement that confirms or kills each lever"
---

This draft is a critical review of ry's plan, written from outside the tracks that carry it out. It
ranks the few changes that could move ry by an order of magnitude, names the work that cannot, and
says what to measure before committing to any of them. It is an opinion backed by measurements and
citations, not a settled design, and it disagrees with `decisions.md` and `r-runtime.md` in places.

## The short version

On package and application code, no system that keeps R's semantics and R's C API has run more
than about 2 times faster than GNU R, and ry is already there (1.9x on package test suites, 2.0x on
application workloads). The roadmap's 10x target for untyped R needs R code to run 30 times faster
and builtins 10 times faster than GNU R's, averaged over all the time in those buckets. No
published system has shown either factor on application-shaped code, and ry's own suites of
loops that the JIT compiles stand at 2.7x and 4.8x. The order-of-magnitude factors that ry has
measured are somewhere else:

| Where | Measured factor | Status in the plan |
| --- | --- | --- |
| Typed ry against GNU R | 15x on kernels, 20x on five applications (typed times under the suites' 0.1 s floor) | One track among several, with no recorded score against Rust |
| Edit to first test result, `ry test --watch` against `devtools::test()` | about 30x to 70x (2 s against 62 s, 1 s against 68 s, one code base) | Shipped, not a benchmark suite |
| A call the compiler removes entirely | 37x to 68x (probes below) | Tier 1 runs under 3% of R-level time on eight of nine workloads |
| A base R closure replaced by a native entry | 3.4x to 6.9x per function (probes below) | Ported one function at a time |
| A call that keeps R's semantics | 0.9x to 1.5x (probes below) | What the native execution model sets out to change |

The ranked levers follow from that table:

1. **Make declared types pay on whole programs**, compiled ahead of time, with "this function runs
   natively" as a property the checker enforces. Expected: 3x to 10x on the typed share of a
   program.
2. **Make the edit-to-result loop the headline benchmark and the product.** Expected: 10x to 70x on
   the time a person or an agent waits per change, and an estimated 2x or more on CI.
3. **Build one optimizing tier that removes calls, and judge it by what it removes.** Expected: 1.3x
   to 1.5x on package and application code, which would put untyped R near 3x GNU R. The design is
   right and the forecast attached to it is not.
4. **Give base R's data-frame layer native kernels**, chosen by measured share. Expected: 10x or
   more on those calls; the effect on a suite depends on a share nobody has measured.
5. **Let ry-owned kernels use all cores in batch runs, and offer plain IEEE sums.** Expected: 2x to
   4x on large vector work on four cores, nothing on test suites.

Two of these recommendations are the user's to decide, because they touch user directives: the 10x
target for untyped R is the user's north star, and single-thread first is a user directive.

## What this rests on

The evidence comes from three places, in decreasing order of trust:

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
lines, without `--typed`, so its annotations are erased. Its 953 `test_that()` blocks ran under
both engines through `ry load --file` with the driver `oracle bench` uses. The package library in
the measuring container held 119 of the benchmark closure's packages, so 391 blocks fail on GNU R
for a missing package and 82 more fail on ry because `quadprog`'s shared object did not find
`libRblas.so` there. The comparison below covers the 480 blocks that pass on both, and it is a
partial run of a small code base, not the benchmark.

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

`MEMORY.md` records the full-size code bases differently: the time inside the tests is 1.21x to
1.35x GNU R's, so ry is slower there, while the whole process takes 0.75x to 0.97x of GNU R's wall
time, because GNU R spends minutes in testthat's `gc()` call per file. `backlog.md` records a
private 300,000-line code base as about 3% slower. The block times above are the counterpart of the
first figure and point the other way, and nothing in the repository says which describes the
current tree ("Blind spots" below).

### Typed ry against Rust

The typed runtime's bar is 1.5 to 2 times the time of equivalent Rust (`typed-runtime.md`). The
[hill climb](/development/design/language-climb/) computes that score with `tools/climb/run.py`,
but `tools/climb/log.tsv` holds no value for it. This review has two estimates:

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
  call site reaches the native entry, and no run reports when one does not.

## The ceiling on untyped R

`r-runtime.md` sets the target: "Untyped R with the JIT on runs at 10 times GNU R's speed on one
thread". It is the user's north star, with 1.5x and 3x as milestones. The roadmap's arithmetic
allows it. With package native code, I/O and waiting held fixed, its Amdahl table per workload
gives 10.96x on the workloads and 15.52x on the corpus for the row "R code 30x, builtins 10x, ry
ports 10x, GC and start-up 10x", and a ceiling of 34.4x on the corpus. So the fixed share is not
the limit. The question is whether those factors per bucket exist, and the evidence says they do
not.

**R code at 30x is an average over all the time in the bucket.** If a tenth of that time stays at
the interpreter's speed (code that runs once, `eval`, reflection, frames that deoptimized), the
bucket cannot pass 10x however fast the rest runs, and 30x needs all but 3% of it gone. The
published record for application-shaped code is far from that:

| System | What it does | Result | Source |
| --- | --- | --- | --- |
| Ř | Speculative inlining, deoptimization, environment and promise elision, contextual dispatch, on GNU R's runtime and builtins | 1.80x over GNU R 4.1 on real programs (0.74x to 18.3x), 3.23x on are-we-fast-yet, 1.70x on shootout; the first in-process iteration runs at 0.37x to 0.50x | [Flückiger's thesis, 2022](https://janvitek.org/pubs/fluckiger-phd-22.pdf) |
| Ř | The same, measured earlier | "ranges from a 0.7× slowdown to a 46× speedup, with the overall speedup being 1.7×" over 59 programs | [OOPSLA 2020](https://janvitek.org/pubs/oopsla20-cd.pdf) |
| rcp | Copy-and-patch compilation of GNU R's bytecode | 1.26x geometric mean, 1.23x on its real programs | [VMIL 2025](https://ch-rigid.github.io/publications/kocourek_copyandpatch_2025.pdf) |
| GNU R's own compiler | Bytecode over the AST interpreter | 5.7x to 7.1x on scalar loops, but "Function calls use the (slow) interpreter mechanism" | [Tierney, 2019](https://stat.uiowa.edu/~luke/talks/Riot-2019.pdf) |
| Purdue FastR | A Java AST interpreter with specialization | 8.5x on shootout, 1.7x on Benchmark 25, "explained by the preponderance of native calls" | [VEE 2014](https://janvitek.org/pubs/vee14.pdf) |
| HHVM | A region JIT with profile-guided inlining, on a warm server whose code carries type hints | The interpreter runs facebook.com at 12.8% of the JIT's speed, so the JIT is worth 7.8x | [PLDI 2018](https://research.facebook.com/file/529018501538081/hhvm-jit-a-profile-guided-region-based-compiler-for-php-and-hack.pdf) |
| V8 | TurboFan over the Ignition interpreter | 4.35x on JetStream, "over 1.5x" on Speedometer, the suite shaped like application code | [v8.dev, Maglev](https://v8.dev/blog/maglev) |
| YJIT | A production JIT for Ruby, which also keeps a C extension API | 15% on Shopify's storefront in production, against about 65% on railsbench for the same release | [Rails at Scale, September 2023](https://railsatscale.com/2023-09-18-ruby-3-3-s-yjit-runs-shopify-s-production-code-15-faster/), [December 2023](https://railsatscale.com/2023-12-04-ruby-3-3-s-yjit-faster-while-using-less-memory/) |
| CPython 3.11 | A specializing interpreter, no JIT | "an average of 25% faster" | [What's new in 3.11](https://docs.python.org/3/whatsnew/3.11.html) |
| CPython's JIT | Copy-and-patch above that interpreter | "about as fast as the existing specializing interpreter" | [PEP 744](https://peps.python.org/pep-0744/) |
| PyPy | A tracing JIT that emulates the C API | About 4x on pure Python; calls into C extensions measured "2.5x to 10x slower" through the emulation before a round of fixes in 2018 | [pypy.org](https://pypy.org/), [cpyext post](https://www.pypy.org/posts/2018/09/inside-cpyext-why-emulating-cpython-c-8083064623681286567.html) |

The best published factor for a JIT over its own interpreter on a real application is HHVM's 7.8x,
on a server that stays warm for hours. ry's own suites say the same. The public JIT suite and the
numeric kernels are made of loops that tier 1 compiles, and they stand at 2.66x and 4.8x over
GNU R, not 30x. The rcp authors name what is missing: "significant performance improvements require
high-level optimizations: inlining, type speculation, and operation elision. Removing explicit
callee environments, skipping promise creation, and avoiding copies provide the largest performance
gains." Ř built all of that and reached 1.8x on real programs.

**Builtins at 10x over GNU R's C has a mechanism for small calls only.** A builtin whose cost is
call overhead can gain that much: `structure()` is 6.9x faster on ry. A builtin whose cost is the
work cannot, on one thread with bit-identical results: `sort`, `exp` plus `log`, `ifelse` and
`mean` are 1.07x to 1.2x in the probes, and the geometric mean over the twelve vector kernels is
1.56x.

**Test suites are cold code.** Most of a suite's code runs a handful of times. Compiled code is
under 0.1% of the code-base run above and 0.7% of the testthat workload, and Ř's first iteration
runs at half of GNU R's speed. On that run, reaching 10x over GNU R from 1.67x means everything
except package native code gets 8 times faster than it is today (an estimate from the profile
shares above: 95% of the time must shrink to 11.7%).

The case for 10x has strong points, and each has an answer.

- *Ř and rcp ran on GNU R's runtime and builtins, and ry owns its base library and compiles at
  install.* That is true, and it is part of why ry reaches 2x on package suites. It supports
  3x. It does not supply a 30x.
- *ry's own probes show 37x to 68x where a call disappears.* They show the factor exists at a call
  site. The target needs it across the bucket, which depends on the share of time at call sites a
  compiler can flatten. That share can be measured cheaply ("What to measure first").
- *The rule that "a plan that can only reach 3x in its bucket is rewritten before it lands" keeps
  designs from settling at a local optimum.* It does, and it is the right filter for designs. It
  is the wrong forecast.

So this review's recommendation to the user is to keep the design rule and change the forecast:

- **Untyped R: plan and publish around 3x GNU R, with 2.5x counted as success**, until a
  measurement shows a path past it.
- **Put the order-of-magnitude goals where an order of magnitude has been measured**: typed code
  and the development loop.

This also answers what "a JIT on the level of Bun" should mean. Bun's JIT is JavaScriptCore, which
Bun did not write. What Bun added is start-up ("Bun processes start 4x faster than Node.js",
[bun.com/docs](https://bun.com/docs)), builtins in a systems language, and one integrated tool for
install, test and build. `r-runtime.md` says so in "The performance bar". ry's equivalents of Bun's
contribution are the lazy loader, `ry test`, the start-up image and the native base, and they are
where ry leads by the widest margin.

## The levers

### 1. Declared types that pay on whole programs

**What it is.** Typed ry is the only place where ry has measured an order of magnitude on user
code: 15.2x over GNU R on kernels and 20.5x on five applications, which is 3x to 10x over untyped
ry on the same programs. Two caveats belong beside those figures. Almost every typed time is under
the suites' 0.1 s floor, so it is flagged too short and counted anyway, and `sales` is refused.

The benchmark code bases do not show that factor. Their templates carry inline annotations, and
`backlog.md` records the typed and untyped finbench slices within 1% of each other (71.5G against
72.1G instructions). The backlog gives no cause. This review's inference is that the code bases'
functions mostly take data frames, vectors and lists, while the typed core compiles closures over
ry's own types and scalars. If that is right, the feature the mandate is built around buys nothing
on the mandate's benchmark, and closing the gap means building and tightening what is already
decided:

- **Build the ahead-of-time compiler.** `decisions.md` records the shape as chosen ("Typed code
  compiles ahead of time, and `ry build --exe` writes an executable"), and `backlog.md` files the
  work as "mid priority" while the typed core compiles a closure at its first call. This is the
  top runtime item, not a middle one.
- **Turn the slow-path report into a guarantee.** `typed-runtime.md` designs `ry check --explain`,
  which lists the places typed code compiles to a slow path, and only `ry install --explain` is
  built. Go one step further: in strict mode, a typed function that cannot run natively is a
  diagnostic at the construct that blocks it, shown in the editor. Today a refusal is a line in
  `RY_TYPED_LOG` at run time, one refusal spreads to the callees, and a call from untyped code
  with an unforced argument takes the interpreter without a message ("Open: the typed core").
  mypyc works this way: a module compiles or the build fails. No JIT can offer that, and ry has the
  checker to do it.
- **Give typed sums one fixed order in plain doubles.** `List.sum` over `F64` keeps the long-double
  fold so that native and interpreted typed code agree. They can agree on a faster rule instead:
  one fixed vectorized summation order that both paths implement. The user directive makes typed
  ry "independent of R's semantics", and the benchmark runner already accepts a relative 1e-10
  difference in a typed checksum.
- **Score against Rust on every step.** The hill climb's fitness function is typed ry over Rust,
  and its log holds no value for it. The estimate above is about 4.7x, with five of 17 programs
  within the 2x bar.

**Who shows it works.** [mypyc](https://mypyc.readthedocs.io/en/latest/introduction.html) compiles
type-annotated Python ahead of time for 1.5x to 5x, and mypy compiled with it is 4x faster.
[Cinder](https://github.com/facebookincubator/cinder) reports Static Python with its JIT at 18x on
a typed Richards. The same Static Python gave Instagram's web tier
[3.7% more requests per second](https://programming-journal.org/2023/7/2/), because production
code is partly typed and crosses the boundary constantly, and sound gradual typing has measured
boundary overheads of up to [105x](https://www2.ccs.neu.edu/racket/pubs/popl16-tfgnvf.pdf). The
spread between 18x and 3.7% is the lesson: the factor arrives when whole call graphs are typed and
compiled together, and disappears when typed islands sit in untyped code. Julia shows the end
state for a numeric language that reads like a dynamic one: 0.8x to 3.5x of C's time on its
[microbenchmarks](https://julialang.github.io/Microbenchmarks/dev/benchmarks/), where R takes 14x
to 466x on everything but matrix multiplication.

**Expected effect.** 3x to 10x on the typed share of a program, by ry's own measurements. On a
whole suite the share decides: if half of a run's time is in typed code that gets 10 times faster,
the run is 1.8 times faster, and at 90% it is 5.3 times faster. The order of magnitude therefore
needs whole modules typed, which makes the migration tooling (`ry migrate`, and the checker
reporting what blocks compilation) part of the lever.

**Cost.** Large: the ahead-of-time pipeline, the diagnostic, and typed versions of the code bases'
hot modules written in ry's own types.

**Depends on.** The typed core's open items: `sales` and `vasicek` do not run natively, and boxes
convert deeply at the boundary (typed `expr_tree` spends 46 of 48 ms converting).

**Biggest risk.** Adoption. A team must rewrite modules into ry's types to see the gain, and the
boundary can eat it. If agents write most of the code, as in this repository, the rewrite is cheap
and the risk shrinks.

### 2. The edit-to-result loop as the headline

**What it is.** `ry test --watch` reloads the package after an edit and runs the affected test
files first, then the rest. `MEMORY.md` records it on one code base: about 2 s to the first test
file against 62 s for `devtools::test()`, and about 1 s per edit against 68 s. Lazy `ry load` takes
0.33 to 0.41 s on a synthetic S4-heavy package where GNU R's `load_all` takes 6.3 to 7.1 s. These
are the largest factors ry has. They need the loader, the dependency index and the runtime in one
tool, so no other R implementation can match them by writing a better JIT. They are also not a
published suite and not a track. Making them the headline means:

- **Publish it as a fifth suite** in `benchmarks.mdx`: time from a one-line edit to the first
  affected test result, and to the last one, on the three code bases, against `devtools::test()`.
- **Select, and skip.** Today a test file is affected when the names it mentions meet the names
  that reach a changed definition (`Loader::callers` over the index, `all.names()` per test file),
  and every other file still runs afterwards. Selecting per `test_that()` block and skipping the
  unaffected ones is new work. It also reopens a recorded choice, "Why not a test index in the
  loader", which kept the loader ignorant of testthat for simplicity.
- **Offer the selection to CI**: `ry test --changed-since REF` runs what a diff can affect. No such
  option exists.
- **Count `ry check` in the same loop.** On the 300,000-line code bases a cold check takes 1.4 to
  1.7 s and the server settles an edit in about 40 ms (`backlog.md`, "Benchmark code bases"). A
  type error reported that fast replaces a test run.

**Who shows it works.** Bun, as argued above. Meta's
[predictive test selection](https://arxiv.org/abs/1810.05286) "reduces the total infrastructure
cost of testing code changes by a factor of two" while reporting over 95% of individual test
failures. The systems that bet on a warm JIT lose this loop: Ř's first iteration runs at 0.37x to
0.50x of GNU R, and on an RMarkdown workload
"[Ř take 405 seconds](https://janvitek.org/pubs/oopsla23.pdf) ... and the interpreter 85 seconds".
HHVM needed [Jump-Start](https://research.facebook.com/publications/hhvm-jump-start-boosting-both-warmup-and-steady-state-performance-at-scale/)
to cut the capacity lost to warm-up by 54.9%.

**Expected effect.** 10x to 70x on the wait per edit (measured once, on one synthetic code base),
and an estimated 2x to 10x on CI time per change, depending on how local a typical change is.

**Cost.** Small to medium. The mechanism exists for files; the work is block-level selection,
skipping with a fallback, the CI entry point and the benchmark.

**Depends on.** Nothing in the runtime tracks.

**Biggest risk.** Unsound selection. Matching by mentioned name over-approximates static
references, but a test that reaches a changed function through `do.call("name")`, `get`, a method
table or a data file must still run. Skipping needs a conservative fallback, a periodic full run
and a measured miss rate.

### 3. One optimizing tier that removes calls

**What it is.** The native execution model in `r-runtime.md` (slot frames, a direct calling
convention between closures, unboxed scalars, whole-package compilation at install; the
orchestrator's Track A) and the inlining tier in "The road to 3x" are one project, and it is the
right one. The probes, Ř and rcp agree that the gain is in the calls, environments, promises and
dispatches that stop existing.

Its targets mix a means with ends. They are "at least 80% of closures in rlang, cli and testthat
running as machine code (5 to 11% today), 30% fewer instructions on the testthat workload, and the
application workloads at twice GNU R's speed." The third is all but met at the published 1.95x.
The second is the right kind of target. The first is not, because native is not the same as fast:

- ry's install-time baseline code ran 8.06G instructions against 7.96G without it, and the backlog
  concludes that it "is no lever until baseline code inlines fast paths".
- A baseline compiler that does inline them is worth 1.26x over GNU R's bytecode interpreter (rcp)
  and 41% to 45% over V8's interpreter (Sparkplug alone, by the
  [Maglev post](https://v8.dev/blog/maglev); [5% to 15%](https://v8.dev/blog/sparkplug) of V8's
  total with the optimizing tier present). CPython's JIT gains
  nothing over its interpreter.
- A closure compiled to machine code that still builds an environment and promises runs at the
  interpreter's speed, as the third group of the probe table shows.

So judge the track by what a call no longer does. On the testthat workload a closure call costs
about 8,100 instructions and frees 7 to 8 objects, 1.2 to 1.4 of them promises (`r-runtime.md`,
`backlog.md`). The end shape in "Closure call setup: the 10x end shape" takes call setup from
about 2,100 instructions to about 200 and from 7 objects to none on a two-function microbenchmark.
Report those per-call counts on the workload after each merge, with the share of dynamic calls that
run inlined.

The inlining tier is the JIT track's recorded end shape and roadmap row (c), "planned". The
backlog's optimizing layer, which carries inlining with allocation sinking, is "not queued; starts
only on the orchestrator's go, after the call-setup enter/leave contract lands". It is the only
part of the JIT plan with a precedent for more than 1.5x on call-heavy code, so everything before
it should be chosen by whether it brings that layer closer.

**Who shows it works, and how far.** Ř, at 1.8x on real code with all of this built. HHVM, at 7.8x
over its interpreter on a warm server. JavaScriptCore's ladder shows the tapering: baseline about
2x the interpreter, DFG about 5x baseline, FTL about 1.5x DFG on long-running code and
[1.1x on JetStream 2](https://webkit.org/blog/10308/speculation-in-javascriptcore/).

**Expected effect.** An estimated 1.3x to 1.5x on package and application suites, taking untyped R
from about 2x to between 2.6x and 3x GNU R. On cold test code the gain comes only from the
install-time part, because nothing warms up: in the code-base run above, the JIT contributes
nothing. The model's own target of 30% fewer instructions on the testthat workload agrees with this
estimate. The 30x in the roadmap's R-code row does not.

**Cost.** Very large; it is the bulk of the runtime effort for months.

**Depends on.** Slot frames in compiled code and the enter and leave contract of "Closure call
setup: the 10x end shape".

**Biggest risk.** Deoptimization that must rebuild what R's reflection can observe (`sys.call`,
`parent.frame`, `substitute`, `on.exit`), and months of the largest effort spent on a result
bounded near 1.5x while levers 1 and 2 wait.

### 4. Native kernels for base R's data-frame layer

**What it is.** The probes show where untyped ry has the most room per call: base R's closures
over data frames cost tens to hundreds of microseconds on GNU R, and ry runs them 1.05x to 1.5x
faster, because it interprets the same R code. A 10-row filter through `[.data.frame` takes 20 µs,
`merge` on 100 rows 231 µs, `data.frame()` 105 µs. Where a native entry replaced a base closure,
the factor is 3.4x to 6.9x (`structure`, `match.arg`, `stopifnot`, `Map`).

The mechanism is settled: "Hot base R closures get hash-guarded native prologues" ported
`match.arg`, `stopifnot`, `structure`, `Reduce`, `tryCatch` and others, and the methods package's
core (`setClass`, `new`, `validObject`) runs natively the same way. The lever is to apply it to the
data-frame family as one design instead of one hotspot at a time: `data.frame`, `[.data.frame`,
`[[<-.data.frame`, `$<-.data.frame`, `rbind`, `merge`, `aggregate`, `split`, `tapply` and
`format`. Each gets a native implementation behind the hash guard, taken only for a plain
`data.frame` with atomic columns, with the R code as the fallback for every subclass.

The recorded policy for package functions is "Generic first": fix every primitive under the R body
that is slower than GNU R's, and port only what remains. The data-frame closures are what remains.
Their bodies are long sequences of small calls. The probes put most primitives at or ahead of
GNU R's, and these closures still gain only 1.05x to 1.5x.

**Who shows it works.** ry's own native entries, at 3x to 7x per function. Morandat and others
found GNU R spending 38% of its time in builtins and 29% in memory management
([ECOOP 2012](https://janvitek.org/pubs/ecoop12.pdf)), and Purdue FastR's 8.5x on shootout fell to
1.7x on a suite dominated by library calls. Bun's second leg is the same idea: hot builtins in a
systems language instead of in the guest language.

**Expected effect.** An estimated 10x to 50x on the calls themselves (a 10-row filter should cost
under 1 µs). The effect on a suite is unknown. `--profile-summary` reports tier-0 time without
saying whose code it ran, and `r-runtime.md` ranks builtins by self time, not base closures by
inclusive time. At a 10% share the suite gains 1.1x, at 30% it gains 1.4x. The share may be small:
the benchmark code bases import data.table and polars, and code that uses those spends little time
in base data frames.

**Cost.** Medium: ten to fifteen functions with exact fallbacks, pinned by the oracle.

**Depends on.** A profile split by owning namespace ("What to measure first").

**Biggest risk.** Fidelity. `[.data.frame` has many corners (`drop`, row names, duplicate names,
matrix columns), and a subclass such as a tibble or a data.table must never take the native path.
The directive that data.table is a normal package bounds this lever: it covers base data frames
only.

### 5. All cores in batch runs, and plain IEEE sums on request

**What it is.** Two settled decisions cap the vector kernels. "Single-core speed first; per-process
costs count once per worker" defers the automatic parallelism of roadmap row (b), and results are
bit-identical to GNU R's, which sends `sum` through an 80-bit long-double fold and rules out
reassociation. The vector probes show the cap: the geometric mean over twelve kernels is 1.56x,
and five of them are within 15% of GNU R.

The recorded reason for deferring is that the target users run test suites with one process per
core, so every core is busy. That holds for test suites. It does not hold for the other thing
these code bases do: a risk batch, a simulation or a model fit runs as one process beside idle
cores. Row (b) is designed and "architecture-ready", so the proposal is to schedule its
element-wise half for that case. Element-wise maps (`exp`, `log`, the distribution functions,
comparisons, `ifelse`) split across threads with bit-identical results, because no element depends
on another. `DataFrame` already runs on Polars' thread pool.

Plain-double sums would be an option, off by default, for code that accepts a difference in the
last digits. `options(ry.blas = "host")` is the precedent: the default BLAS kernels are
bit-identical to GNU R's, and the option trades that for the host library's speed.

**Who shows it works.** [Riposte](https://tableau.com/sites/default/files/2023-01/pact2012-talbot-riposte.pdf)
ran vector R code a mean of 14.4x faster on one thread through fusion and 55x to 670x faster on
32 threads, with no C API to preserve. Polars and DuckDB show that the table engines that win
[db-benchmark](https://duckdblabs.github.io/db-benchmark/) are the parallel ones.

**Expected effect.** An estimated 2x to 4x on four cores for element-wise kernels above about 1e5
elements, and nothing on test suites.

**Cost.** Medium for parallel element-wise kernels behind a size threshold. The numeric option is
small in code and large in testing.

**Depends on.** A user decision. "Single-thread first, parallel-ready architecture" is a user
directive, and bit-identical numerics is a settled agent decision that the oracle enforces.

**Biggest risk.** Oversubscription when a user already runs one process per core, which a
threshold and a worker-count check must prevent, and a second numeric mode that doubles part of
the oracle's work.

## What to stop or demote

- **10x as the forecast and the public bar for untyped R** (the 10x row of the roadmap table in
  `r-runtime.md`). The target is the user's, so this is a recommendation to the user. Keep the
  design rule that rejects plans capped at a local optimum.
- **Percent-level tier-0 items that `backlog.md` keeps open or held**, under "Open: performance and
  memory": "Tier-0 call entry and dispatch" (9.6% of self instructions in total, a few instructions
  per item), "Builtin call shape (held: percent-level tier-0 tuning)" (ceiling 4.5%, expected gain
  2% to 2.5%) and "Queued with measured ceilings". The user has said to stop chasing these. Close
  them instead of holding them. In "Open: data-oriented layouts and explicit allocators", rows 7,
  9, 10 and 11 remove frame, reference and promise work per call and belong to lever 3. The other
  open rows top out at 3.5% each and can close too.
- **Single-function instruction wins as separate efforts**: S3 dispatch resolution, `match.arg`,
  and the xts and zoo paths. Each lands about a percent on one
  workload. Fold what remains into lever 4, chosen from a ranked list. The top-level loop is the
  exception ("Open: collector throughput on the benchmark suites"): 2.55 s against 0.30 s is a
  visible defect in a first script, and compiling top-level code fixes it once.
- **The baseline tier in its present state** ("Open: the baseline tier and JIT on against JIT
  off"). It is opt-in and runs 1% to 2% more instructions than the interpreter, and it costs tier
  tests and tier-fuzzer time. Its recorded purpose is compile latency (28 µs per body against about
  10 µs per bytecode word for Cranelift), and the record names the fix: inlining the common
  instructions' fast paths. Decide it. Either the fix lands and the baseline compiler becomes the
  install-time compiler for cold code, where rcp and Sparkplug show 1.26x to 1.45x over an
  interpreter, or the tier goes and install-time images cover the latency. An opt-in tier that is
  slower than the interpreter is the one state not worth keeping.
- **Fitting the 1e8-row db-benchmark joins into 15 GB** ("Open: DataFrame", top priority). The
  mandate is to outperform data.table, and `MEMORY.md` records that: 5.2 s against 27.3 s at 1e7
  rows and 22.4 s against 69.2 s on the 1e8 group-by. data.table does not fit the 1e8 joins either.
  If the size matters, evaluate Polars' streaming engine before building a custom allocator for
  the reader's transients. Polars reports it as
  "[up to 3-7x faster](https://pola.rs/posts/benchmarks/)" than its in-memory engine, and it works
  in batches. Whether it fits these joins in 15 GB is unverified. DuckDB is the only engine that
  completes the 50 GB joins on the public benchmark, and it does so on disk.
- **Typed ports of the public JIT programs as a goal.** Those programs were chosen by R compiler
  researchers to stress untyped R. The ports found real gaps, listed in "Gaps the typed public
  ports found", and that list is worth finishing. As a measure of typed ry the suite is the wrong
  one, and it has no committed typed results. The distance to Rust on programs written for typed
  ry is the measure.
- **Tuning the JIT's thresholds and feedback recording.** JIT on against JIT off is within 2% on
  every call-heavy measurement in the repository and in this review. Nothing in that range changes
  the picture until the tier removes calls.

## Where the current tracks are right

- **The ABI-compatible `libR`.** PyPy and FastR lost ground at the native boundary. ry's code-base
  run spends 5% of its time in package native code across 293,000 `.Call`s. This was the decision
  that mattered most, and it was made first.
- **The native execution model's design**: slot frames, the enter and leave contract, frame-state
  maps and reification on demand are the prerequisites that Ř and rcp identify. Its forecast and
  its first target need changing, not its design.
- **The vector kernel layer** as one module with slice kernels and a reuse rule. Builtins are 27% of
  the code-base run.
- **Lazy loading and `ry test`**, which produced the largest factor in the project.
- **`DataFrame` on Polars**, which met its mandate.
- **The testthat workload as the runtime's primary metric.** Its profile (tier 0 33%, builtins 26%,
  call setup 25%, compiled code 0.7%) is close to the code-base run's (45%, 27%, 13%, under 0.1%),
  so it is a fair stand-in for shares, though the code bases are colder.
- **The differential oracle as the release gate.** Compatibility is the product.

## What to measure first

Each lever has one cheap measurement that could kill it.

| Lever | Measurement | Kill or reshape if |
| --- | --- | --- |
| 1. Typed programs | Build the current tree and run `just climb --log NOTE`, so the typed-over-Rust score exists. Then generate the code bases with `--typed`, run their tests with `RY_TYPED_LOG=1`, and count typed closures compiled against refused, by reason | Fewer than 20% of the code bases' annotated closures can compile without being rewritten into ry's types. Then lever 1 is a migration project before it is a compiler project |
| 2. Edit loop | Replay 200 random one-function edits on each code base and record the share of test files and blocks whose names meet the change, and the time to the first result, against `devtools::test()` | The median edit selects more than half the tests, which would mean the call graph is too connected for selection to pay |
| 3. Call removal | Flatten the five hottest call chains of the testthat workload by hand in R source, and time the workload. This gives the inlining tier's upper bound without building it | The flattened workload gains less than 1.3x. If it gains 3x or more, this review's ceiling is wrong, and the 10x forecast deserves another look |
| 4. Base kernels | Split tier-0 and call-setup time in `--profile-summary` by owning namespace (base and recommended, testthat with rlang, cli and withr, other packages, the user's package), and list the 50 base closures with the most inclusive time | The data-frame family is under 5% of every code base and of the corpus |
| 5. Cores | Time `exp`, `log` and `pnorm` kernels on one and four threads at 1e5 to 1e7 elements with a bit-identity check, and take the kernel share of one production-shaped batch run | The share of large element-wise kernels in a batch run is under 10% |

One more measurement comes before all of them: run the three full-size code bases on a release
build and publish the result as a suite. It is the benchmark `AGENTS.md` defines, and
`benchmarks.mdx` has no number for it.

## Blind spots

- **The mandate's benchmark has no scoreboard.** `benchmarks.mdx` publishes four suites, and none
  is the three code bases. The recorded test times (1.21x to 1.35x GNU R's at full size, and about
  3% slower on a private code base) and this review's partial run (1.67x faster on a 30,000-line
  code base) cannot both describe the current tree.
- **The benchmarks are ry's own writing.** The code bases are generated from templates, and the
  kernels and applications are written in-house. One user's profile summary is the only outside
  data point in `r-runtime.md`. The targets should lean on more of those, because a generated code
  base rewards whatever its templates happen to exercise.
- **Agents are users too.** An agent runs the check and the tests hundreds of times per
  task, so its throughput is bounded by the loop in lever 2, and it can do the rewrite lever 1
  asks for. Neither the plan nor the benchmarks model that user.
- **Typed speed is not observable.** Nothing built tells a user, or the hill climb, when a typed
  call fell back to the interpreter. The nightly run above lost a factor of 300 on one program
  without a message.
- **Bit-identical numerics is a standing cost that nobody totals.** It decides the sum kernels, the
  fused loops and the software long-double path on targets without x87. Its total over the kernel
  suite is unmeasured, and so is the number of users who need the last digit.
- **Platforms.** `backlog.md` says the base build has run on Linux only. Every number on this page
  and in `benchmarks.mdx` is Linux x86-64.
- **Twenty-four differing values.** In the code-base run, 24 of 480 passing blocks returned
  different values on the two engines. The cause may be the incomplete library or the older binary,
  and it deserves one look, because `MEMORY.md` records a 100% match at full size.
- **This review's own limits.** Lever 2 rests on one measurement of one synthetic code base, and
  lever 4 on per-call probes of 10-row data frames. Both need the measurements in the table above
  before any work starts.
