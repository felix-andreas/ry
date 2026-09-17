---
title: Testing
description: The fixture-testing contract and suite structure
---

This project prefers fixture tests for source-driven behavior. They are:

- easy for a human to read in diffs
- easy to extend into many cases quickly
- a good fit for verifying AI-generated changes against an explicit text contract

Fixture suites should describe the desired semantics as exhaustively as practical. Do not shape the
matrix around what the current implementation already happens to pass. If the suite is still
migrating toward the desired contract, record the missing coverage explicitly — in
`.agents/memory/backlog.md`, or as a case pinned to the wrong-but-current output with a comment
saying so — instead of treating current gaps as intentional.

Use ordinary Rust tests only when the behavior is awkward to express as a rendered fixture.

## The rewrite stack's suites

The greenfield stack (`crates/syntax`, `crates/semantics`) has its own harness,
`syntax::testing::run_fixture_suite`: a `.R.test` file holds `#==== group` / `#---- case`
sections whose source is followed by a `#++++` expectation block; `RY_BLESS=1` rewrites
expectations and `FIXTURE_FILTER=group__case` runs one case. Suites:

- `crates/syntax/tests/syntax` — golden lossless trees plus syntax errors (`debug_dump`)
- `crates/syntax/tests/tsr` — tree-sitter-r's parser corpus converted to the same format
- `crates/syntax/tests/errors` — golden Elm-style error-message rendering, organized by
  area (lexer, delimiters, control flow, functions/calls, `#:` type and directive grammar,
  recovery locality). The coverage contract: every distinct error message the lexer or
  parser can emit has at least one case here, valid-but-tricky shapes are pinned as
  `no errors`, and recovery cases assert that one broken construct reports once and leaves
  the rest of the file clean
- `crates/semantics/tests/typing` — the typing suite: each case runs the full semantic
  pipeline on one package file (shipped stubs installed) and renders every named top-level
  definition's exported scheme (`name: <T: numeric> fn(x: T) -> T`) followed by the file's
  diagnostics (`start..end severity[code] message`, byte offsets). Cases are grouped by feature,
  except `programs.R.test`, which holds whole small programs written the way a real project is:
  a case there earns its place by exercising several features at once, and a clean case with an
  empty diagnostics block is a **zero-false-positive** contract for that style of code
- `crates/semantics/tests/lowering` — the HIR each item lowers to, dumped as an indented
  tree under the item's name (`# f`, or `# <statement>` for an unnamed one), with argument
  tags rendered inline because argument matching is what a call's lowering has to get right.
  This is the parse-tree dump one layer down: the `syntax` suite pins what the parser built,
  this pins what lowering made of it. Its reason to exist is that a rewrite — the native
  pipe becoming a call, a replacement form, `local` — is otherwise tested only through
  whatever type falls out the far end, which cannot tell "lowered to the wrong shape but
  coincidentally typed the same" from "lowered correctly". Cases whose comment says
  `WRONG-BUT-CURRENT` pin a known defect together with the shape it should have, so a fix
  turns them red; the open ones are listed in `.agents/memory/backlog.md`
- `crates/semantics/tests/typing-scripts` — the same pipeline over script documents (one
  sequential top-down scope)
- `crates/semantics/tests/typing-strict` — the strict stream: the per-file typing mode and
  the `strict`-code diagnostics appended after the ordinary rendering
- `crates/semantics/tests/typing-imports` — package-metadata cases: leading `#namespace `
  lines form the case's NAMESPACE source and `#description ` lines its DESCRIPTION source,
  installed as the `PackageMetadata` input before rendering. The directive lines stay in the
  analyzed text as ordinary comments, so expectation offsets are honest. Attach facts need no
  directive: a `library(data.table)` statement in the case source activates the conditional
  stub namespace through the same scan the hosts run. This suite is new-stack only — the
  oracle had no package-metadata concept — and was deliberately absent from the retired differential
  fixture arm
- `crates/format/tests/format` — the formatter golden suite (ported from the legacy suite):
  each case's source formats to the expected block, and the runner re-formats the output to
  assert idempotence on every case; a case whose expectation is a refusal renders the
  structured `FormatError`
