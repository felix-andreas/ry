---
title: The runtime fuzzer
description: "Which ideas ry's differential fuzzer takes from Csmith, YARPGen, Fuzzilli, jsfunfuzz, Hypothesis, libFuzzer, swarm testing, EMI and deterministic simulation, which it declines, and what is left to build"
---

ry's runtime fuzzer exists to find the bugs nobody thought to test for. This page measures it
against the fuzzers that set the standard for compilers and language runtimes, says which of
their ideas it takes and why, and lists what is left to build. The contract for what runs is the
[robustness page](/development/robustness/); this page is the reasoning and the plan.

## What a finding needs

Two bugs a test user reported show the bar. Both sat in common code, and no harness could have
found either:

```r
f <- function(m) { m[1, 1] <- 1; m[2, 2] <- 1; m }
f(matrix(0, 8, 8)); f(matrix(0, 8, 8))   # the second call crashed

m <- c("aä" = "x"); m["aä"]              # NA, where GNU R gives "x"
```

The first needs a value of 64 elements or more, two element stores in one body, and a second
call of the same closure. The second needs a name that is not ASCII, and it needs someone to
look at a string's encoding mark, which printing hides. A fuzzer finds bugs like these only when
four things hold at once:

1. **The generator reaches the state.** Sizes at storage thresholds, closures called again after
   they compile, strings in every encoding, and combinations of features that no one listed.
2. **The observer sees the difference.** Printed output hides encoding marks, attribute order,
   the kind of an `NA` and the object bit, so the comparison has to look below it.
3. **The finding is small.** A 200-line generated program is not a bug report. The same failure
   in five lines is.
4. **There are enough executions, aimed well.** The eight seeds per kind in the default suite
   keep the harness working. Thousands find the shallow bugs, and only feedback from the runtime
   finds the deep ones.

## The ideas, and what ry does with each

