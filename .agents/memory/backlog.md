# Backlog

**Standing goal (user mandate): empty this list and keep the project at rust-analyzer quality.** The beta program that once organized it is complete; shipped work lives as one-line ledger entries at the bottom (rationale in `decisions.md`, contracts in the docs). Every open item sits in one of the sections below.

**Quality bar (acceptance):**
- **Sound on idiomatic R:** no known accepts-then-crashes holes on supported constructs; unsupported constructs may be refused loudly (sound-by-refusal is acceptable) but never silently mistyped.
- **Zero false positives on the ~200 most-used base functions** with `[check] typing = true` on idiomatic call forms.
- **Performance:** keystroke-to-diagnostics p50 ≤ 30 ms / p95 ≤ 100 ms at 300k LoC (read against the raw-parse floor the instrument prints — latency numbers swing ~1.4x with machine load); budgets pinned by `stats_witness` (per-line wall/memory/resolve-step ceilings) with the measurement instruments in `legacy/differential/tests/test_stats.rs`.
- **No server-killing input** (no `unwrap` panics on protocol-legal messages).

## Open — test-user round 3: typing enthusiasts (the type system, not the libraries)

Three simulated users probing **the type checker itself** rather than package coverage, from the docs
and `--help` only: one on parametric polymorphism and higher-order code, one on domain modelling and
nullability, one adversarial about the `#:` surface and where carets land. Each judged **location and
message as separate verdicts**, which is what makes this round different — a diagnostic that reads
perfectly while blaming the wrong expression counts as a failure here.

The four most serious claims were **re-verified independently** before filing; every one held, and two
turned out worse than reported.

Only the **open** findings are below; the closed ones, with what each measurement actually showed, are
in `test-user-reports.md`.

### D. Placement: precise inside an expression, coarse at every compound boundary

Caret placement was found excellent for ordinary nesting (four-deep calls, multi-line arguments,
lambdas, pipes) and the renderer is display-width aware while JSON stays in codepoints — both correct,
which is rarer than it sounds. The failures were all "collapse to the outermost node", and **four of
the five are fixed** under one rule now in the reference (§Where a finding points): a finding
underlines the smallest expression its message is about.

- **FIXED** — comparison operators (`<`, `==`, `>=`, `!=`) underlined the whole binary expression, so
  the underlined text contained both operand types and the message could not be read. They now blame
  the right operand, which is the `found` half of `expected …, found …`, and is where arithmetic
  already pointed. Worth recording: the *behaviour* is deliberate and correct — R coerces across
  atomic families (`10L < "9"` is TRUE, comparing `"10" < "9"` as text) and the reference documents
  the same-family rule as sound-by-refusal with that exact rationale, so only the caret was wrong.
- **FIXED** — a return-type mismatch with an `if`/`else` tail blamed the whole construct, and the
  offending arm's line was never rendered (the range clamps to the construct's first line). The
  declared return is now checked against each expression that can produce the result — a block to
  its tail, an `if`/`else` into both arms — and each failing one reports at its own site, the same
  rule an explicit `return` follows. The whole body's type stays the verdict, so no finding is added
  or dropped; when no single arm is at fault (an `if` with no `else` contributes an implicit `NULL`
  belonging to no expression) the construct keeps the one finding.
- **FIXED** — `$` / `[[` underlined the entire access chain rather than the bad key. Both now point
  at the key, including a position (`x[[5L]]`). `ExpressionKind::Field` gained a `name_range`, the
  same shape `CallArgument::name_range` already used for the same reason; `[[`'s key range came free
  from the argument expression.
- **FIXED (the surplus half)** — surplus positional arguments blamed the callee; they now blame the
  first argument with no formal left to take it, which is the one the reader must remove. A
  *missing* argument still blames the callee: there is no argument to point at.
- **FIXED** — a record mismatch printed two long near-identical type dumps to diff by eye, and a
  nested one never named the path. It now names the one field that failed, the same treatment the
  function-mismatch fix gave a signature: *expected `logical` for field `active`, found `character`*,
  and `retry.count` for a nested one. A field the value lacks, and one it has that is not declared,
  each say so; a **renamed** field is the interesting case, because it goes missing and turns up
  misspelled at once, so the finding names both (*expected a field `identifier` here, and this list
  has `idenifier` instead*) rather than reporting the absence alone. Pairs fields by name, which is
  what `compatible` does, so the explanation cannot disagree with the verdict — and optionality is
  deliberately not compared, because `compatible` does not either. Whole types are still printed when
  the failure is not about one field (a record against a non-record, or against `list[T]`), which is
  the case they do explain. Every site that reports two types side by side goes through one
  `Checker::mismatch`, so the narrowing cannot be present at one and missing at another; that also
  picked up the `@new` nominal path for free.

  **The caret is FIXED too, so the documented exception is gone.** A type carries no source ranges, so
  the field path is walked back against the expression that *built* the record — a `list(...)` call,
  whose tagged arguments are its fields. The value's `ExprId` now rides on `CallArgument` (and reaches
  `@new`, the declared-value checks and a parameter default), and the single `Checker::type_mismatch`
  funnel returns the whole `TypeError` rather than just a kind, so the range narrows with the message
  and cannot narrow at one site but not another. Which part gets the caret follows the message: the
  offending **value** for a field whose type does not fit, the field's **name** for one the type does
  not declare (that message is about the name), and the innermost list for a **missing** field, since
  nothing at the path exists to point at. Where the walk finds nothing — a variable holding a record —
  the whole value stays the blame and the message still names the field; pinned by a fixture.
- **FIXED (the spill half)** — annotation ranges ending at end-of-line spilled onto the next line, so
  editors squiggled across the break. Measured on a file of six deliberately broken `#:` regions:
  **four of eight findings** ran from the end of one line to column 1 of the next. Cause: an error
  reported *at* the current token blames that token, and at the end of a region the current token is
  the **newline**, whose span is exactly the break. A blame range now never crosses a line break — it
  collapses onto the last character of code on its own line (`Parser::on_one_line`, applied in
  `push_error` so the semantic range is right for the JSON, LSP and CLI paths alike, not patched in a
  renderer). Trailing whitespace is skipped so the caret lands on code: "expected a type" for
  `#: integer |` now points at the `|`, and "expected a return type after `->`" at the `>`. Twenty-nine
  fixture expectations moved by one character, every one of them off the newline and onto code. The
  rule is in the reference under §Where a finding points.

  **Still open (the other half)** — a parse error reported past the end of the file. The originally
  filed shape (line 10 of a 9-line file) did **not** reproduce: an unterminated `f <- function(x,` in a
  one-line file now reports at `1:14-1:15`, in range. Either an earlier fix covered it or the note was
  imprecise; needs a fresh repro before it can be worked, and should be re-derived rather than trusted.

### F. Smaller, but cheap

- **Both of the entries that used to sit here were misdiagnosed, and the measurements are the
  keepable part.** They read: "`do.call` returns `Any`, which disables checking *and* blinds `strict`
  — corpus-authored `Any` where the docs say `Any` should appear only when a user writes it", and
  "`strict` only asks whether a binding *is* `Unknown`, not whether it *contains* one". Checked
  against the tool:

  - **Changing `do.call`'s return from `Any` to `Unknown` produces no strict finding at all.** Tried
    it: edited the stub, rebuilt, ran a strict project over `do.call(fun, args)`. Nothing. So `Any`
    is not what blinds strict, and swapping it would have been a change with a false rationale in its
    commit message.
  - **Strict has no binding-level `Unknown` test to widen.** It reports *origins recorded at
    construct sites* (`UnsupportedConstruct`, `UndeterminedReference`, `LoopWidened`,
    `RecursiveUnknown`) — there is nothing that inspects a finished type, so "only asks whether it
    *is* `Unknown`" describes a mechanism that does not exist.
  - The gap the second entry was reaching for **is real**: `g <- f` closes to `g: fn(p: Unknown) -> Unknown`
    (pinned by the `aliased_function_reference_exports_closed` fixture) and strict reports nothing,
    so calls through `g` are unchecked and it looks like a pass.
  - **`Any` is not corpus-authored by accident, and the docs were the wrong half.** The shipped stubs
    declare **176** entries whose return is `Any` — across nine files, not just `base` — and
    `-> Unknown` in **zero**, with the stub header naming each compromise. The reference bullet
    claiming `Any` "should appear only because the user explicitly wrote it" was simply false, and is
    fixed.
  - The one behavioural difference between them, verified: `@if-unknown` coerces an `Unknown` and is
    **refused** on an `Any` ("this is already `Any` — drop the annotation").

  **What is actually open**, then: should strict report a binding whose exported type *contains*
  `Unknown`? That is a design question, not a bug fix, and it needs volume measured before it is
  chosen — strict already emits **2879 findings on dplyr and 2458 on shiny** with typing and strict
  forced on, and `erase_residual_vars` gives every aliased function `Unknown` parameters, so a
  containment sweep would fire on an idiom (`g <- f`) that ordinary R uses constantly. Decide it as a
  design note with numbers, not as a one-line widening.
- **NOT cheap, and the current refusal is the sound choice — measured, so do not "just widen the
  constraint".** Filed as: `logical` is accepted at a declared `integer` parameter but rejected at an
  inferred `numeric` one, so the same function is accepted or rejected depending on whether the type
  was written down. Both halves reproduce (`bump <- function(x) x + 1L; bump(TRUE)` is refused; the
  same body under `#: fn(n: integer) -> integer` accepts it), and R does promote — `TRUE + 1L` is
  `2L`, and the checker's own arithmetic rules say so.

  The obvious repair is to bind the numeric variable to `integer` when a `logical` argument arrives,
  which is exactly R's promotion. It is wrong, and the counter-example is ordinary R:

  ```r
  bump    <- function(x) x + 1L          # R: bump(TRUE) is 2L, integer
  checked <- function(x) { stopifnot(x + 1L > 0L); x }   # R: checked(TRUE) is TRUE, logical
  ```

  Both infer `<T: numeric> fn(x: T) -> …`, and one binding of `T` cannot be `integer` for the first
  and `logical` for the second — so the promotion produces a *wrong return type*, which the project
  ranks below a refusal ("a gap means checks are skipped, not that wrong answers are produced").
  Verified against R 4.3.3.

  Closing it properly needs the promotion to live in the type, not the binding — a scheme like
  `<T: numeric> fn(x: T) -> promote(T)`, i.e. a type-level function the language does not have. Same
  family as the next item, and they should be designed together.

- **A numeric variable shared by two parameters refuses ordinary mixed arithmetic** (found while
  measuring the item above). `add <- function(a, b) a + b` infers `<T: numeric> fn(a: T, b: T) -> T`,
  tying both operands to one variable, so `add(1L, 1.5)` reports ``expected `integer`, found
  `double` `` — R gives `2.5`. This is a false positive on about as plain a piece of R as exists, and
  it is the same missing piece: the operand types need a numeric *join* (`integer` with `double` is
  `double`), not unification.
- `@new` is unrestricted project-wide, so `domain-modeling.md`'s *"the only door in"* and
  `concepts.md`'s *"provably came from there"* overstate it. Either add an encapsulation modifier or
  soften both sentences to "by convention".
- A narrowing failure on a field or behind `&&` produces a message **byte-identical** to having written
  no guard at all. The docs know the fix ("lift the value into a local first"); the diagnostic should
  say it.
- Nominal unions are unchecked through `$` while structural unions are exact; and there is no
  discriminator for nominal types (`is.list` is true of both arms), so tagged unions cannot be narrowed
  at all.
- An alias cycle is reported on the *use* with a whole-statement caret and never at the declaration; an
  unused cyclic alias is not reported at all, though the reference says definition cycles are errors.
- A second `#:` annotation on one line is silently swallowed (first wins), while harmless trailing prose
  errors — the ambiguous input is the quiet one.
- Missing-argument errors do not name the parameter, though the sibling wrong-name error lists all of
  them. Near-miss suggestions have a length floor that misses short field names (`person$nam`).

**What the round says overall.** Both testers who could reach a verdict said the core is real — genuine
HM with generalization, an occurs check, per-parameter variance, working generic nominals, airtight
nominal distinctness, and no cascades outside the `@param` case. The gap is not the engine; it is that
**diagnostics render the artifact unification left behind rather than the fact that failed**, and that
the nominal story protects construction but nothing after it.

## FIXED — goto-definition landed on the wrong token, and a stray quote blanked the rest of a file

Opened by the IDE fuzz battery at a raised budget (`FUZZ_ITERS=1200 cargo test -p ide --test
test_fuzz`): goto-definition from a cursor, followed by references from the definition it lands on,
did not come back to the cursor. Three separate defects were under it, and the middle one is the
serious one.

1. **An unterminated string or backtick name ran to end of file.** R lets either span lines, so the
   scan can only give up at EOF — and the token then swallowed every statement below it. One missing
   quote in a large file took naming, diagnostics and every editor feature out from that line to the
   end of the file. An unterminated token now ends at its line break (`Lexer::end_unterminated_at_line_break`);
   input that closes never reaches the truncation. Pinned by naming fixtures, since the error
   renderer clamps its caret to the first line either way and could not see the difference.
2. **Goto-definition took the item's FIRST `NAME` node.** That reads `name <- value` correctly and
   every other shape wrong: a right assignment declares its name last, so every jump to `total` in
   `compute(1) -> total` landed inside `compute`. Valid R, wrong position, and it was reached by
   hover, goto and the document outline alike. Naming already knows which token declares the item, so
   `declared_name_range` asks it and keeps the syntax scan only for shapes naming binds nothing for.
3. **A declaration was unreachable from its own first character.** Expressions are ranked
   end-inclusively so a cursor just past a name still means that name; where two siblings abut
   (`"s"broken <- …` in recovered source) that handed the cursor the expression it had just left, and
   an unresolved neighbour then refused outright instead of letting the declaration under the same
   cursor answer. The declaration scan is now the fallback for every binding kind, not just
   parameters and loop variables.

## FIXED — a field write was lost across items in a package but not in a script## FIXED — a field write was lost across items in a package but not in a script

Identical code, two answers. A structural record written at top level and read in a later top-level
statement:

```r
#: list{name: character, age: integer}
record <- list(name = "Ada", age = 36L)
record$age <- "now a character"
reading <- want_chr(record$age)      # wants character
```

As a **script** this is clean — the write retypes the field and the read sees `character`. In a
**package** it reports ``expected `character`, found `integer` ``, so the write is lost at the item
boundary and the read answers from the pre-write type. A package's `R/` files are sourced top-down,
so the write really does happen first and the package answer is the false positive.

Found while fixturing the nominal field-write fix, and pre-existing — the structural path is
untouched by it. The write applies *within* one item either way (the same code inside a function body
is clean in both kinds), so this is the cross-item export.

**Diagnosed, not yet fixed.** The export is fine: naming does mint a `TopLevel` binding for a
replacement target's base, so the writing statement item exports `record` with the written type in
its `top_level_bindings`. The fault is precedence in `SalsaGlobals::scheme`, which resolves in this
order:

1. `script_definition` — position-aware, handles definitions *and* statement writers, and is the
   reason scripts get this right. It is unreachable for a package, because `script_items` is only
   populated for scripts.
2. `package_definitions` — definition items only, no statement writers, no position.
3. `conditional_slot_scheme` — where the statement writers actually live.

A name with both a definition and later statement writes stops at (2), so every later write is
invisible. `conditional_slot_items` already collects the writer; nothing ever asks it.

The obvious repair — extend the position-aware same-file lookup to package files — has a constraint
that must not be broken: `package_definitions` encodes "later files, and later assignments within one
file, override earlier ones", so a same-file lookup that short-circuits would lose a later *file's*
override. The two orderings have to compose rather than one replacing the other. The conditional case
(`if (flag) record$age <- ...`) must still join rather than replace, which is what
`conditional_slot_scheme` unions for.

**Fixed by giving package files the position-aware lookup scripts already had.** `SalsaGlobals` built
its ordered item list for scripts only; it is now built for both kinds, because a file is sourced
top-down whichever it is. An **immediate** read consults the nearest earlier writer in its own file
before the project-wide map, which is what makes the rewrite visible.

The composition constraint is handled by splitting on the read kind rather than the document kind: a
**deferred** read stands aside for a package and falls through to the project-wide winner, because a
function body runs after the whole package is sourced, so a later file's override must win. In a
script the closure runs once that file's frame has settled, so it still scans the file. Verified:
forward references from a function body, mutual recursion, self-recursion, and a later file
overriding an earlier one all still resolve correctly.