- `crates/format/tests/format-range` — the range-formatting golden suite: the case source
  carries the selection as `$0` markers (two for a selection, one for a bare caret, none to
  select the whole file) and the expectation lists each edit's source range as `line:column`
  then shows the document the edits produce, with every replacement between `«` and `»`. The
  runner gives a case source the trailing newline the fixture format strips — a file without
  one makes the formatter rewrite its last line, and every expectation here would carry that
  instead of the behavior it is about, so files that genuinely lack one belong in the property
  tests, which can spell the text out exactly
- `crates/ide/tests/ide` — IDE feature fixtures: the case source carries one `$0` cursor
  marker (stripped before analysis) and the expectation renders each feature's result at
  that position (hover line with its absolute range, definition target range, reference
  ranges). A target inside the **stub corpus** renders the token its range covers
  (`declared in stub source 0 at `print``) rather than a byte offset: an offset into the
  shipped corpus shifts whenever an unrelated declaration is added, which forced a re-bless
  on every corpus edit while proving nothing, and the token proves the range points where
  it should

### The retired identity differentials, and the benchmark harness

The rewrite was verified against the frozen previous implementation by a family of
differential suites (typing, scripts, strict, fuzz, the legacy-corpus sweep, the real-file
corpus arm, and a per-position IDE comparison with adjudicated divergence ledgers). That
program is **complete and retired**: the new stack no longer proves equivalence to the
oracle — its own fixture suites are the semantics contract, and improvements land without
oracle adjudication. What remains of the `differential` crate is the cross-stack
**benchmark** harness (`legacy/differential/tests/test_stats.rs`): the same corpus timed
and memory-measured through both stacks, kept until the legacy code is deleted.

### The semantics fuzz harness

Fuzzing is pipeline-wide and from each stage's first commit, not a parser-only concern.
`crates/semantics/tests/test_fuzz.rs` runs generated programs (biased toward reference cycles,
closures, annotations, and typing-mode directives), token soup, and two-file projects through
the full semantic pipeline, checking on every input: never-panic (salsa fixpoints must
converge), determinism across fresh databases, diagnostic-range geometry, and **incremental
equivalence** — output after editing a file through the salsa setter equals a fresh database on
the edited text, and editing back restores the original. `FUZZ_ITERS` scales the budgets; the
bounded default runs in `cargo test -p semantics`, and `fuzz_deep` (ignored) carries long runs.
On a panic the harness prints the generating inputs.

### The formatter fuzz harness

`crates/format/tests/test_fuzz.rs` holds the formatter's arm of the same doctrine. On every
input — valid-program seeds, byte-level seed mutations, token soup, random bytes, every fixture
case source in the repository, and real corpus files when fetched — it checks: never-panic
(`format` either succeeds or refuses with a structured error), determinism, **preservation** of
the code, and **idempotence**: whenever formatting succeeds, formatting the output again must
succeed and reproduce it byte-for-byte. The refusal path is part of the
property: a file with an R-grammar syntax error is refused, while errors raised by the `#:`
annotation grammar (marked `in_annotation` by the parser) only send the affected block down
the verbatim path.

Preservation is the one invariant that can notice the formatter *changing* code — determinism,
idempotence and "the output formats again" all hold for a formatter that silently drops a
statement or misspells a token. It compares the non-trivia token **kind and spelling** sequence of
input and output, with two allowances. Four kinds are excluded entirely — `{`, `}`, `;` and `#:` —
because the formatter may introduce or move them: it braces a single-statement body, splits a `;`
chain into lines, and re-lays-out an annotation block across markers. And a string is compared by
its content rather than its text, because the formatter picks the quote character; a *raw* string
it copies byte-for-byte, so that one keeps its full spelling.

Comparing kinds alone is not enough, and the gap is a miscompile rather than a cosmetic one: R is
case-sensitive, so a formatter emitting the wrong bytes for an identifier — what a stale or
off-by-one source range produces — preserves every kind while renaming the user's variables.
Measured against exactly that injected bug, the kind-only comparison passed all eight arms of this
harness; the spelling comparison fails six of them, and costs nothing.