| Source | The idea | In ry |
| --- | --- | --- |
| Csmith | Generate only programs with one defined meaning, so every difference between two implementations is a bug. | Taken. R has no undefined behavior, but it has addresses, hash order, clocks, the locale and GNU R's own defects. The generators emit none of them: a program is a pure function of its seed (`programs_are_deterministic` pins the grammar kind), the `language` kind byte-compiles its closures up front because GNU R's two evaluators differ in a few messages, and a construct where GNU R itself is wrong is kept out at the generator. The comparison normalizes nothing; the two messages the `language` kind cannot keep apart are rewritten inside the program, where the reader sees it. |
| YARPGen | Generation policies: bias the generator toward what the optimizer acts on, since uniform sampling of a grammar rarely triggers an optimization. | Taken. The value zoo aims at the runtime's own thresholds (lengths 63, 64 and 65, compact sequences, the copy wrapper for long arguments), and a body runs often enough that the first and the compiled call are both observed: a grammar function three times, a metamorphic body between 1 and 300 times, a language closure once, a few times or sixty times. Planned: one template per JIT speculation that first satisfies the guard and then breaks it. |
| Fuzzilli | Mutate an intermediate form in which every mutant is a valid program; keep a corpus of programs that reached new coverage; splice code between corpus programs; templates that warm up the JIT. | Partly. R needs no intermediate form for validity, because every text that parses is a program and an error is behavior to compare, so mutation works on the lossless syntax tree (`oracle` `mutate.rs`). The coverage-guided corpus and splicing are planned (below). |
| jsfunfuzz | Run one program with the JIT off and on and compare; feed the engine's own regression tests back in as seeds. | Taken. Every seed runs under tier 0 and the forced JIT, and the `corpus` kind mutates R's regression tests, the base help examples, the corpus packages' own tests and the oracle cases. |
| Hypothesis, QuickCheck | Integrated shrinking: shrink the choices the generator made, not its output, so every shrunk case is one the generator could produce. | Taken where the input has invariants, declined for programs. The structured component harnesses (the language server simulator, the loader's reloads, bytecode, code images, typed code files) draw from `syntax::testing::Entropy`, so libFuzzer minimizes the choice stream. An R program has no invariant to keep, since any text is a legal input, and a mutated corpus snippet has no choice sequence at all, so programs shrink by delta debugging over the syntax tree (`oracle reduce`), which also removes what no generator choice maps to. |
| libFuzzer, AFL++, LibAFL | Coverage feedback: keep an input when it executes code no earlier input did, and mutate the kept inputs. | Taken for components (the `cargo fuzz` targets), planned for whole programs (below). |
| Swarm testing | Each run enables a random subset of generator features, because a feature that is always on suppresses the bugs that need its absence. | Taken in the `metamorphic`, `builtins`, `api`, `io` and `language` kinds: a seed enables each feature with probability one half, and one seed in eight enables all of them. Planned for the `grammar` kind, where every program draws from every block shape. |
| EMI, metamorphic testing | Derive a second program whose result is known from the first: change code the run never reaches, or wrap, rename and repeat, and the result must not change. | Taken. The `metamorphic` kind runs one body in up to ten of thirteen forms that must agree and checks it inside the program, so it needs no GNU R. The `perturbed` kind inserts dead and effect-free statements into a generated program's function bodies (below). |
| Antithesis, FoundationDB | Deterministic simulation: one seed decides the whole run, faults included, so any failure replays. | Taken for the language server (the simulator) and for the collector (`RY_GC_STRESS=seed:N` decides where collections fall). Planned for the JIT's schedule and for interrupts (below). |

## Effect-free mutations

A mutation that changes what a program computes usually sends it into an early error, and the
rest of the program never runs. A mutation that changes only what the compiler sees keeps the
whole run and still moves slot numbers, constant indices, branch offsets and inlining decisions.
That is the useful half of EMI when a reference implementation exists: GNU R runs the mutant
too, so the fuzzer does not need to know which code is dead.

The mutator (`oracle::mutate::perturb`) has two kinds for it. *Dead code* inserts a statement
that never runs, such as `if (FALSE) {...}` around a statement taken from elsewhere in the
program, an unused local closure, or a loop over `NULL`. *Neutral code* inserts a statement with
no effect on the result, such as `sys.call()`, `gc(FALSE)`, a `tryCatch` around `NULL`, or, in a
function with parameters, `x <- x` and `force(x)`. Both apply inside function bodies, where the
compiler works. The `perturbed` kind makes four to twelve attempts on a grammar, metamorphic or
language program and keeps each mutant that parses, and the `corpus` kind draws them among its
other mutations.

## What is left to build

In the order of expected yield.

### The JIT's schedule from the seed

`RY_JIT=force` compiles a body at its second call and a loop at its first back edge. That is one
schedule, and bugs hide in the others: a body compiled with ten calls of type feedback, a loop
compiled at its fortieth iteration, a callee compiled before its caller. `RY_JIT_THRESHOLDS`
sets the entry and back-edge thresholds, and no oracle engine uses it: a seeded engine draws
both per program from the seed, the way the seeded collection stress draws where collections
fall. The step after it is a seeded guard failure: a compiled guard that fails on purpose hands
the activation back to tier 0 where no test ever made it do so.

### Coverage-guided whole programs

The differential fuzzer starts a process per engine for every program (GNU R, tier 0 and the
forced JIT by default), so a core runs a few programs a second at best. It cannot tell a program
that reached new code from the thousandth one that did not.
An in-process target changes both: the runtime starts once, a `metamorphic` program is generated
from libFuzzer's bytes through `Entropy`, and it runs under invariants. The program checks
itself, so the target needs no GNU R: a line that ends in `DIFFERENT`, a panic or a sanitizer
report is the failure. Programs that reach new coverage stay in the corpus, and `oracle fuzz`
replays that corpus against GNU R, which is where the differences from R show.

Two things stand in the way. The oracle's generators take a `SplitMix64`, so they move to
`Entropy` first. And a process that runs many programs carries state from one to the next
(options, registered methods, loaded namespaces), so a failure must replay from its input alone:
the target runs each program in a fresh environment, and the kinds that define classes stay with
the process-per-program fuzzer.

### Measured coverage

`oracle fuzz --coverage` counts which exported functions the generated programs name, which says
nothing about the bytecode instructions, the JIT's lowerings or the C API entries they execute.
A coverage build (`-C instrument-coverage`) run over a few thousand seeds of every kind gives
that as lines per file, and the largest unreached regions become the next generator features.

### Interrupts and failed allocations

An interrupt can arrive at any safepoint, and an allocation can fail at any allocation. Neither
is injectable, so no harness checks that the interpreter is left clean after one. Both get the
treatment the collector has: a seed decides at which safepoint or allocation the fault fires,
and the program observes it through `tryCatch(interrupt = )` or the allocation error.

## Declined

- **A typed generator that never produces an error.** Csmith needs one, because an ill-typed C
  program has no meaning. In R the error is the meaning: which condition, with which message and
  call, is among the things packages depend on. About one grammar block in five is ill-typed on
  purpose.
- **Comparing engines by a hash of the final state**, as Csmith does. The deep observer's dump
  of every top-level value costs more per program and tells the reader which value differed and
  how, which a hash does not.
- **A second independent implementation as referee.** Compiler fuzzers vote among three
  compilers. R has one reference implementation, so a difference is read by a person, and the
  reducer exists to make that cheap.
