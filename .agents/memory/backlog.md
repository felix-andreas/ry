# Backlog

**Standing goal (user mandate): empty this list and keep the project at rust-analyzer quality.** The beta program that once organized it is complete; shipped work lives as one-line ledger entries at the bottom (rationale in `decisions.md`, contracts in the docs). Every open item sits in one of the sections below.

**Quality bar (acceptance):**
- **Sound on idiomatic R:** no known accepts-then-crashes holes on supported constructs; unsupported constructs may be refused loudly (sound-by-refusal is acceptable) but never silently mistyped.
- **Zero false positives on the ~200 most-used base functions** with `[check] typing = true` on idiomatic call forms.
- **Performance:** keystroke-to-diagnostics p50 ≤ 30 ms / p95 ≤ 100 ms at 300k LoC (read against the raw-parse floor the instrument prints — latency numbers swing ~1.4x with machine load); budgets pinned by `stats_witness` (per-line wall/memory/resolve-step ceilings) with the measurement instruments in `legacy/differential/tests/test_stats.rs`.
- **No server-killing input** (no `unwrap` panics on protocol-legal messages).

## Open: where findings point, and what the type system still refuses

Three simulated users probed the type checker itself rather than package coverage, working from the
docs and `--help` only. One worked on parametric polymorphism and higher-order code, one on domain
modelling and nullability, and one adversarially on the `#:` surface and where carets land. Each
judged location and message as separate verdicts, which is what made this round different. A
diagnostic that reads perfectly while blaming the wrong expression counted as a failure.

Only the open findings are below. `test-user-reports.md` holds the closed ones with what each
measurement showed.

### Placement

Caret placement is good for ordinary nesting, which covers four-deep calls, multi-line arguments,
lambdas and pipes. The renderer is display-width aware while JSON stays in codepoints. Every failure
found was the same shape, which is a collapse to the outermost node, and one rule now covers them in
the reference under "Where a finding points": a finding underlines the smallest expression its
message is about. Comparison operators, a return-type mismatch with an `if` and `else` tail, `$` and
`[[`, a surplus positional argument, a record mismatch and an annotation range ending at
end-of-line all follow it now.

**One shape is still open.** A parse error was reported past the end of the file. The filed shape,
which was line 10 of a 9-line file, does not reproduce. An unterminated `f <- function(x,` in a
one-line file reports at `1:14-1:15`, which is in range. Either an earlier fix covered it or the
note was imprecise. This needs a fresh reproduction before it can be worked, and it should be
re-derived rather than trusted.

### Should strict mode report a binding whose exported type contains `Unknown`?

This is a design question, not a bug fix, and it needs volume measured before it is chosen.

The gap is real. `g <- f` closes to `g: fn(p: Unknown) -> Unknown`, which the
`aliased_function_reference_exports_closed` fixture pins, and strict reports nothing. Calls through
`g` are therefore unchecked while the run looks like a pass.

Two earlier diagnoses of it were wrong, and the measurements are the keepable part.

- **`Any` is not what blinds strict mode.** Changing `do.call`'s return from `Any` to `Unknown`
  produces no strict finding at all. This was tried: the stub was edited, the binary rebuilt, and a
  strict project run over `do.call(fun, args)`. Nothing.
- **Strict mode has no binding-level `Unknown` test to widen.** It reports origins recorded at
  construct sites, which are `UnsupportedConstruct`, `UndeterminedReference`, `LoopWidened` and
  `RecursiveUnknown`. Nothing inspects a finished type.
- **`Any` in the corpus is deliberate.** The shipped stubs declare 176 entries whose return is
  `Any`, across nine files rather than just `base`, and none returning `Unknown`. The stub header
  names each compromise.
- The one behavioral difference between them is that `@if-unknown` coerces an `Unknown` and is
  refused on an `Any`, saying that the value is already `Any` so the annotation should be dropped.

Measure before deciding. Strict mode already emits 2879 findings on dplyr and 2458 on shiny with
typing and strict forced on, and `erase_residual_vars` gives every aliased function `Unknown`
parameters, so a containment sweep would fire on `g <- f`, an idiom ordinary R uses constantly.
Decide it as a design note with numbers, not as a one-line widening.

### Promotion needs to live in the type, not in the binding

Two findings are the same missing piece, and they should be designed together.

**A `logical` is accepted at a declared `integer` parameter and refused at an inferred numeric one.**
Both halves reproduce. `bump <- function(x) x + 1L; bump(TRUE)` is refused, and the same body under
`#: fn(n: integer) -> integer` accepts it. R does promote, because `TRUE + 1L` is `2L`, and the
checker's own arithmetic rules say so. Verified against R 4.3.3.

Do not just widen the constraint. The obvious repair is to bind the numeric variable to `integer`
when a `logical` argument arrives, which is exactly R's promotion, and it is wrong. The
counter-example is ordinary R.

```r
bump    <- function(x) x + 1L                          # R: bump(TRUE) is 2L, integer
checked <- function(x) { stopifnot(x + 1L > 0L); x }   # R: checked(TRUE) is TRUE, logical
```

Both infer `<T: numeric> fn(x: T) -> ...`. One binding of `T` cannot be `integer` for the first and
`logical` for the second, so the promotion produces a wrong return type. This project ranks a wrong
answer below a refusal, because a gap means checks are skipped rather than that wrong answers are
produced.

**A numeric variable shared by two parameters refuses ordinary mixed arithmetic.**
`add <- function(a, b) a + b` infers `<T: numeric> fn(a: T, b: T) -> T`, which ties both operands to
one variable, so `add(1L, 1.5)` reports ``expected `integer`, found `double` `` where R gives `2.5`.
That is a false positive on about as plain a piece of R as exists. The operand types need a numeric
join, where `integer` with `double` is `double`, rather than unification.

Closing either properly needs a scheme like `<T: numeric> fn(x: T) -> promote(T)`, which is a
type-level function the annotation language does not have.

### Smaller open items

- `@new` is unrestricted project-wide, so `domain-modeling.md` saying it is "the only door in" and
  `concepts.md` saying a value "provably came from there" both overstate it. Either add an
  encapsulation modifier or soften both sentences to say "by convention".
- A narrowing failure on a field, or behind `&&`, produces a message byte-identical to the one for
  writing no guard at all. The docs know the fix, which is to lift the value into a local first. The
  diagnostic should say it.
- A nominal union is unchecked through `$` while a structural union is exact. There is also no
  discriminator for a nominal type, because `is.list` is true of both arms, so a tagged union cannot
  be narrowed at all.
- An alias cycle is reported on the use, with a whole-statement caret, and never at the declaration.
  An unused cyclic alias is not reported at all, although the reference says a definition cycle is
  an error.
- A second `#:` annotation on one line is silently swallowed, because the first wins, while harmless
  trailing prose errors. The ambiguous input is the quiet one.
- A missing-argument error does not name the parameter, although the sibling wrong-name error lists
  all of them.
- A near-miss suggestion has a length floor that misses a short field name, such as `person$nam`.

### What the round said overall

Both testers who could reach a verdict said the core is real. They found genuine Hindley-Milner with
generalization, an occurs check, per-parameter variance, working generic nominals, airtight nominal
distinctness, and no cascades outside the `@param` case. The gap is not the engine. Diagnostics
render the artifact unification left behind rather than the fact that failed, and the nominal story
protects construction but nothing after it.

## Open: performance and memory

An independent review profiled before it proposed. Every number below was taken on a 4-vCPU
container with other work running, so treat each as an upper bound. Every in-container timing is
effectively a two-core number.

### Writes inside a test block enter the package namespace, and that is a correctness question

This was framed as the interface fixpoint being superlinear in the size of the cyclic definition
group. It is not. Profiled on `targets` 1.12.0, where `ry check` took 16.6 s on the JSON path so the
reporter was not involved, `analysis-stats` attributed 16,341 ms of the 16,681 ms typecheck to one
245-line file, `R/class_active.R`.

What it scales with is not the file count. 284 package files cost 0.35 s. Adding 238 unrelated
package files costs 0.37 s. Adding the 238 `tests/testthat` files costs 16.6 s, and reclassifying
those same files as scripts brings it back to 0.99 s. Bisecting by test-file count gives 384 ms at
zero, 1.6 s at 30, 3.7 s at 60, 9.7 s at 120 and 22 s at 238, which is about 90 ms per added test
file.

The mechanism comes from per-item execution counts and times. Query executions grow only 3.6 times
while wall time grows 25 times, so re-execution volume is not the cause. One statement item was
re-executed 139 times and its own cost grew with the file count. A write inside a `test_that` or
`tar_test` block binds at the item's top level, because a bare `{...}` is not a scope, so
`conditional_slot_items` publishes every one of them as a package-namespace conditional slot. A
cross-file deferred read of a common local name such as `out` or `envir` then joins over every
conditional writer of that name project-wide, which is hundreds of statement items across 238 files,
and each join needs that item's check, which drags in the R6 record type.

Two causes were ruled out by direct experiment, so nobody should repeat them. It is not the fixpoint
round cap, because setting `SCHEME_ROUND_CAP` from 16 to 2 changed 9,690 ms to 9,760 ms. It is not
the type-size ceiling, because lowering `TYPE_SIZE_CEILING` from 100,000 to 2,000 changed nothing.

The performance half is fixed, and the ledger records both parts. What remains is the semantics.
A write inside a `test_that` or `tar_test` block still enters the package namespace, which is wrong
for those callees.

**The rule has to key on the callee, and R decides it that way.** This was checked rather than
assumed.

- The block's write does bind outward for `suppressWarnings({v <- 1})`, `invisible`, `system.time`,
  `try` and `withCallingHandlers`, because a promise is forced in the caller's frame. A blanket rule
  would manufacture a false `unresolved` finding on `try({cfg <- read()}); use(cfg)`.
- It does not bind for `local({v <- 1})`, or for the `eval(substitute(b), new.env())` pattern that
  `test_that` uses.

The honest options are a known-verb list, covering testthat's `test_that`, `describe` and `it` and
matching the existing `library`, `on.exit` and `local` precedent, or a stub annotation in the vein of
`@masked`. A verb list alone is not enough for a real project, because `targets` wraps `test_that` in
its own `tar_test`, and hardcoding a package's private wrapper is not a rule.

One blocker comes with whichever is chosen, and it is measured. Scoping those blocks adds 87
`unused` warnings to `targets`. They are a mix of genuine dead stores, such as
`expect_silent(tmp <- f(x))`, and cases that only look dead because a name is used in a nested
closure. That needs its own answer before the change can land.

### Memory, and the rest of the package-path measurements

A review-authored synthetic of 1,550 files, 277,586 lines and 14,771 items reported 55.9 s and
5,488 MiB peak as package documents against 5.95 s and 343 MiB as scripts, superlinear in file count
at 6.1 s for 400 files, 10.8 s for 800 and 68 s for 1,550. **That does not reproduce, and the
generator's shape was never recorded.** A fresh synthetic package of 1,500 files, 42,000 lines and
10,500 items, holding five functions plus a shared top-level conditional write per file, which is
the shape the join punishes hardest, costs 0.49 s and 111 MiB peak after the bound and 0.85 s
before it. Treat the old figures as unverified unless someone reconstructs the generator.

Other packages move much less between the package and script shapes, at 1.70 against 1.21 for
ggplot2, 0.72 against 0.58 for dplyr, and 0.65 against 0.59 for shiny. That fits the cause above,
because they have far fewer test-block writes landing in the namespace.

Sampling had put the time in cycle bookkeeping around `statement_binding_scheme` and `item_check`,
with 54 of 72 sampled thread stacks in `DependencyGraph::block_on` and `targets` getting no parallel
speedup at all, at 17.43 s and 17.81 s on one core against 17.43 s and 16.99 s on four, while
ggplot2 managed 1.15 times. That was a symptom of the unbounded conditional-slot joins, and bounding
them fixed the parallelism too. Re-measured with `taskset` pinning core counts, best of two:
`targets` goes 998 ms to 778 ms to 599 ms at one, two and four cores, which is 1.67 times;
data.table goes 420 ms to 309 ms to 278 ms, which is 1.51 times; ggplot2 goes 1,388 ms to 1,255 ms
to 1,033 ms, which is 1.34 times. Read those against the container's ceiling, because its 4 vCPUs
deliver only about 1.8 times native compute at four threads. `targets` is therefore at roughly 93%
of what is achievable here, and a number from real hardware would be worth having.

Two facts about the fan-out are worth knowing. `check` fans out over `available_parallelism()` while
`fmt` is a plain loop, measured at 646 ms on one core against 644 ms on four. There is also no flag
to control concurrency. `available_parallelism()` honors CPU affinity and cgroup quotas, so
`taskset -c 0-N` is the only lever today. That is enough for measuring, and not something a user
would find.

The memory note of about 300 MiB at 302k lines holds for the script shape, at 343 MiB for 278k
lines, and is sixteen times off for the package shape. Memory attribution in the healthy shape is
parse at 82 MiB, lowering and naming at 158 MiB, typecheck at 47 MiB and diagnostics at 28 MiB. HIR
and naming dominate resident memory at rest, not interned types.

### Fan the formatter out

On an identical file set of ggplot2's 339 files, 64,302 lines and 2.0 MiB, single-threaded on both
sides, `fmt --check` takes 1.01 s and `check` takes 2.26 s. The formatter costs half what the type
checker does.

Format is 113 ms of parse plus about 890 ms of render, so the render is about eight times the parse,
at 1.9 MiB/s against about 18 MiB/s for parsing. The actionable part is to fan `fmt` out the way
`check` already does. The render's factor of eight was not localized and needs its own profile
before anyone touches it.

### Suspected: `Checker::infer` deep-clones an `Expression` per call

`let expression = self.module.expression(id).clone();` sits on the checker's hottest path. It clones
a `NameRef(String)`, a `Call{arguments: Vec<Argument>}` and a `Binary{special_name: Option<String>}`
per node, while most arms then re-extract only `Copy` fields. The clone exists only to release the
borrow on `self.module`. It appeared in the profile solely as `drop_in_place` and allocator frames,
so its cost was never isolated. Measure before acting.

### Open from a fixed entry: the `BTreeMap` choice in `ItemNaming`

`resolutions`, `bindings`, `non_locals`, `quiet_reads` and `namespace_reads` are pure lookups with
no ordering requirement, and `BTreeMap::get` showed up under `infer_read`. Switching `item_hir` and
`item_naming` to `returns(ref)` removed the node-walk half of that cost, so what is left is lookup
only. Measure before switching, and check iteration order wherever any of these is walked to build
diagnostics.

### Judged fast enough, so do not invent work here

Single-package cold analysis is 1.9 s and 88 MiB peak for ggplot2's 68k lines, 1.3 s and 72 MiB for
mgcv's 37k lines after the chain fix, and 1.4 s for the 64k lines of `targets` as the instrument
classifies it. Parse is only 2% to 7% of the pass, at about 0.9 microseconds per line.

`item_spans` identity is clean. `item_span_positions` is a memoized index, so `item_span_range` is a
constant-time probe, and `item_spans` itself is consumed per file. Incrementality genuinely holds:
every project reported zero item rechecks per keystroke and zero resolve steps, with edited-file
diagnostics at a 2.5 ms median on ggplot2 and a 21.3 ms median at 277,586 lines, which is inside the
stated bar. rowan re-anchoring is not a quadratic either, because `child_or_token_at_range` is a
binary search.

### Two measurement lessons from this review

**Measure both sides through the same output mode.** The review reported a fixed case at 0.07, 0.13,
0.26 and 0.56 s against baselines of 0.28, 0.61, 2.15 and 6.22 s. The fixed figures came from the
JSON path and the baselines from human output, so the reporter's own quadratic sat inside the
comparison.

**Split analysis from reporting before drawing a conclusion about either.** An earlier entry claimed
that position barely matters, from two 20,501-line projects with 500 findings each costing 6.9 s and
8.0 s. Those projects are analysis-bound, at 4.6 s for JSON against 41 ms for the reporter, so they
were never evidence about the reporter at all.

## Open: abstraction and duplication

An independent review looked for duplicated sources of truth and for abstractions that earn nothing.
These are the findings that are still open.

### Rename accepts `..1` as a new name, and the identifier rule is written twice

The server's `is_valid_r_identifier` restates a rule that `syntax::is_syntactic_name` already owns.
Same reserved-word list, same start and continue classes, same `.5` exclusion. Call the lexer's rule
instead of keeping a second copy, which removes about 60 lines.

The dot-dot part of the finding as originally filed was wrong, and it was checked against R. Both
`... <- 1` and `..1 <- 1` run, and `... <- 5` genuinely binds, because `get("...")` returns 5. They
are not invalid assignment targets.

The real defect is narrower and is only about a `..1`-style name. The assignment succeeds but the
read cannot, because `..1` resolves as a positional slot of an enclosing `...` rather than as a
variable. `..1 <- 7; ..1` fails with "..1 used in an incorrect context, no ... to look in". Renaming
a variable to `..1` therefore turns every one of its reads into a run-time error silently, and
rename must refuse it. `...` is a legal name, so refusing that one needs a different justification,
such as shadowing the forwarding mechanism, or none at all.

Moving to `is_syntactic_name` does not fix this on its own, because the lexer's rule accepts both
spellings too.

### A dead-code batch, all of it hidden behind `let _ =`

Seven items, compile-verified as unreachable, across 44 lines. They survive because a `let _ = ...`
keeps the binding alive, which is also why the compiler never flagged them. That is the pattern to
grep for.

### One rule table, written twice

The lint rule metadata is restated rather than derived, so a rule can be added to one table and not
the other. Make one the single source of truth and generate the second view from it.

### A `use`-qualification sweep

About 80 sites fully qualify a function whose module is not imported at all, which is against the
house style. A type is imported directly, and a function gets at least one module-level import
unless ambiguity forces qualification. The change is mechanical, so do it as its own pass to keep
the diff readable.

## Open: what the fuzzers actually feed the code

An independent review measured the generators rather than reading them. Every number came from a
probe that reimplements each generator arm byte for byte, with the same RNG constants, seeds and
budgets, and from real `-C instrument-coverage` region counts. A default battery run generates
11,735 inputs, and 98.7% of the wall clock goes to the two batteries with the worst input quality.

### The format battery's 4,500 generated inputs add 8 regions of 2,951

Leave-one-out region coverage of `format.rs`, where the whole battery reaches 2,771 of 2,951:
dropping `fuzz_random_bytes` loses none, dropping `fuzz_seed_mutations` loses none, and dropping
token soup loses 4. Dropping all three as a block takes 2,771 to 2,763. `fixture_sources_hold_invariants`
alone covers 2,762 and contributes 410 unique regions.

The earlier figure of 14 in 1500 for the random-byte arm reaching the formatter body is confirmed,
and it is worse than it reads. Eleven of the 14 are empty or whitespace, so the arm formats a
program with at least one token three times in 1500, and never one with ten tokens.

Delete the random-byte arm. Cut token soup to about 200, because it is the cheapest source of
parser-error shapes at 33 of 35. Seed the mutations from the fixture corpus instead of from the 35
hand-written seeds.

### The generators cannot express most of the type system

Normalizing diagnostics to message shapes, all three semantics fuzz arms reach 60 shapes, of which
33 are parser errors, 9 are distinct `type-mismatch` shapes and none are lints. The legacy corpus
reaches 110 and the typing fixtures reach 80.

In 250 generated programs the annotation grammar produces `TYPE_REF`, `TYPE_FUNCTION` and
`TYPE_RECORD`, and no unions, binders, applications, vectors, `list[T]`, tuples, parens, optional
`[x]:` parameters or rest `...r:` parameters. The generator calls 6 of 872 declared stub names and
reaches 1 of 37 overload sets. No harness emits `library(...)`, so every conditional namespace and
the whole non-standard-evaluation ladder is unreachable, and nothing anywhere emits `setClass`,
`setGeneric`, `R6Class` or `new()`. `metadata.rs` sits at 7.63% of regions, and `lints.rs` produces
no findings in 250 programs.

### Grammar-directed generation, prototyped and measured

A 250-line recursive generator over the R grammar crossed with the `#:` grammar produced 1,500
programs in 0.338 s.