The invariant battery itself is exported as
`format::check_format_invariants` (with `syntax::testing::check_parse_invariants` for the
parser and `semantics::testing::check_semantics_input` for the semantic pipeline — the
latter folds every lint under an everything-on configuration into the rendering, so the
lint layer inherits the never-panic, determinism, geometry, and incremental invariants) so
the coverage-guided targets below share the exact same contracts. The `format` and `semantics`
harnesses additionally carry a `fuzz_regressions_hold_invariants` battery pinning inputs those
targets have broken; `syntax` and `ide` have no such battery yet.

### The IDE fuzz harness

`crates/ide/tests/test_fuzz.rs` sweeps every feature over a sample of byte offsets in each input:
every token boundary plus a coarse stride, so off-boundary and mid-character positions stay
covered. At each offset nothing may panic and every range handed to the editor must lie inside the
text it indexes.

Containment alone is a weak oracle — it says a range is *somewhere* in the file, not that it is on
the right thing — so two further properties hold wherever the cursor sits on a name. **Name
identity**: every rename edit, every reference, and the definition target must cover text spelling
that same name (backtick quoting normalized, since `` `x` `` and `x` name one binding). **Round
trip**: if the cursor navigates to a declaration, asking for references *from* that declaration
must come back to the cursor's own token. Both were measured against an injected off-by-one at the
single place item-relative ranges are re-anchored to absolute offsets: the containment-only battery
passed, name identity failed on the first seed.

Completion is sampled once per completion context — the kind of token the cursor sits in or after —
rather than at every offset. It is three orders of magnitude more expensive per call than any other
feature here, it asserts only that a label is non-empty, and what it offers is decided by syntactic
context rather than by the exact byte, so a full sweep re-derives the same candidate list many times
over. Sampling by context halves the harness's wall clock and keeps the assertion.

### The mined legacy corpus

`crates/syntax/tests/corpus-legacy/*.R.corpus` holds 1,967 distinct R programs extracted from the
frozen stack's fixture suites, and `syntax::testing::legacy_corpus_sources` reads them. The `syntax`,
`format` and `semantics` batteries each run the whole set.

They are kept for their **inputs**, not their expectations. That stack's suites hold ~2,830 curated
edge cases, but their expected output cannot be ported — the naming suite renders binding-resolution
trees, and the type suites use an older notation (`fn(x: ?1) -> ?1` where the shipping crates render
`<T> fn(x: T) -> T`) — so bulk-blessing them would encode today's behavior as the contract rather than
check it. Case-name overlap with the current suites is 15 of 138 for ide and 1 across the whole
typecheck suite, so the corpus was reimplemented rather than ported, and **nothing in the shipping
crates ran a single one of those programs** until this arm existed: the harness that runs them drives
them against the frozen oracle.

The invariants need no expectations, which is what makes the inputs usable on their own: never panic,
lossless reprint and tree geometry in `syntax`, the preservation oracle in `format`, and diagnostic
ranges inside their file in `semantics`. The semantics arm shares **one** database across the whole
corpus rather than using the per-input battery — a fresh database costs a stub re-parse each — so it
asserts what a shared database can and leaves determinism and incremental equivalence to the generated
arms. Each arm asserts the corpus is non-empty, so deleting it fails loudly instead of passing green
on zero cases.

Regenerating: the corpus is derived from `legacy/analysis-legacy/tests/**/*.test` by taking each
case's source between its `#---- <id>` header and the `#++++` expectation, stripping the per-file
headers multi-file cases use, and deduplicating. It is committed rather than derived at test time so
it survives the eventual deletion of that directory.

### The annotation round-trip oracle

`crates/semantics/tests/test_roundtrip.rs` asserts that every type the checker prints is a type a
user can write back. `#: TYPE` says the annotated value is compatible with `TYPE`, and the checker
has just proved the value *has* the type it rendered — so re-declaring an inferred scheme above its
own definition must add no finding.

It is the only test that compares the renderer against the annotation grammar. Every other suite
reads a rendering as a *string*, so a type that prints beautifully and parses back as something else
is invisible to all of them. Two live bugs were found this way and are fixed: a record field name
that is not a syntactic R name rendered unquoted, and `` list{`a,b`: integer} `` read back as a
different type entirely rather than failing; and `scalar numeric`, a bound the checker renders, was
not one the grammar accepted, making every scheme carrying it unwritable.