## Open — performance & memory review

An independent review that profiled before proposing. Its first finding is fixed (operands of an
arithmetic or comparison operator were inferred twice per level, so a nested chain cost
2^operators); these are the rest, in the order it recommended. Every number was taken on a 4-vCPU
container with other work running, so treat them as upper bounds, and note that all in-container
timings are effectively ≤2-core numbers.

### FIXED — the human reporter was quadratic in findings × file length

Neither review saw it, because both measured through `analysis-stats` or `--output json`, which never
touch this path. On one file of 8,000 items where every item reports, the whole cold analysis was
452.6 ms and `--output json` finished in 550 ms while plain `ry check` took 10,527 ms.

Localized by instrumenting the reporter rather than by inference: at 8,000 findings, `render` was
6,811 ms of which `read_span` alone was **6,728 ms — 98.8%**; the snippet-rule filter was 5 ms and
writing 88 ms. A standalone probe confirmed the shape in miette itself: `SourceCode for str` costs
31 µs per span on a 1,000-line file and 245 µs on an 8,000-line one, because it locates a span's lines
by walking from byte zero. Once per finding, that is findings × file length.

Fixed by answering `read_span` over a bounded window — the span's lines plus a margin wider than the
requested context — and translating the result back into whole-file coordinates. It **delegates to
miette inside the window** rather than reimplementing `SpanContents`, so there is one implementation of
what a snippet contains. A `LineStarts` table per file (miette's own line rule: `\n`, `\r\n`, or a lone
`\r`) makes locating the window a binary search.

Interleaved, one file where every item reports: 1,000 findings 161→60 ms, 2,000 498→131, 4,000
1,705→217, 8,000 **6,483→530 ms (13×)**, and the curve is now linear in findings rather than quadratic.
On real packages, data.table 1,056→326 ms (3.1×). Rendered stderr is **byte-identical** on data.table,
dplyr, ggplot2 and shiny, 1.24 MB of it on data.table alone, related-note snippets included. A
differential test sweeps our window against miette's whole-file walk over `\n`, `\r\n`, lone-`\r` and
mixed line endings, spans mid-line, across a break, at the very start and very end, and a file with no
trailing newline — asserting data, span, line and line count all match.

**Correction to this entry's own earlier evidence.** It claimed "position barely matters" from two
20,501-line projects with 500 findings each costing 6.9 s and 8.0 s. That was inferred without
splitting analysis from reporting, and it was wrong: those projects are analysis-bound (JSON 4.6 s,
reporter 41 ms), so they were never evidence about the reporter at all. The claim happens to hold —
`read_span` cost does track file length per span, measured directly above — but it was asserted from a
measurement that could not show it.

### FIXED — a file-local name lookup scanned the file's item list

`SalsaGlobals::frame_definition` answered "which item of this file binds this name" with a linear
reverse scan of the item list, once per name looked up, so a file with many items *and* many distinct
cross-item references cost their product. Neither factor is superlinear alone, which is why it hid —
measured with each held fixed in turn:

| shape | 2,500 | 5,000 | 10,000 | 20,000 |
|---|---|---|---|---|
| items fixed at 200, call arguments vary | 43 ms | 70 ms | 131 ms | 245 ms |
| call arguments fixed at 200, items vary | 76 ms | 137 ms | 269 ms | 576 ms |
| **both vary together, references distinct** | 153 ms | 393 ms | 1,269 ms | **4,305 ms** |

Both edges linear; together quadratic, and the joint cost is five times their sum — the signature of a
per-lookup scan, since 200 × 20,000 is 4M steps against 20,000 × 20,000 at 400M.

Replaced by `file_binders`, a memoized per-file index from name to the items binding it in file order.
The ordering rule is preserved exactly: a binary search takes the last binder strictly above the
reading item for an immediate read, and the last binder anywhere for a deferred read in a script,
while a deferred read in a package still stands aside for the project-wide winner. Interleaved:
20,000 × 20,000 goes **4,277 ms → 985 ms (4.3×)**, and the curve flattens from 30× to 11.6× across an
8× growth — near-linear, with a mild residue not chased further.

Finding sets byte-identical on all four corpus packages and `targets`, and corpus timings unchanged
within noise, which is expected: this removes a cliff rather than general cost. Two fixtures pin the
ordering — three bindings of one name with an immediate read resolving to the middle one, and a
script closure seeing the last binding in the file. Both fail against a first-instead-of-last error
(along with three pre-existing fixtures) and both pass against the pre-change code, so they guard the
contract rather than encode the refactor.

### The package-path cost is test-block writes entering the package namespace — root cause found, one part fixed

The review framed this as the interface fixpoint being superlinear in the size of the cyclic
definition group. **It is not.** Profiled on `targets` 1.12.0, where `ry check` took 16.6 s on the JSON
path (so not the reporter): `analysis-stats` attributes 16,341 ms of the 16,681 ms typecheck to **one
245-line file**, `R/class_active.R`. The instrument could only say this after the document-kind fix
above; before that it measured a different program.

What it scales with is not the file count. 284 package files cost 0.35 s; adding 238 *unrelated* package
files costs 0.37 s; adding the 238 `tests/testthat` files costs 16.6 s, and reclassifying those same
files as scripts brings it back to 0.99 s. Bisecting by test-file count: 0 → 384 ms, 30 → 1.6 s,
60 → 3.7 s, 120 → 9.7 s, 238 → 22 s, about 90 ms per added test file.

The mechanism, from per-item execution counts and times: query executions grow only 3.6× while wall
time grows 25×, so it is not re-execution volume — one statement item was re-executed 139 times and its
own cost grew with the file count. Writes inside a `test_that`/`tar_test` block bind at the **item's top
level**, because a bare `{...}` is not a scope, so `conditional_slot_items` publishes every one of them
as a package-namespace conditional slot. A cross-file (deferred) read of a common local name like `out`
or `envir` then joins over every conditional writer of that name project-wide — hundreds of statement
items across 238 files — and each join needs that item's check, which drags in the R6 record type.

Two things ruled out by direct experiment, so nobody repeats them: it is **not** the fixpoint round cap
(setting `SCHEME_ROUND_CAP` from 16 to 2 changed 9,690 ms to 9,760 ms) and **not** the type-size ceiling
(lowering `TYPE_SIZE_CEILING` from 100,000 to 2,000 changed nothing).

**Fixed here:** the quoting-form part. `quote`/`substitute`/`bquote`/`expression` arguments were binding
their assignments, so `quote(x <- 1)` published `x` and every quoted call was type-checked. That alone
takes `targets` from 16.6 s to 9.9 s and removes 151 findings, all false positives (139 arity and type
errors reported against calls inside `quote({...})` — code R does not run there — and 12 unresolved
reads of names mentioned in a quotation). See the type-system reference for the contract.

**FIXED, and the design question turned out not to be on the critical path.** The remaining cost was the
*join*, not the binding: `conditional_slot_scheme` called `statement_binding_scheme` for every writer of
a name, unbounded, from a per-item read — so a name written at 238 documents' top levels made every read
of it pay for all of them. Bounding the join at eight writers and widening past that to `Unknown` takes
`targets` from 7.14 s to 0.74 s interleaved (**9.7×**, and 16.6 s → 0.74 s together with the quoting fix)
with **byte-identical finding sets** on `targets` and all four corpus packages, and no regression
elsewhere (dplyr 0.47→0.40 s, ggplot2 1.21→1.08 s, shiny and data.table flat). The bound is honest on its
own terms: a union of dozens of unrelated types is not a fact a check can use, and real conditional slots
have a handful of writers. Contract in the type-system reference; fixtures pin both sides of the bound.

**Still open as a semantics question, but no longer blocking performance.** Writes inside a
`test_that`/`tar_test` block still enter the package namespace, which is wrong for those callees — a
correctness matter now, not a speed one. Scoping a block argument the way `local` is scoped also reaches
0.89 s, but it cannot be done bluntly, and R decides the question by callee, which was checked rather
than assumed:

- `suppressWarnings({v <- 1})`, `invisible`, `system.time`, `try`, `withCallingHandlers` — the block's
  write **does** bind outward, because a promise is forced in the caller's frame. A blanket rule would
  manufacture false `unresolved` findings on `try({cfg <- read()}); use(cfg)`.
- `local({v <- 1})` and the `eval(substitute(b), new.env())` pattern that `test_that` uses — it does
  **not** bind.

So the rule has to key on the callee, and the honest options are a known-verb list (testthat's
`test_that`/`describe`/`it`, matching the existing `library`/`on.exit`/`local` precedent) or a stub
annotation in the vein of `@masked`. A verb list alone is not enough for real projects: `targets` wraps
`test_that` in its own `tar_test`, and hardcoding a package's private wrapper is not a rule. Whichever
is chosen, one blocker comes with it, measured: scoping those blocks adds 87 `unused` warnings to
`targets`, a mix of genuine dead stores (`expect_silent(tmp <- f(x))`) and cases that only look dead
because a name is used in a nested closure. That needs its own answer before the change can land.

### Memory, and the rest of the package-path measurements

A review-authored synthetic of 1,550 files / 277,586 lines / 14,771 items reported **55.9 s and
5,488 MiB peak** as package documents against **5.95 s and 343 MiB** as scripts, superlinear in file
count (400 files 6.1 s, 800 files 10.8 s, 1,550 files 68 s). **That does not reproduce, and the
generator's shape was never recorded.** A fresh synthetic package of 1,500 files / 42,000 lines /
10,500 items — five functions plus a shared top-level conditional write per file, the shape the fixed
join punishes hardest — costs **0.49 s and 111 MiB peak** after the bound, and 0.85 s before it. Treat
the old figures as unverified unless someone reconstructs the generator; the shape that demonstrably
cost is the unbounded join, and it is fixed.

Other packages move much less as-package versus as-script (ggplot2 1.70/1.21, dplyr 0.72/0.58, shiny
0.65/0.59), which fits the cause above: they have far fewer test-block writes landing in the namespace.

Sampling had put the time in **salsa cycle bookkeeping** around `statement_binding_scheme` and
`item_check`, with 54 of 72 sampled thread stacks in `DependencyGraph::block_on` and `targets` getting
**no parallel speedup at all** (17.43/17.81 s on one core against 17.43/16.99 s on four, ggplot2
1.15×). That was a symptom of the unbounded conditional-slot joins, and **bounding them fixed the
parallelism too**. Re-measured with `taskset` pinning core counts, best of two: `targets` 998 → 778 →
599 ms at 1/2/4 cores (**1.67×**), data.table 420 → 309 → 278 (1.51×), ggplot2 1,388 → 1,255 → 1,033
(1.34×). Read those against the container's ceiling — its 4 vCPUs deliver only ~1.8× of native compute
at 4 threads — so `targets` is at roughly 93% of what is achievable here, and real hardware numbers
would be worth having.

Two facts about the fan-out worth knowing: `check` fans out over `available_parallelism()` while
**`fmt` is a plain loop** (646 ms on one core against 644 ms on four, measured), and there is **no flag
to control concurrency**. `available_parallelism()` honours CPU affinity and cgroup quotas, so
`taskset -c 0-N` is the only lever today — enough for measuring, not something a user would find.

The memory note of ~300 MiB at 302K LoC holds for the script shape (343 MiB at 278K LoC) and is 16× off
for the package shape. For reference, memory attribution in the healthy shape is parse +82 MiB,
lower+naming +158 MiB, typecheck +47 MiB, diagnostics +28 MiB — HIR and naming dominate resident memory
at rest, not interned types.

### FIXED — a per-item query re-derived the whole file

Three per-item costs that were each O(items in the file), so the analysis path was quadratic in a
file's top-level items: `SalsaGlobals::arithmetic_classes` collected a `Vec<String>` of every item name
and re-scanned it, `item_tree` was `returns(clone)` so the whole item list was cloned per item, and
`for_item` found an item's index by linear search. Now a memoized per-file query, a borrow, and a
memoized position index.

Measured on the analysis path with interleaved runs (`--output json`, so the reporter quadratic above
does not contaminate it): 2,000 items 566 ms → 160 ms, 8,000 items 7,045 ms → 600 ms. That is quadratic
(4× items, 12.5× time) becoming linear (4× items, 3.75× time), **11.7× at 8,000 items**. Unique corpus
records byte-identical on all four packages.

Worth knowing why the review's own numbers for this looked different: it reported the fixed case as
0.07/0.13/0.26/0.56 s while its baselines were 0.28/0.61/2.15/6.22 s, but the fixed figures match the
JSON path and the baselines match human output — the reporter quadratic sat inside the comparison.
Measure both sides through the same output mode.

### The formatter is *not* slower than the type checker — it is single-threaded