| metric | best current arm | grammar prototype |
|---|---|---|
| parses clean | 35.3% | 100% |
| formats with 10 or more tokens | 8.0% | 41.3% mutated |
| `parser.rs` regions | 87.27% | 93.46% |
| semantic regions across 9 files | 11,073 | 12,888 |
| diagnostic shapes | 60 | 95 |
| distinct `type-mismatch` shapes | 9 | 17 |
| lint shapes | 0 | 5 |
| `ide::type_definition` hits | 0 of 7,884 | 80 of 23,826 |

Add it alongside the existing arms rather than replacing them, because token soup still owns 24
parser-error shapes. Semantic coverage then goes from 11,073 to 13,306 regions. The typing fixtures
still lead at 14,580, so the corpus beats synthesis and both beat noise.

### The best inputs are already in the tree, and only one crate uses them

`fixture_sources_hold_invariants` exists only in `format`. The same 388 typing-fixture sources run
through the semantic pipeline reach 86.92% of `check.rs` and 80 shapes, against the entire generated
battery's 54.35% and 60. Wire it into `syntax`, `semantics` and `ide`, and use both corpora as
mutation seeds rather than only as fixed inputs.

The legacy-corpus arm as landed deserves one fair criticism. It runs one shared database over all
1,967 files, calls `file_diagnostics` only, and asserts never-panic plus range geometry. That shared
project changes what is tested: `unresolved` collapses from 284 to 148 while `duplicate` explodes
from 20 to 2,182, because 1,967 unrelated files redeclare each other's names. Run per file with the
full battery and it reaches 110 shapes. Fix it by batching into projects of a sane size and running
the full `check_semantics_invariants`. The cost is fresh databases, at about 9 ms each in release
and 113 ms in debug, so this wants the batteries in release or a lower count of databases per input.

### The IDE battery costs 216 s and never reaches the type-driven features

`type_definition` returns `Some` zero times in 7,884 offsets. That is structural, because it needs a
`TyKind::Named` and no IDE seed declares a `@type`. `signature_help` fires at 0.5%, and 86.7% of
inputs do not parse. Swapping the 10 hand-written seeds for grammar-generated programs that declare
and use nominals took `ide.rs` from 52.58% to 68.38% on fewer inputs. Cap a generated program at
about 10 statements, because a 150-input grammar sweep cost 81 s against 45 s for 300 tiny mutated
ones.

### The semantics incremental invariant never performs a small edit

`check_pipeline_reporting` derives its edit by generating an unrelated program, so the computed
splice covers 97.3% of the old text on average and only one pair in 250 touches 10% or less.
`syntax::reparse` therefore takes the full-parse fallback essentially always, and the engine sees
whole-file invalidation every time. The splice-reuse path and per-item early cutoff, which are the
architecture's core claim, are never exercised. Derive the edit from the source instead, by
replacing or inserting one statement at a boundary.

### Smaller items, each with its measurement

- **The syntax edit stream degenerates, and its coverage is fine anyway.** No buffer of 400 parses
  clean, only 89 distinct shapes appear, and 60 steps lex to two tokens because an inserted `"`
  swallows the buffer. Yet `reparse.rs` sits at 96.12% against a grammar-directed stream's 96.44%.
  Reset the buffer every 20 steps or so and skip a near-empty one. Do not rewrite it.
- **Coverage-guided fuzzing has never run.** `cargo fuzz` is not installed, `fuzz/corpus/` does not
  exist, no CI job invokes it, and the root `cargo test` resolves to the product crate so
  `fuzz_deep` never runs either. Meanwhile the `REGRESSIONS` arrays, 21 in `format` and 2 in
  `semantics`, are documented finds from exactly that mechanism. `fuzz/` does still compile, in 83 s
  under `cargo +nightly check`. Judging coverage costs one component, because
  `rustup component add llvm-tools-preview` produced every number in this review.
- **Two arms silently contribute no inputs.** `corpus/` does not exist and nothing in CI or the
  justfile fetches it, so `fuzz_corpus_seeded` in `syntax` and in `format` returns early. That is
  about 1,050 budgeted inputs that never run, behind a skip line `cargo test` hides. It is the same
  "green means nothing ran" species the `FIXTURE_FILTER` guard already fixed.
- **Three surfaces have no fuzz coverage at all.** Generated `.Rtypes` text never reaches the stub
  loader, which leaves `stubs.rs` at 60.67%. `PackageMetadata` appears in no harness. The IDE arm
  only ever uses `DocumentKind::Package`.
- **The review's fourth item here was wrong, and the correction is the useful part.** It reported
  `syntax::literate::r_source_of_literate` as having no test of any kind. It has eight unit tests,
  and three of them already assert the byte-length invariant, including one for multibyte prose. The
  real gap was only that the invariant was pinned on four hand-written documents rather than over
  generated input, which the `syntax` battery now closes.

## Open: what each hour of fuzzing compute buys

A third independent review asked what protection each hour of compute and each minute of developer
time actually buys. Every number came from a 4-vCPU machine, in the debug profile unless stated.

The headline is that fuzzing is 327.3 s of a 672 s local gate, which is 49%, and CI runs none of it.
The rest of the gate is `legacy/` at 249.0 s, the shipping crates' fixture suites at 68.8 s, and
`crates/ry`, which is the only thing CI runs, at 22.4 s.

This review disagrees with the input-generation review above, and the disagreement is real. That one
wants the generated arms shrunk across the board. This one measures per-arm cost and wants `syntax`
and `format` grown, at 0.19 to 0.31 ms per input, while `ide` and `semantics` shrink, at 257 to
644 ms per input. Both conclude independently that `format::fuzz_random_bytes` should be deleted.

### CI runs 171 of the workspace's 712 tests, and its second job runs none

The root `Cargo.toml` sets `default-members = ["crates/ry"]` and the workflow omits `--workspace`.
`cargo test --all-targets --all-features -- --list` therefore reports 171 tests, all in `crates/ry`,
against the workspace's 712. Every fuzz arm lives in another crate. The second job runs
`-- --ignored`, which lists no tests for the same reason, so it is a full release build with
`lto = true` followed by an empty test run, on every push, under a 45-minute timeout.

### `ide::fuzz_deep` makes a workspace-wide CI run time out

`ide::fuzz_deep` runs `iterations().max(5000)` sweeps. That `.max` floor means `FUZZ_ITERS` cannot
lower it, which is the bug. Measured in release on that exact input shape, which is 1 to 12
concatenated seeds averaging 214 bytes over 111 offsets per sweep, one sweep costs 665 ms. Five
thousand sweeps is 55.4 minutes on 4 cores, against the job's 45-minute timeout on a 2-to-4 vCPU
runner. That is before `semantics::fuzz_deep`, projected at about 4 minutes, and before the
`test_stats.rs` instruments, which hard-assert on a corpus CI never fetches.

Fix the floor, make the stats instruments skip on an unfetched corpus the way `test_corpus.rs` does,
and raise the timeout with measured headroom. All three come before the human moves
`.github/pending-ci.yml` into place.

### This is not a fuzzer, it is a fixed 498-program corpus re-derived at 124 s a run

Every generator seeds `SplitMix64` from compile-time constants with no entropy. Two runs of the
semantics generator produce an identical set of 498 distinct programs, while drawing 100,000 times
from the same generator reaches 87,203 distinct programs. The default budget samples 0.57% of its
own generator's reach and re-samples exactly that 0.57% forever.

Split the two jobs it conflates. Keep a small fixed-seed arm as the regression net it actually is.
Give the exploratory arms a `FUZZ_SEED` environment variable defaulting to random, print the seed on
every failure, and run them on a schedule rather than in the blocking gate. Do not randomize the
blocking gate, because a failure surfacing on an unrelated change is a worse trade.

### The syntax arm is the best asset in the test architecture and gets 0.5% of the budget

Distinct observable parser behaviors, counting error templates and node kinds, against iteration
count:

| syntax arm | 100 | 500 | 1500 (default) | 6000 | 20000 | last new behavior |
|---|---|---|---|---|---|---|
| token_soup | 67 | 98 | 109 | 124 | 132 | iteration 19,024 |
| random_bytes | 19 | 36 | 42 | 51 | 62 | iteration 17,012 |
| seed_mutations | 71 | 107 | 124 | 143 | 158 | iteration 18,253 |

All three at 20,000 iterations, which is thirteen times the default, cost 7 s total in debug and
were still finding new parser behavior at iteration 19,024. Cost per input across the battery is
0.19 ms for syntax, 0.31 ms for format, about 257 ms for semantics and 644 ms for ide. That is a
spread of 3,400 times, with the weakest oracle sitting on the most expensive input. Raise the syntax
budget to 20,000, which costs 7 s for 28% more distinct behaviors, and pay for it out of the
completion fix.

### The battery pays a debug tax of 6.1 times for identical assertions

| binary | debug | release | speedup |
|---|---|---|---|
| syntax test_fuzz | 1.58 s | 0.10 s | 15.8x |
| format test_fuzz | 2.31 s | 0.21 s | 11.0x |
| semantics test_fuzz | 123.68 s | 8.62 s | 14.3x |
| ide test_fuzz | 199.77 s | 44.40 s | 4.5x |
| total | 327.34 s | 53.33 s | 6.1x |

Building the four release test binaries costs 143 s warm.

The semantics figure has one root cause, measured directly. `install_shipped_stubs` is 0.4 ms,
because it sets an input, but a fresh database plus stubs plus a render is 118.2 ms against 5.0 ms
without stubs. That is 113.2 ms of stub re-parse per fresh database.
`check_semantics_invariants` builds three fresh databases per input, at 394.8 ms, so 86% is stub
re-parse, and 97.7% in release. A render on a warm shared database is 3.8 ms, which is thirty times
cheaper. An earlier fix reduced the database count from four and the ratio barely moved, because it
was never about database count alone.

Run the fuzz targets in release from `just gate`, and make a shared database the default arm, with
fresh ones only where determinism or incrementality genuinely needs them.

### Coverage-guided fuzzing has no ratchet, so nothing it learns survives the run

`fuzz/` does compile on stable today. `cargo check --all-targets` inside it takes 75 s cold with an
isolated target directory and 1.6 s warm, so the compile-rot risk flagged earlier is real but has
not happened.

`scripts/seed-fuzz-corpus.rs` writes 1,416 files and 5.7 MB into each of three corpora in 2 s, but
it globs `.Rtypes` under `crates/`, where there are none. All 11 `.Rtypes` files, and 33 `.exports`
files, live in the top-level `types/`. It therefore seeds no stub files, none of the 1,967 mined
`corpus-legacy/*.R.corpus` programs, and it never looks at `corpus/`. Its own doc comment claims it
adds the shipped stubs, and the testing page repeats that. Both are false.

`fuzz/corpus`, `fuzz/artifacts` and `fuzz/Cargo.lock` are gitignored and no job persists them, so
every run restarts from the same seeds and rediscovers the same shallow frontier. `REGRESSIONS`
exists in `format` with 21 entries and in `semantics` with 2, and in neither `syntax` nor `ide`.
Both constants were last modified about 60 commits ago, so the pinning path works, at 0.12 s for
`format` and 0.94 s for `semantics`, and is simply not being fed.

In value order: persist the corpus, fix the seeder to read `types/` and `corpus-legacy/`, and add
the 1.6 s `cargo check` of `fuzz/` to the gate.

### The one failure mode fuzzing exists to catch is the one that prints no input

`catch_unwind` appears once in `syntax`, once in `format`, twice in `semantics` and not at all in
`ide`. In `syntax` and `format` the single wrapper is on the legacy-corpus arm only.

Every `assert!` in `check_parse_invariants` and `check_format_invariants` embeds `{input:?}`, so an
assertion failure replays fine. A genuine panic inside a generative arm prints a backtrace with no
input, and a stack overflow aborts printing nothing. A stack overflow is a live risk the harness
itself acknowledges, through `deep_nesting_is_refused_not_fatal`.

This is recoverable today only because the seed is fixed, and that property disappears the moment
`FUZZ_SEED` lands, so this has to land with it. `semantics::check_pipeline_reporting` is the correct
pattern and is already in the tree.

### Two arms are silently dead and one is 99% wasted

`syntax::fuzz_corpus_seeded` costs 0.11 s and `format::fuzz_corpus_seeded` costs 0.16 s, which is
process startup only. Both skip through an `eprintln!` the blocking job never displays.

`format::fuzz_random_bytes` reaches the formatter body 14 times in 1500, which is 0.9%, against
token soup's 90. So 1,396 of its 1,500 iterations only re-test a refusal path `syntax` already
covers on the same generator. Drop it and give the budget to `fuzz_seed_mutations`.

### Radical alternatives, costed

- **The release profile for the battery.** 327 s becomes 53 s, for 143 s of warm build. This is the
  highest value per line in the pipeline.
- **A nightly coverage-guided run with a persisted corpus.** About 10 minutes per target, so about
  30 minutes a night on a free runner. Worth it only with persistence. Without it, that is 30
  minutes a night rediscovering the same frontier. A human has to add it, because it needs workflow
  scope.
- **OSS-Fuzz.** The three targets are already thin wrappers over exported batteries, so it is a good
  fit. Do it only after the replay path, meaning `catch_unwind` plus seed printing, and the pinning
  path exist. Otherwise reports arrive with nowhere to go.
- **`cargo-llvm-cov` over the batteries.** Not attempted. The saturation curves above answer the
  same question in about 7 s. Reach for llvm-cov when deciding which invariants to add, not how many
  iterations to run.

### Who lands what

An agent can land all of these today: completion sampling, the release profile in `just gate`, the
syntax budget raise, `catch_unwind` plus seed printing, `FUZZ_SEED`, the seeder fix, the `fuzz/`
type-check guard, the `fuzz_deep` floor and the stats-instrument skip that unblock the CI move, and
the dead-arm cleanup.

A human is required for three things: the `git mv` of `.github/pending-ci.yml`, which needs workflow
scope and must not happen before the `fuzz_deep` floor is fixed, any scheduled fuzz job and its
corpus cache, and an OSS-Fuzz submission.

## Open: test and fuzz architecture

A second independent review of the fuzzing and test architecture, with every claim measured rather
than read off the code. The findings it shares with the two reviews above are written up there. These
are the ones only it found, ranked, each stating the change and why it is worth it.

### The formatter battery cannot notice the formatter deleting code

`check_format_invariants` asserts determinism, idempotence, and that the output re-formats. A
formatter that silently dropped a statement passes all three.

The missing oracle is the sequence of non-trivia token kinds, excluding `{`, `}`, `;` and
`ANNOTATION_MARKER`, because the formatter legitimately adds braces, splits `;` chains and re-lays
out `#:` blocks. It was built and run, with no mismatch over 1,182 fixture case sources, of which
972 formatted, and 20,000 fuzz-shaped inputs, of which 4,073 formatted. The weaker formulations do
not hold: raw token equality fails on 50 brace and semicolon insertions, and text equality fails on
9 normalizations of `'x'` to `"x"`. So the assertion to write is kinds with those four excluded, at
about ten lines.

### `crates/ry` has no property coverage, and the protocol edge panics

The obvious property over `crates/ry/src/position.rs`, that every byte offset converts and
round-trips, finds 11 panics and 2 round-trip failures in twelve lines. There are two distinct
defects.

- `line_column_utf16` and `line_column_chars` slice `text[start..start + byte_column]` and panic for
  an offset inside a character. `line_column_utf16(1)` on `"é😀x\n"` is an example.
- `offset_utf16` clamps to a line length that has already had `\r\n` trimmed, so an offset at a CRLF
  terminator does not round-trip. On `"a\r\nb\r\n"`, offset 2 comes back as 1.

One caveat, stated honestly: every offset this module produces is on a boundary, so the panic was
not shown reachable from today's callers. But "an IDE feature never panics on a stale range" is a
stated soundness invariant with nothing enforcing it, and the CRLF break sits on the Windows default
path.

### The IDE arm asserts almost nothing

Of 13 feature calls per offset, only `hover` is checked, and only for determinism. Eight are
`let _ = ...`, which is never-panic only.

Nothing asserts the invariant that actually matters and that memory already states: every range a
feature returns lies inside the file. `definition`, `references`, `rename`, `type_definition`,
`code_actions`, `inlay_hints` and `document_symbols` all hand ranges straight to the editor. Assert
in-bounds ranges and determinism for every range-returning feature, and pay for it with the offset
sampling the economics review costs out. That is the same wall clock for several times the oracle.

### About 280 lines of fuzz harness are copy-pasted across four crates

`SplitMix64` exists four times, three of them byte-identical and the fourth adding `chance()`.
`iterations()` exists four times identically. `corpus_sample()` exists twice, byte-identical apart
from one comment. The byte-mutation loop exists three times.

All four test crates already depend on `syntax::testing` for `env_var`, so the shared home exists.
Put `rng`, `iterations`, `corpus_sample` and `mutate_bytes` there. That replaces four copies with
one, and it is not a new abstraction.

### Seeds are hand-maintained while 1,183 fixture sources sit unused

`syntax::testing::parse_fixture_files` is public with no callers. Its doc comment advertises the
cross-stack differential harness, which retired with the identity-parity program.

Meanwhile the batteries seed from 81 hand-written strings and never see the 1,183 fixture cases,
which are the richest R corpus in the repo and the one that grows with every change. Seed the
`syntax` and `format` batteries from it, delete the three `SEEDS` lists, which is about 110 lines,
and keep a hand seed only where it encodes something the fixtures do not. For `semantics`, use
fixture sources as mutation seeds rather than one per input, because 1,183 times 480 ms is 9.5
minutes.

### Two real invariants hold today, are unasserted, and cost about five lines each

- **An `ERROR` node implies a reported error.** Over 20,000 fuzz-shaped inputs there was no
  counterexample. The converse legitimately fails, with `for (x in items)` among six examples, so
  only this direction is assertable. Without it, a silent parse failure lets the formatter mangle a
  file that reports clean.
- **Reporting is monotone.** One, two, four, sixteen and sixty-four copies of one faulty item yield
  one, two, four, sixteen and sixty-four findings. One, four, thirty-two and one hundred
  twenty-eight faulty statements in one item yield two, eight, sixty-four and two hundred
  fifty-six. This is precisely the invariant a per-item or per-file finding cap breaks, which is the
  regression three adoption reviews called a blocker, and nothing guards it.

### A timing-based scaling guard does not discriminate, so read this before retrying

The idea is right and the obvious implementation does not work in the default battery. The attempt
is recorded with numbers so the next one starts further along.

Normalizing by the growing unit is the first trap. Per-declaration cost falls as declarations grow,
because most of the run is fixed work, so that assertion passes against the very bug it targets.

The sound method is a four-corner interaction test. Measure `(I, D)`, `(2I, D)`, `(I, 2D)` and
`(2I, 2D)`, then compare the last against the additive prediction, because a table copied per item
is exactly the interaction term. Measured in debug at `I=200` and `D=300`:

| | per-item copy present | after the fix |
|---|---|---|
| additive prediction | 699.7 ms | 663.2 ms |
| actual at (2I, 2D) | 717.2 ms | 641.5 ms |
| interaction | +17.5 ms | -21.7 ms |

A 39 ms swing on a 700 ms total is about 5%, which is indistinguishable from load noise. The signal
separates only at sizes where the copy dominates, which is around 2,000 items by 2,400 declarations,
where the release measurement showed 71 ms against 27 ms. Four corners at that size cost far more
than a default suite should. An `#[ignore]`d witness was considered and rejected, because the
extended job that would run it does not currently run any of these suites, so it would be machinery
nobody invokes.

What would work is a structural assertion that counts the copies directly, rather than a timing one.
Nothing observable exists for it today.

### No witness measures one file with many top-level items

`stats_witness` is corpus-wide, meaning many files. The shape that hid the known quadratic, which is
one file with many annotated items, is measured by nothing.

It is linear today: 100, 400 and 1,600 items cost 0.24 s, 0.65 s and 3.36 s, which is 2.43, 1.62 and
2.10 ms per item. One assertion that milliseconds per item at 1,600 stays within about twice
milliseconds per item at 200 is the only cheap guard against the highest-severity performance class
this project has actually shipped.

### Smaller confirmed items

- `crates/format/tests/test_format_docs.rs` falls back to `text.to_owned()` when a block header is
  not `# name: directive`, which publishes unformatted input as if it were formatter output. No
  block hits it today, across 48 blocks with no fallbacks, so it is latent. Call `panic!` instead,
  in one line.