It runs over every fixture case source, skipping any that does not already check clean (the oracle
is "re-declaring adds no finding", which needs a baseline with none) and any definition that already
carries a `#:` block (that is the user's text echoed back, not a rendering the checker chose). One
database is re-pointed with the salsa setter rather than rebuilt per probe — a fresh database costs
a re-parse of the whole stub corpus, which is 232 s against 4.5 s for the same 516 schemes.

`KNOWN_UNWRITABLE` names the schemes that do not round-trip today. They are all one open defect — a
parameter's default value is ignored when its type generalizes, so `function(x = 1) x` infers
`<T> fn([x]: T) -> T` and writing that back fails with ``expected `T`, found `double` ``. They are
listed by case id rather than skipped by shape, so a new unwritable rendering of any kind fails the
test instead of blending into a category.

### The tree-sitter acceptance differential

`crates/syntax/tests/test_corpus.rs` compares our "this file has errors" verdict against
`tree-sitter-r` as a second opinion, in both directions and over two input sets.

`corpus_acceptance` runs over the fetched `corpus/` and gates **ours-only-error** — we reject what
tree-sitter accepts, which means a grammar gap. `in_tree_acceptance` runs over inputs that are
always present (the mined legacy corpus plus every fixture case source) and gates the other
direction, **theirs-only-error**: we accept what tree-sitter rejects.

That second direction is the only thing in the project that bounds the parse-error count from
*below*. Every other parser invariant bounds it from above — the cascade guard caps how many errors
we report, and nothing asserts that a broken file reports any at all — while the parser carries a
lot of dedup and first-wins suppression, so over-suppression is the live regression risk. Measured
against an injected `push_error` that drops zero-width ranges, all four `test_fuzz` binaries stayed
green while 24 of 489 broken files went silently clean; this differential went from 2 divergences to
23. It costs about 100 ms.

Each direction is gated only where it is meaningful. Ours-only is noise on in-tree input, which is
full of `#:` annotations tree-sitter reads as comments and of deliberately broken sources;
theirs-only is gated there against
`crates/syntax/tests/in-tree-acceptance-allowlist.txt`. An allowlist entry is adjudicated by running
**R itself** and must say which of two things it is: tree-sitter being wrong, or a known gap in this
parser. There is currently one of each.

### Coverage-guided fuzzing

`fuzz/` is a cargo-fuzz crate (its own workspace, excluded from the main one) with libFuzzer
targets `parse`, `format`, and `semantics`, each a thin wrapper over the exported
invariant batteries (the `semantics` target derives its document kind and incremental edit
from the input bytes, so one byte stream drives the full battery including the splice
cache). It
needs nightly: `cargo install cargo-fuzz`, then from `fuzz/`:

```sh
cargo +nightly -Zscript ../scripts/seed-fuzz-corpus.rs   # seed corpus from all fixture case sources
cargo +nightly fuzz run format -- -max_total_time=600 -print_final_stats=1
cargo +nightly fuzz run parse  -- -max_total_time=600
```

The seeder extracts every fixture case's source across the repository into
`fuzz/corpus/{parse,format,semantics}/` (content-addressed, so re-running only adds). Corpus and
artifacts are gitignored — durable regressions belong in the `REGRESSIONS` battery or a
fixture case, not in the corpus. On a find: `cargo +nightly fuzz tmin format <artifact>`
minimizes it; fix the bug, replay the artifact, add the minimized input to `REGRESSIONS`
(plus a golden fixture case when the shape deserves a pinned rendering). A coverage report
for judging corpus quality needs the llvm-tools component:
`cargo +nightly fuzz coverage format && cargo cov -- show` per the
[cargo-fuzz coverage guide](https://rust-fuzz.github.io/book/cargo-fuzz/coverage.html).
Deep runs are manual/scheduled work; the bounded in-tree batteries remain the default-suite
gate.

### The range-formatting property battery

`format::check_range_format_invariants` sweeps a bounded, deterministic spread of selections
over one input — the whole file, bare carets, whole lines, several lines, part-line spans, and
a range past the end — and is run from `crates/format/tests/test_format_range.rs` over every
fixture case source in the repository and the mined legacy corpus, and from every generator arm
of `test_fuzz.rs`. Per selection it asserts: determinism, and refusal exactly when whole-file
formatting refuses; edits ordered, disjoint, whole lines and in bounds; that applying them keeps
every token; that the applied document still formats to what the original formats to; that every
edit is one whole-file formatting would have made; that selecting the whole file reproduces
whole-file formatting byte for byte; and that the region the edits produced is already laid out,
so a second pass over it is a no-op.

The last two are the load-bearing ones and are worth keeping that way: they are what caught a
span cut where two lines merely looked alike (an annotation block lost its closing `#: }`) and a
span that flipped a call's hug decision by splicing one formatted argument into it.

### The lint fixture suites

`crates/semantics/tests/lints/` runs `lints::lint_file` under the default configuration and
`tests/lints-style/` under `naming-style = "snake_case"` plus `unused-parameter = "warn"`
(both off by default); each case renders every finding as `start..end severity[code] message`
(or `clean`). Run with `cargo test -p semantics --test test_lint_fixtures`.

### The CLI contract suite

`crates/ry/tests/test_cli.rs` drives the real `ry` binary end to end: diagnostic
rendering (1-based character positions, snippet windows, underlines), JSON Lines output, the documented exit-code
contract (0 clean, 1 findings, 2 usage/configuration/IO errors), configuration discovery and
failure, per-file `# typing:` directives, suppression comments, NAMESPACE validation, project
stub overrides (including loader-problem reporting), data-masked evaluation, and the
formatter's `--check`/`--diff`/in-place modes.

### The LSP behavioral suite

`crates/ry/tests/test_lsp.rs` spawns the real `ry server` process and drives it over
LSP stdio with an async-lsp test client. Coverage: capability negotiation (position encodings,
pull diagnostics, refresh, label offsets, snippets), document sync (incremental and full-text,
untitled/non-`file:` buffers, close-time disk rereads), the two-wave push contract and its
settled-superset invariant, burst settling under latest-edit-wins cancellation, pull
diagnostics (result ids, unchanged reports, retryable cancellation, push suppression),
diagnostic refresh on save and config change, the config matrix (live reload, ancestor
configs, failure keeps the previous config, the config-file diagnostic), every feature
endpoint including UTF-16/UTF-8 range correctness with BMP and non-BMP content and
out-of-bounds safety, semantic tokens for `#:` bodies, and `.Rtypes`/NAMESPACE buffer serving.
Formatting is covered on both requests, because they are one code path over different ranges:
whole-document formatting replacing only the lines that change and making no edits at all on an
already formatted file, and range formatting over a statement, a bare caret, a whole-line
selection that stops at the next line's first column, a range sent end first, a statement nested
inside a function, a `# fmt: off` region inside the selection, lines already laid out, a file
that does not parse, a range past the end of the document, UTF-16 columns over non-BMP text, and
a CRLF document.

The cancelled-pull test is deterministic through a fault-injection seam: with the
`RY_TEST_DELAY_PULL_MS` environment variable set, the server announces each diagnostics pull
by creating the `RY_TEST_PULL_MARKER` file and then holds the pull (until the cancellation
token flips, bounded by the delay) before computing. The test sends its `didChange` only after the
marker appears, so the edit's flip provably lands while the pull is in flight — the retryable
`SERVER_CANCELLED` response, and the successful retry after it, can be asserted without
sleeping-and-hoping. The variables exist only for this test; production runs never set them.

### The perf and memory witnesses

`legacy/differential/tests/test_stats.rs` (ignored; needs the fetched corpus and a release
build) carries the measurement instruments — one process per stack reporting wall, phase
splits, and resident/peak memory into `target/stats-{new,legacy}.txt` — and `stats_witness`,
the CI-checkable assertion form: cold-pass wall per line, resident bytes per line, and
resolve steps per line (the resolve-memoization regression tripwire) against budgets set from
the measured gate numbers with headroom.

## Fixture format

Every suite on the shipping stack runs through one harness, `syntax::testing::run_fixture_suite`
(shared by the `syntax`, `semantics`, `format`, and `ide` crates). A `.test`
file holds groups of cases:

```text
#==== group_name
#---- case_name
<input source>
#++++
<expected rendered output>
```

Rules:

- `group__case` is the stable test identity; names must be unique across the suite — duplicates are
  rejected rather than silently shadowing one another
- what the expectation *shows* is the suite runner's contract (schemes, diagnostics, tree dumps,
  hover output, …); suite-specific behavior belongs in the runner, not in fixture syntax
- the runner only loads files with the `.test` extension, so a suite directory may hold notes
  (a `README.md` describing its rendered-output contract, say) without the runner treating them
  as cases

## Focused runs

Run one focused fixture case with `FIXTURE_FILTER` and the suite's test target, for example:

```sh
FIXTURE_FILTER=group__case cargo test -p semantics --test test_typing_fixtures -- --nocapture
FIXTURE_FILTER=group__case cargo test -p format --test test_format_fixtures -- --nocapture
FIXTURE_FILTER=group__case cargo test -p ide --test test_ide_fixtures -- --nocapture
```

A filter that names no case in any suite fails rather than passing, because one test target drives
several suites: a suite that does not hold the case skips quietly, but an id no suite holds is a
typo, and a run of zero cases would otherwise report a pass.

The default crate test command while iterating is `cargo test -p semantics` (analysis behavior);
`just gate` runs the whole battery plus clippy and a formatting check before a change lands.

Every command that is meant to cover the project spells out `--workspace --exclude zed_ry`. The
manifest sets `default-members = ["crates/ry"]` so that a bare `cargo run` starts the CLI, and the
same setting makes a bare `cargo test`, `cargo clippy` or `cargo build` select that one package —
which leaves every suite on this page except the CLI and LSP ones silently out of scope. `zed_ry` is
the exclusion because it targets wasm. Run `just gate` rather than a bare `cargo test` before
landing a change; the CI workflow is missing the selection today, which is tracked as open work.

## Blessing expectations

Set `RY_BLESS=1` to rewrite the `#++++` expectation blocks in the source `.test` files in
place from the actual runner output instead of failing on a mismatch:

```sh
RY_BLESS=1 cargo test -p semantics --test test_typing_fixtures
```

Behavior:

- only the content body of each `#++++` block is rewritten; directive lines and surrounding spacing
  are preserved, so a blessed file is byte-identical to what a human would have written and
  re-running without bless passes
- `FIXTURE_FILTER` still applies, so you can bless a single case
- blessing an already-correct suite changes no bytes

Review every blessed change before committing: bless captures whatever the runner currently
produces, so it records an intentionally wrong outcome when the implementation is wrong.
Fixtures are the desired-semantics contract, not a regression archive — accept a changed
expectation only when the behavior or wording intentionally improved.

## The frozen legacy stack's harnesses

The previous implementation stays in-tree (`legacy/analysis-legacy`, `legacy/engine-legacy`,
`legacy/ry-legacy`, with its own `legacy/fixtures` harness) as the cross-implementation oracle
the differential compares against, and as the benchmark baseline. Its suites still run —
`cargo test -p analysis-legacy` / `-p engine-legacy` / `-p ry-legacy`, and the workspace-wide
battery covers them — but the stack is frozen: do not extend its fixtures or harnesses, and never
share code between the two stacks. Its `fixtures` crate parses the same `Simple` shape plus a
`MultiFile` shape (explicit file paths and grouped workspace edits) that its engine-era suites use.
Beyond fixtures, the legacy engine carries its own differential regression net (engine output
asserted equal to a from-scratch rebuild over adversarial edit streams, per-position IDE parity
against a fresh-analysis oracle, exec-counter and memory witnesses); it documents the bar the
rewrite's own harnesses were built to meet.

## Testing guidance

- Prefer adding or tightening fixtures before writing parser-local or crate-local unit tests unless the behavior is genuinely awkward to express as a fixture.
- When adding a new phase or module, add or extend a fixture suite for that phase before relying on ad hoc unit tests.
- Use the lightest fixture change that captures the failing shape.
- Keep fixture expectation changes deliberate when output changes.
- Prefer keeping typecheck coverage in fixtures unless the behavior is too low-level or too
  mechanical to express clearly in rendered fixture output.