On an identical file set (ggplot2's 339 files, 64,302 lines, 2.0 MiB), single-threaded on both sides:
`fmt --check` 1.01–1.02 s against `check` 2.26 s on one core. The formatter is **half** the type
checker's cost, not double. The backlog's earlier comparison pitted a parallel command against a
sequential one — `check` fans out over `available_parallelism()`, `fmt` is a plain `for` loop over
files (`taskset -c 0` and `-c 0-3` give the formatter the same 1.0 s, confirming it).

What is true: format is 113 ms parse plus ~890 ms render, so the render is ~8× the parse (1.9–2.0
MiB/s against ~18 MiB/s for parsing). Actionable: fan `fmt` out the way `check` already does. The
render's 8× was not localized and needs its own profile before anyone touches it.

### FIXED — `returns(clone)` on the two hottest per-item queries

`item_hir` and `item_naming` were `returns(clone)`, and `ItemNaming` is `BTreeMap`/`BTreeSet`
throughout, so each fetch was a node-by-node allocation walk. The review estimated 3–5 fetches per
item and "roughly 8–13% of the cold pass, estimated, not measured end to end". Both halves were then
measured rather than assumed: a counting probe puts ggplot2 at 14,753 `item_hir` and 16,173
`item_naming` fetches for 2,736 items — 5.4 per item, matching the estimate — costing 178 ms and
265 ms of fetch time, and data.table at 16,385 and 21,139 fetches costing 64 ms and 118 ms.

Now `returns(ref)`, with the call sites borrowing. Interleaved over three rounds, medians: ggplot2
1,035 → 949 ms (8%), `targets` 640 → 559 ms (13%), data.table flat within noise. So the estimate held
where it bit, and the realized saving is smaller than the measured fetch time because the memo lookup
remains — only the clone is gone. Finding sets byte-identical on all four corpus packages and
`targets`.

**Peak memory is unchanged** (ggplot2 127.4 → 127.0 MiB, `targets` 94.2 → 93.8 MiB), which is the
expected result and worth stating so nobody re-measures hoping otherwise: the clones were transient,
so they cost allocator traffic rather than resident set, and peak is dominated by the memos themselves.

**Still open from the same entry:** the `BTreeMap` choice. `resolutions`, `bindings`, `non_locals`,
`quiet_reads` and `namespace_reads` are pure lookups with no ordering requirement, and `BTreeMap::get`
showed up under `infer_read`. Removing the clone removed the node-walk half of that cost, so what is
left is lookup only — measure before switching, and check iteration order where any of these is walked
to build diagnostics.

### FIXED — per-item clones of project-wide tables

`check_item_with_annotation` cloned the whole project `@type`/`@alias` map and the arithmetic-class set
per item and stored both owned on `InferenceTable`. Both are per-*file* facts and both are pure
lookups, so they are now memoized per-file tracked queries returning `&'db`, with the table holding
`Option<&'db …>`.

Measured interleaved at a fixed 2,000 items as the declaration count grows, typecheck goes
0 → 18.0/18.3 ms, 600 → 32.0/19.4 ms, 2,400 → **71.3 ms cloned against 27.5 ms borrowed** (2.6×), and
the residual growth after the fix is the real work of resolving more nominals rather than copying.
Finding sets byte-identical on all four corpus packages and on `targets`; no package regressed.

### Suspected — `Checker::infer` deep-clones an `Expression` per call

`let expression = self.module.expression(id).clone();` sits on the checker's hottest path, cloning
`NameRef(String)`, `Call{arguments: Vec<Argument>}` and `Binary{special_name: Option<String>}` per
node, while most arms then re-extract only `Copy` fields — the clone exists only to release the borrow
on `self.module`. It appeared in the profile solely as `drop_in_place` and malloc/free frames, so its
cost was never isolated. Measure before acting.

### Judged fast enough — do not invent work here

Single-package cold analysis (ggplot2 68K lines 1.9 s / 88 MiB peak; mgcv 37K lines 1.3 s / 72 MiB
after the chain fix; targets 64K lines 1.4 s as the instrument classifies it), with parse only 2–7% of
the pass at ~0.9 µs/line. `item_spans` identity is clean — `item_span_positions` is a memoized index
so `item_span_range` is an O(1) probe, and `item_spans` itself is consumed per file; the quadratic
above is a different query, not a regression of that one. Incrementality genuinely holds: every
project reported zero item rechecks per keystroke and zero resolve steps, with edited-file diagnostics
at 2.5 ms median on ggplot2 and **21.3 ms median at 277,586 lines**, inside the stated bar. And rowan
re-anchoring is not the quadratic — `child_or_token_at_range` is a binary search.

## Open — abstraction & duplication review

An independent review looking for duplicated sources of truth and abstractions that earn nothing. Two
findings from it are already fixed (a `GlobalEnv` fact that could be silently forgotten, and an
operator list restated in two places); these are the rest.

### FIXED — three copies decided what a package file is, and they disagreed

`cli.rs`'s `shares_a_namespace`, the server's `is_package_path` and `stats.rs` each answered "does this
file share a namespace with its siblings". The first two counted `R/` **and** `tests/testthat/`;
`stats.rs` counted `R/` only, directly under a comment claiming it ordered files "exactly as the CLI and
server do". Confirmed before fixing: on a testthat package `analysis-stats` reported 3 diagnostics
where `check` reported 0. Now one predicate, called from all three, with the sorting key kept separate
from the classification — collapsing those two questions into one flag is what caused it. Pinned by a
CLI test that fails against the old instrument (2 diagnostics) and passes now.

### Rename accepts `...` and `..1` as new names, and the identifier rule is written twice

The server's `is_valid_r_identifier` restates a rule `syntax::is_syntactic_name` already owns —
same reserved-word list, same start/continue classes, same `.5` exclusion — so call the lexer's
instead of keeping a second copy (≈−60 lines).

The dot-dot part of the finding as originally filed was **wrong and was checked against R**: `... <-
1` and `..1 <- 1` both run, and `... <- 5` genuinely binds (`get("...")` returns 5), so these are not
invalid assignment targets. The real defect is narrower and only about `..1`-style names: the
assignment succeeds but the *read* cannot, because `..1` is resolved as a positional slot of an
enclosing `...` rather than as a variable — `..1 <- 7; ..1` fails with ``..1 used in an incorrect
context, no ... to look in``. So renaming a variable to `..1` silently turns every one of its reads
into a runtime error, which rename must refuse; `...` is a legal name and refusing it needs a
different justification (shadowing the forwarding mechanism) or none at all. Note that moving to
`is_syntactic_name` does **not** fix this on its own — the lexer's rule accepts both spellings too.

### A dead-code batch, all of it hidden behind `let _ =`

Seven items, compile-verified as unreachable, 44 lines. They survive because a `let _ = …` keeps the
binding alive, which is also why the compiler never flagged them — the pattern to grep for.

### One rule table, written twice

The lint rule metadata is restated rather than derived, so a rule can be added to one table and not
the other. Single source of truth, then generate the second view.

### `use`-qualification sweep

Roughly 80 sites fully qualify a function whose module is not imported at all, against the house style
(types imported directly; functions get at least one module-level import unless ambiguity forces
qualification). Mechanical, but do it as its own pass so the diff stays readable.

### Two process defects worth fixing while in the area

- `decisions.md` has drifted into a chronological execution log with Roughly-era naming, violating the
  timeless, context-free rule it is itself supposed to enforce. Rewrite the stale entries as settled
  decisions or drop them.
- Memory says `zed_roughly` in the gate commands; the crate is `zed_ry`. Cargo does not fail on the
  wrong name — `cargo tree --workspace --exclude zed_roughly` prints `warning: excluded package(s)
  'zed_roughly' not found in workspace` and carries on, so the exclusion silently does nothing and the
  one warning scrolls past in a long build log.

## Open — oracles, not inputs: what the batteries cannot see

The two reviews below measure what the fuzzers *feed* the code. This section is about what they
*ask* of it, and it was written after a crash class shipped past every battery: an inference variable
escaping the item whose table minted it. The output stayed well formed — ranges in bounds, output
deterministic, incremental equal to fresh — so all four batteries were green while **every** package
in the pinned CRAN manifest leaked one and a quarter of them crashed `ry check`. An oracle that only
asks "is the answer well formed" cannot see a wrong answer, which the testing section of `MEMORY.md`
already says; these are the concrete follow-ons.

### Assert internal invariants in the code, not only output properties in the harness

A `debug_assert` at a seam turns every input in every suite — fixtures included — into a test for that
invariant, at zero release cost. The export-edge one (a closed scheme carries no inference variable)
landed with the fix. Worth adding next, each stated as the seam it guards:

- every expression type an item records resolves in the table that produced it (the same defect from
  the other end);
- an item's declared-name range lies inside the item and spells the item's name — the fuzzer needed
  1,200 iterations to stumble on a jump that landed on the wrong token, and this assertion catches it
  on the first fixture that uses `->`;
- HIR expression ranges and naming binding ranges lie inside their item's range;
- occurrence sets returned by references/rename are all spelled alike (the IDE battery checks this
  locally — promote it so every caller is covered).

### Relational oracles find real bugs in valid code; add more of them

The round trip (definition → references → back to the cursor) is the only relational oracle in the
tree and it found a wrong jump on ordinary R. Each of these is one extra call and needs no expected
output: rename edits ≡ references; hover at the definition target names the same symbol as hover at
the cursor; completion at a name's first character offers that name; goto-definition is idempotent.

### Wire the pinned CRAN corpus in as a test — and run each package as its own project

`scripts/corpus-manifest.txt` pins 69 packages, `corpus/` is gitignored, and nothing fetches it (the
review below records two arms silently contributing zero inputs). Twenty packages found five distinct
crashes in ten minutes of wall clock. Two details decide whether it works:

- each package is **its own project** (one `ry.toml`, its own `R/`), because the defects live on the
  item-to-item interface — a per-file arm cannot see them, and one shared database over every file
  distorts the diagnostics instead (`duplicate` explodes, as measured below);
- typing and strict **on**, or most of the checker never runs.

### Make a corpus finding reproducible before trying to minimize it

The same package panicked on one run and not the next: item checks run across threads, and which
items are visited first decides whether a dangling id lands out of range. A delta-debug pass over a
non-deterministic reproducer wastes an hour and can conclude the opposite of the truth. The corpus
arm should force a single thread, and a file-level-then-statement-level minimizer belongs in
`scripts/` rather than being rewritten per crash.

### Budget: keep the default cheap, and mutate for the shape that found these

The IDE round trip needs ~1,200 iterations to surface and the default 300 never does. Raising the
default is the wrong lever (that battery already costs 216 s): every failing input had the same shape
— two tokens abutting with no separator, `"s"broken <- …` — which the seed-mutation arm produces by
accident and a grammar generator produces almost never. Add "delete a separator" as an explicit
mutation, and run the high budget nightly.

## Open — fuzzing input-generation review (measured)

An independent review of what the fuzzers actually feed the code, with every number produced by a
probe that reimplements each generator arm byte-for-byte (same RNG constants, seeds and budgets) and
real `-C instrument-coverage` region counts. **11,735 generated inputs per default battery run**;
98.7% of the wall clock goes to the two batteries with the worst input quality.

### The format battery's 4,500 generated inputs add 8 regions of 2,951

Leave-one-out region coverage of `format.rs` (whole battery 2,771/2,951): dropping `fuzz_random_bytes`
loses **0**, dropping `fuzz_seed_mutations` loses **0**, dropping token soup loses 4. Dropping all three
as a block: 2,771 → 2,763. `fixture_sources_hold_invariants` alone covers 2,762 and contributes **410
unique regions**. The earlier "14 in 1500" figure for the random-byte arm reaching the formatter body is
confirmed and is worse than it reads: 11 of the 14 are empty or whitespace, so it formats a program with
at least one token **3 times in 1500**, and never one with ten tokens. Delete the random-byte arm, cut
soup to ~200 (it is the cheapest source of parser-error shapes — 33 of 35), and seed mutations from the
fixture corpus instead of the 35 hand seeds.

### The generators cannot express most of the type system

Diagnostics normalized to message *shapes*: all three semantics fuzz arms reach **60 shapes (33 of them
parser errors, 9 distinct `type-mismatch`, 0 lint)** against the legacy corpus's 110 and the typing
fixtures' 80. In 250 generated programs the annotation grammar produces `TYPE_REF`/`TYPE_FUNCTION`/
`TYPE_RECORD` and **zero** unions, binders (`<T>`), applications (`Box<T>`), vectors, `list[T]`, tuples,
parens, optional `[x]:` or rest `...r:` parameters. The generator calls **6 of 872 declared stub names**
and reaches **1 of 37 overload sets**. No harness emits `library(...)`, so every conditional namespace and
the whole NSE ladder is unreachable; no `setClass`/`setGeneric`/`R6Class`/`new()` anywhere.
`metadata.rs` sits at **7.63%** regions, and `lints.rs` produces **zero findings in 250 programs**.

### Grammar-directed generation, prototyped and measured rather than projected

A ~250-line recursive generator over the R grammar × the `#:` grammar, 1,500 programs in **0.338 s**:

| metric | best current arm | grammar prototype |
|---|---|---|
| parses clean | 35.3% | **100%** |
| formats with ≥10 tokens | 8.0% | **41.3%** mutated |
| `parser.rs` regions | 87.27% | **93.46%** |
| semantic regions (9 files) | 11,073 | **12,888** |
| diagnostic shapes | 60 | **95** (+59 new) |
| distinct `type-mismatch` shapes | 9 | **17** |
| lint shapes | 0 | **5** |
| `ide::type_definition` hits | **0 / 7,884** | 80 / 23,826 |

Added *alongside* the existing arms (not replacing — soup still owns 24 parser-error shapes), semantic
coverage goes 11,073 → **13,306 regions (+12.9 points)**. Typing fixtures still lead at 14,580, so the
corpus beats synthesis and both beat noise.

### The best inputs are already in the tree and only one crate uses them

`fixture_sources_hold_invariants` exists **only in `format`**. The same 388 typing-fixture sources through
the semantic pipeline reach **86.92% of `check.rs`** and 80 shapes, against the entire generated
battery's 54.35% and 60. Wire it into `syntax`, `semantics` and `ide`, and use both corpora as *mutation
seeds* rather than only fixed inputs.

**A fair criticism of the legacy-corpus arm as landed**: it runs one shared database over all 1,967 files,
`file_diagnostics` only, asserting never-panic plus range geometry. That shared project changes what is
tested — `unresolved` collapses from 284 to 148 while `duplicate` explodes from 20 to **2,182**, because
1,967 unrelated files redeclare each other's names. Per-file with the full battery it reaches 110 shapes.
Fix by batching into projects of a sane size and running the full `check_semantics_invariants`; the cost
is fresh databases (~9 ms each in release, ~113 ms in debug), so this wants the batteries in release or a
lower db-per-input count.

### The IDE battery costs 216 s and never reaches the type-driven features

`type_definition` returns `Some` **0 times in 7,884 offsets** — structurally, since it needs a
`TyKind::Named` and no IDE seed declares a `@type`. `signature_help` fires at 0.5%. 86.7% of inputs do not
parse. Swapping the 10 hand seeds for grammar-generated programs that declare and use nominals took
`ide.rs` from 52.58% to **68.38%** on *fewer* inputs. Cap generated programs at ~10 statements — a
150-input grammar sweep cost 81 s against 45 s for 300 tiny mutated ones.

### The semantics incremental invariant never performs a small edit

`check_pipeline_reporting` derives its "edit" by generating an unrelated program, so the computed splice
covers **97.3% of the old text on average** and only 1 pair in 250 touches ≤10%. `syntax::reparse` takes
the full-parse fallback essentially always and salsa sees whole-file invalidation every time, so the
splice-reuse path and per-item early cutoff — the architecture's core claim — are never exercised.
Derive the edit from the source instead: replace or insert one statement at a boundary.

### Smaller, each with its measurement

- **The syntax edit stream degenerates but its coverage is fine.** 0/400 buffers parse clean, only 89
  distinct shapes, and 60 steps lex to two tokens because an inserted `"` swallows the buffer — yet
  `reparse.rs` is at 96.12% against a grammar-directed stream's 96.44%. Reset the buffer every ~20 steps
  and skip near-empty ones; do **not** rewrite it.
- **Coverage-guided fuzzing has never run.** `cargo fuzz` is not installed, `fuzz/corpus/` does not exist,
  no CI job invokes it, and the root `cargo test` resolves to the product crate so `fuzz_deep` never runs
  either — while the `REGRESSIONS` arrays (21 in `format`, 2 in `semantics`) are documented finds from
  exactly that mechanism. `fuzz/` does still compile (`cargo +nightly check`, 83 s). Coverage judgement
  costs one component: `rustup component add llvm-tools-preview` produced every number in this review.
- **Two arms silently contribute zero inputs.** `corpus/` does not exist and nothing in CI or the justfile
  fetches it, so `fuzz_corpus_seeded` in `syntax` and `format` returns early — ~1,050 budgeted inputs that
  never run, behind a skip line `cargo test` hides. Same "green means nothing ran" species the
  `FIXTURE_FILTER` guard already fixed.
- **Surfaces with no fuzz coverage**: generated `.Rtypes` text never reaches the stub loader
  (`stubs.rs` 60.67%); `PackageMetadata` appears in no harness; the IDE arm only ever uses
  `DocumentKind::Package`. **The review's fourth item here was wrong and is corrected**: it reported
  `syntax::literate::r_source_of_literate` as having "no test of any kind", but it has eight unit tests
  and three of them already assert the byte-length invariant, including one for multibyte prose. The real
  gap was only that the invariant was pinned on four hand-written documents rather than over generated
  input, which the `syntax` battery now closes.

## Open — fuzzing economics and feedback-loop review (measured)

A third independent review, asking what protection each hour of compute and minute of developer time
actually buys. Every number below was produced on a 4-vCPU machine, debug profile unless stated. The
headline: **fuzzing is 327.3 s of a 672 s local gate (49%), and CI runs none of it.** The rest of the
gate is `legacy/` at 249.0 s (37.3%), the shipping crates' fixture suites at 68.8 s (10.3%) and
`crates/ry` — the only thing CI runs — at 22.4 s (3.4%).

Where this review **disagrees** with the input-generation review above, and the disagreement is real:
that one wants the generated arms shrunk across the board; this one measures per-arm cost and wants
`syntax` and `format` **grown** (they are 0.19–0.31 ms/input) while `ide` and `semantics` shrink
(257–644 ms/input). Both independently conclude `format::fuzz_random_bytes` should be deleted.

### The `extended` CI job runs zero tests — verified, not projected

`cargo test --all-targets --all-features -- --list --ignored` → **0 tests, 0 benchmarks**, across all
six binaries. Because the root `Cargo.toml` sets `default-members = ["crates/ry"]` and the workflow
omits `--workspace`, the `extended` job is a full `lto = true` release build followed by an empty test
run, on every push, with a 45-minute timeout. The blocking `check` job lists **171 tests, all in
`crates/ry`**; the workspace has **707**, so **536 (75.8%) never run in CI**. `decisions.md` states "a
bounded pass runs in the default test suite so CI fuzzes on every change" — that claim is false and has
been for the pipeline's whole life. Fix the wording in `decisions.md` and the testing page independently
of the workflow move.

### FIXED — `ide::completion` cost ~160–190 s per gate run to assert that strings are non-empty

Per-offset on one warm database, 25 offsets: **completion 23.490 ms**, hover 0.044, hover_debug 0.039,
code_actions 0.035, rename 0.033, definition 0.029, references 0.027, type_definition 0.024,
signature_help 0.016. Completion is 587.26 ms of the 592.4 ms those nine features spend. Removing it
from the harness's seed sweep: 6.34 s → 1.27 s debug (80%), 1.43 s → 0.08 s release (94%). The ide
binary is 199.77 s of the 667.5 s battery, so completion alone is **~26% of the entire workspace test
suite** — and its only assertion is `assert!(!item.label.is_empty())`. It is also mostly redundant: over
the 10 seeds, 245 swept offsets produce **64 distinct results (73.9% duplicates)**.

Fixed by sampling once per completion context — the kind of token the cursor sits in or after. Two
sharper-looking variants were measured and rejected: filtering to token boundaries saved only 5%
(in short inputs nearly every sampled offset already is one), and keying on the *pair* of
surrounding kinds saved 29%. Per-token-kind halves the harness (29.8 s → 16.8 s at `FUZZ_ITERS=50`,
against a 6.2 s floor with completion removed entirely), and the full `ide` binary went 195 s → 94 s
*while gaining* the two range oracles below.

### This is not a fuzzer; it is a fixed 498-program corpus re-derived at 124 s a run

All generators seed `SplitMix64` from compile-time constants with no entropy. Two runs of the semantics
generator produce an **identical** set of **498 distinct programs**; drawing 100,000 times from the same
generator reaches **87,203** distinct programs. The default budget samples **0.57% of its own generator's
reach** and re-samples exactly that 0.57% forever. Split the two jobs it conflates: keep a small
fixed-seed arm as the regression net it actually is, and give the exploratory arms a `FUZZ_SEED` env var
defaulting to random, printing the seed on every failure, run on a schedule rather than in the blocking
gate. Do **not** randomise the blocking gate — a failure surfacing on an unrelated change is a worse
trade.

### The syntax arm is the best asset in the test architecture and gets 0.5% of the budget

Distinct observable parser behaviours (error templates + node kinds) vs. iteration count:

| syntax arm | 100 | 500 | **1500 (default)** | 6000 | 20000 | last new behaviour |
|---|---|---|---|---|---|---|
| token_soup | 67 | 98 | **109** | 124 | 132 | iteration 19,024 |
| random_bytes | 19 | 36 | **42** | 51 | 62 | iteration 17,012 |
| seed_mutations | 71 | 107 | **124** | 143 | 158 | iteration 18,253 |

All three at 20,000 iterations — 13× the default — cost **7 s total in debug** and were still finding new
parser behaviour at iteration 19,024. Cost per input across the battery: syntax **0.19 ms**, format
0.31 ms, semantics ~257 ms, ide **644 ms** — a 3,400× spread, with the weakest oracle sitting on the most
expensive input. Raise the syntax budget to 20,000 (+7 s, +28% distinct behaviours) and pay for it out of
the completion fix.

### Activating `.github/pending-ci.yml` as staged makes the `extended` job time out — a THIRD blocker

`ide::fuzz_deep` runs `iterations().max(5000)` sweeps; that `.max` floor means `FUZZ_ITERS` **cannot
lower it**, which is the bug. Measured in release on that exact input shape (1–12 concatenated seeds, avg
214 bytes, 111 offsets/sweep): **665 ms per sweep × 5,000 = 55.4 minutes** on 4 cores, against the job's
`timeout-minutes: 45` on a 2–4 vCPU runner. That is before `semantics::fuzz_deep` (~4 min projected) and
the `test_stats.rs` instruments that hard-assert on a corpus CI never fetches. The two blockers already
in this file are joined by this one, and it is the one that costs 45 minutes of runner time to discover.
**Fix the floor, make the stats instruments skip on an unfetched corpus the way `test_corpus.rs` does,
and raise the timeout with measured headroom — before the human `git mv`.**

### The battery pays a 6.1× debug tax for identical assertions

| binary | debug | release | speedup |
|---|---|---|---|
| syntax test_fuzz | 1.58 s | 0.10 s | 15.8× |
| format test_fuzz | 2.31 s | 0.21 s | 11.0× |
| semantics test_fuzz | 123.68 s | 8.62 s | 14.3× |
| ide test_fuzz | 199.77 s | 44.40 s | 4.5× |
| **total** | **327.34 s** | **53.33 s** | **6.1×** |

Building the four release test binaries costs 143 s warm. Root cause of the semantics figure, measured
directly: `install_shipped_stubs` is 0.4 ms (it sets a salsa input) but a fresh database + stubs + render
is **118.2 ms** against **5.0 ms** without stubs — **113.2 ms of stub re-parse per fresh database**.
`check_semantics_invariants` builds three fresh databases per input (394.8 ms), so **86% is stub
re-parse** (97.7% in release). A render on a warm shared database is 3.8 ms — 30× cheaper. The earlier
"four databases" fix landed and the ratio barely moved, because it was never about database count alone.
Run the fuzz targets in release from `just gate`, and make a shared database the default arm with fresh
ones only where determinism/incrementality genuinely needs them.

### Coverage-guided fuzzing has no ratchet — nothing it learns survives the run

`fuzz/` **does compile on stable today** (`cargo check --all-targets` inside it: 75 s cold with an
isolated target dir, **1.6 s warm**), so the compile-rot risk flagged earlier is real but not yet
realised. `scripts/seed-fuzz-corpus.rs` writes 1,416 files / 5.7 MB into each of three corpora in 2 s,
but it globs `.Rtypes` under `crates/` where there are **0** — all 11 `.Rtypes` (and 33 `.exports`) live
in the top-level `types/`. So it seeds **zero stub files**, and zero of the 1,967 mined
`corpus-legacy/*.R.corpus` programs, and never looks at `corpus/`. Its own doc comment ("plus the shipped
stubs") and the testing page repeating it are both false. `fuzz/corpus`, `fuzz/artifacts` and
`fuzz/Cargo.lock` are gitignored and no job persists them, so every run restarts from the same seeds and
rediscovers the same shallow frontier. `REGRESSIONS` exists in `format` (21) and `semantics` (2), none in
`syntax` or `ide`, and both constants were last modified ~60 commits ago — the pinning path works
(`format` 0.12 s, `semantics` 0.94 s) and is simply not being fed. In value order: persist the corpus,
fix the seeder to read `types/` and `corpus-legacy/`, and add the 1.6 s `cargo check` of `fuzz/` to the
gate.

### The one failure mode fuzzing exists to catch is the one that prints no input

`catch_unwind` counts: syntax **1**, format **1**, semantics **2**, ide **0** — and in `syntax`/`format`
the single wrapper is on the legacy-corpus arm only. Every `assert!` in `check_parse_invariants` and
`check_format_invariants` embeds `{input:?}`, so assertion failures replay fine; a genuine **panic**
inside the generative arms prints a backtrace with **no input**, and a stack overflow (a live risk the
harness itself acknowledges with `deep_nesting_is_refused_not_fatal`) aborts printing nothing.
Recoverable today only because the seed is fixed — that property disappears the moment `FUZZ_SEED` lands,
so this must land *with* it. `semantics::check_pipeline_reporting` is the correct pattern already in tree.

### Two arms are silently dead and one is 99% wasted

`syntax::fuzz_corpus_seeded` 0.11 s and `format::fuzz_corpus_seeded` 0.16 s — process startup only, both
skipping via an `eprintln!` the blocking job never displays. `format::fuzz_random_bytes` reaches the
formatter body **14/1500 (0.9%)** against token soup's 90/1500, so 1,396 of 1,500 iterations only re-test
a refusal path `syntax` already covers on the same generator. Drop it and give the budget to
`fuzz_seed_mutations`.

### Radical alternatives, costed

- **Release profile for the battery** — 327 s → 53 s, +143 s warm build. Highest value-per-line change in
  the pipeline.
- **Nightly coverage-guided run with a persisted corpus** — 10 min/target ≈ 30 min/night on a free
  runner. Worth it *only* with persistence; without it, 30 min/night of rediscovering the same frontier.
  Needs a human (workflow scope).
- **OSS-Fuzz** — the three targets are already thin wrappers over exported batteries. Good fit, but only
  after the replay path (`catch_unwind` + seeds) and the pinning path exist, or reports arrive with
  nowhere to go.
- **`cargo-llvm-cov` over the batteries** — not attempted; the saturation curves above answer the same
  question in ~7 s. Reach for llvm-cov when deciding *which* invariants to add, not how many iterations.

### Who lands what

Agent-side today: completion sampling, release profile in `just gate`, the syntax budget raise,
`catch_unwind` + seed printing, `FUZZ_SEED`, the seeder fix, the `fuzz/` type-check guard, the
`fuzz_deep` floor and stats-instrument skip that unblock the CI move, the dead-arm cleanup, and the
corrected wording in `decisions.md`/`testing.md`. Human required: the `git mv` of
`.github/pending-ci.yml` (workflow scope, and **not before the `fuzz_deep` floor is fixed**), any
scheduled fuzz job and its corpus cache, OSS-Fuzz submission.

## Open — test & fuzz architecture review

An independent review of the fuzzing and test architecture, with every claim measured rather than
read off the code. Ranked; each item states the change and why it is worth it.

### The CI gate runs the product crate's tests only — no fixture suite, no fuzz battery

`Cargo.toml`'s `default-members = ["crates/ry"]` times `--all-targets` **without** `--workspace`
resolves to the default member, so the blocking job runs 169 `crates/ry` tests and none of
`syntax`/`semantics`/`format`/`ide`: no typing, lint, format, ide, tsr or errors fixtures, and no
fuzz arm. `cargo test -q --all-targets -- --list | grep -iE "fixture|fuzz"` is empty. The `extended`
job repeats the defect, so `fuzz_deep` never runs either. The widened commands are already staged in
`.github/pending-ci.yml` and need a human with `workflow` scope to move the file.

Two blockers before that move is safe, both agent-side:

- Activating the staged file **reds the extended job immediately**: the five instruments in
  `legacy/differential/tests/test_stats.rs` hard-assert on a corpus CI never fetches, unlike every
  other corpus-dependent test, which skips with a note (the pattern is in `test_corpus.rs`). Make
  them skip the same way.
- The fuzz doctrine's own claim — "a bounded pass runs in the default test suite so CI fuzzes on
  every change" in `decisions.md`, and the testing page by implication — is false until the move
  lands. Correct the wording, or land the move first.

### The default fuzz pass spends ~95% of its time re-parsing the shipped stubs

Measured in isolation: `ide --test test_fuzz` 294.7 s, `semantics --test test_fuzz` 141.6 s, against
`syntax` 0.89 s and `format` 0.34 s. The cause is not iteration count — a fresh database with the
shipped stubs costs 113.3 ms per round versus 1.4 ms without them, and `stub_library` is 114.1 ms to
parse and intern 601 names (0.008 ms memoized). `check_semantics_invariants` builds **four** fresh
databases per input, so ~456 of its 480 ms is re-parsing `types/*.Rtypes`. `StubLibrary<'db>` is
interned per database, so the levers are database count and iteration shape, not caching.

- Delete the `second` rendering in `crates/semantics/src/testing.rs`: the later
  `assert_eq!(before, first)` already compares two independently built fresh databases. Four
  databases become three, −25% for five lines removed.
- Sample offsets in the `ide` arm instead of sweeping every one. `ide::completion` costs 15.9 ms warm
  on a 3-byte file and 20.5 ms on a 1.6 kB one — the work is per call over the 601-name corpus, not
  per byte — so sweeping every offset is what makes this the most expensive test in the repo.
  Token boundaries plus every k-th offset keeps the defect classes at ~1/8 the cost.
- Durable fact worth keeping: raising `FUZZ_ITERS` on a semantics arm buys stub re-parsing at ~114 ms
  a database, not coverage.

### The formatter battery cannot notice the formatter deleting code

`check_format_invariants` asserts determinism, idempotence, and that the output re-formats. A
formatter that silently dropped a statement passes all three. The missing oracle is the non-trivia
token **kind** sequence, excluding `{`/`}`/`;`/`ANNOTATION_MARKER` (the formatter legitimately adds
braces, splits `;` chains, and re-lays-out `#:` blocks). It was built and run: 0 mismatches over 1182
fixture case sources (972 formatted) and 20 000 fuzz-shaped inputs (4073 formatted). The weaker
formulations do not hold — raw token equality fails on 50 brace/semicolon insertions, text equality
on 9 `'x'`→`"x"` normalizations — so kinds-with-those-four-excluded is the assertion to write, at
about ten lines. Same run: the `format` harness's random-byte arm reaches the formatter body 14 times
in 1500 (0.9%), a path `syntax`'s battery already covers on the same generators; shrink or drop it.

### `crates/ry` has no property coverage, and the protocol edge panics

The obvious property over `crates/ry/src/position.rs` — every byte offset converts and round-trips —
finds 11 panics and 2 round-trip failures in twelve lines. Two distinct defects:
`line_column_utf16`/`line_column_chars` slice `text[start..start + byte_column]` and panic for an
offset inside a character (`line_column_utf16(1)` on `"é😀x\n"`), and `offset_utf16` clamps to a line
length that has already had `\r\n` trimmed, so an offset at a CRLF terminator does not round-trip
(`"a\r\nb\r\n"` offset 2 comes back 1). Reachability caveat, stated honestly: every offset this
module *produces* is on a boundary, so the panic was not shown reachable from today's callers — but
"IDE features never panic on stale ranges" is a stated soundness invariant with nothing enforcing it,
and the CRLF break sits on the Windows default path.

### `FIXTURE_FILTER` with a name that matches nothing passes green

`FIXTURE_FILTER=does_not__exist cargo test -p semantics --test test_typing_fixtures` reports `ok. 4
passed; 0 failed` having run zero cases. This is the documented iteration loop, so a typo'd or
renamed case id reads as "my fix works" during exactly the work that renames case ids. Count matched
cases and assert the filter matched at least one — three lines, and the highest value-per-line item
in the review.

### The IDE arm is the most expensive test in the repo and asserts almost nothing

Of 13 feature calls per offset only `hover` is checked, for determinism; eight are `let _ = …`, i.e.
never-panic only. Nothing asserts the invariant that actually matters and that memory already states:
every range a feature returns lies inside the file. `definition`, `references`, `rename`,
`type_definition`, `code_actions`, `inlay_hints` and `document_symbols` all hand ranges straight to
the editor. Assert in-bounds ranges and determinism for every range-returning feature, and pay for it
with the offset sampling above — same wall clock, several times the oracle.

### ~280 lines of fuzz harness are copy-pasted across four crates

`SplitMix64` exists four times (three byte-identical, the fourth adds `chance()`), `iterations()` four
times identically, `corpus_sample()` twice byte-identically apart from one comment, and the
byte-mutation loop three times. All four test crates already depend on `syntax::testing` for
`env_var`, so the shared home exists: put `rng`, `iterations`, `corpus_sample` and `mutate_bytes`
there. This replaces four copies with one; it is not a new abstraction.

### Seeds are hand-maintained while 1183 fixture sources sit unused, and the reader is dead code

`syntax::testing::parse_fixture_files` is public with zero callers — its doc comment advertises the
cross-stack differential harness, retired with the identity-parity program. Meanwhile the batteries
seed from 81 hand-written strings and never see the 1183 fixture cases, the richest R corpus in the
repo and the one that grows with every slice. Seed the `syntax` and `format` batteries from it, delete
the three `SEEDS` lists (~110 lines), and keep hand seeds only where they encode something fixtures do
not. For `semantics` use fixture sources as *mutation* seeds, not one per input: 1183 × 480 ms is
9.5 minutes.

### The libFuzzer corpus seeder reads the wrong directory for stubs

`scripts/seed-fuzz-corpus.rs` looks for `.Rtypes` under `crates/`, where there are none — every stub
lives in the top-level `types/`. So the `.Rtypes` grammar gets no seed coverage, and both the script's
docstring and the testing page's description of it are false. The same 173-line script hand-rolls
SHA-1 (55 lines) purely for a filename libFuzzer never inspects, and hand-rolls a third copy of the
fixture-file parser; nothing seeds `corpus/**/*.R` even once a human has fetched it. Rewrite it as a
workspace target over `parse_fixture_files`, the top-level `types/`, and `corpus/` when present:
~130 lines lighter and the bug gone.

### Two real invariants hold today, are unasserted, and cost ~5 lines each

- **An `ERROR` node implies a reported error.** Over 20 000 fuzz-shaped inputs: zero counterexamples.
  The converse legitimately fails (`for (x in items)` among six examples), so only this direction is
  assertable. Without it, a silent parse failure lets the formatter mangle a file that reports clean.
- **Monotone reporting.** 1/2/4/16/64 copies of one faulty item yield 1/2/4/16/64 findings; 1/4/32/128
  faulty statements in one item yield 2/8/64/256. This is precisely the invariant a per-item or
  per-file finding cap breaks — the regression three adoption reviews called a blocker — and nothing
  guards it.

### A timing-based scaling guard was attempted and does NOT discriminate — read this before retrying

The idea below is right, but the obvious implementation does not work in the default battery, and the
attempt is recorded with numbers so the next one starts further along.

Normalizing by the growing unit is the first trap: per-*declaration* cost falls as declarations grow,
because most of the run is fixed work, so that assertion passes against the very bug it targets. The
sound method is a **four-corner interaction test** — measure (I, D), (2I, D), (I, 2D), (2I, 2D), and
compare the last against the additive prediction, since a table copied per item is exactly the
interaction term. Measured in debug at I=200, D=300:

| | per-item copy present | after the fix |
|---|---|---|
| additive prediction | 699.7 ms | 663.2 ms |
| actual (2I, 2D) | 717.2 ms | 641.5 ms |
| interaction | **+17.5 ms** | −21.7 ms |

A 39 ms swing on a 700 ms total is ~5%, indistinguishable from load noise. The signal only separates at
sizes where the copy dominates — around 2,000 items × 2,400 declarations, which is where the release
measurement showed 71 ms against 27 ms — and four corners at that size cost far more than a default
suite should. An `#[ignore]`d witness was considered and rejected: the extended job that would run it
does not currently run any of these suites, so it would be machinery nobody invokes.

What would work is a structural assertion rather than a timing one — something that counts the copies
directly — but nothing observable exists for it today.

### No witness measures one file with many top-level items

`stats_witness` is corpus-wide, i.e. many files. The shape that hid the known quadratic — one file,
many annotated items — is measured by nothing. It is linear today: 100/400/1600 items cost
0.24/0.65/3.36 s, i.e. 2.43/1.62/2.10 ms per item. One assertion that ms-per-item at 1600 stays within
~2× of ms-per-item at 200 is the only cheap guard against the highest-severity performance class this
project has actually shipped.

### Smaller confirmed items

- `crates/format/tests/test_format_docs.rs` falls back to `text.to_owned()` when a block header is not
  `# name: directive`, publishing **unformatted input** as if it were formatter output. No block hits
  it today (48 blocks, 0 fallbacks), so this is latent; `panic!` instead, one line.
- `crates/syntax/tests/test_error_messages.rs` draws its own carets with its own line/column math, so
  the golden suite pins a caret the product never prints. There are three newline-table
  implementations in the shipping crates (`ry::position::LineIndex`, `format`'s `line_starts`/
  `line_of`, this renderer); one `LineIndex` in `syntax` used by all three deletes two.
- Dead code: `let _ = line_start;` in `crates/syntax/src/testing.rs` and `let _ = before_len;` in
  `crates/syntax/tests/test_fuzz.rs` keep unused locals alive; `floor_char_boundary`/
  `ceil_char_boundary` in that same file, and the same backward scan in `crates/semantics/src/testing.rs`,
  are now in std; `probe/` is an empty untracked directory at the repo root.
- The testing page claims each harness pins every input its targets have ever broken; only `format`
  and `semantics` have a `fuzz_regressions_hold_invariants`. `syntax` and `ide` have none.
- `fuzz/` is its own workspace, so nothing in the default battery type-checks the three libFuzzer
  targets — renaming a battery function breaks them silently until someone runs cargo-fuzz.

### Suspected, not confirmed

- `ide::completion` does O(corpus) work per call and is not memoized (15.9 ms warm, essentially
  file-size independent). That is a debug figure; one release measurement is warranted before any
  product-latency claim, given it is a per-keystroke path.
- `ProjectFiles::set_files` — the file-set change the server makes on open/close/create/delete — is
  never fuzzed; every incremental arm mutates `set_text` only. Probing add/remove/reorder against
  fresh databases with cross-file interface cycles found them all equivalent, so this is a coverage
  gap rather than a live bug. It is the mutation that moves cross-file resolution winners.
- Lowering, naming and inference have no stage-local invariants; they are covered only transitively
  through `render_semantics`. Defensible as end-to-end fuzzing, but the doctrine's per-stage wording
  overstates what is asserted. There is no naming invariant ("every `NameRef` resolves or is
  reported") and no HIR range-containment invariant.

### Judged fine — do not spend time here

The fixture format and harness (1183 ids across 11 suites, zero malformed, duplicate-id rejection
works across files, bless rewrites only the expectation span); the four empty-expectation cases, all
legitimate negative contracts; the `syntax` fuzz suite, which is the best thing in the test
architecture — five real invariants plus splice-reparse equivalence against from-scratch, tree *and*
errors, over an edit stream, for 0.89 s; keeping regressions in `REGRESSIONS` rather than a committed
corpus, with `fuzz/corpus` gitignored. Also deliberately **not** worth fixing: `render_semantics`
versus the typing runner's `render_file` share a scheme loop but render different fact sets on
purpose, and `render_with_strict` building a second database per case costs 114 ms × 22 cases.

## Open — user reports (from the maintainer, not a simulated round)

### REPL resize is not tracked

The width fix itself is closed and archived in `test-user-reports.md`. What stays open: a stock
terminal session handles `SIGWINCH` by calling `R_SetOptionWidth`, which R does not export, and the
console feed is the only other way in — the mechanism already shown to desynchronise the editor when
used between prompts. Worth revisiting if someone finds a third route.

### A variable is reported as unused — NEEDS A REPRO

Filed from a maintainer note that arrived truncated ("…variable is reported as unused"), so the
triggering shape is unknown. Do not guess at it: the `unused` warning has several paths (script
liveness, package top level, parameters, `shadows-namespace` interaction) and a fix aimed at the
wrong one is worse than none. Ask for the snippet before working it.

## Open — diagnostic wording is not styled consistently

Six findings shown side by side in the README read as though five people wrote them: three
`type-mismatch` messages are lowercase sentence fragments with em dashes, `Unexpected comma after last
argument` and `Use TRUE, not T, for Boolean values` are capitalised (the second imperative), and
``` `tmp` is assigned but never used. ``` is the only one carrying a full stop. Caught by a README
reviewer who noticed the page claims a finding "says the same thing" everywhere while the sample
visibly disagrees with itself.

**Measured before acting, and the proposed rule does not survive it.** Across 62 message literals in
`diagnostics.rs`: 36 lowercase with no period, 14 lowercase with a period, 9 capitalised. So
"lowercase fragment, no period" is the plurality but nothing like a convention — and it cannot be
applied blanket, because the capitalised ones are capitalised for reasons: `R's $ operator` is a
proper noun, and `I could not resolve …` / `I do not know the type …` / `I cannot construct an
infinite type` are first-person sentences. Lowercasing those yields `r's` and `i could not`.

The rule that actually fits the corpus is two-shape: a message that is a **complete sentence** is
sentence-cased and ends with a period; a message that is a **fragment** (the expected/found family)
is lowercase with none. Under that rule the real violations are far fewer than "five people wrote
them" suggests — mostly sentences missing their period.

**Unblocked** — the reporter change that re-captured every rendered sample has landed, so a message
reworded here only invalidates the samples that quote it. Whichever samples they are must be
re-captured in the same slice: there is still no harness for that (see the next item), so it is a
manual pass over the pages named in the docs-sample item below.

## Open — nothing verifies the rendered samples in the docs

Roughly two dozen blocks across `features`, `getting-started`, `reference/{cli,configuration,diagnostic-codes}`,
`type-checking/{tutorial,concepts,domain-modeling}`, `index.astro` and the README's generated
`.github/diagnostic.svg` show real tool output. Nothing checks them, so they drift silently and the
drift is only ever found by someone re-capturing by hand. Two things this proves rather than predicts:

- A reporter change left every one of them stale for days, and the cost of re-capturing them by hand
  is most of what made that change expensive to land.
- A **parser** change — narrowing an unterminated argument list's boundary — silently falsified the
  headline missing-comma sample in `features.md`, the very example chosen to show off the parser. It
  was not noticed until the sample was re-run for an unrelated reason, and `main` had shipped it
  wrong. This is the failure the "run the tool before claiming what it does" rule exists to prevent,
  and it does not scale to prose that was true when written.

**Shape of the fix.** Each sample needs its project (a `ry.toml`, one or more source files) and its
command recorded next to it, so a harness can rebuild the project, run the command, and diff the
captured block. Two candidate homes: a fixture-suite-style directory whose renderer output *is* the
docs block, or front-matter/HTML comments in the pages naming a fixture directory. Prefer whichever
lets the docs stay readable as prose — the samples are teaching material, not test data. The blocks
that excerpt one finding out of several need a way to say so (a first-N-findings or a filter-by-code
selector), because several pages legitimately do that.

Until it exists, treat "changed a message or a blame range" as implying a manual sweep of the pages
above, and say in the commit which ones were re-run.

## FIXED — release-artifact versions had drifted apart

`Cargo.toml` was `0.3.0-alpha`, `editors/code/package.json` `0.3.0`, `editors/zed/extension.toml`
`0.2.4-alpha`, and nothing kept the three in step.

**Decided: one source of truth with one mechanical derivation, enforced by a test rather than a
script.** The workspace `Cargo.toml` version is the truth, and the VS Code manifest carries it with
any prerelease suffix removed (that manifest's version has to be a plain `major.minor.patch`). The
derivation is mechanical, so a mismatch is always a stale file and never a judgement call.

**Superseded in part:** this first also required the Zed manifest to carry the version verbatim,
which was wrong — that extension ships no binary, so it versions on its own line. See the decision
record in `decisions.md`.

A *stamping script* was considered and not written. A script only helps if someone runs it, and the
workspace CI that would is still staged in `.github/pending-ci.yml` awaiting a human `git mv` — while
`cargo test` runs on every slice. So the enforcement is two tests in
`crates/ry/tests/test_release_metadata.rs`, and the assertion message names the exact line to write.
Confirmed by running it against the drift before fixing it: it failed with
``editors/zed/extension.toml is stale: write `version = "0.3.0-alpha"` `` and simultaneously confirmed
the VS Code number was already correct — so the "may be deliberate" guess about that one was right.
The Zed half of that verdict did not survive review: the drift there was the *design*, not a bug.

Its own file rather than an addition to `test_cli.rs`, which is explicitly the *binary's* behaviour
contract (rendering, JSON, exit codes) — shipped-artifact metadata is a different component.

Related and user-owned: the extension is published as `felix-andreas.roughly`, and
`felix-andreas.ry` is a 404, so every README and docs link that the rename swept to the new identifier
points at nothing until it is republished. The README now links the working identifier and says why.
Verified: `felix-andreas.roughly` returns 200, `felix-andreas.ry` returns 404.

## Open — test-user round 2 findings

Five simulated users, each on a distinct project of their own writing, learning the tool from the
docs alone (no source access) and trying to reach a clean run. Reports are the
`users/feedback-*.md` files from that round. **Recorded here before any of it is fixed**, per user
directive.

### From the Shiny dashboard user (571-line app, 15 planted mistakes, reached a clean run in ~40 min)

Their verdict: three real bugs found and welcomed, but "a clean run on a Shiny project means much
less than the docs imply" — they finished with exit code 0 and five real bugs still in the code.
Their own top three, in their order:

1. **One `library(pkg)` anywhere in the project disabled nearly all bare-name resolution, including
   typos of in-scope locals and parameters. FIXED.** Their repro: a file containing only
   `library(shiny)` made `repositry` — a one-letter typo of a **parameter in lexical scope on the same
   line** — stop being reported, along with every other bare unresolved name; three planted typos
   survived a `no problems` run on the real app. The blanket tolerance itself is right (an unstubbed
   package's export set is unknowable), so the fix narrowed it in two ways: the near-miss carve-out
   now covers locals and parameters of the enclosing item as well as top-level project symbols (a
   name one edit away from a binding of your own is a typo, not somebody else's export), and a
   `library()` naming the project itself buys no tolerance at all (the package author's #2). Both are
   now documented on the `unresolved` row of `diagnostics.md`, in the reference's resolution rules,
   and on the limitations page.
2. **The required/optional annotation mismatch FIXED.** Optionality now comes from the formals — a
   formal with a default is optional in R and no annotation can change that — so the exported
   signature takes it from the code and the annotation's disagreement is reported once at the
   definition, naming the fix (`write [currency]`). Callers of correct R are clean. The `[name]`
   bracket form is now in the guide as well as the reference.
3. **A parameter's type is not seeded from its default value** — `f <- function(s = settings) s$hsot`
   is missed while `settings$hsot` and a local alias are both caught. **Analysed, and the obvious fix
   is a false-positive generator, so it needs the design rather than a patch.** Binding the parameter
   to its default's type makes every caller passing anything else wrong, and reporting the field
   against the default's shape breaks the commonest idiom of all: `function(x = list()) x$name`, where
   the default is an empty accumulator and callers supply the real record — R answers `NULL` there, so
   an error would be plain wrong. The honest model is that such a parameter is
   `default's type | whatever callers pass`, which is still open, so a field access on it should be at
   most `T | NULL` (the same rule the accumulator fix established for unions) rather than an error.
   Recovering the user's case needs to know the argument is never supplied anywhere — whole-program
   information the checker does not have. Leave it missed rather than trade it for the class of false
   positive this round spent its time removing.

Also from them: `$` on an unannotated parameter constrains nothing, so the parameter accepts
anything; `unresolved` changes severity when `strict` is on (documented now, in the diagnostics table
and the reference); R6 is invisible (correctly
documented, cost recorded); and they asked for a page saying what does and does not work for Shiny.

### From the TDD package author (`tallyr`, 500 lines, green in real R 4.3.3)

Their package **passes in real R** (`pkgload::load_all` + `test_dir`), so every warning below is
provably a false positive. Clean run cost 5 edits on 500 lines; they would enable `typing` in CI
tomorrow and would **not** enable `strict`, `shadows-namespace` or `unused-parameter` — "three of
five opt-in lints are unusable on a package with an S3 class and a testthat suite".

1. **`structure(list(...), class = "x")` erasing the record type FIXED.** It yielded `Unknown`,
   against a documented promise of "a plain record; the class attribute is data", so the field typo
   the guide headlines was caught on a bare `list()` and **silently dropped** once `class =` was
   added — the objects real packages are built from. **31 of their 34 strict findings traced to this
   one call.** `structure()` now returns its first argument's type, so the record survives.
   Deliberately NOT changed: the class attribute still does not mint a nominal type — `@new` remains
   the only nominal introduction, and reading `class[1]` instead would type a
   `c("grouped_df", "tbl_df", "tbl", "data.frame")` value as `grouped_df` and then reject it at a
   `data.frame` parameter. See `contributing/design/inline-type-syntax.md` §3.
2. **`library(yourpkg)` in `tests/testthat.R` FIXED.** The project's own name — `DESCRIPTION`'s
   `Package` field, now carried on the metadata input — earns no tolerance: its export set is not
   unknowable, those exports being the project's own definitions. `usethis` generates that file, so
   this was switching off unresolved detection in every testthat package. The Shiny user's #1 (the
   near-miss carve-out reaching locals and parameters) is the other half and is also fixed.
3. **`tests/testthat/helper-*.R` FIXED.** Files directly under `tests/testthat/` now share one
   namespace, as testthat's own loading does (helpers first, then tests) — so a shared fixture is
   neither `unused` at its definition nor `unresolved` at its uses, while a real typo in a test still
   reports with the helper suggested. Documented in "where Roughly looks" and on the limitations
   page.
4. **`shadows-namespace` fires on locals inside `test_that()` and calls them "Top-level binding".**
   Diagnosed but deliberately not patched yet: naming is *right* that the binding lives in the file's
   frame, because R braces are not a scope and the checker cannot know `test_that` makes an
   environment. What the lint needs is **syntactic** nesting — "written as a direct statement of the
   file" — which the binding model does not currently distinguish from "belongs to the file's frame".
   Adding that must not disturb the unused rules, where `TopLevel` means package-visible; both shadow
   lints are default-off, so this waits for the model change rather than a special case. The wording
   is part of the bug: a local two levels deep should never be called a top-level binding.
5. **`unused-parameter` flagging user-defined S3 generics and their methods FIXED**, along with the
   default-on `unused` reporting an S3 method as dead (a separate finding from the first round, same
   missing knowledge). A project's own generics are now discovered by their bodies — a top-level
   definition whose read set contains `UseMethod` — across the whole package namespace, so a generic
   in `R/speak.R` covers `speak.dog` in `R/dog.R`; the generic itself is exempt too, since it declares
   the dispatch argument and never touches it. `is_s3_method_name`/`s3_generics` live in
   `semantics.rs` beside the other project-level projections, shared by the lint and the unused walk.
6. **A bad `importFrom` FIXED — it is an error now**, matching its `export()` sibling: R refuses to
   *load* such a package, so it is not survivable advice and must not pass a `--min-severity error`
   gate. A bad `pkg::name` *read* stays a warning, and the reference says why: a bad import stops
   loading outright, a bad qualified read fails only if that line runs.
7. **`#: @new` does not stop `structure()` being a strict origin** — the documented S3 remedy failing
   at exactly the site it exists for. Related to (1).
8. **S3 generic/method signature consistency is unchecked** — both planted violations missed, and
   `R CMD check` catches both. Roughly already does the rarer undefined-export check.

Docs gaps they hit: the adjacency rules say a plain `#` comment breaks `#:` attachment and never
mention `#'`, so interleaving `#: @param` with roxygen's `#' @param` — the first thing a roxygen
user tries — fails; the guide never shows `...` or `[optional]` in an annotation, and **every** S3
method needs both; `configuration.md` omits `DESCRIPTION` as a project root marker.

What they praised: the partial-match catch (`sourc =` → `source`, which R silently accepts and
nothing else in their toolchain catches), `NAMESPACE` being genuinely load-bearing with import-typo
and undefined-export validation at the right line, **`#:` verified invisible to `R CMD check`**, the
formatter and JSON output as production-ready, and the annotation-adjacency message for naming
roxygen2 and giving the fix. They also confirmed the `expect_error` decision was right and that the
"testing that something is rejected" subsection worked first try.

### From the Quarto/Rmd analyst (656 lines across two literate documents, 10 planted mistakes)

Their verdict: **"today, no"** — they would hit the ggplot2 errors within an hour and be fully off by
end of day, *never knowing the default checks were not running*. **"With #1, #2, #3 fixed,
permanently yes, including CI. That gap is three bugs, not a redesign."**

1. **`library()` of a common package killing unresolved-name checking project-wide FIXED** — the
   third and loudest report of the same hole. The fix is that **a manifest is enough**: a namespace
   with an `.exports` list has a knowable export set, so the blanket tolerance never applies to it,
   and no types are needed. Fifteen manifest-only CRAN namespaces now ship (`tibble`, `tidyr`,
   `readr`, `purrr`, `stringr`, `forcats`, `lubridate`, `magrittr`, `rlang`, `glue`, `scales`,
   `knitr`, `jsonlite`, `R6`, `tidyverse`), every name from them typing `Unknown` while typos beside
   them report with suggestions. `library(tidyverse)` additionally activates the nine packages it
   *attaches* (`stubs::META_PACKAGE_MEMBERS`) — a meta-package re-exports almost nothing, and
   activating members is also what hands such a project dplyr's and ggplot2's typed declarations
   instead of a manifest's `Unknown`. A package with no manifest (`janitor`) still earns the
   tolerance, correctly. Remaining: `janitor` and any other package a user names — the remedy is a
   two-line project stub, and the limitations page now says so on the user-facing side.
2. **ggplot2 `+` chains FIXED.** The corpus declared `+.ggplot` but nothing for a component pair,
   so `theme_minimal() + theme(...)` and a chain whose left operand was lost through `%>%` fell
   through to the numeric rules and reported arithmetic on a `gg`. R routes all of it through the
   single `+.gg` method, which the corpus now declares; a genuine mistake (`plot + 1L`) is still
   caught, naming both classes.
3. **Inline `` `r expr` `` FIXED.** The conversion recognizes inline spans as code: the delimiter and
   language tag blank to spaces, the expression keeps its bytes and its offset, and the closing
   backtick becomes the `;` that separates two inline expressions on one prose line. So a value a
   report only displays is used rather than unused, and a typo inside an inline expression reports at
   the right line and column. A plain Markdown code span (`` `total` ``), a span naming another
   language, `` `rate` `` (not an `r` tag), and an unclosed span all stay prose.
4. **`source()` is not followed, so a helpers file is pure noise in both directions** — all ten
   helper functions reported `unused`, and every *correct* call to them reported `unresolved`,
   indistinguishable from their planted typo. It also killed the wrong-arity catch, which works well
   within a file.
5. **Byte-offset columns and the misaligned caret FIXED.** A reported column now counts characters —
   in the rendered header, in `--output json`, and in the server's `file:line:column` hover strings —
   and the caret is padded by terminal cells, so it lands under the glyph even for double-width text.
   See the decision record; the JSON field documentation moved with it.
6. **5 of 10 planted bugs caught, and all 5 were cosmetic** (`=`, `T`/`F`, trailing comma). Missed:
   two column typos, two function-name typos, one wrong arity. Unknown *functions* inside `mutate`
   and `filter` are swallowed along with the column names.
7. **`fmt` on a target with no R in it FIXED** — it reports `0 files formatted` and exits 0, as
   `check` already did. A stage with nothing to do must not fail a pipeline, and a pre-commit hook
   handing the formatter a literate document it deliberately skips must not fail the commit.
8. An unclosed chunk reports their English prose as an unresolved variable rather than the missing
   fence. **The `strict` severity escalation is no longer silent** — the `unresolved` row of
   `diagnostics.md` and the strict-mode section of the reference both say that turning strict on
   raises every `unresolved` finding to an error without changing the count, and why that matters to a
   `--min-severity error` gate. The behaviour itself is right (a name the checker cannot see is a hole
   in the checked surface); being undocumented where a user looks was the bug.

What they praised, and it is worth recording because it was the part they expected to be broken:
**the literate handling itself is the strongest part of the tool.** Line numbers exact across all 545
lines of `.qmd`/`.Rmd`, ASCII columns exact, prose ignored, every chunk-header form parsed (including
`#| fig-cap: "A caption with = signs"` not fooling the `=` lint), `{python}`/`{sql}`/`{bash}`/`{ojs}`
and bare fences all correctly skipped, CRLF and Sweave and no-YAML all mapped correctly, suppression
comments working inside chunks. Also **zero false positives on 50 lines of idiomatic tidyverse** —
bare column names through the whole verb set, `case_when`, `across(where(is.numeric), ~ .x * 2)`,
tidyselect helpers, joins and `aes()` — which was their biggest fear going in.
`getting-started.mdx`'s "where Roughly looks" section is "the best thing in the docs". The guide
loses them where its showcase uses `list(...)`: swapping it for `read.csv()` makes the identical
mistake report nothing, and the page calls that "the whole pitch" with no caveat at the point of
contact.

### From the ETL engineer (919-line data.table/DBI pipeline, 12 planted mistakes, 8 caught)

Their verdict: `fmt --check` in CI today; `check --min-severity error` after two fixes; `typing =
true` as a merge gate **not yet** — "not because it's noisy, but because after #1, #2 and #10 a green
run doesn't yet mean what it needs to mean at 3am". Adoption cost: 34 warnings on first run, 30 of
them `unknown package namespace 'DBI'`, fixed by a **3-line `DESCRIPTION` with `Imports:`** that no
doc page mentions for this purpose — while the path the docs *do* recommend (hand-writing
`stubs/*.Rtypes`) cost five files, fixed less, and was actively harmful (see 3).

1. **Column-name typos inside `DT[...]` are invisible** — `orders[, net := gross_ammount * 1.19]` and
   `by = currrency` both silently accepted. Meanwhile the only name it *did* report was a correct one
   (see 5). Exactly inverted: silent where the typo is real, noisy where the name is right. A known
   gap, but `limitations.md` should say plainly that column names are never validated.
2. **`library(pkg)` for an unstubbed package disables `unresolved` project-wide** — the **fourth
   independent report**, and their version is the most damning: `library(totallyMadeUpPackage)`, a
   package that *does not exist*, buys the same blanket tolerance. Their "clean run was a mirage".
3. **CI BREAKER: setting `[check] exclude` disables the built-in `renv/`/`packrat/` skip.** No key →
   1 file; `exclude = []` → 1 file; `exclude = ["nothing-here/"]`, matching nothing → renv and
   packrat both walked. The docs' own example `exclude = ["scripts/"]` would drag an entire renv
   library into CI. The user-supplied list must *extend* the vendored defaults, not replace them.
4. **Any `stubs/` file deactivates the shipped conditional namespaces.** One stub for an unrelated
   package made 19 `data.table::` calls unresolved while bare `fread` still resolved, and made
   `data.table` unusable as an annotation type name — which silently suppressed a real type error.
5. **`setkey`/`setorder`/`unique(DT, by=)` FIXED.** `setkey` and `setorder` name columns rather than
   values, so they are `@masked` like the other NSE verbs (the `v`-suffixed forms take a character
   vector and never needed it). `unique`'s fallback candidate is variadic now, because `unique` is a
   generic whose methods take arguments base's signature does not name — the typed candidates stay
   exact, so `unique(c(1L, 2L))` is still `integer[]`. Verifying that turned up one more: every
   column-name parameter in the data.table stub was declared scalar `character`, so
   `setkeyv(DT, c("id", "date"))` — the whole point of the `v` forms — was an error. They are
   `character[]` now, which accepts one name or several.
6. **`data.table` is rejected where `data.frame` is expected, and the wrong direction is accepted.**
   `needs_df(data.table(a = 1L))` is legal R and reports `expected data.frame, found data.table`;
   `needs_dt(data.frame(a = 1L))`, the direction that really is wrong, reports nothing. This is
   **nominal subtyping** — R's class vector for a data.table is `c("data.table", "data.frame")` — and
   it is the same deferred design as S4's `contains=` and R6's `inherit=`: `TyKind::Named` matches by
   exact name and nothing in the compatibility relation carries a hierarchy. The tractable corner is a
   *declared*, acyclic nominal-extends-nominal relation in the stub vocabulary; do it as that design,
   not as a data.table special case.
7. **`on.exit()` reads FIXED.** R stores the expression and runs it at return, so it observes the
   *last* value of what it reads; a read inside it now keeps every write of that name in the frame
   alive, exactly as a closure capture does. A genuine dead store beside a guard still reports.
8. **Recursion defeats `is.logical()` narrowing** — the identical non-recursive function is clean.
9. **A maybe-`NULL` value is only caught by arithmetic.** `toupper`, `nchar` and `substr` on a
   `character | NULL` all pass, despite the guide promising this class of check.

What they praised: record-field inference from a plain `list()` with `Did you mean batch_size?`
("excellent and worth adopting for alone"); named-argument typo suggestions, arity checks,
argument-order errors ranged on the argument, branch-union arithmetic and non-function calls all
correct with "the best error wording I've seen in an R tool"; and **the data.table bracket really
works** — a 162-line transform module using `:=`, `.SD`, `.SDcols`, `.N`, `dcast` and a non-equi join
with `by = .EACHI` produced zero NSE complaints. Exit codes, JSON Lines, `--min-severity`, config
discovery, unknown-key warnings and suppression comments all matched the docs. 0.15s on 919 lines in
a debug build.

### From the time-series quant (620 lines, 10 planted mistakes, 3 caught)

Their headline: "the date/time operator modelling is the best static checking of R dates I have
seen — and it is surrounded by false positives that fire on ordinary code, plus a `Date` type that
evaporates the moment a date touches `c()`, `[`, `min()`, or a `for` loop." They would adopt lints
and `unresolved` today, and `typing` on one module "because the date operator work is worth real
money" — but not tell anyone with matrix-heavy code to turn typing on until (1) and (3) are fixed.

1. **Their soundness diagnosis was WRONG, and the real problem behind it is FIXED.** Verified:
   `min(Date)` is not typed `integer` — it selects the corpus's trailing `Any` candidate, so the check
   is *skipped*, not wrong, and the quality bar holds. What the finding exposed is that `Any` is
   exempt from strict mode, so the feature whose whole purpose is finding gaps missed the commonest
   one. An overload selection that commits an `Any` return now records a strict origin, so
   `min(d)` on a `Date` is reported under `strict = true`.
2. **CONFIRMED and genuinely unsound-adjacent: a `<T: numeric>` bound admits a `Date`, and the body's
   arithmetic then fails in R.** `#: <T: numeric> fn(a: T, b: T) -> T` called as `add_them(d1, d2)`
   is accepted, while the direct `d1 + d2` is correctly rejected with `` `+` is not defined between
   `Date` and `Date` `` — so a one-line helper launders a real error. This is the
   `declares_arithmetic` relaxation: a class that declares *any* arithmetic method satisfies
   `Numeric` wholesale, which does not follow — `Date` declares `+.Date` for `Date + integer` and has
   no `Date + Date`. **This is the traits / third-constraint-kind tripwire, now tripped a fifth
   time**, and it is the mechanism behind their missed "adding two dates via a helper" bug. A
   constraint meaning "supports this operator with these operands" replaces both this relaxation and
   the operand tie. Do not patch it again; design it.
   Also: `sort`/`head`/`rev` on dates report ``found `T[]` ``, leaking an unbound type variable.
3. **FP — `x[i, j]` on an unannotated parameter FIXED.** A subject whose shape the author never
   wrote down is now sound-by-refusal for *any* index shape, which is what the reference already
   promised; a subject whose shape *was* written down still refuses a shape no rule covers
   (`c(1L, 2L)[1L, 2L]` is an error).
4. **FP — `c()` on a classed value FIXED, and it does better than not erroring:** a class declaring
   a `c.Class` method keeps its class through concatenation (`c(d1, d2)` is a `Date`, and
   `dates + d2` is still refused as `Date + Date`), which is R's own dispatch rule. A nominal with no
   such method is indeterminate rather than an error.
5. **FP — `vapply(x, f, numeric(1))` errors whenever the callback is not statically `double`.** R
   accepts a wider template; they confirmed with `roughly run` that
   `vapply(1:3, function(i) i, numeric(1))` works.
6. **`Date` does not survive real code.** Survives `d + 1L`, `d2 - d1`, `d1 < d2`, `format`,
   `seq.Date` and `while` cursors; **lost to `Unknown`** through `dates[i]`, `dates[mask]`,
   `for (d in dates)` and `Reduce`; **wrong** through `min`/`max`; **an error** through `c()`. The
   same bug is caught in one place and missed in another — `cursor + cursor` after a `while` loop is
   flagged, the identical `d + d` inside `for (d in dates)` is not. No doc describes this boundary.
7. **`#: @type TradeDate {Date}` disables all Date arithmetic** — `settle - trade` is rejected — which
   kills the guide's own recipe for domain types over dates. The working route is a project
   `.Rtypes` declaring `-.SettleDate`, which the guide never mentions (and a stub cannot see an
   inline `@type`, the gap already recorded below). Also **`Date[]` is inexpressible**, and
   `Days + Days` yields `double`, so a units tag dies on arithmetic.
8. **Strict mode's own advice does not work.** The message says "add a type annotation" and **no
   annotation form silences it** — `#:`, `@if-unknown`, `@trust` and `#: Any` all still report, the
   last contradicting `reference.md` directly. Only `# roughly: allow(strict)` works.
9. **`&` and `|` are unmodelled**, so `TRUE & FALSE` is `Unknown` and every logical mask kills
   downstream checking. The reference documents `&&`/`||` with no counterpart, and the only mention
   of `&` is a parenthetical inside the NSE discussion.
10. **Formatter — `m[i, , drop = FALSE]` becomes `m[i,, drop = FALSE]`**, removing the space that makes
   the empty dimension slot visible, in exactly the code where miscounting slots is the bug. It is
   idempotent, so deliberate, and undocumented.
11. **Misleading message — any `X[Y]` annotation reports "only one compact annotation fits in a `#:`
    block — separate the annotations with a blank line" when there is only one annotation.**

What they praised: **every date expression R rejects or warns on was caught across 21 probes, with
zero misses and zero false alarms**, including `Sys.time() - Sys.Date()` — which R only *warns*
about and which they have shipped to production. `` `-` is not defined between `POSIXct` and
`Date` `` is "the best message in the tool". Also: field-typo detection with the full inferred record
shape, cross-file resolution with typo suggestions at zero config, arity messages in plain language,
syntax-error recovery, clean CI-ready JSON, 31.5k lines across 120 files in 2.2s on a debug build,
and `roughly run` with an embedded R letting them verify every claim ("underadvertised"). Units
nominals and domain matrix types both work and produce excellent messages.

## Open — documentation review findings

Four independent reviews read the docs cold (a first-hour newcomer, an information architect, an
accuracy auditor executing every claim against the binary, and a positioning analyst who also
measured the competition). The accuracy fixes landed; what remains is below.

**Bugs the reviews found, each with a repro:**

- **Numeric conditions FIXED** (`if (length(x))`, `while (n)`, `!length(x)` — R coerces them, zero
  false and anything else true; `character`/`complex`/`raw`/vector conditions stay errors). One
  residual: a condition whose type is still *undetermined* is bound to `logical`, so
  `function(n) while (n) ...` infers `n: logical` and rejects a numeric caller. Fixing that
  properly needs a "coercible to a condition" constraint — a fourth constraint kind, which is the
  documented tripwire for designing traits rather than accreting (see `contributing/design/open-questions.md`).
- **Named-before-positional matching FIXED.** `match_arguments` walked the argument list once, in
  source order, so a positional argument could take a formal that a *later* named argument was
  going to claim (`vapply(xs, character(1), FUN = f)` reported a bogus "FUN given twice").
  Matching is now two passes in `argument_targets` — names claim their formals, then positionals
  fill what is left — computed once and shared by the checking loop and the rest-parameter
  forwarding scan, which previously duplicated the accounting and so duplicated the bug.
- **The accumulator idiom FIXED, on the design the report proposed.** `$` on a union subject no
  longer demands the field on every member: a field some shapes carry and others do not reads as
  `T | NULL`, because that is what R answers for a name a list lacks. A field **no** shape carries
  stays an error — the typo check is the whole value — and a structural refusal (`$` on an atomic
  vector) is still hard from any member. The "did you mean" is drawn from every field the union can
  carry, so the suggestion survives a member with no fields at all.
- **Literate-document prose blanking FIXED, and it hid a worse bug.** A non-breaking space is
  whitespace to Rust's `char::is_whitespace` and an unexpected character to R's lexer, so prose
  containing one reported a syntax error against a blank line. The same code blanked per
  *character* rather than per *byte*, so any non-ASCII prose shifted every byte offset after it —
  every downstream range is a byte offset, so diagnostics in later chunks were silently
  misplaced. Both are fixed by blanking each character to its own `len_utf8()` in spaces; the unit
  test that was supposed to catch this asserted char count instead of byte length.
- **Self-checking ggplot2 reports 1132 findings and takes 2.4s** — the one package a stub ships
  for. The proposed cause (the shipped stub colliding with the package's own definitions) does
  **not** reproduce minimally: a package named `ggplot2` that defines `geom_point` and calls it
  checks clean, so project-wins-over-corpus works. The real cause is unidentified and needs the
  actual source (fetch the corpus); the likely candidates are ggplot2's own ggproto/R6 layer and
  its NSE, both known gaps, in which case the number is honest rather than a bug.
- **Duplicate type names go unreported in script files.** `reference.md` says the duplicate-`@type`
  error fires "regardless of file"; it fires in package files only. The value-name analogue is
  deliberately exempt for scripts, type names are not.

**Documentation that is still wrong (verified, not yet fixed):** `reference.md` documents a
rest-parameter spelling (`...items`) that never parses, and cites `contributing/design/open-questions.md`
— a published contract pointing at an unpublished file; `stdlib-stubs.md` names six symbols
(`BuiltinKind`, `parse_surface_type`, …) that exist only in the frozen legacy tree, and puts `...`
last in `paste` when the real declaration has it first (the position is load-bearing);
`architecture.md` still uses internal gate vocabulary; `development.md`'s re-bless command omits
`ROUGHLY_BLESS=1`; the `stub` diagnostic code and the SCREAMING_SNAKE naming exemption are emitted
but documented nowhere; five smaller `language-server.mdx` items (a `bun run package` with no root
`package.json`, three wrong VS Code palette titles, 4-of-5 code actions, `PAREN_EXPR` folding
omitted, a `--verbose` example whose own help says it is ignored).

**The structural problem.** The site was organised around Roughly's subsystems rather than around
anything a reader wants to *do*. Largely addressed: `typing/guide.md` is now a **tutorial** — eight
numbered steps from a zero-config first run to strict mode, each with a runnable example and output
captured from the binary, ending where the reference begins — and `stdlib-stubs.md` is no longer an
internal RFC. Also landed: the diagnostics reference (all fourteen codes, tables built by running
the tool), a CI workflow, and `limitations.md`.

**Comparison page DONE** (`comparison.md`): a capability table plus a "where you should use
something else" section that hands formatting to Air and rule breadth to Jarl and lintr outright,
and states the alpha/one-maintainer position. Every claim about another project was verified from
that project's own documentation, and no benchmark number appears that was not measured here —
second-hand numbers about competitors are the fastest way to lose the argument.

**Still missing:** a **"why a type checker for R"** explanation page (where the intrigue lives) and
a dedicated **adoption how-to** (the ladder is on the limitations page but deserves its own).

**Positioning (partly addressed — the comparison page landed and the primacy claim is now
defensible: "the only one that infers types" rather than "the first one", since two unmaintained
annotation-only attempts exist and a hostile reader finds them in one search).** The headline leads
with the formatter and linter — the two things Roughly loses at
today (Posit's Air is bundled in Positron; Jarl ships 71 rules with `--fix` and an LSP, 140× faster
than lintr) — and buries the one thing nobody else has. Measured facts the docs never state: 854
files / 166k lines in 3.6s, and lintr 38.2s vs 0.47s on dplyr. Research confirms **no one has ever
shipped a static type checker for R**: Vitek's group proved it viable (~80% of CRAN functions
monomorphic or nearly, 1.98% contract-failure rate) then pivoted to a JIT IR, the one Damas-Milner
attempt (`RTypeInference`) went dormant in 2021, and Posit's `ark` README states it plans
"sophisticated static analysis of R code" citing rust-analyzer while Positron ships a Rust type
checker for *Python*. Full reports and their paste-ready rewrites are the four
`docs-review-*.md` files produced by that round.

## Open — adoption review findings (unfixed items, each with a minimal repro)

Three independent black-box adoption reviews (an analysis-script user, a CRAN package author, a
numerical-computing user; docs + `--help` only, no source access) simulated real projects and
converged on the same walls. What they found and is now fixed is in the ledger; what remains is
below, ranked by how often a real user hits it.

- **Overload selection with a flexible argument FIXED** (fact-versus-guess probing — see
  `decisions.md`): a candidate that fits without narrowing the caller's open types beats one that
  does not, and a single fitting candidate is selected outright. The apply family is now unblocked
  but still untyped where its result shape is *value*-dependent: `sapply`/`mapply`/`Map`/`tapply`
  stay `Any` because `simplify = FALSE` and a vector-returning callback change the result shape
  without changing any argument type, so no overload set can discriminate them — typing only their
  *parameters* (leaving the return `Any`) is the reachable win, at the cost of rejecting R's
  function-name-as-string form (`sapply(x, "length")`, already rejected for `lapply`).
- **The three object systems (S3 partial, S4 and R6 recognition-only). Re-measured, and two of the
  four claims this entry used to make were stale — including the one it ranked first.**

  - **`setGeneric` is FIXED and was already fixed when this said otherwise.** The entry claimed
    "`setGeneric("f", ...)` does not define `f`, so every call to a project's own S4 generic reports
    `unresolved`", and ranked it the top fix. It does not reproduce: `set_generic_target` binds the
    name, handles the `methods::setGeneric` form and the `name =` argument, and a control probe in the
    same file confirms the `unresolved` check was live (`definitely_not_defined` reported; `area` did
    not). Anyone who took the ranking at face value would have spent a cycle fixing a non-bug.
  - **R6 was mischaracterised.** "R6 has no stub at all (`R6::R6Class` reports `unknown package
    namespace R6`)" — R6 *does* ship an export manifest and is a conditional namespace, so it resolves
    as soon as the project declares it (`DESCRIPTION` `Imports: R6`) or attaches it (`library(R6)`);
    both verified clean. The message appears only for `R6::` in a project that declares neither, which
    is the documented rule for *any* undeclared namespace and is deliberate. What is actually missing
    is **typed** declarations — the class, its fields and its methods are `Unknown`, so `obj$typo()` is
    silent and completion after `self$` offers every record field in the workspace.
  - **Still true: an S4 slot typo is silent.** `setClass("A", representation(x = "numeric"))` then
    `new("A", y = 1)` reports nothing; R halts with ``invalid name for slot of class "A": y``. `x@slot`
    has no type either, and `setClass`/`setMethod`/`new` are `Any` stubs.
  - **Still true: `UseMethod` is not modelled** — a generic call is `Unknown` — and
    `structure(list(...), class = "dog")` produces a plain record, so the class attribute is data. S3
    *operator* dispatch is real (`+.Date`, `Arith.X`, `Ops.X` are built and dispatched, and the linter
    knows `generic.class` names).

  All three systems are recognized structurally by the IDE outline (`classify_symbol_call`), which is
  where the type-side work can start. **Revised fix order, cheapest real win first: the S4 slot-name
  check**, then R6 class typing, then S4 slot types, then `UseMethod`.

  The slot check is bounded — `setClass` names the slots, `new("Class", ...)` names its arguments —
  but **`contains =` is the trap**: a subclass legitimately takes its parent's slots, verified against
  R (`setClass("C", contains = "P", …); new("C", x = 1, y = 2)` runs), so a check that does not follow
  the inheritance chain turns correct code into a false positive. `representation(...)` and the
  `slots =` form both need reading, and a class assembled dynamically must fall back to silence.
- **Matrix SHAPE is still untracked.** `%*%`/`%o%`/`%x%` now return the `matrix` nominal and the class
  has its arithmetic and comparison methods, so matrix expressions type and compose — but
  `matrix`/`t`/`solve`/`dim`/`crossprod`/`diag`/`apply` still return `Any`, so a transposed dimension
  or a non-conformable product is invisible. Declaring them `-> matrix` is easy; the value is in
  *dimensions*, which needs a shape-carrying matrix type (see the data.frame row-type design). Note
  the trap this session hit: making a constructor return a real nominal without also declaring the
  class's operator methods turns every `m + 1` into a false error.
- **A project's own `%op%` stays untyped by design** (the result is `Unknown`, a strict-mode origin):
  it may be an NSE wrapper whose right operand is quoted, like magrittr's `%>%`, and checking that as
  an ordinary call would reject correct code. Lowering `%op%` to the call it is (the documented `|>`
  precedent) would type it and give goto/references on the operator — weigh that against the NSE risk
  and the `unresolved` a bare-script `%>%` would gain.
- **A list of functions FIXED**, and the cause was not the union: `function_compatible` demanded an
  *exact* parameter count, so `mean` (one required plus two optional) could not serve a
  one-argument callback interface, and the union of two such functions failed member-wise. Arity is
  a range now — a function serves an interface when it accepts every call shape the interface
  promises — so extra optional parameters are fine while requiring too many, or refusing an
  argument the interface sends, still fail. `lapply(list(mean, sd), function(g) g(1:3))` is
  `list[double]`. `lapply` keeps its input's names too (`list[named: T]` declared as its narrower
  first candidate), which is what forced the overload tiebreak to become plain first-match — see
  `decisions.md`.
- **Everything from a `data.frame` is `Unknown`, and `Unknown` satisfies every annotation** — so on
  data-frame-heavy code annotations look protective and are not. This is the design consequence that
  decides the tool's value for analysis users; it needs at minimum a way to *see* that a check was
  skipped (strict mode, once it reports origins).
- **An S3 method declared in R being reported `unused` FIXED** — the unused walk shares the
  method-name knowledge with the lints, and a project's own generics count, not just the corpus's.
  Dispatch is still not a read, so the exemption is by name shape (`generic.class` for a generic that
  exists), which is the same signal R itself uses to find the method.
- **Only the STUB route to an operator method on a project nominal is blocked.** Declaring the method
  as an annotated R function works (verified above), which is what an author writes anyway for a real
  S3 class; the gap is narrower than "operator methods need ergonomics" — a `.Rtypes` stub cannot see
  a `@type` the R source declares (`this declaration does not load: I do not know the type Meters`),
  so only the stub spelling is unreachable. Decide whether stub sources should see project `@type`
  declarations at all, or whether the R-side declaration is simply the answer and the docs should say
  so.
- **Smaller, each with a one-line repro in the reports:** messages leak unbound type variables (`list[T] | T[]`) and expand an alias on
  only one side of an expected/found pair; `unused` false-positives on a write followed by `break`;
  closure re-entry is unmodelled, so the `if (!is.null(cache)) return(cache); cache <<- v` memo idiom
  yields `T | NULL`; (a generic parameter rejecting a non-`NULL` default is CORRECT, not a gap — `<T> fn(x: T, [fallback]: T)` defaulting to `0L` would return `0L` from a call the signature promises returns `character`; `T | NULL` with a `NULL` default works because `NULL` is a declared member); the `unused` write-then-`break`
  false positive is NOT reproducible (verified across `for`/`while`/`repeat` and both used and genuinely
  dead writes) — drop it unless a concrete shape resurfaces; no `--fix`, no stdin, and no CLI way to ask "what type is this?" (which makes debugging an inference surprise guesswork for a
  CLI-only user). `sum(1, 2, 3,)` formatting to `sum(1, 2, 3, )` is NOT a defect and stays: the
  trailing comma introduces a missing argument, so it parses identically to the `alist(, )` idiom the
  space serves, and the `trailing-comma` lint reports the mistake.
- **Literate documents are analysed by `check` but not by the editor.** `.Rmd` / `.qmd` / `.Rnw`
  chunks are converted to an R program by blanking every non-R character (`syntax::literate`), so
  ranges need no translation and `check` reports at the original line and column. The LSP path still
  ignores them: `did_change` hands the engine incremental edits against the document the editor
  holds, and the converted text is a different buffer, so wiring it needs the original text kept
  alongside the analysed one (or the conversion applied per-edit). The formatter deliberately stays
  out — most of an `.Rmd` is prose.
- **An unannotated helper that wraps an operator over a class fails, and the tie is why.**
  `add_layer <- function(plot, layer) plot + layer` infers `<T: numeric> fn(plot: T, layer: T)` — the
  two flexible operands are tied to ONE variable — so `add_layer(base, geom_point())` reports
  `expected ggplot, found gg`. Same shape for dates: `add_days <- function(d, n) d + n`. Annotating
  the helper fixes it and the message is clear, but the tie is an over-commitment: R's `+` never
  required its operands to share a type, and a class that declares `+.Class` accepts pairings the tie
  forbids. **This is the "traits" / third-constraint-kind question in `contributing/design/open-questions.md`, now tripped a
  third time by shipped features** — the right fix is a "supports this operator" constraint instead of
  `Numeric`, replacing both the tie and the `declares_arithmetic` relaxation that lets an
  arithmetic-declaring class satisfy `Numeric` today. Next stub corpus addition that returns a real
  nominal will trip it again.
- **A type error inside `expect_error(...)` — DECIDED: it stays reported**, and
  `# roughly: allow(type-mismatch)` is the answer (decision record in `decisions.md`; documented on
  the diagnostics page). Suppressing inside expectation payloads would blind genuine mistakes in
  tests and needs an open-ended list of function families. The open follow-up is the stronger form:
  a suppression that reports when the expected finding does *not* appear, like
  `@ts-expect-error` — a feature for every code, not a special case.

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

## Closed — the overload corpus is NOT inflated by missing grammar (investigated; premise was false)

This was filed as "two absent stub-grammar features"; both features exist, and collapsing the corpus
buys one line. Recorded so it is not re-derived.

**The constrained binder already works**, in `#:` annotations and `.Rtypes` files alike. Measured:
`zzabs : <T: numeric> fn(x: T) -> T` in a project stub types `zzabs(1L)` as `integer`, `zzabs(2.5)`
as `double`, `zzabs(c(1L, 2L))` as `integer[]`, `zzabs(c(1.5, 2.5))` as `double[]`, and rejects
`zzabs("no")`. Vectors included — so the "shape-mirroring return" is not a separate missing feature
either; it falls out of the binder.

**The extra candidates are not redundancy, they carry facts a binder cannot state.** Three of them:

- **`logical` promotes to `integer`, it does not preserve.** R gives `abs(TRUE)` → `integer`, so a
  type-preserving binder would be wrong; the concrete `fn(x: integer) -> integer` candidate is what
  catches logical, because a concrete `integer` parameter accepts `logical` by coercion while a
  `numeric`-constrained *variable* refuses it. That asymmetry is deliberate and correct here.
- Sets whose int and logical arms are already unioned into one line (`cumsum`, `cummin`, `cummax`)
  gain nothing: the binder replaces a line that already covers two cases.
- `min`/`max`/`range`/`sort` carry a `character` candidate, which no numeric binder subsumes.

A trial collapse of `abs` from five candidates to four was behaviour-identical across seven probes
(scalar/vector × integer/double, logical, logical vector, and the wrapper) — and `abs` is the only
set with that scalar-and-vector-times-int-and-double shape. One line, in one function, is not worth a
corpus-wide edit, so it was reverted.

Wording trap, still worth keeping: never say these functions "have no principal scheme" — they do,
and now the declaration language *can* spell it. The reason for the sets is R's coercion table, not
the grammar.

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

## FIXED — strict mode now surfaces reads tolerated by an unknown attached package

The blanket tolerance was the one hole strict mode left open, and three docs pages had claimed the
opposite before saying plainly that it did not close it. With a `library(<package with no
manifest>)` in the project, a read of a name nothing defines produced **no finding at all** under
`[check] typing = true, strict = true`. That is the failure mode this project treats as worst: a
clean run that reads as "I understood everything" while an unknown `library()` silently switched a
whole class of checking off project-wide.

Closed by making the two streams share one decision instead of one of them guessing. The tolerance
was a `continue` buried in a 65-line loop in `unresolved_diagnostics`; that loop is now
`classify_non_local_read`, returning `Resolvable` / `Tolerated` / `Unresolved`. The ordinary check
reports the last, strict reports the middle — so strict reports **exactly** the reads the ordinary
check let through, and the two cannot drift. (Duplicating the tolerance rule into `strict_diagnostics`
was the obvious alternative and is the shape that caused the arithmetic-constraint bug: two places
deciding the same thing from different sources.)

Verified end to end rather than reasoned: strict on reports each tolerated read; strict **off** is
still silent, which is the whole point of the tolerance; a near miss of a name the project itself
binds was never tolerated and stays an `unresolved` finding; and the remedy the message names — a
`stubs/<pkg>.Rtypes` — actually closes it.

**This cannot be a fixture, and finding that out cost a blessed pair of them that tested nothing.**
The tolerance keys on `PackageMetadata`, a salsa input only a real project sets, so a single-file
fixture never triggers it — the two fixtures written first blessed as ordinary `unresolved` warnings
while their comments claimed to be testing strict. They were deleted; the test lives in the CLI
suite (`strict_reports_a_read_the_attached_package_tolerance_silenced`), which builds real projects,
and asserts all three behaviours above.

## FIXED (htmltools, and now mgcv too — a THIRD cause) — spinning on CPU at flat memory

**`mgcv` is closed, and it was neither of the causes below.** It was the exponential re-inference of
arithmetic operands: `R/gamlss.r` holds machine-written symbolic derivatives, one statement with 248
arithmetic operators, and each level re-walked both operand subtrees. Measured on the release binary:
before, `ry check /tmp/pkgbench/mgcv` did not finish in 180 s; after, 1.98 s and 2.01 s. The fix is in
`infer_binary`, and the finding is written up in the performance review section above. The rest of this
entry is the `htmltools` history, kept for the self-referential-record analysis it contains.



Checking `htmltools`'s package directory (7,669 lines) runs past **five minutes** at 100% CPU and
**42 MB RSS**, measured at 182 seconds. Constant memory is the distinguishing fact: it rules out the
non-converging-fixpoint shape that made `rlang` fail, and that diagnosis held — fixing the fixpoint
took `rlang` from a 213-second death to a 9-second clean run and left `htmltools` timing out
unchanged, with no cycle panic in its output. So this is an algorithm that is superlinear or
non-terminating within a bounded working set. `mgcv` (37,253 lines) behaves the same way.

### Localised and profiled; the fix is NOT where the time is spent

**One file does it.** Timing each `htmltools/R/*.R` alone: every file completes under 900 ms except
`tag_query.R` (1,563 lines), which alone exceeds 25 s. Start there, not with the package.

**The shape.** `tagQuery_` defines a local closure `newTagQuery(selected)` that returns
`structure(list(...))` whose ~40 fields are closures each calling `newTagQuery` again. So the record
type is *self-referential through its own fields*, and the type expands per level rather than being
folded. Prefix-bisection points at the line completing the first such method — but note prefix
bisection is confounded here, because a truncated prefix has a syntax error and syntax errors suppress
checking; the cliff is partly that the code became parseable.

**Where the time goes**, from a symbolised sample of the running process:
`semantics::types::substitute_rigid` recursing into `salsa::interned::…::intern`. Every instantiation
of a scheme mentioning that type walks and re-interns the whole thing.

**The time is NOT wasted work, which is the finding that matters.** Two candidate fixes in
`substitute_rigid` were implemented and measured, and both were reverted:

- returning `ty` unchanged when the substitution is empty (monomorphic instantiation);
- a memoised per-interned-type `rigid_names` set, returning any subtree whose rigids are disjoint from
  the substitution unchanged.

Neither moved `tag_query.R` at all, and an interleaved A/B on 323K lines showed **no** difference
(baseline 4.69–4.93 s, patched 4.69–4.75 s). So the substituted rigids genuinely pervade a genuinely
enormous type: the walk is doing real work on a type that should never have grown that large.

**Corrected by measurement: `substitute_rigid` WAS the hot path, and the earlier "no output" readings
were an instrumentation artifact.** A counter printing to stderr through a pipe loses its output when
`timeout` kills the process, so three separate probes read as silence and were taken as evidence the
function was cold. Redirecting stderr to a file instead showed **278 million calls in 40 seconds**.
Always redirect a probe to a file when the process under test will be killed, and always make the
probe fire once on entry so its silence can be distinguished from its absence.

**Fixed, and it is the DAG-as-tree bug.** Interning makes a type a DAG — one subtree is reached by
every path mentioning it — and a record whose ~40 fields all return that record is reached once per
field per level, so an unmemoised walk is exponential in depth. `substitute_rigid` now memoises per
node for the duration of one top-level call, which is sound because the substitution is fixed for that
call and a node's answer cannot depend on how it was reached. On the pathological file the walk drops
from 278M calls to under 2M; interleaved on both corpora it wins every round by ~2%, with identical
findings.

**The hang is still open, and the memo did not fix it** — a second bottleneck now dominates
`tag_query.R`. Re-sample the profile to find it; the sampler and the corrected probe method are the
tools to use.

### The type's growth is now measured, and it is combinatorial, not iterative

Instrumenting the captured-write join to report the **tree** size (paths, not distinct nodes) of each
type it erases, largest-so-far only:

```
877 -> 8823 -> 104655 -> 104657 -> 1046623 -> 5000000 (probe ceiling)
```

Roughly ten times per step. So the type genuinely explodes, every walk over it is a symptom, and
making individual walks cheaper cannot fix it — which matches the evidence, because each memoised
walk simply hands the hot spot to the next one.

**Six fixes have been implemented, measured, and reverted.** Recorded so none is tried a seventh time:

1. `substitute_rigid` early-out on an empty substitution — no effect.
2. `substitute_rigid` memoised disjoint-rigid skip — no effect.
3. `substitute_rigid` matching by reference instead of cloning `TyKind` — no effect.
4. `substitute_rigid` per-node memo — **kept** (278M calls to under 2M, ~2% on both corpora,
   identical findings) but does **not** fix the hang.
5. `erase_vars` as a tracked query — no effect on the hang, unmeasured on normal input, reverted.
6. Capping how many times one captured slot's join may grow — no effect, and the *reason* is the
   useful part: the cap is per slot, and each of the ~40 method slots grows only once or twice. The
   explosion is the **product across slots**, not iteration within one, so no per-slot counter can
   see it.

**FIXED, exactly there.** `type_size` is a tracked query counting a type **as a tree** — paths, not
distinct nodes — saturating at a ceiling of 100,000, and `Checker::record` gives any composite past
that ceiling `Unknown` instead. Tree size is the number that matters because a consumer walking a type
pays the tree it denotes, while sharing keeps the stored graph small; a distinct-node count is blind to
exactly the case this exists for. Only composites are measured, so scalars never pay for the ask.

Results: `tag_query.R` **>200 s → 56 ms**, the whole `htmltools` package **>5 min → 129 ms**. Findings
are byte-identical across 1,951 files of real CRAN sources (p18, MASS, ggplot2), and interleaved it is
~7% *faster* on 323k lines, winning every round — the ceiling cuts off moderately oversized types too.
`record` is the right site because every expression's inferred type passes through it; the capture-join
site, tried first, fires only nine times on that file and bounding it changed nothing.

**`mgcv` was NOT the same bug, and it is now closed** — see the head of this entry. The shared-cause
assumption in this item's title was wrong twice over: the cause turned out to be exponential
re-inference of arithmetic operands, not type growth at all.

**Remaining from the original item.** The bound has to be on the size of a constructed type itself — Widening past the bound to `Unknown` is the sound-by-refusal move the loop join already
makes for a variable whose type keeps growing structurally. Computing the size cheaply needs care: the
DAG is small while the tree is enormous, so a distinct-node count will not see the problem and a naive
tree count is itself exponential — it wants a memoised size where `size(node) = 1 + sum(size(child))`,
which is O(DAG) and yields the true tree magnitude.
That is the recursion-widening question (fold to a recursive nominal, or widen to `Unknown` past a size
bound), which is a semantics design decision rather than an optimisation, and wants a decision record.
A size bound on constructed types would also be a general safety net: nothing currently caps how large
one type may get.

**Measurement trap, learned here the hard way.** Do not A/B two binaries in separate blocks on this
machine. A non-interleaved comparison showed baseline 9.6 s against patched 4.8 s — an apparent 2×
win that was entirely load drift from a build still finishing during the baseline block. Interleaving
the two binaries run-by-run showed the true difference: zero. Any perf claim here needs interleaved
runs.

Both outliers from the same investigation are now accounted for. **`MASS` is closed** — it took 6.5 s
and now takes 0.31 s and 0.36 s, from the operand-inference fix; the R6 suspicion was wrong, it was
arithmetic chains. **`targets`** was the other, and its cause is the conditional-slot finding above,
where the remaining half is written up; R6 was a suspect there too and is likewise not the cause.

**Missing end-to-end coverage for both cycle fixes.** Neither has a fixture. The failing inputs are
whole CRAN packages, and synthetic cases built from the suspected mechanisms did not reproduce
either one — for the non-convergence, a self-growing definition, three mutual-recursion shapes and an
overloaded-call cycle all converge fine, because a single item pins and settles and the bug needs
several members' pins to interact. What is pinned instead is the structural property the fix rests on
(`refusal_is_idempotent` in `semantics.rs`), which is the part that can be tested without
reproducing the cycle. The end-to-end guard rests on the corpus suites.

## REFUTED — the formatter is NOT slower than the type checker; it is single-threaded

The premise was a measurement error, and the correction is in the performance review section above:
`ry check .` fans out over `available_parallelism()` while `ry fmt` is a plain `for` loop over files,
so the original 5.6 s versus 10.1 s compared a parallel command against a sequential one. On an
identical file set with both single-threaded, the formatter costs **half** what the type checker does
(1.01 s against 2.26 s on one core). What remains open is the actionable part — fan `fmt` out the way
`check` already does — plus an unlocalized fact worth a profile of its own: the render is ~8× the
parse (1.9 MiB/s against ~18 MiB/s).

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

## FIXED — the `rofy` crate is deleted

The user gave an explicit go for `rofy` alone, conditional on parity, and then asked for it. **Every
other crate under `legacy/` still needs its own explicit go and stays in-tree until then** — see the
note below on why this is not a precedent.

Parity was established by reading both crates rather than assuming. `rofy`'s whole surface was
multiline editing, command history with reverse search, an optional vi mode, syntax highlighting, a
hinter, and a vi-aware prompt. `crates/repl` has every one — `LexerValidator`, `FileBackedHistory`,
`reedline::Vi` behind `--keybindings vi`, `LexerHighlighter`, `DefaultHinter`, `RPrompt` — and exceeds
them with Tab completion through a `ColumnarMenu` and history *persisted to a file* where `rofy` kept
it in memory for the session. One deliberate difference: highlighting runs off ry's own lexer rather
than tree-sitter, which is the better answer — one parser, not two.

Nothing depended on it. The removal took the crate, the `rofy` and `publish-rofy` justfile recipes,
the `--exclude rofy` in the staged CI and the justfile gate, and prose in `decisions.md`,
`contributing/development.md` and `contributing/design/repl.md`. The R-`parse()` acceptance
cross-check the old entry warned about never used `rofy` — `corpus_acceptance` compares against
tree-sitter-r; `decisions.md` only said to run it locally "like `rofy`", an analogy now rewritten.

**The payoff is the gate, as predicted.** The canonical invocation is now
`cargo test --workspace --exclude zed_ry` — one exclusion, not two, and one fewer thing a future
session gets wrong. Deleting the crate also dropped `extendr-api`, `extendr-engine` and `libR-sys`
from the workspace (89 lines out of `Cargo.lock`), which removes the build-time dependency on a local
R entirely.

**This is not a precedent for the rest of `legacy/`, and the reason is measured.** `rofy` was a
predecessor of a shipped component with a 266-line surface that could be read in full. `analysis-legacy`
is different in kind: it holds **2,830 fixture cases** against the new stack's 1,192, and **the new
code does not run a single one of them** — `legacy/fixtures` is a harness-only crate and
`analysis-legacy/tests/test_fixtures.rs` drives those cases against the frozen oracle, with no
new-stack test reading those directories. Case-name overlap is 15 of 138 for ide and 1 for the whole
typecheck suite, so the corpus was reimplemented rather than ported. Name overlap understates
behavioural overlap and should not be read as 2,830 cases of missing coverage — but it does establish
that nothing has shown the new suites cover what those do.

**The inputs are now mined, which is the part that transfers.** 1,967 distinct sources live in
`crates/syntax/tests/corpus-legacy/` and run in the `syntax`, `format` and `semantics` invariant
batteries (see the testing page). Expectations deliberately did **not** come with them: the naming
suite renders binding-resolution trees and the type suites use an older notation, so bulk-blessing
would encode today's behavior as the contract. What ran was measured rather than assumed — all 2,447
extracted sources through `ry check`, **zero crashes and zero non-clean exits** — and the invariants
pass on all of them, so this arm is a regression net rather than a bug-finder today.

Two findings from the mining worth keeping. The frozen `type_syntax` suite stores **bare annotation
bodies** without the `#:` marker, because that stack parsed the type grammar standalone; 287 of its 303
cases therefore read as `expected a statement, found @` until the marker is prepended, which is a
format difference and not a parser gap. And automated *semantic* mining has a high noise floor: the
sources are fragments whose declaring context lives in the case's other files, so `@new Person` alone
reports an unknown type. Adjudicating the type suites needs per-case context, not a bulk pass.

What is left before deleting that directory is the **expectation** half: a differential triage
emitting (id, source, frozen expectation, new rendering), bucketed by shape, adjudicated per suite
against the type-system reference. Priority `naming` (513 cases, no new-stack counterpart at all),
then typecheck, type_syntax, diagnostics, ide.

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

## Post-beta (explicitly out of scope for now)

- Tags / discriminated unions via a compiler-known stdlib `match` (design in `contributing/design/open-questions.md` first).
- S3 dispatch modeling (`UseMethod`) — prerequisite for honest `print`/`summary`/`plot`.
- data.frame column-level typing; matrix dimensionality; real S4 typing.
- Traits/typeclasses (tripwire: the third constraint kind).
- CRAN stub auto-generation via R introspection, R-version-keyed corpora, stubtest validation (R-dependent). (NAMESPACE/DESCRIPTION awareness moved to Open — semantics by user ask.)

## Shipped ledger (one line each; rationale in `decisions.md`, contracts in the docs site)

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