- `crates/syntax/tests/test_error_messages.rs` draws its own carets with its own line and column
  math, so the golden suite pins a caret the product never prints. There are three newline-table
  implementations in the shipping crates: `ry::position::LineIndex`, `format`'s `line_starts` and
  `line_of`, and this renderer. One `LineIndex` in `syntax`, used by all three, deletes two of them.
- Dead code. `let _ = line_start;` in `crates/syntax/src/testing.rs` and `let _ = before_len;` in
  `crates/syntax/tests/test_fuzz.rs` keep unused locals alive. `floor_char_boundary` and
  `ceil_char_boundary` in that same file, and the same backward scan in
  `crates/semantics/src/testing.rs`, are now in std. `probe/` is an empty untracked directory at the
  repo root.
- The testing page claims each harness pins every input its targets have ever broken. Only `format`
  and `semantics` have a `fuzz_regressions_hold_invariants`. `syntax` and `ide` have none.
- `fuzz/` is its own workspace, so nothing in the default battery type-checks the three libFuzzer
  targets. Renaming a battery function breaks them silently until someone runs cargo-fuzz.

### Suspected, not confirmed

- `ide::completion` does work proportional to the corpus per call and is not memoized, at 15.9 ms
  warm and essentially independent of file size. That is a debug figure. One release measurement is
  warranted before any claim about product latency, because it is a per-keystroke path.
- `ProjectFiles::set_files`, which is the file-set change the server makes on open, close, create
  and delete, is never fuzzed. Every incremental arm mutates `set_text` only. Probing add, remove
  and reorder against fresh databases with cross-file interface cycles found them all equivalent, so
  this is a coverage gap rather than a live bug. It is the mutation that moves cross-file resolution
  winners.
- Lowering, naming and inference have no stage-local invariants. They are covered only transitively
  through `render_semantics`. That is defensible as end-to-end fuzzing, and the doctrine's per-stage
  wording overstates what is asserted. There is no naming invariant saying that every `NameRef`
  resolves or is reported, and no HIR range-containment invariant.

### Judged fine, so do not spend time here

The fixture format and harness are sound: 1,183 ids across 11 suites, none malformed, duplicate-id
rejection works across files, and bless rewrites only the expectation span. The four
empty-expectation cases are all legitimate negative contracts. The `syntax` fuzz suite is the best
thing in the test architecture, with five real invariants plus splice-reparse equivalence against
from-scratch, covering the tree and the errors, over an edit stream, for 0.89 s. Keeping regressions
in `REGRESSIONS` rather than in a committed corpus is right, with `fuzz/corpus` gitignored.

Two more things are deliberately not worth fixing. `render_semantics` and the typing runner's
`render_file` share a scheme loop and render different fact sets on purpose. `render_with_strict`
builds a second database per case, which costs 114 ms across 22 cases.

## Open: reports from the maintainer

### REPL resize is not tracked

The width fix itself is closed and archived in `test-user-reports.md`. What stays open is the
mechanism. A stock terminal session handles `SIGWINCH` by calling `R_SetOptionWidth`, which R does
not export. The console feed is the only other way in, and that is the mechanism already shown to
desynchronize the editor when used between prompts. Worth revisiting if someone finds a third route.

### A variable is reported as unused, and this needs a reproduction

The maintainer's note arrived truncated, reading only that a variable is reported as unused, so the
triggering shape is unknown. Do not guess at it. The `unused` warning has several paths, which are
script liveness, the package top level, parameters, and the interaction with `shadows-namespace`. A
fix aimed at the wrong one is worse than none. Ask for the snippet before working it.

## Open: diagnostic wording is not styled consistently

Six findings shown side by side in the README read as though five people wrote them. Three
`type-mismatch` messages are lowercase sentence fragments. `Unexpected comma after last argument` and
`Use TRUE, not T, for Boolean values` are capitalized, and the second is imperative. ``` `tmp` is
assigned but never used. ``` is the only one carrying a full stop. A README reviewer caught it,
noticing that the page claims a finding says the same thing everywhere while the sample visibly
disagrees with itself.

**The obvious rule does not survive measurement.** Across 62 message literals in `diagnostics.rs`,
36 are lowercase with no period, 14 are lowercase with a period, and 9 are capitalized. So
"lowercase fragment, no period" is the plurality and nothing like a convention. It also cannot be
applied blanket, because the capitalized ones are capitalized for a reason. `R's $ operator` opens
with a proper noun, and `I could not resolve ...`, `I do not know the type ...` and `I cannot
construct an infinite type` are first-person sentences. Lowercasing those yields `r's` and
`i could not`.

The rule that fits the corpus has two shapes. A message that is a complete sentence is sentence-cased
and ends with a period. A message that is a fragment, which is the expected-and-found family, is
lowercase with no period. Under that rule the real violations are far fewer than "five people wrote
them" suggests, and are mostly sentences missing their period.

This is unblocked. The reporter change that re-captured every rendered sample has landed, so
rewording a message now invalidates only the samples that quote it. Those samples have to be
re-captured in the same change. There is no harness for that yet, so it is a manual pass over the
pages named in the next item.

## Open: nothing verifies the rendered samples in the docs

About two dozen blocks show real tool output. They live across `features`, `getting-started`,
`reference/cli`, `reference/configuration`, `reference/diagnostic-codes`,
`type-checking/tutorial`, `type-checking/concepts`, `type-checking/domain-modeling`, `index.astro`
and the README's generated `.github/diagnostic.svg`. Nothing checks them, so they drift silently and
the drift is only ever found when someone re-captures by hand.

Two things this proves rather than predicts.

- A reporter change left every one of them stale for days, and the cost of re-capturing them by hand
  is most of what made that change expensive to land.
- A parser change, narrowing an unterminated argument list's boundary, silently falsified the
  headline missing-comma sample in `features.md`. That is the very example chosen to show off the
  parser. Nobody noticed until the sample was re-run for an unrelated reason, and `main` had shipped
  it wrong. This is the failure the "run the tool before claiming what it does" rule exists to
  prevent, and that rule does not scale to prose that was true when it was written.

**Shape of the fix.** Each sample needs its project, meaning a `ry.toml` and one or more source
files, and its command recorded next to it, so a harness can rebuild the project, run the command
and diff the captured block. There are two candidate homes. One is a fixture-suite-style directory
whose renderer output is the docs block. The other is front matter or an HTML comment in the page
naming a fixture directory. Prefer whichever keeps the docs readable as prose, because the samples
are teaching material rather than test data. A block that excerpts one finding out of several needs
a way to say so, through a first-N-findings selector or a filter by code, because several pages
legitimately do that.

Until it exists, treat "changed a message or a blame range" as implying a manual sweep of the pages
above, and say in the commit which ones were re-run.

## Open: findings from the second simulated-user round

Five simulated users each worked on a distinct project of their own writing, learned the tool from
the docs alone with no source access, and tried to reach a clean run. The reports are the
`users/feedback-*.md` files from that round. The findings that were fixed are ledger entries at the
bottom of this file. What follows is what is still open, by theme, with the tester who found it
named where their judgment is the point.

### A clean run still does not mean enough

Every tester reached this conclusion independently, and it is the round's headline. The Shiny
dashboard tester finished with exit code 0 and five real bugs still in the code. The ETL engineer
would not use `typing = true` as a merge gate, not because it is noisy but because "after these, a
green run doesn't yet mean what it needs to mean at 3am". The Quarto analyst would have been "fully
off by end of day, never knowing the default checks were not running".

Two holes of that kind are still open.

- **A package with no manifest still buys the blanket tolerance.** Fifteen manifest-only CRAN
  namespaces now close the common case, but a package a user names, such as `janitor`, still
  switches unresolved detection off project-wide. The remedy is a two-line project stub, and the
  limitations page says so on the user-facing side.
- **A column name inside `DT[...]` is never validated.** `orders[, net := gross_ammount * 1.19]`
  and `by = currrency` are both silently accepted, while the only name that did report was a correct
  one. That is exactly inverted: silent where the typo is real and noisy where the name is right.
  `limitations.md` should say plainly that column names are never validated.

### Two configuration defects that break CI

- **Setting `[check] exclude` disables the built-in `renv/` and `packrat/` skip.** With no key the
  walk finds 1 file. With `exclude = []` it finds 1 file. With `exclude = ["nothing-here/"]`, which
  matches nothing, it walks renv and packrat both. The docs' own example, `exclude = ["scripts/"]`,
  would drag an entire renv library into CI. A user-supplied list must extend the vendored defaults
  rather than replace them.
- **Any file in `stubs/` deactivates the shipped conditional namespaces.** One stub for an unrelated
  package made 19 `data.table::` calls unresolved while bare `fread` still resolved, and it made
  `data.table` unusable as an annotation type name, which silently suppressed a real type error.

### The traits tripwire, tripped a fifth time

A `<T: numeric>` bound admits a `Date`, and the body's arithmetic then fails in R. A helper
annotated `#: <T: numeric> fn(a: T, b: T) -> T` and called as `add_them(d1, d2)` is accepted, while
the direct `d1 + d2` is correctly rejected with ``\`+\` is not defined between \`Date\` and \`Date\```.
A one-line helper therefore launders a real error.

The mechanism is the `declares_arithmetic` relaxation: a class that declares any arithmetic method
satisfies `Numeric` wholesale. That does not follow. `Date` declares `+.Date` for `Date + integer`
and has no `Date + Date`.

A constraint meaning "supports this operator with these operands" replaces both this relaxation and
the operand tie. Do not patch it again. Design it.

One related leak: `sort`, `head` and `rev` on dates report ``found \`T[]\```, which leaks an unbound
type variable.

### Nominal subtyping, which is the same deferred design as S4 and R6 inheritance

`data.table` is rejected where `data.frame` is expected, and the wrong direction is accepted.
`needs_df(data.table(a = 1L))` is legal R and reports `expected data.frame, found data.table`, while
`needs_dt(data.frame(a = 1L))`, the direction that really is wrong, reports nothing. R's class vector
for a data.table is `c("data.table", "data.frame")`.

`TyKind::Named` matches by exact name and nothing in the compatibility relation carries a hierarchy.
The tractable corner is a declared, acyclic nominal-extends-nominal relation in the stub vocabulary.
Do it as that design, not as a data.table special case.

### `Date` does not survive real code

It survives `d + 1L`, `d2 - d1`, `d1 < d2`, `format`, `seq.Date` and a `while` cursor. It is lost to
`Unknown` through `dates[i]`, `dates[mask]`, `for (d in dates)` and `Reduce`. It is wrong through
`min` and `max`. The same bug is therefore caught in one place and missed in another: `cursor + cursor`
after a `while` loop is flagged, and the identical `d + d` inside `for (d in dates)` is not. No doc
describes this boundary.

Two related gaps. `#: @type TradeDate {Date}` disables all Date arithmetic, so `settle - trade` is
rejected, which kills the guide's own recipe for domain types over dates. The working route is a
project `.Rtypes` declaring `-.SettleDate`, which the guide never mentions, and a stub cannot see an
inline `@type` anyway. Separately, `Date[]` is inexpressible, and `Days + Days` yields `double`, so a
units tag dies on arithmetic.

### Strict mode's own advice does not work

The message says to add a type annotation, and no annotation form silences it. `#:`, `@if-unknown`,
`@trust` and `#: Any` all still report, and the last contradicts the reference directly. Only
`# ry: allow(strict)` works.

### Missing type-system coverage, each with its repro

- **A parameter's type is not seeded from its default value.** `f <- function(s = settings) s$hsot`
  is missed while `settings$hsot` and a local alias are both caught. The obvious fix generates false
  positives, so this needs the design rather than a patch. Binding the parameter to its default's
  type makes every caller passing anything else wrong, and reporting the field against the default's
  shape breaks the commonest idiom of all, `function(x = list()) x$name`, where the default is an
  empty accumulator and callers supply the real record. R answers `NULL` there, so an error would be
  plain wrong. The honest model is that such a parameter is the default's type unioned with whatever
  callers pass, which is still open, so a field access on it should be at most `T | NULL`, the same
  rule the accumulator fix established for unions. Recovering the tester's case needs to know the
  argument is never supplied anywhere, which is whole-program information the checker does not have.
  Leave it missed rather than trade it for the class of false positive this round spent its time
  removing.
- **`&` and `|` are unmodelled**, so `TRUE & FALSE` is `Unknown` and every logical mask kills
  downstream checking. The reference documents `&&` and `||` with no counterpart, and the only
  mention of `&` is a parenthetical inside the non-standard-evaluation discussion.
- **A maybe-`NULL` value is only caught by arithmetic.** `toupper`, `nchar` and `substr` on a
  `character | NULL` all pass, despite the guide promising this class of check.
- **Recursion defeats `is.logical()` narrowing.** The identical non-recursive function is clean.
- **`$` on an unannotated parameter constrains nothing**, so the parameter accepts anything.
- **`#: @new` does not stop `structure()` being a strict origin**, so the documented S3 remedy fails
  at exactly the site it exists for.
- **S3 generic and method signature consistency is unchecked.** Both planted violations were missed,
  and `R CMD check` catches both. ry already does the rarer undefined-export check.
- **`vapply(x, f, numeric(1))` errors whenever the callback is not statically `double`.** R accepts
  a wider template, which the tester confirmed with `ry run` on
  `vapply(1:3, function(i) i, numeric(1))`.
- **`source()` is not followed**, so a helpers file is pure noise in both directions. All ten helper
  functions reported `unused`, and every correct call to them reported `unresolved`, which is
  indistinguishable from the planted typo. It also killed the wrong-arity catch, which works well
  within a file.
- **A function name inside `mutate` and `filter` is swallowed** along with the column names.

### Two lints need a model change rather than a patch

- **`shadows-namespace` fires on a local inside `test_that()` and calls it a top-level binding.**
  Naming is right that the binding lives in the file's frame, because R braces are not a scope and
  the checker cannot know `test_that` makes an environment. What the lint needs is syntactic
  nesting, meaning "written as a direct statement of the file", which the binding model does not
  currently distinguish from "belongs to the file's frame". Adding that must not disturb the unused
  rules, where `TopLevel` means package-visible. Both shadow lints are default-off, so this waits for
  the model change rather than a special case. The wording is part of the bug, because a local two
  levels deep should never be called a top-level binding.
- **`unused-parameter`, `strict` and `shadows-namespace` were all unusable** on a package with an S3
  class and a testthat suite, in the package author's words. Two of the three causes are fixed. The
  shadow lint above is the third.

### Formatter and message defects

- **`m[i, , drop = FALSE]` becomes `m[i,, drop = FALSE]`**, which removes the space that makes the
  empty dimension slot visible, in exactly the code where miscounting slots is the bug. It is
  idempotent, so it is deliberate, and it is undocumented.
- **Any `X[Y]` annotation reports "only one compact annotation fits in a `#:` block, separate the
  annotations with a blank line" when there is only one annotation.**
- **An unclosed chunk reports the tester's English prose as an unresolved variable** rather than the
  missing fence.

### Documentation gaps the round hit

- The adjacency rules say a plain `#` comment breaks `#:` attachment and never mention `#'`. So
  interleaving `#: @param` with roxygen's `#' @param`, which is the first thing a roxygen user
  tries, fails.
- The guide never shows `...` or `[optional]` in an annotation, and every S3 method needs both.
- `configuration.md` omits `DESCRIPTION` as a project root marker, and a three-line `DESCRIPTION`
  with `Imports:` was what fixed 30 of the ETL engineer's 34 first-run warnings. The path the docs do
  recommend, hand-writing `stubs/*.Rtypes`, cost five files, fixed less, and was actively harmful.
- The guide's showcase uses `list(...)`. Swapping it for `read.csv()` makes the identical mistake
  report nothing, and the page calls that the whole pitch with no caveat at the point of contact.

### What the round praised

Recording this matters, because several of these were the parts the testers expected to be broken.

**Literate handling is the strongest part of the tool.** Line numbers were exact across all 545 lines
of `.qmd` and `.Rmd`, ASCII columns exact, prose ignored, and every chunk-header form parsed,
including `#| fig-cap: "A caption with = signs"` not fooling the `=` lint. `{python}`, `{sql}`,
`{bash}` and `{ojs}` fences and bare fences were all correctly skipped, CRLF and Sweave and no-YAML
all mapped correctly, and suppression comments worked inside chunks.

**Date and time operator modelling is the best static checking of R dates the quant had seen.** Every
date expression R rejects or warns on was caught across 21 probes, with no misses and no false
alarms, including `Sys.time() - Sys.Date()`, which R only warns about and which they have shipped to
production. ``\`-\` is not defined between \`POSIXct\` and \`Date\``` was "the best message in the
tool".

**The data.table bracket really works.** A 162-line transform module using `:=`, `.SD`, `.SDcols`,
`.N`, `dcast` and a non-equi join with `by = .EACHI` produced no complaints about non-standard
evaluation. There were also zero false positives on 50 lines of idiomatic tidyverse, covering bare
column names through the whole verb set, `case_when`, `across(where(is.numeric), ~ .x * 2)`,
tidyselect helpers, joins and `aes()`, which was the Quarto analyst's biggest fear going in.

Also praised: record-field inference from a plain `list()` with a did-you-mean suggestion, which one
tester called worth adopting for alone; named-argument typo suggestions, arity checks and
argument-order errors ranged on the argument; branch-union arithmetic and non-function calls; the
partial-match catch, which R silently accepts and nothing else in their toolchain catches; `NAMESPACE`
being genuinely load-bearing, with import-typo and undefined-export validation at the right line;
`#:` verified invisible to `R CMD check`; the formatter and JSON output as production-ready; exit
codes, JSON Lines, `--min-severity`, config discovery, unknown-key warnings and suppression comments
all matching the docs; syntax-error recovery; and `ry run` with an embedded R letting a tester verify
every claim, which they called underadvertised. Units nominals and domain matrix types both work and
produce good messages. One tester confirmed the `expect_error` decision was right and that the
"testing that something is rejected" subsection worked first try. Performance was never a complaint:
0.15 s on 919 lines and 31.5k lines across 120 files in 2.2 s, both on debug builds.

## Open: what the documentation reviews left

Four independent reviews read the docs cold: a first-hour newcomer, an information architect, an
accuracy auditor who executed every claim against the binary, and a positioning analyst who also
measured the competition. The four `docs-review-*.md` files from that round hold the full reports
and their paste-ready rewrites.

Most of what they found is fixed. The structural rewrite landed, so the type-checking guide is a
tutorial of eight numbered steps from a zero-config first run to strict mode, each with a runnable
example whose output came from the binary, ending where the reference begins. The stubs page is no
longer an internal RFC. The diagnostics reference covers all fourteen codes in tables built by
running the tool, and a CI workflow page and a limitations page landed with it. `why-ry.md` answers
why a type checker for R, and `guides/adopting.md` is the adoption how-to. Re-verified against the
tree: the rest-parameter spelling, the stale symbol names, the internal gate vocabulary, the bless
environment variable, the `stub` diagnostic code, the `SCREAMING_SNAKE_CASE` exemption and the
duplicate-`@type` error in a script are all correct now.

### The comparison page does not exist

An earlier entry recorded it as done, naming `comparison.md`. No such file has ever been committed,
and nothing in the docs mentions Air, Jarl or lintr.

It is worth writing, and the earlier entry describes the right shape: a capability table plus a
section on where you should use something else, handing formatting to Air and rule breadth to Jarl
and lintr outright, and stating the alpha and one-maintainer position. Verify every claim about
another project from that project's own documentation, and use no benchmark number that was not
measured here, because a second-hand number about a competitor is the fastest way to lose the
argument.

Two measured facts the docs never state and should: 854 files and 166k lines in 3.6 s, and lintr at
38.2 s against 0.47 s on dplyr.

### The headline leads with the two things ry loses at

The landing page says "Editor support, error checking and formatting for R", which leads with the
formatter and the linter. Posit's Air is bundled in Positron, and Jarl ships 71 rules with `--fix`
and an LSP at 140 times lintr's speed. The one thing nobody else has is buried.

Research confirms that nobody has ever shipped a static type checker for R. Vitek's group proved it
viable, finding about 80% of CRAN functions monomorphic or nearly so with a 1.98% contract-failure
rate, then pivoted to a JIT intermediate representation. The one Damas-Milner attempt,
`RTypeInference`, went dormant in 2021. Posit's `ark` README states that it plans "sophisticated
static analysis of R code" and cites rust-analyzer, while Positron ships a Rust type checker for
Python.

The primacy claim is already defensible in its current form, which is that ry is the only one that
infers types rather than the first one, because two unmaintained annotation-only attempts exist and
a hostile reader finds them in one search.

### Self-checking ggplot2 reports 1132 findings and takes 2.4 s

That is the one package a stub ships for. The proposed cause, the shipped stub colliding with the
package's own definitions, does not reproduce minimally: a package named `ggplot2` that defines
`geom_point` and calls it checks clean, so project-wins-over-corpus works.

The real cause is unidentified and needs the actual source, so fetch the corpus. The likely
candidates are ggplot2's own ggproto and R6 layer and its non-standard evaluation, both known gaps,
in which case the number is honest rather than a bug.

### One residual from the numeric-condition fix

A condition whose type is still undetermined is bound to `logical`, so `function(n) while (n) ...`
infers `n: logical` and rejects a numeric caller. Fixing that properly needs a "coercible to a
condition" constraint, which is a fourth constraint kind and therefore the documented tripwire for
designing traits rather than accreting them. `contributing/design/open-questions.md` holds the
reasoning.

## Open: what the adoption reviews left

Three independent black-box adoption reviews simulated real projects from the docs and `--help`
only, with no source access. They were an analysis-script user, a CRAN package author and a
numerical-computing user, and they converged on the same walls. What they found that is now fixed is
in the ledger. What remains is below, ranked by how often a real user hits it.

### The apply family is unblocked but still untyped where its result shape depends on a value

Overload selection with a flexible argument works now, through fact-versus-guess probing that
`decisions.md` records. `sapply`, `mapply`, `Map` and `tapply` stay `Any`, because `simplify = FALSE`
and a vector-returning callback change the result shape without changing any argument type, so no
overload set can discriminate them.

The reachable win is typing only their parameters and leaving the return `Any`. That costs R's
function-name-as-string form, so `sapply(x, "length")` would be rejected, as it already is for
`lapply`.

### The three object systems, re-measured

Two of the four claims this entry used to make were stale, including the one it ranked first.

- **`setGeneric` was already fixed when the entry said otherwise.** It claimed that
  `setGeneric("f", ...)` does not define `f`, so every call to a project's own S4 generic reports
  `unresolved`, and it ranked that the top fix. It does not reproduce. `set_generic_target` binds
  the name, handles the `methods::setGeneric` form and the `name =` argument, and a control probe in
  the same file confirmed the `unresolved` check was live, because `definitely_not_defined` reported
  and `area` did not. Anyone who took the ranking at face value would have spent a cycle fixing a
  non-bug.
- **R6 was mischaracterized.** The entry said R6 has no stub at all, because `R6::R6Class` reports
  an unknown package namespace. R6 does ship an export manifest and is a conditional namespace, so
  it resolves as soon as the project declares it through `Imports: R6` in `DESCRIPTION` or attaches
  it with `library(R6)`. Both were verified clean. The message appears only for `R6::` in a project
  that declares neither, which is the documented rule for any undeclared namespace and is
  deliberate. What is actually missing is typed declarations. The class, its fields and its methods
  are `Unknown`, so `obj$typo()` is silent and completion after `self$` offers every record field in
  the workspace.
- **Still true: an S4 slot typo is silent.** `setClass("A", representation(x = "numeric"))` followed
  by `new("A", y = 1)` reports nothing, where R halts with `invalid name for slot of class "A": y`.
  `x@slot` has no type either, and `setClass`, `setMethod` and `new` are `Any` stubs.
- **Still true: `UseMethod` is not modelled**, so a generic call is `Unknown`, and
  `structure(list(...), class = "dog")` produces a plain record, so the class attribute is data. S3
  operator dispatch is real, because `+.Date`, `Arith.X` and `Ops.X` are built and dispatched, and
  the linter knows `generic.class` names.

All three systems are recognized structurally by the IDE outline, in `classify_symbol_call`, which
is where the type-side work can start. The fix order, cheapest real win first, is the S4 slot-name
check, then R6 class typing, then S4 slot types, then `UseMethod`.

The slot check is bounded, because `setClass` names the slots and `new("Class", ...)` names its
arguments. But `contains =` is the trap. A subclass legitimately takes its parent's slots, verified
against R, where `setClass("C", contains = "P", ...)` followed by `new("C", x = 1, y = 2)` runs. A
check that does not follow the inheritance chain therefore turns correct code into a false positive.
Both the `representation(...)` and the `slots =` form need reading, and a class assembled
dynamically must fall back to silence.

### Matrix shape is untracked

`matrix`, `t`, `solve`, `dim`, `crossprod`, `diag` and `apply` all return `Any`, so a transposed
dimension or a non-conformable product is invisible. Declaring them `-> matrix` is easy. The value
is in dimensions, which needs a shape-carrying matrix type. The data.frame row-type design is the
same shape of problem.

One trap came out of the work that returned the `matrix` nominal: making a constructor return a real
nominal without also declaring that class's operator methods turns every `m + 1` into a false error.

### Everything from a `data.frame` is `Unknown`, and `Unknown` satisfies every annotation

On data-frame-heavy code, annotations therefore look protective and are not. This is the design
consequence that decides the tool's value for analysis users. It needs at minimum a way to see that
a check was skipped, which is what strict mode does once it reports origins.

### An unannotated helper that wraps an operator over a class fails, and the tie is why

`add_layer <- function(plot, layer) plot + layer` infers `<T: numeric> fn(plot: T, layer: T)`, which
ties the two flexible operands to one variable, so `add_layer(base, geom_point())` reports
`expected ggplot, found gg`. Dates have the same shape, in `add_days <- function(d, n) d + n`.

Annotating the helper fixes it and the message is clear, but the tie is an over-commitment. R's `+`
never required its operands to share a type, and a class that declares `+.Class` accepts pairings
the tie forbids.

This is the same third-constraint-kind question recorded above and in
`contributing/design/open-questions.md`. The right fix is a "supports this operator" constraint in
place of `Numeric`, replacing both the tie and the `declares_arithmetic` relaxation. The next stub
corpus addition that returns a real nominal will trip it again.

### A project's own `%op%` stays untyped by design

The result is `Unknown`, which is a strict-mode origin. It may be a non-standard-evaluation wrapper
whose right operand is quoted, as magrittr's `%>%` is, and checking that as an ordinary call would
reject correct code.

Lowering `%op%` to the call it is, following the documented `|>` precedent, would type it and give
goto-definition and references on the operator. Weigh that against the non-standard-evaluation risk
and the `unresolved` finding a bare-script `%>%` would gain.

### Only the stub route to an operator method on a project nominal is blocked

Declaring the method as an annotated R function works, and that is what an author writes anyway for
a real S3 class. The gap is narrower than "operator methods need ergonomics". A `.Rtypes` stub
cannot see a `@type` the R source declares, reporting instead that the declaration does not load
because it does not know the type. So only the stub spelling is unreachable.

Decide whether a stub source should see a project's `@type` declarations at all, or whether the
R-side declaration is simply the answer and the docs should say so.

### Literate documents are analysed by `check` but not by the editor

An `.Rmd`, `.qmd` or `.Rnw` chunk is converted to an R program by blanking every non-R character, in
`syntax::literate`, so ranges need no translation and `check` reports at the original line and
column.

The LSP path still ignores them. `did_change` hands the engine incremental edits against the
document the editor holds, and the converted text is a different buffer, so wiring it up needs the
original text kept alongside the analysed one, or the conversion applied per edit. The formatter
deliberately stays out, because most of an `.Rmd` is prose.

### The stronger suppression form

A type error inside `expect_error(...)` stays reported, and `# ry: allow(type-mismatch)` is the
answer. `decisions.md` holds the record and the diagnostics page documents it. The open follow-up is
the stronger form, which is a suppression that reports when the expected finding does not appear,
like `@ts-expect-error`. That is a feature for every code, not a special case.

### Smaller items, each with a one-line repro in the reports

- A message leaks an unbound type variable, as `list[T] | T[]`, and expands an alias on only one
  side of an expected-and-found pair.
- Closure re-entry is unmodelled, so the memo idiom
  `if (!is.null(cache)) return(cache); cache <<- v` yields `T | NULL`.
- There is no `--fix`, no stdin input, and no CLI way to ask what type an expression has, which
  makes debugging an inference surprise guesswork for a CLI-only user.

Two items from the reports were checked and are not defects, recorded so they are not re-filed. A
generic parameter rejecting a non-`NULL` default is correct, because `<T> fn(x: T, [fallback]: T)`
defaulting to `0L` would return `0L` from a call the signature promises returns `character`, while
`T | NULL` with a `NULL` default works because `NULL` is a declared member. The `unused`
false-positive on a write followed by `break` does not reproduce, verified across `for`, `while` and
`repeat` and over both used and genuinely dead writes, so drop it unless a concrete shape
resurfaces. Separately, `sum(1, 2, 3,)` formatting to `sum(1, 2, 3, )` is not a defect and stays:
the trailing comma introduces a missing argument, so it parses identically to the `alist(, )` idiom
the space serves, and the `trailing-comma` lint reports the mistake.

## Open

 — semantics

- (Stub completeness audit CLOSED by the export-manifest layer — see the decision record and `stdlib-stubs.md` §Export manifests. `uname`-style reports remain user-project names: the fix stays a project stub or the DESCRIPTION-import tolerance.)

- **Legacy ide fixture port DONE** (fixtures directive, first half): 81 cases ported into `crates/ide/tests/ide/*_ported.R.test` (real legacy corpus: 134 cases / 206 operation sites; ~36 already covered; 15 skipped as genuinely multi-file — the harness is one `SourceFile` per case; deliberate improvements blessed). Cross-file navigation coverage now rests on the LSP tests — consider a multi-file fixture harness extension if that surface grows.

- (Design forks all DECIDED — two-flexible comparison stays unconstrained without a third constraint kind, union compatibility commits flexibles at first use in program order, NAMESPACE bare-resolution stays ungated; decisions.md has the three records.)
- **FIXED — an annotation in a call's argument list is now reported.** It attached to nothing and
  said nothing, so a deliberately wrong type beside a lambda argument was invisible. The cause:
  `statement_annotations` sees an `ARGUMENT` node next, which is not an expression kind, so the block
  classifies as dangling and never attaches — and the placement walk visits only statement sequences,
  never an argument list, so nothing reported it either.

  **The scope is narrower than this item claimed, and getting that wrong twice is the lesson.** The
  first implementation reported every annotation the placement walk did not reach, which is a false
  positive on four positions that do attach; it was built, measured, and reverted. Verified against a
  no-annotation control, because an uncontrolled probe read `lapply`'s own "not a function" error as
  evidence the annotation had applied:

  | position | parent node | attaches? |
  | --- | --- | --- |
  | braceless function body | `FUNCTION_DEF` | yes |
  | braceless `if` branch | `IF_EXPR` | yes |
  | parenthesised expression | `PAREN_EXPR` | yes |
  | call argument, any target | `ARGUMENT_LIST` | **no** |

  So the check keys on `ARGUMENT_LIST` alone. Two fixtures pin the reports and two pin the
  attaching positions, so a future widening has to break them first.

  Residual gap, unchanged: reporting the silence does not give the lambda parameter an annotatable
  position, which stays the one genuine expressiveness argument for inline type syntax
  (`contributing/design/inline-type-syntax.md` §3). The message says to lift the function to its own
  binding rather than "move it up a line", which would annotate the wrong thing.
- Overload candidates when touched: `is`, `extends`, `grep(value =)`, `cor` (vector vs matrix — needs matrix nominals). `Date`/`POSIXct` arithmetic refuses loudly today — revisit if real code makes it noisy.
- **A list operation over a RECORD still loses the field types.** `rev`/`unique`/`head`/`tail`/`Filter` now declare a `list[named: T]` candidate ahead of the plain list one, so a name survives and a field read is `T | NULL` instead of a missing-field error — but a fixed-shape input coerces to a name-keyed list on the way in, so the exact field types are gone and the read stays nullable. Only a shape-mirroring return ("the same record") fixes it, and the type language has no way for a stub to say that; `rev` is the case where the claim would be exactly right (it reorders and drops nothing), while `head`/`tail`/`Filter` genuinely may drop a name and are correctly nullable. Same family as the data.frame row-type and matrix-shape designs.

## Open — lowering fidelity (each one pinned wrong-but-current in `crates/semantics/tests/lowering/`)

Found by dumping the HIR directly instead of reading it off the far-end type. Each has a fixture case
whose comment says the expected shape, so fixing one turns its case red and forces a deliberate
re-bless.

- **A trailing empty argument position is dropped, so `m[1, ]` and `m[1]` lower identically**
  (`indexing__a_trailing_empty_index_position_is_dropped`). R distinguishes them — `` `[`(m, 1, ) ``
  is arity 3, `` `[`(m, 1) `` arity 2 — and for a data frame they return different things (a row vs
  a column). The cause is in the *parser*: `argument_list` emits an empty `ARGUMENT` only when a
  comma arrives while an argument is still expected, so a leading or interior hole survives
  (`m[, 1]` is right) and a trailing one vanishes. Fixing it needs the parser to close a pending
  position when the closer follows a comma; the HIR side already models the hole
  (`Argument { value: None }`).
- **The R 4.3 extraction placeholder is not desugared**
  (`pipes__an_extraction_placeholder_is_not_desugared`). `x |> _$a` is `x$a` in R, and `_[[i]]`,
  `_[i]`, `_@s` likewise. `lower_pipe` accepts only a `CALL_EXPR` right-hand side, so these stay an
  opaque `Binary Pipe` whose field access reads a name `_` that exists nowhere. `pipe_shape` needs a
  second shape for "the placeholder is the head of an extraction chain", substituting the piped
  value for the `_` in place.
- **An empty control-flow head slots the BODY into the condition**
  (`broken__an_empty_if_condition_slots_the_body_into_the_condition`, plus the `while` and `for`
  siblings). `if () 1L` lowers to `If(condition: 1L, then: Missing)` because the `IF_EXPR` arm reads
  its children positionally and the parser emits no placeholder for the missing head. Contained
  today only because a broken item's type diagnostics are suppressed — verified: a sibling item in
  the same file still type-checks, so the suppression is per item, not per file — but IDE reads of
  the region see the wrong slot. The fix is to key the slots off the head delimiters rather than off
  child order.
- **A hexadecimal literal keeps a text no consumer can parse**
  (`literals__a_hexadecimal_literal_keeps_a_text_no_consumer_can_parse`). Literals store source text
  and every reader parses it with Rust's decimal `parse`, which rejects `0x`. Observable: with
  `pair: list{a: integer, b: character}`, `pair[[2L]]` resolves `character` while `pair[[0x2L]]`
  falls back to `integer | character`. `integer_literal_position` and `is_whole_number_double` both
  need a radix-aware parse.
- **`:=` publishes a definition the HIR does not make.** `classify_top_level` lists `COLON_EQ` among
  the assignment spellings, so `x := 1L` names its item `x` and a later `y <- x` resolves — but
  lowering (correctly) makes it a call to a function `:=` that binds nothing, and R binds nothing
  either. Two sources of truth for what an item defines, and the item tree is the wrong one. Pinned
  by `assignment__a_walrus_lowers_to_a_call_because_it_binds_nothing`, whose header shows the
  disagreement.

## Open — naming fidelity (found by testing name resolution directly, in `crates/semantics/tests/naming/`)

Found by rendering `ItemNaming` instead of reading resolution off a downstream type or diagnostic.
Ordered by severity. The first four were re-verified against the shipping binary on a throwaway
project before being written down here.

- **FIXED — `<<-` inside `local()` missed the enclosing function frame and produced a wrong TYPE.**
  The super-assignment search was bounded at `current_function_depth()`; with scopes
  `[TopLevel, Function(f), Local]` that is `1`, so `0..1` skipped `f`'s own frame at index 1 and the
  write escaped to the global environment. The bound is now `self.scopes.len() - 1` — everything
  strictly outside the current scope — which coincides with the old one whenever the current scope
  *is* the function frame, which is why the closure spelling was always right. Checked against R:
  `function() { v <- 1L; local({ v <<- "two" }); v }` returns `"two"` and now types
  `integer | character` like its closure twin; two `local`s deep still reaches the frame (R: `"deep"`);
  and an intervening `local` that binds the same name still catches the write, leaving the outer slot
  alone (R: `1`). Only the super-assignment site changed — `current_function_depth` still bounds the
  read and capture logic, where a function boundary genuinely is the thing that matters.
- **FIXED — a named data argument broke positional masking, and the `base::` spelling masked
  nothing.** Two false `unresolved` findings on code R runs. The positional counter was not advanced
  past formals already claimed by name, so `with(data = frame, column_a)` read `column_a` as the data
  and evaluated it in the caller's frame; matching now follows R's own rule (names claim their formal
  first, remaining positionals fill what is left), which also makes the reordered
  `with(column_a, data = frame)` correct. And the `Namespace` arm consulted only stub-declared
  `@masked` verbs, so `base::with` and `base::subset` masked nothing; the base family is now
  recognized under `base` as well as bare. Controls confirm the data argument itself is still
  checked (`with(no_such_frame, …)` still reports), an in-item local `with` still masks nothing, and
  `somepkg::with` is still treated as its own function.
- **A *top-level* definition of a masking verb does not suppress masking, unlike an in-item one.**
  Found while adding the controls above, and **pre-existing** — verified identical before the fix.
  `with <- function(data, expr) expr` at top level followed by `with(frame, name)` in another item
  still masks, because cross-item resolution happens above `item_naming`, so the callee read is not
  in `resolutions` and the shadow is invisible to the walk. The same item-firewall limitation the
  naming suite already states; the fix needs the file's own top-level binders consulted at the
  masking check, which `file_binders` can answer.
- **FIXED — `switch` was walked as an ordinary call, so its branches were sequential writes.** Two
  halves, in both passes. Naming reported a false `unused` on the first branch's write
  (`switch(key, a = { r <- 1L }, b = { r <- 2L })` — live whenever `key == "a"`) and missed the
  `maybe-undefined` on a later read, which R reports as `object 'r' not found` when nothing matches.
  The checker had the same shape: it unioned the branch *values* correctly but inferred them in
  sequence, so a later branch's write won outright and
  `switch(k, a = { r <- 1L }, { r <- "d" })` typed `r` as plain `character` where the `if` spelling
  joins to `character | integer`. Both now fork from the entry state per alternative and join, which
  is `infer_if`'s two-arm shape generalized to many. Checked against R for each shape: a matched key
  returns its branch, an unmatched one with no default errors, a default catches it, and
  `switch(k, a = , b = …)` falls through to one branch rather than two. A branch that cannot fall
  through (`stop()`) contributes no state, as a diverging `if` arm does not, and a local binding
  named `switch` makes the call an ordinary one again.
- **FIXED — `repeat`'s post-loop state wrongly included the never-assigned path.** `loop_body` reused
  the converged loop-*head* state as the exit state, so `Unassigned` from the first iteration survived
  a loop that always assigns. Fixed properly rather than by dropping the join: `break` now records the
  reaching-write state where it occurs, and a loop that cannot be skipped exits through exactly those
  points joined with the body's end state. That is precise in both directions —
  `repeat { x <- 1L; break }` reports nothing (R returns 1) while
  `repeat { if (cond) break; y <- 1L; break }` still does (R errors when `cond` is TRUE). A loop with
  no `break` at all keeps the conservative head join, since it leaves by a jump the walk does not
  model. Found only because the new `maybe-undefined` code made the state visible for the first time.
- **FIXED — a write-only `<<-` reported its initializer unused, and deleting it changed behaviour.**
  `make_flag <- function() { flag <- FALSE; function() flag <<- TRUE }` gave a false `unused flag`,
  but the initializer is what makes `<<-` find a slot at all: remove it and the write goes to the
  global environment instead. The read path already marked a frame's writes used on a capture
  (`mark_slot_read`); the `<<-` target path now does the same for the frame it resolves into. Fixing
  this was not optional alongside the `local()` fix above — that fix makes the write land on the
  frame's slot, which is exactly what turned the initializer into an apparent dead store. Verified a
  real dead store still reports, and that an outer binding shadowed by an intervening frame is still
  correctly dead.
- **Rebinding `local` makes the rebinding itself read as a dead store.** The `Local` HIR node carries
  no callee expression, so nothing reads a user-defined `local` and a false `unused local` fires. The
  docs sanction treating the syntactic call as the construct; they do not mention that the shadowing
  definition then reports as dead.
- **FIXED — the "might be undefined" warning the reference promised now exists, as an opt-in.**
  `maybe_undefined` was computed by naming and surfaced nowhere. It is now the `maybe-undefined`
  code, gated on `[check] maybe-undefined = true`. **Off by default on measured evidence**: with it
  on, six real packages report 442 findings (data.table 242 in 12k lines, shiny 86, MASS 45,
  ggplot2 42, dplyr 18, targets 9), and the dominant shape is correlated guards the flow cannot
  see — in `data.table/R/print.data.table.R` the flagged `index_dt` is assigned only in one branch,
  but the read is guarded by `show.indices`, which the *same* branch sets to `FALSE`. Safe code,
  unprovable by flow. Default-on would have been the "a clean run means nothing" failure the
  adoption reviews already flagged. A top-level variable's unwritten path is exempt, per the
  contract: at run time it reaches the enclosing environment.
- **`library`/`require`/`help` quoting ignores local shadowing, unlike every sibling recognizer.**
  `quote`, `on.exit` and the masking family all guard with `!resolutions.contains_key(callee)`; the
  attach family does not. The docs call this a limitation, but the inconsistency lives inside one
  function.

Renderer gaps in the naming suite itself, none of them a product bug:

- **A replacement base renders as a read (`->`) though it is also the write**, because
  `assignment_targets` collects only `ExpressionKind::Assign { target }` and for `x$field <- v` the
  target is the `Field` node, not the base name.
- **An *unresolved* replacement base renders two contradictory lines at one span** — both
  `u -> b0` and `u -> non-local deferred`, because the base expression id lands in `resolutions` and
  in `non_locals`. Both underlying facts are right (unresolved read plus slot-creating write); the
  rendering is what is wrong, and this is the shape where the missing write/read distinction becomes
  actively misleading. No case covers it yet.
- **`quote(x <- 1L)` mints a vestigial slot.** `premint_frame_assignments` walks call arguments
  without knowing about quoting, so a `b0 x local` binding appears that nothing writes and nothing
  resolves to. Harmless; a premint fix should re-bless the case that pins it.

## Open — fuzzing oracle-strength review (measured, and it found two live bugs)

The third of three independent fuzzing reviews, asking the complementary question to the other two:
not *what inputs do we feed* or *what does it cost*, but **what can be wrong while every arm stays
green**. Method: injected-bug experiments against a byte-copy of `crates/` built as its own workspace,
so the shipping `test_fuzz` targets ran verbatim against mutated code. Baseline and restored-copy
controls both green. Oracle corpus = 1,967 mined legacy-corpus programs + 1,192 fixture sources.

### FIXED — the type renderer printed types that could not be written back, and one that meant something else

Every user-visible rendering of a type — hover, inlay hints, `expected X, found Y` — goes through
`TypeRenderer`, and nothing checks that the string it produces is readable by the `#:` grammar or that
it denotes the type it came from. The oracle is the type system's own contract: `#: TYPE` asserts the
value is compatible with `TYPE`, and the checker just proved the value *has* that type, so
re-declaring the rendered scheme above the definition must add no finding. 3,159 sources → 1,131
re-declarations → **41 violations** (5 grammar refusals, 39 type errors). Two classes, both confirmed
end-to-end with the real binary:

- **Record field names are rendered unquoted.** `list(\`max size\` = 10L)` renders
  `list{max size: integer}`, which the annotation grammar refuses. A stress sweep of 14 shapes fails
  7 — and one fails *silently*: `list(\`a,b\` = 1L)` renders `list{a,b: integer}`, which **parses as
  `list{a}`**, producing a bogus `type-mismatch` plus a bogus "I do not know the type `a`".
- **`scalar numeric` is rendered but is not a writable constraint.** `function(n) 1:n` renders the
  scheme `<T: scalar numeric> fn(n: T) -> double[]`; only `numeric` and `atomic` are writable
  (`type-system.md` §Type parameters), so the renderer and the grammar disagree.

This is the same class the codebase already documents one instance of — "a function member of a union
must render parenthesized … a type copied out of a finding into an annotation changes meaning". The
rule was applied to unions and never to field names or constraint spellings, and nothing enforces it
generally. Cost of the oracle: 36 s over the whole in-tree corpus in release, dominated by the known
~113 ms/db stub tax; under a second with a shared database. Scoped to `semantics/tests/typing` it is a
default-suite-sized battery today.

### FIXED — a `<T: numeric>` binder's constraint was dropped inside a self-recursive body

`type-system.md` §Type parameters is explicit that with `<T: numeric> fn(x: T) -> T` the body may use
`x` numerically. Confirmed false positive:

```
#: <T: numeric> fn(n: T) -> integer
countdown <- function(n) if (n <= 0L) 0L else countdown(n - 1L)
  x expected a numeric value (`integer` or `double`), found `T`
```

Narrowed by minimal pairs: the same annotation over a non-recursive body using `x + 1L` or `x > 0L` is
clean, and the *unannotated* `countdown` is clean — so writing down the checker's own inferred type
turns a clean file into a failing one.

**Root cause, and it is the design-review shape this file already records once**: two places decided
whether a type satisfies a numeric constraint and they read different scopes. `Checker` carried
`rigid_constraints`, so the *operand* path knew `<T: numeric>` admits arithmetic — which is why
`x + 1L` was clean — while `constraint_rejects` in the unification path had no case for
`TyKind::Rigid` at all and fell through to `false`. A self-recursive call is exactly where the two
meet: it instantiates the scheme, producing a fresh constrained variable, and unifies it with the
body's own rigid `T`. Fixed by moving `rigid_constraints` off `Checker` and onto `InferenceTable`,
where admissibility is decided — the same move `arithmetic_classes` already made for the same
reason. A binder is admitted when its declared bound implies the required one, expressed as
`declared.join(required) == declared` so the lattice order is not enumerated a second time.
Verified still enforced: `<T>` with no bound used numerically is refused, and calling a numeric
scheme with `character` is refused.

### FIXED — an omitted optional argument yielded `Any`, discarding the default's known type

`function(x = 1) x` inferred `<T> fn([x]: T) -> T`, and a call omitting the argument left the
parameter a free variable, so `lucky()` was **`Any`** rather than `double`. `Any` is compatible with
everything, so the consequence was silence: `nchar(lucky())` — `nchar(1)` in R — reported nothing.
The same gap made the checker reject a scheme it had inferred itself, which is how the round-trip
oracle found it.

Both halves are fixed and the oracle's allowlist is now empty (527 schemes, 0 unwritable).

- **`FunctionType.named` is now `Vec<Parameter>` rather than `Vec<RecordField>`**, carrying
  `default: Option<Ty>`. Reusing the record-field struct for parameters was the reason the default
  had nowhere to live; a record field has no default and never will. The default's type had to be
  threaded through every traversal that walks a function type — substitution, `erase_vars`,
  `resolve`, `adjust_levels`, `occurs`, `walk_unbound_vars`, `contains_unknown`, `type_size` — and
  skipping any of the first three would have been unsound rather than imprecise (a variable hiding
  in a default, un-substituted or un-level-adjusted). `Parameter::types()` is the one iterator they
  all go through so a future field cannot be half-walked.
- **A call that omits an optional argument unifies the parameter with the default's type.** Skipped
  when the call forwards `...`, where the argument may be arriving through the dots. A default that
  cannot fit its own parameter is the definition's mistake and stays reported there.
- **A declared default is checked against an instantiation of the declared type, not the rigid
  binder.** A binder is the caller's choice and omitting the argument is the one call where the
  default makes that choice, so `#: <T> fn([x]: T) -> T` over `function(x = 1) x` is honest.
  Controls verified: a concrete declared type still refuses a `NULL` default, a wrong-typed one, and
  `<T: numeric>` still refuses a character default.
### FIXED — formatter preservation was kind-only, so a token's spelling could change invisibly

`significant_kinds` compares `Vec<SyntaxKind>`. A formatter emitting the wrong *bytes* for a token —
exactly what a stale or off-by-one `raw()` range produces — preserves kinds perfectly, and R is
case-sensitive, so this is a miscompile. Injected bug: uppercase the first letter of IDENT tokens ≥4
chars. The shipping `format` battery passed **8/8**, including `fixture_sources_hold_invariants` and
`legacy_corpus_holds_invariants`; a `(kind, text)` oracle caught **1,723 of 2,731** sources. Pristine
baseline 0 violations, cost **0.1 s** for all 3,159 sources — cheaper than the check it replaces.
(Control: a mutant dropping the `L` suffix *was* caught, because `1L`→`1` crosses a kind boundary. The
blind spot is precisely within-kind.) Worth having alongside it: **format ⇒ semantics agreement**,
formatting must not change the diagnostic multiset — pristine 0 divergent over 2,224, cost 20–35 s.

### FIXED — nothing bounded the parse-error count from below; the oracle already existed and never ran

`check_parse_invariants` guards against an error *cascade* but never asserts that a broken file reports
anything, while the parser carries a lot of dedup and first-wins logic (`error_at`, `error_unclosed`,
`statement_left_group_open`). Injected bug: drop zero-width ranges in `push_error`, a plausible "a
zero-width caret underlines nothing" polish change. All four `test_fuzz` binaries stayed green while
750 → 694 parse errors and **24 of 489 broken files became silently clean**. The catch:
`crates/syntax/tests/test_corpus.rs::corpus_acceptance` is exactly the right differential and it
`return`s silently because it points only at the gitignored fetched `corpus/`. Pointed at in-tree
inputs, `theirs-only-error` (we accept, tree-sitter rejects) goes **2 → 22** on the corpus and
**11 → 815** over 20,000 seed mutations. The `ours-only-error` direction is noisy (mostly `#:`
annotations tree-sitter reads as comments) so gate only the `theirs-only` direction; a baseline of 11
in 20,000 is small enough to allowlist. `tree-sitter-r` is already a dev-dependency; cost **97 ms**
for the in-tree corpus, 765 ms for 20,000 mutations. **An oracle explicitly marked unproven:** "an
`ERROR` node implies at least one reported error" holds (0 violations over 3,159 sources and 400,000
fuzz inputs) but did *not* fire on this bug — the dropped errors came from files with no `ERROR` node.
Prefer the differential.

### FIXED — IDE ranges were checked for in-bounds-ness, never for what they cover

The ide battery asserts `range.end() <= text.len()`. A rename whose edits are all shifted one byte
passes — and corrupts the user's file. Injected bug: off-by-one at the one place in `occurrences`
where item-relative ranges are re-anchored to absolute offsets, which is the design's own documented
single re-anchoring edge. The shipping battery passed **2/2**; a *name-identity* oracle (the text at
every definition target and rename edit must be the identifier under the cursor) caught **748** bad
edits, and a *round-trip* oracle (definition at a reference lands on a range `references` reports, and
back) caught **358**. Pristine baseline 0 over 3,466 identifier positions and 1,389 definitions, cost
**4 s**.

### Oracles that hold — coverage the fuzzers lack, no bug found, each measured

| oracle | result | cost |
|---|---|---|
| **middle edits** through incremental equivalence (the in-tree arm replaces whole text; the cargo-fuzz target truncates and appends — neither covers a common prefix *and* suffix) | 568 checked, 0 divergent | 9 s |
| **project-file-set churn** — no arm ever changes `ProjectFiles`, though hosts add and remove files constantly | 120 removals, 0 divergent | 2 s |
| **parallel vs sequential** on generated input (`ry check` really does fan out; `test_parallel.rs` covers 3 hand-written programs) | 30 × 8 files, 0 divergent | 0.9 s |
| **two-wave superset** — a `parse_stage_diagnostics` finding that vanishes from `file_diagnostics` is an editor flicker; asserted only for hand-written cases | 3,159 sources, 0 lost | 25 s |
| **leading-comment metamorphic** | 2,197 compared, 0 divergent | 36 s |
| **alpha-rename metamorphic** | 3/1,162 hits, all three the transform's own fault (an edit-distance suggestion, a stub-shadowing name, string-form binders an IDENT-only rename missed); capture-avoiding transform 0/994 — usable only with the filters | 20 s |

### Open — the parser accepts escape sequences R rejects (found by the differential's first run)

The lexer's string scanner skips any escaped character wholesale, with a comment calling escape
validity "a semantic concern" — but no semantic layer checks it, so every malformed escape is
silently accepted. Verified with R as referee: `"\u{1F600}"` has five hex digits and `\u` takes at
most four, so R refuses it with `invalid \u{xxxx} sequence` while `ry` reports nothing. R's rules to
implement: `\x` 1–2 hex, `\u` 1–4 hex (bare or braced), `\U` 1–8 hex (bare or braced), `\0`–`\7`
octal 1–3 digits, the named escapes, and `unrecognized escape in character string` for anything
else. The fixture case `syntax/tests/syntax::quoting__escape_soup` currently sits in
`crates/syntax/tests/in-tree-acceptance-allowlist.txt` labelled as this gap; implementing the check
should remove that entry and re-bless the case.

**One surface with no oracle at all:** `PackageMetadata` is referenced by zero fuzz arms, so attach
tolerance, `imports_every_name` and the whole NAMESPACE/DESCRIPTION layer are fuzz-dark — and fixtures
cannot reach them either, only `crates/ry/tests/test_cli.rs` can.

**One hypothesis killed:** the semantics battery is *not* comparing mostly-empty renderings — over 250
generated programs exactly 1 is empty and the mean is 462 characters. Determinism and incremental
equivalence are meaningful self-consistency checks. They remain content-blind in the sense the three
injected bugs demonstrate: a uniformly wrong answer is deterministic, incremental, and in-bounds.

## Open — a package's own `pkg::name` reads are resolved but not validated

FIXED: the project's own package (from `DESCRIPTION`'s `Package` field) is now a known namespace
whatever the stubs say, so `withr::defer()` inside `withr` reads the project's own definition and has
its type instead of `Unknown`, and the `unknown package namespace` false positive is gone. The own
package also wins over a stub namespace of the same name, matching the rule that a package binding
shadows a stub name.

Still open: **the name itself is not checked**, so `withr::typoed_name()` reports nothing. Validating
it against the project's definitions was implemented, measured, and removed — across the CRAN corpus
every candidate report was a false positive, because a package's export set is not the set of names
its sources bind:

- a **re-export** — `shiny` has `importFrom(htmltools, validateCssUnit)` beside
  `export(validateCssUnit)`, so the name is exported with no definition in the package;
- an **S4 generic** from `setGeneric("raster", ...)` (`raster`), which S4 opacity already covers;
- a **lazy-loaded dataset** under `data/` — `survival::survexp.us` lives in `data/survexp.rda`;
- a binding installed by **`.onLoad`** (`cli`'s `symbol`);
- an **S3 generic re-exported from another package** (`broom`'s `glance`, from `generics`).

Closing this needs the package's real export set, which means reading `NAMESPACE` `export()` *and*
resolving re-exports, plus a decision about `exportPattern` (a regex over names, so it makes the set
unknowable and must fall back to silence). Worth doing — a typo in a self-qualified call is
otherwise invisible — but it is a namespace-model slice, not a one-line check.

## Open — a stdlib wrapper loses all type information (found while investigating the above)

`function(x) abs(x)` infers `fn(x: T) -> Any`, and the same holds for `sum`, `cumsum` and every other
set: wrapping a standard-library numeric function in one of your own throws the types away. The cause
is the fact-beats-guess rule — the `Any` fallback fits while binding nothing, so it beats every
candidate that would narrow `x`.

The rule is right in general and the fallback is load-bearing: a nominal with an `Arith.`/`+.` method
satisfies the numeric constraint, so forcing `x` numeric would reject a user's S3 class that
legitimately defines `abs.myclass`. Any fix has to keep that working, which is why this is a design
slice and not a tweak — the candidate shape is "if every non-fallback candidate imposes the same
constraint, imposing it is a fact rather than a guess", which needs a decision record and adversarial
review before it is written.

**Prerequisite FIXED, and it was a live false positive on its own.** The escape hatch above only half
worked: `declares_arithmetic` consulted the **stub library alone**, while operator dispatch resolves a
method through the global scope, which includes the project's own sources. So a package defining
`+.Money` had its `+` dispatched correctly and its class *refused* by the numeric constraint —
`bump <- function(x) x + 1L; bump(price)` reported ``expected a numeric value (`integer` or
`double`), found `Money` `` on code R runs fine (checked: R prints 6). The set is now computed from
stubs **and** the project (`GlobalEnv::arithmetic_classes`, memoized per corpus and per project;
a script's own top level counts too, the way its `@type` declarations already do) and carried on the
inference table beside `definitions`. A class that declares no arithmetic method is still refused —
R halts on that one too, checked both ways. Measured on the corpus: no finding changes across
data.table, dplyr, ggplot2 and shiny, so this loosening removed nothing that was load-bearing there;
the fixtures are the coverage.

Worth knowing for the design slice above: the escape hatch it depends on is only now actually
general. Any measurement of "how often would constraining `x` reject real code" taken before this
would have overstated the cost.

## Open — a misplaced config key was a silent no-op, and the class of bug is not closed

FIXED for the specific case: a key written at the top level that belongs under a table now names the
table (`ignoring config key `typing` — it belongs under `[check]``) instead of saying only that it is
unknown. Writing `typing = true` outside `[check]` had loaded clean, checked nothing, and reported
"no problems" — the same failure mode as an unresolvable namespace disabling unresolved-name
checking, and the one this project treats as the worst kind: a clean run indistinguishable from a run
that never happened.

Still open, and the reason this stays filed: **an ignored key is a warning, and a warning can be
missed.** A config file is small, hand-written and rarely revisited, so the cost of refusing outright
is low and the cost of proceeding is a project that silently is not checked. Consider making an
unknown key a hard error when the file is a *local* `ry.toml` while keeping the warning for forward
compatibility only where it is actually needed. Decide it deliberately; the current forward-compat
rationale (`config.rs`, `Config::unknown_keys`) is written down and is not obviously wrong.

## Open — a `--jobs` flag, and why the fan-out is not linear

Two user-requested items, related but separate.

### `--jobs N`, spelled like cargo's

`check` fans out over `std::thread::available_parallelism()` and there is **no way to control it**.
The only lever today is CPU affinity — `taskset -c 0-N` works because `available_parallelism()`
honours affinity and cgroup quotas — which is fine for measuring and undiscoverable for a user. Add
`--jobs N` (cargo's spelling, `-j` short form), defaulting to the current behaviour, so the number is
explicit and reproducible instead of depending on what the scheduler happens to expose. It also makes
cross-machine measurement possible without fighting `taskset`.

`fmt` is a plain loop over files and does not fan out at all — measured 646 ms on one core against
644 ms on four. Either wire it to the same flag or say plainly in the docs that it is single-threaded;
what should not stand is a `--jobs` flag that silently governs one subcommand and not the other.

### Why the speedup is not linear, and what the default should be

Measured with `taskset`, best of two, on a 4-vCPU container:

| | 1 core | 2 cores | 4 cores | speedup |
|---|---|---|---|---|
| ggplot2 | 1,388 ms | 1,255 ms | 1,033 ms | 1.34× |
| data.table | 420 ms | 309 ms | 278 ms | 1.51× |
| `targets` | 998 ms | 778 ms | 599 ms | 1.67× |

Read those against the container's own ceiling: its 4 vCPUs deliver roughly 1.8× of native compute at
4 threads, so `targets` is already near what this machine can give and the numbers here **cannot
distinguish real contention from the container**. Getting that separation needs a run on real
hardware at 1/2/4/8/16 cores — the user has offered to measure, and `--jobs` above is what makes that
clean.

What is worth investigating once there are honest numbers, in order of prior suspicion:

- **Amdahl, not contention.** Rendering is deliberately sequential in discovery order, and the
  project-wide interface walk (`interface_sccs`) is one query on one thread. If the serial fraction is
  ~40% the observed 1.34–1.67× is simply correct, and the answer is to parallelise the walk or accept
  it — not to hunt locks. Measure the serial fraction first; it is the cheapest thing to rule in.
- **Salsa cycle bookkeeping.** This *was* the dominant cost — 54 of 72 sampled stacks in
  `DependencyGraph::block_on`, with `targets` getting no speedup at all — and bounding the
  conditional-slot join fixed it. Re-sample before assuming any of it is left.
- **The warm-up phase.** The cold pass warms per-item naming across cores first, precisely because
  computing it inside the interface walk serializes the front half. Check the fan-out is actually
  balanced there: files are dealt largest-first, which helps, but one enormous file still pins a
  thread.

On the default: keep `available_parallelism()`. It already honours cgroup quotas and affinity, which
is what a CI container needs, and nothing measured so far suggests over-subscription hurts. Revisit
only if the real-hardware curve turns over at high core counts — that would point at memory bandwidth
or allocator contention, and the fix would be a cap rather than a different formula.

## Open — editor & polish

- Hover type fences (user-confirmed: no highlighting in current editor builds): the server tags the fences `roughly-type` and the VS Code extension in-repo ships a grammar for that id — needs a released extension update to reach users. Zed renders the fence plain until its extension registers an equivalent fence language (tree-sitter grammar required); consider falling back to tagging fences `r` for Zed if that proves distant.

## Open — structure & performance

- **Diagnostics-phase remainder:** the duplicate-binding/duplicate-type O(files²) walk is killed (see the ledger); the post-burst workspace revalidate (~1.1s at 713K LoC, user measurement) should shrink too — every file's diagnostics used to depend on every file's ranges through those walks, so any edit re-executed all of them — but re-measure on the real workspace to confirm before closing.
- **The rewrite is complete and shipping** (decisions.md "target architecture" record): every phase gate holds — corpus/round-trip/acceptance/fuzzing, semantic parity via the differentials, cutover suites, perf + memory + keystroke budgets, order-independent fixpoint, multi-core stress. The legacy crates stay in-tree **by user directive** until the user asks for the final deletion sweep; when that comes, migrate the remaining fixture data out of the legacy trees and archive a final corpus parity report first.
- **Parallel cold pass: measure on real hardware before optimizing further.** The investigation (`crates/roughly/examples/parallel_probe.rs` is the reproduction tool) found: (a) the long-recorded "4 workers buy only 1.2x" was mostly a measurement artifact — this container's 4 vCPUs deliver only ~1.8x of lock-free native compute at 4 threads (~1.2x at 2), so no in-container parallel number is meaningful; (b) the one real structural serializer was `interface_sccs` demanding naming for every item inside one salsa query (25-51% of cold wall depending on package shape) — fixed by the CLI's parallel per-item naming warm before the fan-out (mgcv cold pass 0.99s → 0.80s even in the throttled container); (c) salsa's same-query blocking is negligible (53 blocks per 5,657 executions) and the interface DAG is wide (ggplot2: 1,198 items, depth 22), so no fixpoint-scheduling work is warranted until a real-hardware measurement says otherwise.
- **Coverage-guided fuzzing landed** (`fuzz/` crate, testing.md documents the workflow): libFuzzer targets `parse`, `format`, and `semantics` over the exported invariant batteries plus `scripts/seed-fuzz-corpus.rs` (a `cargo +nightly -Zscript` single-file script, like all of `scripts/`); the lint layer is folded into the semantics battery (everything-on config), closing the last unfuzzed stage. First sessions found and fixed nine formatter bugs and two splice-equivalence bugs (a middle ending mid-construct must refuse suffix reuse; an empty-suffix splice must not rebase the old end-of-file error) — all pinned in per-harness `REGRESSIONS` batteries. Remaining: a scheduled deep-fuzz run on real CI hardware, and an `llvm-cov` coverage report (recipe in testing.md; skipped in-container for disk).
- CI: the widened whole-workspace workflow is staged in `.github/pending-ci.yml` — a human must `git mv` it into `.github/workflows/` (automated tokens lack workflow scope). Until then CI gates only the product crate's own suites; the workspace battery runs locally per slice. Authoritative perf numbers need the CI runner.

## Open — website & docs

- (The landing-page hero animation is user-owned — do not touch.)
- **One-line installer.** Today the non-Rust route is download-a-tarball-from-Releases; there is no
  `curl … | sh`, Homebrew, or winget path (uv and Ruff both ship one). The installation page states
  this is planned with no date — if the plan changes, that claim has to change with it.
- **Every release is marked a pre-release**, so `releases/latest/` resolves to the old `0.1.1` tag
  rather than the newest build. The CI guide works around it by pinning an explicit tag; promoting a
  release would let the docs recommend `latest` instead.
- **Rework the typing reference's presentation** (`reference/type-system.md`, ~2700 lines): tables and
  short bullets instead of prose subsections, preserving every normative claim. Deliberately deferred
  out of the docs restructure so the contract got a dedicated pass.

## Open — REPL (v1 shipped; the analysis wiring is the open rung)

- **v1 SHIPPED and e2e-VERIFIED against real R** (`crates/repl` behind `roughly repl`; `contributing/design/repl.md` has the architecture, status, and the two pty-harness requirements): runtime-loaded R (no build-time link — the workspace builds R-less everywhere), reedline console inside the ReadConsole hook, lexer highlighting, conservative completeness with R's continuation as the safety net, SIGINT interrupt routing. The pty e2e suite (skip-if-no-R) runs green against real R — agent containers CAN install R (recipe in MEMORY.md short-term), so run `cargo test -p roughly --test test_repl_e2e` before REPL-touching changes, anywhere.
- **Analysis-backed Tab completion SHIPPED** (first analysis rung; `contributing/design/repl.md` has the seam design): typed signatures for stdlib names, session bindings, `pkg::` exports, manifest names — `SessionCompleter` seam keeps the repl crate syntax-only, `AnalysisCompleter` in roughly runs `ide::completion` over the session-as-script. **Open — remaining rungs:** live-session facts (the R environment listing unioned into completions), pre-evaluation diagnostics on pending input, hover on the input line, graphics-device story (versioned mirror structs, see the design record). The headless runner is shipped.
- **REPL Windows: real-machine smoke test pending.** The embedding is implemented (`contributing/design/repl.md` has the recipe: Rstart callbacks via R_DefParamsEx's version handshake, sibling-DLL preloading, RGui→LinkDLL switch, UserBreak+deferred interrupt pair) and compile/clippy-verified against x86_64-pc-windows-gnu — but no Windows machine with R has ever executed it. Smoke: `roughly repl` (prompt, evaluate, Ctrl-C, vi mode) and `roughly run` (output, exit 0/1). Known caveat to watch: terminal VT input handling in the editor layer.

## Open — rename to `ry`: what is left

**Done.** The language is `ry`. The crate is `crates/ry`, published as `ry-lang` (plain `ry` is taken
on crates.io) with both the library and binary named `ry`. Docs, editors, scripts, CI and the README
carry the new name; documentation is pointed at ry-lang.org.

**Three surfaces keep their former spelling permanently**, because each lives where a rename breaks
silently: `roughly.toml` is still read (both names are checked in one directory before walking up, so
the new name wins a tie); `# roughly: allow(...)` still suppresses, which matters most because that
one lives inside users' source files; and every `RY_*` variable falls back to its `ROUGHLY_*` name.
The VS Code extension reads `ry.*` settings and falls back to `roughly.*`. The REPL history directory
is moved once rather than renamed, so nobody loses their history.

**Left for the user, because an agent should not decide them:**

- **The GitHub repository name.** Docs, badges and install commands now say `felix-andreas/ry`, so
  they are wrong until the repository is renamed. GitHub redirects the old URLs, so this is safe to do
  whenever — but it is currently the one inconsistency in the tree.
- **The VS Code Marketplace identifier.** `package.json` now says `felix-andreas.ry`; the Marketplace
  does **not** redirect an identifier, so publishing under it creates a new listing and starts
  installs and ratings from zero. Revert that one field if keeping the listing matters more.
- **Registering `ry-lang` on crates.io**, and `ry-lang.org`.
- **The landing-page hero animation** (`docs/src/pages/index.astro`) still spells out "Roughly" in
  particles — `ROUGHLY_LINES` is the ASCII art it draws. It is user-owned by standing instruction, so
  it was left untouched deliberately; it needs the new name from whoever owns it.

## Open: what is left before the legacy tree can be deleted

Every crate under `legacy/` needs its own explicit go from the user and stays in-tree until then.
The `rofy` deletion is not a precedent, and the reason is measured. `rofy` was a predecessor of a
shipped component with a 266-line surface that could be read in full. `analysis-legacy` is different
in kind. It holds 2,830 fixture cases against the new stack's 1,192, and the new code runs none of
them. `legacy/fixtures` is a harness-only crate, and `analysis-legacy/tests/test_fixtures.rs` drives
those cases against the frozen oracle, with no new-stack test reading those directories. Case-name
overlap is 15 of 138 for the IDE suite and 1 for the whole typecheck suite, so the corpus was
reimplemented rather than ported. Name overlap understates behavioral overlap, so do not read it as
2,830 cases of missing coverage. It does establish that nothing has shown the new suites cover what
those do.

The inputs are mined already, and that is the part that transfers. 1,967 distinct sources live in
`crates/syntax/tests/corpus-legacy/` and run in the `syntax`, `format` and `semantics` invariant
batteries. The testing page describes them. Expectations deliberately did not come with them,
because the naming suite renders binding-resolution trees and the type suites use an older notation,
so bulk-blessing would encode today's behavior as the contract. What runs was measured rather than
assumed: all 2,447 extracted sources through `ry check`, with zero crashes and zero non-clean exits,
and the invariants pass on all of them. This arm is therefore a regression net rather than a
bug-finder today.

Two findings from the mining are worth keeping. The frozen `type_syntax` suite stores bare
annotation bodies without the `#:` marker, because that stack parsed the type grammar standalone.
287 of its 303 cases therefore read as "expected a statement, found @" until the marker is
prepended, which is a format difference and not a parser gap. Automated semantic mining also has a
high noise floor, because the sources are fragments whose declaring context lives in the case's
other files, so `@new Person` alone reports an unknown type. Adjudicating the type suites needs
per-case context rather than a bulk pass.

**What is left is the expectation half.** Write a triage that emits `(id, source, frozen
expectation, new rendering)`, bucket it by shape, and adjudicate per suite against the type-system
reference. Start with `naming`, which has 513 cases and no new-stack counterpart at all, then
typecheck, `type_syntax`, diagnostics and IDE.

The performance witnesses that apply to the new stack alone move out of `legacy/differential` before
the deletion sweep. That is the sweep's only other prerequisite.

## Open: a size bound on a constructed type, and a decision on recursion widening

`TYPE_SIZE_CEILING` caps a type at the point it is recorded, which stops the pathology. The general
question is still open: nothing caps how large one constructed type may get in the first place.

Widening past a bound to `Unknown` is the sound-by-refusal move the loop join already makes for a
variable whose type keeps growing structurally. The alternative is folding to a recursive nominal.
Choosing between them is a semantics design decision rather than an optimization, so it wants a
decision record.

Computing the size cheaply needs care. The graph is small while the tree is enormous, so a
distinct-node count will not see the problem and a naive tree count is itself exponential. It wants
a memoized size where the size of a node is one plus the sum of its children's sizes, which is
linear in the graph and yields the true tree magnitude. `types::type_size` already has that shape.

## Open: neither cycle fix has end-to-end coverage

Neither the non-convergence fix nor the missing cycle recovery has a fixture. The failing inputs are
whole CRAN packages, and a synthetic case built from the suspected mechanism reproduced neither. For
the non-convergence, a self-growing definition, three mutual-recursion shapes and an overloaded-call
cycle all converge fine, because a single item pins and settles while the bug needs several members'
pins to interact. What is pinned instead is the structural property the fix rests on, which is
`refusal_is_idempotent` in `semantics.rs`. That is the part testable without reproducing the cycle.
The end-to-end guard rests on the corpus suites.

## Post-beta (explicitly out of scope for now)

- Tags / discriminated unions via a compiler-known stdlib `match` (design in `contributing/design/open-questions.md` first).
- S3 dispatch modeling (`UseMethod`) — prerequisite for honest `print`/`summary`/`plot`.
- data.frame column-level typing; matrix dimensionality; real S4 typing.
- Traits/typeclasses (tripwire: the third constraint kind).
- CRAN stub auto-generation via R introspection, R-version-keyed corpora, stubtest validation (R-dependent). (NAMESPACE/DESCRIPTION awareness moved to Open — semantics by user ask.)

## Shipped ledger (one line each; rationale in `decisions.md`, contracts in the docs site)

- **A numeric condition is accepted**, because R coerces one, where zero is false and anything else
  is true. That covers `if (length(x))`, `while (n)` and `!length(x)`. A `character`, `complex`,
  `raw` or vector condition stays an error.

- **Named arguments claim their formals before positionals fill what is left.** `match_arguments`
  walked the argument list once in source order, so a positional argument could take a formal that a
  later named argument was going to claim, and `vapply(xs, character(1), FUN = f)` reported a bogus
  "FUN given twice". `argument_targets` now computes the two passes once and shares them with the
  checking loop and the rest-parameter forwarding scan, which previously duplicated the accounting
  and therefore duplicated the bug.

- **`$` on a union subject no longer demands the field on every member.** A field some shapes carry
  and others do not reads as `T | NULL`, because that is what R answers for a name a list lacks. A
  field no shape carries stays an error, because the typo check is the whole value, and a structural
  refusal such as `$` on an atomic vector is still hard from any member. The did-you-mean is drawn
  from every field the union can carry, so the suggestion survives a member with no fields at all.

- **Literate prose is blanked per byte, not per character.** A non-breaking space is whitespace to
  Rust's `char::is_whitespace` and an unexpected character to R's lexer, so prose containing one
  reported a syntax error against a blank line. The same code blanked per character, so any
  non-ASCII prose shifted every byte offset after it, and every downstream range is a byte offset,
  so diagnostics in later chunks were silently misplaced. Each character now blanks to its own
  `len_utf8()` in spaces. The unit test that should have caught it asserted char count instead of
  byte length.

- **A duplicate type name reports in a script too.** The reference said the duplicate-`@type` error
  fires regardless of file, and it fired in package files only. The value-name analogue stays
  deliberately exempt for scripts. Type names are not.

- **A list of functions works, and the cause was not the union.** `function_compatible` demanded an
  exact parameter count, so `mean`, which has one required and two optional parameters, could not
  serve a one-argument callback interface, and the union of two such functions failed member-wise.
  Arity is a range now: a function serves an interface when it accepts every call shape the
  interface promises. Extra optional parameters are therefore fine, while requiring too many, or
  refusing an argument the interface sends, still fail. `lapply(list(mean, sd), function(g) g(1:3))`
  is `list[double]`. `lapply` also keeps its input's names, through `list[named: T]` declared as its
  narrower first candidate, which is what forced the overload tiebreak to become plain first-match.

- **An S3 method declared in R is not reported `unused`.** The unused walk shares the method-name
  knowledge with the lints, and a project's own generics count rather than only the corpus's.
  Dispatch is still not a read, so the exemption is by name shape, meaning `generic.class` for a
  generic that exists, which is the same signal R itself uses to find the method.

- **`%*%`, `%o%` and `%x%` return the `matrix` nominal**, and the class has its arithmetic and
  comparison methods, so a matrix expression types and composes.

- **A `library()` of an unstubbed package no longer disables bare-name resolution project-wide.**
  Four independent testers reported this, and the most damning version was
  `library(totallyMadeUpPackage)`, naming a package that does not exist, buying the same blanket
  tolerance. The tolerance itself is right, because an unstubbed package's export set is unknowable,
  so the fix narrowed it three ways. A manifest is enough, so a namespace with an `.exports` list
  has a knowable export set and never earns the tolerance. The near-miss carve-out now covers locals
  and parameters of the enclosing item as well as top-level project symbols, because a name one edit
  away from a binding of your own is a typo rather than somebody else's export. And a `library()`
  naming the project itself, through `DESCRIPTION`'s `Package` field, buys nothing, because those
  exports are the project's own definitions. `usethis` generates the `tests/testthat.R` that does
  this, so it was switching unresolved detection off in every testthat package.

- **`structure(list(...), class = "x")` keeps its record type.** It yielded `Unknown` against a
  documented promise that this is a plain record whose class attribute is data, so the field typo
  the guide headlines was caught on a bare `list()` and silently dropped once `class =` was added.
  Those are the objects real packages are built from, and 31 of one tester's 34 strict findings
  traced to this one call. `structure()` now returns its first argument's type. The class attribute
  deliberately still does not mint a nominal type, because `@new` remains the only nominal
  introduction and reading `class[1]` would type a `c("grouped_df", "tbl_df", "tbl", "data.frame")`
  value as `grouped_df` and then reject it at a `data.frame` parameter.

- **Optionality comes from the formals.** A formal with a default is optional in R and no annotation
  can change that, so the exported signature takes optionality from the code and an annotation's
  disagreement is reported once at the definition, naming the fix. Callers of correct R are clean.

- **Files directly under `tests/testthat/` share one namespace**, as testthat's own loading does,
  with helpers first and then tests. A shared fixture is therefore neither `unused` at its definition
  nor `unresolved` at its uses, while a real typo in a test still reports with the helper suggested.

- **A project's own S3 generics are discovered by their bodies.** A top-level definition whose read
  set contains `UseMethod` is a generic, across the whole package namespace, so a generic in
  `R/speak.R` covers `speak.dog` in `R/dog.R`. The generic itself is exempt too, because it declares
  the dispatch argument and never touches it. That fixes `unused-parameter` flagging a project's own
  generics and their methods, and the default-on `unused` reporting an S3 method as dead.
  `is_s3_method_name` and `s3_generics` live in `semantics.rs` beside the other project-level
  projections, shared by the lint and the unused walk.

- **A bad `importFrom` is an error**, matching its `export()` sibling, because R refuses to load such
  a package, so it is not survivable advice and must not pass a `--min-severity error` gate. A bad
  `pkg::name` read stays a warning, because a bad import stops loading outright while a bad
  qualified read fails only if that line runs.

- **ggplot2 `+` chains type.** The corpus declared `+.ggplot` and nothing for a component pair, so
  `theme_minimal() + theme(...)`, and a chain whose left operand was lost through `%>%`, fell through
  to the numeric rules and reported arithmetic on a `gg`. R routes all of it through the single
  `+.gg` method, which the corpus now declares. A genuine mistake such as `plot + 1L` is still
  caught, naming both classes.

- **An inline `` `r expr` `` span is code.** The conversion blanks the delimiter and language tag to
  spaces, the expression keeps its bytes and its offset, and the closing backtick becomes the `;`
  that separates two inline expressions on one prose line. A value a report only displays is
  therefore used rather than unused, and a typo inside an inline expression reports at the right line
  and column. A plain Markdown code span, a span naming another language, a `` `rate` `` span that
  is not an `r` tag, and an unclosed span all stay prose.

- **`fmt` on a target with no R in it reports that it formatted no files and exits 0**, as `check`
  already did. A stage with nothing to do must not fail a pipeline, and a pre-commit hook handing the
  formatter a literate document it deliberately skips must not fail the commit.

- **`setkey`, `setorder` and `unique(DT, by=)` work.** `setkey` and `setorder` name columns rather
  than values, so they are `@masked` like the other non-standard-evaluation verbs, while the
  `v`-suffixed forms take a character vector and never needed it. `unique`'s fallback candidate is
  variadic, because `unique` is a generic whose methods take arguments base's signature does not
  name, and the typed candidates stay exact so `unique(c(1L, 2L))` is still `integer[]`. Verifying
  that turned up one more defect: every column-name parameter in the data.table stub was declared a
  scalar `character`, so `setkeyv(DT, c("id", "date"))`, which is the whole point of the `v` forms,
  was an error. They are `character[]` now, which accepts one name or several.

- **A read inside `on.exit()` keeps every write of that name in the frame alive**, exactly as a
  closure capture does, because R stores the expression and runs it at return so it observes the last
  value of what it reads. A genuine dead store beside a guard still reports.

- **An overload selection that commits an `Any` return records a strict origin.** `min(Date)` is not
  typed `integer`. It selects the corpus's trailing `Any` candidate, so the check is skipped rather
  than wrong, and the quality bar holds. What that exposed is that `Any` was exempt from strict mode,
  so the feature whose whole purpose is finding gaps missed the commonest one.

- **`x[i, j]` on an unannotated parameter is sound-by-refusal for any index shape**, which is what
  the reference already promised. A subject whose shape was written down still refuses a shape no
  rule covers, so `c(1L, 2L)[1L, 2L]` is an error.

- **`c()` on a classed value keeps the class when a `c.Class` method is declared**, which is R's own
  dispatch rule, so `c(d1, d2)` is a `Date` and `dates + d2` is still refused as `Date + Date`. A
  nominal with no such method is indeterminate rather than an error.

- **The IDE fuzz harness samples completion once per context instead of per offset.** Completion
  was 587.26 ms of the 592.4 ms that nine features spent over 25 offsets, on a warm database, and
  its only assertion was that a label is non-empty. It was also mostly redundant, because 245 swept
  offsets over 10 seeds produced 64 distinct results. Sampling per completion context, which is the
  kind of token the cursor sits in or after, halved the harness and took the whole `ide` binary from
  195 s to 94 s while gaining two range oracles. Two sharper-looking variants were measured and
  rejected: filtering to token boundaries saved 5%, because in a short input nearly every sampled
  offset already is one, and keying on the pair of surrounding kinds saved 29%.

- **One predicate decides whether a file shares a namespace with its siblings.** `cli.rs`'s
  `shares_a_namespace`, the server's `is_package_path` and `stats.rs` each answered that question,
  and they disagreed. The first two counted `R/` and `tests/testthat/` while `stats.rs` counted `R/`
  only, directly under a comment claiming it ordered files exactly as the CLI and server do. On a
  testthat package `analysis-stats` reported 3 diagnostics where `check` reported 0. All three now
  call one predicate, with the sorting key kept separate from the classification, because
  collapsing those two questions into one flag is what caused the drift. A CLI test pins it.

- **The human reporter was quadratic in findings times file length, and is now linear in findings.**
  Neither review saw it, because both measured through `analysis-stats` or `--output json`, which
  never touch that path. On one file of 8,000 reporting items, the whole cold analysis was 452.6 ms
  and `--output json` finished in 550 ms while plain `ry check` took 10,527 ms. Instrumenting the
  reporter put 6,728 ms of `render`'s 6,811 ms in `read_span`, which is 98.8%, because miette's
  `SourceCode for str` locates a span's lines by walking from byte zero: 31 microseconds per span on
  a 1,000-line file and 245 on an 8,000-line one. `read_span` now answers over a bounded window, the
  span's lines plus a margin wider than the requested context, and translates back into whole-file
  coordinates. It delegates to miette inside the window rather than reimplementing `SpanContents`,
  so one implementation decides what a snippet contains, and a per-file `LineStarts` table makes
  locating the window a binary search. Interleaved, 8,000 findings go from 6,483 ms to 530 ms.
  Rendered stderr is byte-identical on data.table, dplyr, ggplot2 and shiny, related-note snippets
  included. A differential test sweeps the window against miette's whole-file walk over every line
  ending, spans mid-line, across a break, at the start and the end, and a file with no trailing
  newline.

- **A quoting form no longer binds its assignments.** `quote`, `substitute`, `bquote` and
  `expression` arguments were binding, so `quote(x <- 1)` published `x` and every quoted call was
  type-checked. Removing that takes `targets` from 16.6 s to 9.9 s and removes 151 findings, all
  false positives: 139 arity and type errors against calls inside `quote({...})`, which R does not
  run there, and 12 unresolved reads of names mentioned in a quotation. The type-system reference
  holds the contract.

- **A conditional slot's join is bounded at eight writers and widens past that to `Unknown`.**
  `conditional_slot_scheme` called `statement_binding_scheme` for every writer of a name, unbounded,
  from a per-item read, so a name written at 238 documents' top levels made every read of it pay for
  all of them. The bound takes `targets` from 7.14 s to 0.74 s interleaved, and to 0.74 s from
  16.6 s together with the quoting fix, with byte-identical finding sets on `targets` and all four
  corpus packages and no regression elsewhere. It is honest on its own terms, because a union of
  dozens of unrelated types is not a fact a check can use and a real conditional slot has a handful
  of writers. Fixtures pin both sides of the bound.

- **Three per-item queries stopped re-deriving the whole file.** `SalsaGlobals::arithmetic_classes`
  collected a `Vec<String>` of every item name and re-scanned it, `item_tree` was `returns(clone)`
  so the whole item list was cloned per item, and `for_item` found an item's index by linear search.
  They are now a memoized per-file query, a borrow and a memoized position index. On the JSON path,
  8,000 items go from 7,045 ms to 600 ms, which turns a quadratic into a linear curve.

- **`item_hir` and `item_naming` return by reference.** Both were `returns(clone)`, and `ItemNaming`
  is `BTreeMap` and `BTreeSet` throughout, so each fetch was a node-by-node allocation walk. A
  counting probe put ggplot2 at 5.4 fetches per item, costing 178 ms and 265 ms of fetch time.
  Interleaved medians improve 8% on ggplot2 and 13% on `targets`. Peak memory is unchanged, which is
  the expected result: the clones were transient, so they cost allocator traffic rather than
  resident set, and peak is dominated by the memos themselves.

- **The project type map and arithmetic-class set are borrowed per file, not cloned per item.**
  `check_item_with_annotation` cloned both and stored them owned on `InferenceTable`. Both are
  per-file facts and pure lookups, so they are memoized per-file tracked queries returning a
  reference. At a fixed 2,000 items with 2,400 declarations, typecheck goes from 71.3 ms to 27.5 ms.

- **A package file got the position-aware lookup a script already had, so a later top-level write is
  no longer lost.** `SalsaGlobals` built its ordered item list for scripts only. A file is sourced
  top-down whichever kind it is, so an immediate read now consults the nearest earlier writer in its
  own file before the project-wide map. The composition constraint splits on the read kind rather
  than the document kind: a deferred read in a package stands aside and falls through to the
  project-wide winner, because a function body runs after the whole package is sourced, so a later
  file's override must win. In a script the closure runs once that file's frame has settled, so it
  still scans the file. `file_binders` indexes it per file, which took an interleaved 20,000 by
  20,000 case from 4,277 ms to 985 ms.

- **A composite type past `TYPE_SIZE_CEILING` records as `Unknown`, which closed a hang.** A record
  whose forty fields all return that record grew through 877, 8823, 104655 and 1046623 nodes. One
  1,563-line file ran past 200 s and now takes 56 ms, and the package holding it went from over five
  minutes to 129 ms. Findings are byte-identical across 1,951 files of real CRAN sources, and
  interleaved it is about 7% faster on 323k lines. Six other fixes were implemented, measured and
  reverted first, and `MEMORY.md` records the walk-with-a-memo rule they taught.

- **Exponential re-inference of arithmetic operands is fixed in `infer_binary`.** One statement with
  248 arithmetic operators re-walked both operand subtrees per level. `mgcv` did not finish in 180 s
  and now takes about 2 s. `MASS` went from 6.5 s to about 0.3 s from the same fix. R6 was suspected
  in both and was the cause in neither.

- **Strict mode reports a read that the attached-package tolerance silenced.** `unresolved_diagnostics`
  buried the tolerance in a `continue`. `classify_non_local_read` now returns `Resolvable`,
  `Tolerated` or `Unresolved`. The ordinary check reports the last and strict reports the middle, so
  strict reports exactly the reads the ordinary check let through and the two cannot drift.

- **Release-artifact versions have one source of truth each.** The workspace `Cargo.toml` version is
  the truth, and the VS Code manifest carries it with any prerelease suffix removed, because that
  manifest needs a plain `major.minor.patch`. Two tests in `crates/ry/tests/test_release_metadata.rs`
  enforce the derivation, and the assertion message names the exact line to write. A stamping script
  was considered and not written, because a script only helps if someone runs it while `cargo test`
  runs on every slice. The Zed manifest versions on its own line, which `decisions.md` records.

- **The formatter is not slower than the type checker.** The original reading compared a parallel
  command against a sequential one, because `ry check .` fans out over `available_parallelism()`
  while `ry fmt` is a plain loop. On an identical file set with both single-threaded, the formatter
  costs 1.01 s against the checker's 2.26 s. Fanning `fmt` out is still open, as is the unlocalized
  fact that the render is about eight times the parse, at 1.9 MiB/s against about 18 MiB/s.

- **The overload corpus is not inflated by a missing grammar feature.** The constrained binder works
  in a `#:` annotation and in a `.Rtypes` file alike, and shape-mirroring falls out of it: a project
  stub declaring `zzabs : <T: numeric> fn(x: T) -> T` types `zzabs(1L)` as `integer` and
  `zzabs(c(1.5, 2.5))` as `double[]`, and rejects `zzabs("no")`. The extra candidates carry facts a
  binder cannot state. `abs(TRUE)` is an `integer`, so a type-preserving binder would be wrong, and
  a concrete `integer` parameter accepts `logical` by coercion where a numeric-constrained variable
  refuses it. `min`, `max`, `range` and `sort` carry a `character` candidate no numeric binder
  subsumes. Collapsing `abs` from five candidates to four was behavior-identical and was reverted,
  because one line in one function is not worth a corpus-wide edit.

- **The `rofy` crate is deleted.** `crates/repl` covers its whole surface and exceeds it with Tab
  completion and history persisted to a file, and it highlights off ry's own lexer rather than a
  second parser. The canonical test invocation is now `cargo test --workspace --exclude zed_ry`, one
  exclusion rather than two, and the workspace lost `extendr-api`, `extendr-engine` and `libR-sys`,
  which removes the build-time dependency on a local R.

- **A non-converging cycle now terminates, because the refusal no longer depends on the round that produced it:** `item_check_recover` pinned only `scheme` at the round cap and took `ItemCheck`'s six other fields from the freshly recomputed value, so every round returned a different check, the recovery's own equality test could never succeed, and salsa iterated to its `MAX_ITERATIONS` of 200 — 184 rounds past the cap — then panicked with `too many cycle iterations`, or exhausted memory first, whichever the machine reached (the two symptoms were always one bug). Its sibling recoveries were safe only incidentally: `global_scheme_recover` and `statement_binding_recover` return a bare `TypeScheme`, so their pin is already a constant, and `item_check` is the only one of the three with a composite return. The pin now re-pins what was already returned, a fixed point by construction, and cuts both export surfaces rather than just one — `top_level_bindings` was leaking moving schemes out of an item that had already been declared non-converging, which its own doc comment said it did not. Verified on the real reproduction: rlang's whole package directory went from a 213-second death to a clean 9-second run reporting 806 findings, and the flattened 163-file variant that had been OOM-killed now finishes in 14 seconds. `refusal_is_idempotent` pins the property; `htmltools` still stalls with no cycle panic, confirming it is a genuinely separate pathology.

- **`ry check` no longer aborts on cyclic package interfaces:** `scc_schemes` was the one query in the interface-fixpoint chain with no salsa cycle recovery, while `item_check` and `global_scheme` — the queries either side of it — both had one. Its doc comment asserted that member checks run directly so "no salsa cycle forms", and the decision record reserved recovery "as a backstop for edges the static graph cannot see"; the backstop was never installed, so such an edge aborted the process. `interface_sccs` builds edges only from names appearing in an item's source, so a name the checker *constructs* is invisible to it and the group is not as maximal as the fixpoint assumes. Recovery pins the group to `Unknown` — the answer the round cap already gives — and refuses on first disagreement rather than iterating, because the group runs its own bounded fixpoint internally and letting salsa iterate it too multiplies those rounds into an out-of-memory kill (measured: the iterating version turned the panic into exit 137, which is worse than the bug). Verified on the real reproduction: rlang's `R/` went from exit 101 to a clean run with 838 findings.

- **A manifest is enough to keep unresolved detection alive, and fifteen CRAN namespaces now ship
  one:** attaching a package whose exports the checker cannot enumerate disables the check
  project-wide, which three independent adoption reviews reported as the single worst hole — a clean
  run was indistinguishable from "not checked". Manifest-only namespaces (no `.Rtypes`, every name
  `Unknown`) close it for the tidyverse, `knitr`, `rlang`, `glue`, `magrittr`, `scales`, `jsonlite`
  and `R6`; `library(tidyverse)` activates the nine members it attaches, so dplyr's and ggplot2's
  *typed* declarations come with it. The generator script now refuses to overwrite a manifest recorded
  against a newer R than the running session, because doing so drops the names that version added and
  turns each use into a false `unresolved`.

- **A bad `NAMESPACE` `importFrom` is an error, and the strict severity jump is documented:** R
  refuses to load a package whose import names a non-export, so a warning let it pass a
  `--min-severity error` gate — it now matches its `export()` sibling. A bad `pkg::name` read stays a
  warning (it fails only if the line runs), and the reference states the asymmetry. Separately, two
  reviews were surprised that `strict = true` raises every `unresolved` finding to an error; the
  behaviour is right, so the diagnostics table and the strict-mode section now say so.

- **Reported columns count characters, and the caret lands under the glyph:** byte columns disagreed
  with every editor on any line carrying non-ASCII text and pushed the caret right of the code it
  accused — sometimes past the end of the line. Columns are characters now (header, JSON, and the
  server's human-readable locations); caret padding is terminal cells, so double-width text aligns
  too. The `--output json` field documentation changed with it (see `decisions.md`).

- **S3 dispatch counts as a use, and a project's own generics are real generics:** the default-on
  `unused` lint called `speak.dog` dead and the opt-in `unused-parameter` called a generic's dispatch
  argument ignored — both false positives on working code, since dispatch is neither a read nor a call
  the checker sees. A generic is now any top-level definition whose read set contains `UseMethod`,
  unioned across the package namespace (a generic in one file covers a method in another), and both
  the generic and its `generic.class` methods are exempt. `is_s3_method_name`/`s3_generics` sit in
  `semantics.rs` with the other project-level projections rather than in the lint module, because two
  diagnostic layers share them.

- **A package's own `library()` call no longer silences unresolved names:** the unknowable-export-set
  tolerance skips the project's own name (`DESCRIPTION`'s `Package`, now on the metadata input), so the
  `library(yourpkg)` `usethis` writes into `tests/testthat.R` stops switching off unresolved detection
  package-wide. Two independent adoption reviews hit this.

- **Shape-preserving stubs actually preserve the shape:** `Filter` returned `list[T]` for every input, so `Filter(f, c(1, 2, 3)) + 1` — R selects with `[`, which keeps the atomic type — was a hard type error; it now declares the atomic, named-list and plain-list forms in that order. `rev`/`unique`/`head`/`tail` gained the named-list candidate too, so a name read off a reordered or sliced list is no longer a missing-field error (see the residual record in §Open).

- **Overload sets now read like the corpus is written, and `lapply` keeps its input's names:** the "all fits are guesses → last candidate wins" tiebreak is gone (a general `Any` fallback is already a *fact*, which was the only case it protected), so declaration order means first-match everywhere and a narrower candidate can be declared first — which is what let `lapply` gain `fn(x: list[named: T], ...) -> list[named: U]` without handing a value use of the name the narrow contract. The all-fail diagnostic no longer buries the answer behind "I tried all N signatures": when every candidate fails identically, or one candidate got strictly further into the argument list than the rest, that candidate's own finding is reported at its own argument's range. Signature help stopped requiring a commitment, so an incomplete call (`lapply(|)` — no candidate matches yet) lists the set instead of showing nothing, with the committed candidate rendered instantiated for the call site.

- **`roughly check` stack overflow fixed:** the check fan-out's scoped worker threads ran on default 2 MiB stacks while cross-file interface resolution recurses per dependency edge — a ~500-file definition chain aborted the whole command (`analysis-stats` survived only because the command thread carries `ANALYSIS_STACK_SIZE`). All three scoped spawns in `cli.rs` now use `ANALYSIS_STACK_SIZE`; verified on the 5,000-file synthetic tree (SIGABRT → clean exit-1 with findings).

- **Duplicate-diagnostics O(files²) walk killed:** `duplicate_binding_diagnostics`/`duplicate_type_diagnostics` rebuilt a project-wide occurrence map per file AND made every file's diagnostics depend on every file's ranges (any edit re-executed them all). Now: range-free per-file name projections (`top_level_binding_names`/`type_declaration_names`, value-equal under body edits) feed memoized project duplicate maps filtered to real duplicates (near-empty in healthy projects — the value-equality firewall), and per-file diagnostics fetch ranges only for files actually involved in a duplication. Same-tree A/B at ~530K LoC / 5,000 files: semantic render 12.1s → 3.7s, cold 22.8s → 14.4s, diagnostics byte-count identical.

- **Export manifests — every real R export resolves:** `types/<ns>.exports` (generated from live R by `scripts/export-manifests.R`) covers every namespace R ships in three tiers — default-attached (now incl. `datasets`, famous frames typed `data.frame`) bare-visible unconditionally; `QUALIFIED_ONLY_NAMESPACES` (tools/parallel/compiler/grid/splines/stats4/tcltk) always valid after `::` but bare only when attached/declared; conditional CRAN namespaces gated with their stubs. Manifest names resolve (typing `Unknown` without a typed declaration), feed completion and typo suggestions, and a unit test pins every `.Rtypes` value declaration as a real export of its namespace (caught `traceback`/`standardGeneric` misfiled — both are base exports). Kills the could-not-resolve false-positive class for the whole shipped standard library.

- **Diagnostics-phase whale killed (was 69-80% of the cold pass at scale):** `declared_global_variable` scanned every project file per non-local read — O(reads × files), ~375M memo lookups on a script-heavy workspace — and script-local cross-statement reads paid the project-wide guard chain before their own frame-slot resolution. Now one memoized project-level `globalVariables` union set (`project_global_variable_declarations`) plus cheapest-first guard order (masked → frame slots → package/stub → super-globals → declared globals → imports). Reproduced at 305K LoC / 2,500 files: diagnostics 22.7s → 2.9s, cold total 28.0s → 8.9s, diagnostic output byte-identical; `analysis-stats` permanently splits the phase into semantic render / lints / assembly — the instrument that found it.

- **Formal-aware `@masked` + the conditional dplyr namespace:** the stub loader records each masked verb's pre-`...` formal names (`StubLibrary::masked` map) and naming resolves arguments matching them (position or name) while masking what `...` absorbs — zero-formal masks (`join_by`) mask everything, restoring the reference's documented contract; `dplyr.Rtypes` ships as the second conditional namespace with class-preserving `@masked` verbs (`<T> fn(.data: T, ...) -> T`), class-preserving joins, and the tidy-select/verb vocabulary, so piped verb chains type end to end with masked column reads (`dplyr` fixture group in typing-imports; decision record has the collision/shadowing note).
- **The native pipe types as the call it is:** `hir::lower_pipe` desugars `x |> f(y)` to `f(x, y)` (R's own parse-time rewriting) with the `_` placeholder substituted as the one named argument R allows it to be (`pipe_shape`, syntax-level; the `_` token never lowers, nothing dangles); everything R rejects keeps the opaque-operator lowering. Naming/typing/overloads/arity/strict/IDE inherit the real call; piped-value errors blame the left-hand range; pipelines type end to end. Gate terms renegotiated via two `ACCEPTED_DIVERGENCES` entries (oracle never modeled pipes); the two strict cases that pinned pipe-as-origin repurposed to `%in%`; `%>%` deliberately stays opaque (a real function, not sugar). Reference documents the rule under Function calls → The native pipe.
- **data.table awareness (NSE ladder rung 1):** a conditional stub namespace (`crates/semantics/stubs/data.table.Rtypes` — the `@type data.table` nominal + ~45 declarations) joins stub assembly only when `metadata::namespace_active` (DESCRIPTION/NAMESPACE declaration, or a `library()`-family call found by the per-file `file_attached_namespaces` scan, unioned into `PackageMetadata.attached` by the hosts — server incrementally per synced file + idle prime); a bracket whose subject is the `data.table` nominal masks all index reads (`ItemCheck::masked_reads` skips the unresolved warning — kills the `DT[speed > 20]` false-positive class) and classifies its result by `j`'s syntax (no/empty `j`, `:=`, `.()`/`list()`, grouped `j` keep the subject's class; the rest refuse as Unknown with a strict origin). Typing-reference "Data-masked evaluation" + stdlib-stubs "Conditional namespaces" are the contracts; `datatable` fixture group in the differential-excluded typing-imports suite; decision record has the cycle/keystroke-cost analysis.
- **`analysis-stats` ported to the new stack (user ask):** `roughly debug analysis-stats [path]` (`crates/roughly/src/stats.rs`) assembles the workspace exactly as `check` does (stubs, metadata, Collate order) and reports staged cold-pass phase timings (load / parse / lower+naming / typecheck / render) with per-phase resident-set growth and peak, slowest-files-by-typecheck, and a typing-burst probe on the slowest + median + small files with `CHECK_EXECUTIONS`/`RESOLVE_CALLS` attribution and the raw re-parse latency floor. Forces typing on with a note; documented on the development docs page; CLI contract test pins the report sections.
- **Missing-comma parse recovery (user ask, rust-analyzer style):** when a token that can start a new element follows a complete argument or parameter, the parser reports `` missing `,` between these arguments``/``…parameters`` anchored at the empty range right after the previous element — on that element's line, not wherever the next element starts — and parses the next element normally (a proper `ARGUMENT`/`PARAMETER` node, no `ERROR` wrapping, so downstream analysis sees the intended list). Junk that cannot start an element keeps the old `expected `,` or …, found …` recovery. `starts_expression` mirrors `primary`'s entry set (kept in lockstep); golden error cases pin single-line, cross-line, and junk-recovery shapes.
- **`DESCRIPTION` `Collate` file order implemented:** `parse_description_collate` (`Collate`, falling back to `Collate.unix`); both hosts rank package files by Collate index before the path tiebreak (unlisted files order after the listed ones), the server reorders the project when a DESCRIPTION change moves the collation, and a CLI contract test pins the winner flip. Closes the reference's "Project file order" promise, which had no implementation.
- **NAMESPACE/DESCRIPTION metadata feeds resolution (user ask):** `semantics::metadata` owns the NAMESPACE parser (moved from the host crate) plus a DCF DESCRIPTION dependency parser and the singleton `PackageMetadata` input; `importFrom(pkg, name)` names and stub-described `import(pkg)` exports are known bare reads, a stub-less `import(pkg)` tolerates all otherwise-unresolved bare reads (export set unknowable — the zero-false-positive rule), and `pkg::` reads of declared-but-undescribed namespaces stay quiet. Hosts install the input next to the stubs (server refreshes on NAMESPACE buffer sync + NAMESPACE/DESCRIPTION watcher events, diffing parsed facts). New `typing-imports` fixture suite (metadata directives documented in testing.md; excluded from the differential arm — the oracle has no metadata concept); typing-reference "Package imports" section is the contract; decision record in decisions.md.
- **Blame-range precision (trailing trivia + parens):** the real-file corpus scan found two systematic same-finding range near-misses vs the oracle — expression ranges swallowing trailing whitespace/comments the Pratt loop consumed while peeking for the next operator (fixed: HIR lowering stores the trivia-trimmed significant range), and blame sites reporting a parenthesized wrapper instead of the expression inside (fixed: type-error blame drills through `Paren`; strict origins and missing-formal reads deliberately keep binding-site ranges). Both pinned by fixture cases verified to fail without the fixes; the scan's 39 paren + 8 trivia near-misses went to zero and file-level corpus matching rose 3,293 → 3,303/4,638.
- **`[` on vectors defined (was the largest real-code gap):** the typing reference specifies the full index-shape × subject-shape matrix (scalar-like numeric/character index → the scalar-claim element, with the negative-index caveat documented under the flexible-operand compromise; vector-like and logical-mask indexes → the subject's vector shape, names surviving; character indexes legal on any vector; undetermined indexes claim scalar and stay unconstrained; list/function/complex/raw indexes error) and `subset_result` implements it through a `vector_index_shape` classifier. Seven fixture cases pin every row; the oracle never defined vector `[`, so its refusals are an oracle-deficit class in the shared filter (1,415 accepted on the real-file corpus — the rewrite stopped reporting them on real code) plus per-case adjudications in the fixture and IDE arms. The NSE/data-masking design ladder is recorded as typing-design question 7.
- **Corpus-scale verification + dots-forwarding arity fix:** the real-file corpus arm reran after the cross-item-read and typed-NA changes — 3,322/4,638 files match (up 54 from the prior sweep; oracle-deficit acceptances rose ~975 because the rewrite now resolves reads the oracle cannot, e.g. R6 `self`/`private`). The sweep's one genuine false-positive class is fixed: a call argument that is the enclosing function's bare `...` forwards an unknown number of arguments, so such calls now skip both arity checks (`CallArgument::forwards_dots`; typing-reference Function calls documents the rule). The sweep harness also gained the documented-but-missing oracle-side panic guard (an oracle panic is counted and its file skipped instead of killing the run).
- **Callback-idiom stub sweep closed by audit:** the capped-stub premise no longer holds — every high-use base/stats/utils stub already declares its real optional formals (`nchar`'s type/allowNA/keepNA included), and the remaining single-parameter stubs are genuinely unary in R; a fixture pins both directions (`nchar(x, type = "bytes")` directly and `lapply(list(...), nchar)` through callback forwarding).
- **Overload/compatibility fixture sweep + reference fix:** ten typing cases pin overload-set rules (undetermined arguments use the general candidate, the catch-all corpus convention, value-use resolution, local shadowing disabling the set), numeric-variable generalization (`-x`, `x / 2`), and function-type compatibility (by-name parameter pairing, the rename refusal, parameter contravariance both directions, variadic-never-pairs-with-fixed); a CLI contract test reaches the no-matching-overload error through a fully-constrained project stub override (unreachable via shipped stubs — every set ends in an `Any` catch-all). Writing them exposed a reference/implementation contradiction on non-call uses of overloaded names: the implementation deliberately resolves to the last (most general) candidate with recorded rationale; the reference wrongly said first — the reference is fixed.
- **Indexing/guard fixture sweep:** thirteen typing cases pin the documented `[[`/`[`/`$` rules (vector element extraction, the map-like positional/name-based asymmetry, declaration-ordered record positions, backtick fields, record slices, unpinned-parameter tolerance) and the guard-narrowing rules (negated guards, `is.numeric`/`is.function` families, scalar+vector family membership, guards that cannot fire, expression guards never narrowing, the tested-then-unguarded finding) — all matched the contract on first bless. The per-position IDE differential over the new cases caught one real defect, fixed: `$` field completion now offers non-syntactic names in their backtick-quoted (insertable) spelling via the new `syntax::is_syntactic_name`; plus one adjudication (the rewrite hints a scheme for an unpinned-field reader the oracle leaves untyped).
- **Operator/constant fixture sweep + typed-NA fix:** eight typing cases pin the documented operator rules that had no fixture (`%%`/`%/%`, `^`/`**`, unary `-`/`!`, comparison families, `:` shapes incl. the scalar-numeric bound, `&&`/`||` scalar-only) and the reserved constants; writing them caught and fixed a real bug — the typed `NA_*` constants all lowered through the bare-`NA` catch-all and inferred `logical` (now `LiteralKind::Na(NaAtom)` carries the atom at lowering).
- **Per-item interface projections:** whole-project walks (`interface_sccs`, `conditional_slot_items`, `script_definition`'s statement branch) read `item_interface_reads` / `item_top_level_names` — small tracked projections of naming — instead of full `ItemNaming`, so a body edit that shifts ranges without changing a name backdates and no project-wide walk re-executes per keystroke (event-counted by `examples/keystroke_probe.rs`: walks went 8/8 edits → 0/8; misc.r 8.2→4.9ms, zxx.R 5.9→3.3ms medians in-container). New-item edits still re-run the walks, as they must. Architecture.md documents the projection firewall.
- **Shadow lints landed (default-off):** `shadows-builtin` (a top-level binding over a `base` export) and `shadows-namespace` (over another stub namespace's name, message names the shadowed symbol) in `lints::shadow_lints` — driven purely by the stub corpus's `exports_by_namespace`/`declaring_namespace` because bare resolution is ungated, so no NAMESPACE-import plumbing and no CLI/LSP drift; dotted S3 names are naturally exempt (not exports). Fixtures in the lints-style suite, CLI contract test, linter + configuration docs updated.
- **Top-level unwritten-path reads observe the cross-item binding:** a top-level slot's read-before-write (a loop's first iteration, a rebinding statement's right-hand side) resolves through `GlobalEnv::scheme(name, deferred=false)` — nearest earlier item in scripts, definition winner/conditional slot in packages — mirroring the unused check's cross-item-read rule; the observed type is materialized as the slot's pre-state (`pre_materialized`, re-established after each loop-pass rollback) so the loop join keeps it and first-iteration type errors survive to the reported stable pass. Self-referential-only names keep the tolerant `Unknown` (cycle recovery's initial), and sequential script rebindings now chain types (`n <- n + 0.5` after `n <- 1L` is `double`). Typing-reference "Definite assignment" documents the rule.
- **Missing-diagnostic probes closed:** generic-application arity errors at the applied name (`generic type `Box` expects 1 type argument, but found 2.`), non-generic-with-arguments, and bare-generic-must-be-applied (except under `@new`, whose representation check infers arguments) — vocabulary-side checks in the annotation-rule family over lowering-recorded `applied_references`, with mis-applied names flooring to `Unknown` in the relations so the one arity error never cascades. Probes confirmed missing-required-argument and duplicate-formal detection already existed; their wording now says what happened (`a required argument is missing`, `names the argument `x` more than once`).
- **Legacy-corpus parity closed; the corpus arm is a default gate:** all 1,523 comparable single-file case inputs from the frozen stack's own suites match through the shared differential policy (1 adjudicated acceptance: the oracle blames a vector-element violation at the alias declaration where the rewrite blames the use site). The sweep drove, slice by slice: the annotation block-form validation package (attachment/dangling rules incl. the blank-line association fix, directive ordering, duplicate/unknown type parameters, applied binders, `@new` shape + on-alias, vector-element atomicity, nesting caps, definitions top-level-only; refused blocks drop their payload), declared-function shape checks (optional-needs-default, rest-position, both variadic directions; renderer places `...` at its boundary), expression-level annotations (the constructor idiom — statement-level attachment at any depth through one application seam), alias-typed callees calling through their expansion, elided nested returns meaning `NULL`, frame-scoped capture liveness (`Scope::id`), conditional top-level slots exporting per-binding schemes (`ItemCheck::top_level_bindings`, `statement_binding_scheme` with cycle recovery, joined across writers), export-edge generalization of constrained residual variables (`close_scheme`: `<T: numeric>` survives, unconstrained erases), and `missing()` supplied-state flow (`EnvEntry::MissingFormal`: reading a no-default formal on the missing branch errors; writes supply it; the marker is branch-local). Plus the differential fuzz arm (six gaps) and the unknown-type-name class (the reported `Instument` bug) from the same assessment.

- **Error-message release pass:** the golden error suite grew 14 -> 78 cases organized by area, now covering every distinct lexer/parser message template plus recovery-locality and valid-stays-clean pinning (testing.md documents the coverage contract); writing it exposed and fixed a real cascade class — lexer `ERROR_TOKEN`s re-diagnosed by the parser (up to 4 reports for one mistake, now 1) via silent placeholder-atom consumption + statement-level suppression; the fuzz harness gained error-quality invariants (non-empty messages, in-bounds ranges, cascade bound linear in tokens). CI was red on the recorded newer-clippy trap (collapsible_match on 1.97) — fixed against CI's actual toolchain.
- **Docs accuracy pass + truthful landing examples:** development.md rewritten for the shipping crate layout, testing.md's legacy-era half compressed into a scoped frozen-stack section, linter/configuration/stdlib-stubs stale claims fixed (missing-comma retirement, stub path), shipped stubs migrated into `crates/semantics/stubs/` (the product no longer include_str!s from the legacy tree), the staged CI's rationale refreshed — and the landing page's formatter examples replaced after verifying each against the real binary: the old panels showed behavior the formatter doesn't have (bracing `if` one-liners, splitting one-line pipelines, aligning `=` columns); the new panels show verified auto-bracing of loops, operator/comma spacing, and multi-line indent normalization, with the tabs' reserved-dimensions fix confirmed already in place.
- **Formatter docs generation ported to the product:** `crates/format/tests/test_format_docs.rs` regenerates `docs/formatter.md` from `crates/format/tests/formatter.template.md` through the shipping formatter (every template example formats byte-identically to the legacy output); the legacy generator and its template copy are removed.
- **Recursion precision + strict attribution:** the canonical interface fixpoint types converging recursion precisely (top-level `fact`: `fn(n: integer) -> integer`; mutual groups generalize — the old tolerant-`Unknown` self-recursion contract is superseded in the typing reference), and strict mode now attributes the remainder: a cycle member with a clean body whose exported scheme still carries `Unknown` gets a binding-level `RecursiveUnknown` origin; pinned-at-cap cycles keep their read-site origins (decisions.md).
- **Corpus growth + wording polish:** the fetch manifest gained 28 large CRAN packages (Matrix/MASS/mgcv/survival/Hmisc/caret/sf/...) and the source-extension pattern widened to R's full set (`.R/.r/.S/.s/.q` — mgcv ships `.r`, Hmisc `.s`; the four corpus loaders match): corpus 507K -> 965K lines (81 packages, 30.5 MiB, 4640 files), all parsing with zero acceptance/round-trip divergences. At the new scale (per-process instrument protocol — batching the stats tests in one process pollutes the RSS numbers): new stack 19.0s (19.7µs/line) / 1.0 GiB resident vs legacy 30.3s / 2.0 GiB — 0.63x wall, 0.50x memory; parallel == sequential findings exactly (the canonical-fixpoint invariant holds at scale). The one new acceptance divergence was a real lexer bug (`..2dge` mislexed as `..2` + `dge` — R has no dot-dot token at lex time, so a longer name wins; fixed with a golden case). Diagnostic wording pass per the Rust/Elm bar: real pluralization for call-arity/named-argument errors, the `invalid semantics:` prefix dropped and the three `#:` block-form refusals rewritten to name the fix (separate blocks with a blank line), `NotAFunction`/`MixedListElements`/index-shape phrasing tightened.
- **Product-surface polish batch (new stack):** hover definition summaries ("Local variable/Package global, defined at `path:line:col`", stub origin namespace + overload count, maybe-undefined note), `debug = true` hover debug sections (Lowering/Naming/Parsing), S4/R6 document-symbol hierarchy (kinds + R6 member children, workspace symbols include members with real kinds, `fn(params)` details), a deterministic cancelled-pull LSP test (`ROUGHLY_TEST_DELAY_PULL_MS` fault-injection seam holds the pull until the edit's flip lands), **unused warnings on by default** (user directive; `[check] unused = false` opts out) with two script-unused fixes it forced: bare-statement reads (`print(x)`) keep bindings alive, and a definer inside an R-grammar error region never warns.

- **`roughly check` runs on the engine:** the CLI builds the same query graph the server uses (server `ProjectFiles` ordering, shared `assemble_engine_file_diagnostics` in `crates/roughly/src/diagnostics.rs`), so it inherits every engine performance property; `run_full` remains purely the differential oracle. Cluster repro check: 1.34s → 0.28s.
- **Per-definition interface-SCC rounds:** mutually-referencing file clusters (one giant SCC under file-granular edges — THE real-workspace whale, 95% of a 700K-LoC user cold pass) now re-infer per member definition for provably-decomposable files (`scc_definition_plan`), with change-driven skips at both granularities, per-file contribution merges (exact last-writer-wins), one snapshot/rollback inference state per fixed point, change-event oscillation history, and a dense borrow-only `SymbolScc` Tarjan. Cluster repro: cold typecheck 3.6s → 0.2s, member-file keystroke 359 → 34ms. `analysis-stats` now bursts a median and a small file besides the slowest.
- **Checker constant factors (~4× whole-file inference):** allocation-free free-variable walker (no more resolve-clone per recursion level), dense `EntryTable` vector for the union-find, Tarjan letrec-membership (was per-candidate transitive walks), precomputed winner-test lookups + batch exported bindings, FxHash for the never-iterated state maps; per-file interface edges (`WalkShadowed`/`FileInterfaceEdges`) projected per symbol (hub sweep 763→33ms).
- **One inference per file per revision:** `Typecheck` owns `FileInference { check, exports }`; `ExportedSchemes` is a shared-pointer projection (still the value-eq firewall); the could-not-resolve typo hint is memoized per symbol with an allocation-free corpus scan; the letrec candidate-edge scan is one arena pass; `Diagnostics` fetches tree-readers adjacently (one parse per file on cold prime). Cold 10.1s → 3.7s at 302K LoC, keystroke 5.4 → 1.7ms; `analysis-stats` splits lint / package-naming / render stages, and a `profiling` cargo profile (release + symbols) supports sampling.
- **Interface routing:** same-file backward references are walk-resolved, never interface-imported (kills the fake same-file SCC/Tarjan blowup — 100× on chain files; counter witness); scripts overlay their declarations on the memoized package type environment (was O(scripts × package files)).
- **Semantics core:** multi-member unions; mutable-slot model with union joins; `<<-`/`->`/replacement forms; coercion policy; name-aware signature matching; `Unknown`/`Any` tolerance in `c`/`for`/`$`/`[[`/`[`; flow-sensitive guard narrowing (+ divergence-aware joins, `missing()` supplied-state); `is.null` shaping of unconstrained variables (unannotated coalesce); elided annotation returns; `...` as positioned rest parameter end-to-end; variadic bridging into callbacks; `switch`/`return` as checked control flow; dispatch-table `[[` unions; computed-key container refinement; positional `[[` record extraction; S4 `@` slot lowering.
- **Stub unlock:** `T[]` constrained generics; overload sets (probe-committed, two-round selection, signature-help/hover display); opaque `@type` nominals (data.frame/factor/matrix/…); named-into-rest absorption (typed `read.csv`, `lm`); ~530-declaration corpus across 6 namespaces; project-stub namespaces (`pkg::name`); NAMESPACE import validation + `unused-import` lint; `library()`/`require()` NSE quoting; stub-error surfacing on every surface.
- **Trust & UX:** config subsystem rebuild (nearest-ancestor discovery, per-lint severity, config-file diagnostics, reload refresh); per-file typing directives `# typing: on|off|strict` (tri-state `TypingMode`, one gate everywhere); data-masked NSE resolution (data.table brackets, with-family); strict-mode product story; unified lint framework; CLI rendering + exit-code contract; `roughly debug analysis-stats` workspace performance diagnosis.
- **Editor:** hover quality (`name : TYPE`, overload notes, constraint display); annotation cursor features via re-lexing (hover/goto/completion in `#:` comments); insert-annotation code action (round-trips); unused fade-outs; formatter rewrite with `#:` block awareness; letrec naming (local recursive closures resolve).
- **Engine & scheduling:** red-green core with per-symbol interface firewalls, SCC fixed point, tombstones, eviction, stacker-grown validation spine; durability tiers (open docs LOW, corpus HIGH; sound downgrade re-min through cutoff nodes); memoized completion index + `NamesGlobal`-valued symbol index (zero-copy reads); two-wave diagnostics publish + idle-time semantic wave + lossless preemption pairing + background prime; error-tolerant lowering ("a broken region reports its syntax error and nothing else"); differential correctness vs from-scratch oracle over adversarial edit streams, byte-exact, IDE features included; committed latency witnesses (at-rest reads ≤ 32 memos, size-independent post-keystroke walk, blast-radius exec counters); memory shape at scale (rope-only corpus inputs + on-demand trees, single-retained modules, boxed annotations: 1 GiB → ~300 MiB at 302K LoC) and O(open) keystroke validation (fold split over the `OpenFiles` seam, FxHash memo table: 11K → ~280 slots/keystroke); `analysis-stats` reports per-phase memory, typing-burst recompute counts, and walk attribution.
- **Docs:** getting-started leads with a real bug; installation split out; typing guide + reference as contracts; architecture/structure/testing contributor pages; linter/configuration/stdlib-stubs pages current.
