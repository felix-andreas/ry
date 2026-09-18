---
title: Testing
description: The fixture-testing contract and suite structure
---

This project prefers a fixture test for source-driven behavior. A fixture is
easy for a human to read in a diff, it is easy to extend into many cases
quickly, and it verifies an AI-generated change against an explicit text
contract.

A fixture suite should describe the desired semantics as exhaustively as
practical. Do not shape the matrix around what the current implementation
already passes. If a suite is still moving toward the desired contract, record
the missing coverage explicitly, either in `.agents/memory/backlog.md` or as a
case pinned to the wrong-but-current output with a comment saying so. Do not
treat a current gap as intentional.

Use an ordinary Rust test only when the behavior is awkward to express as a
rendered fixture.

## The suites

The shipping crates share one harness,
`syntax::testing::run_fixture_suite`. A `.R.test` file holds `#==== group` and
`#---- case` sections. Each case's source is followed by a `#++++`
expectation block. `RY_BLESS=1` rewrites the expectations, and
`FIXTURE_FILTER=group__case` runs one case.

- `crates/syntax/tests/syntax` holds golden lossless trees and syntax errors,
  rendered by `debug_dump`.
- `crates/syntax/tests/tsr` holds tree-sitter-r's parser corpus, converted to
  the same format.
- `crates/syntax/tests/errors` holds golden Elm-style error messages,
  organized by area: the lexer, delimiters, control flow, functions and calls,
  the `#:` type and directive grammar, and recovery locality. The coverage
  contract has three parts. Every distinct error message the lexer or the
  parser can emit has at least one case here. A valid but tricky shape is
  pinned as `no errors`. A recovery case asserts that one broken construct
  reports once and leaves the rest of the file clean.
- `crates/semantics/tests/naming` holds the name-resolution suite. It records
  which binding every name in an item resolves to, rendered flat and
  source-ordered. This is the only suite that tests naming directly.
  Everywhere else naming is tested through its consequences, which are a type
  that comes out right and a diagnostic that does or does not fire. Those
  consequences cannot see a read that resolves to the wrong binding while
  still producing the same type.
- `crates/semantics/tests/typing` holds the typing suite. Each case runs the
  full semantic pipeline on one package file, with the shipped stubs
  installed. It renders every named top-level definition's exported scheme, as
  in `name: <T: numeric> fn(x: T) -> T`, followed by the file's diagnostics,
  as in `start..end severity[code] message`, with byte offsets. Cases are
  grouped by feature, except `programs.R.test`. That file holds whole small
  programs written the way a real project is written. A case there earns its
  place by exercising several features at once, and a clean case with an empty
  diagnostics block is a zero-false-positive contract for that style of code.
- `crates/semantics/tests/lowering` holds the HIR each item lowers to, dumped
  as an indented tree under the item's name. An unnamed item is dumped under
  `# <statement>`. Argument tags are rendered inline, because argument
  matching is what a call's lowering has to get right. This is the parse-tree
  dump one layer down. The `syntax` suite pins what the parser built, and this
  one pins what lowering made of it. It exists because a rewrite, such as the
  native pipe becoming a call, a replacement form, or `local`, is otherwise
  tested only through whatever type falls out the far end. That cannot tell
  "lowered to the wrong shape but coincidentally typed the same" from "lowered
  correctly". A case whose comment says `WRONG-BUT-CURRENT` pins a known
  defect together with the shape it should have, so a fix turns the case red.
  `.agents/memory/backlog.md` lists the open ones.
- `crates/semantics/tests/typing-scripts` runs the same pipeline over script
  documents, which have one sequential top-down scope.
- `crates/semantics/tests/typing-strict` holds the strict stream. It appends
  the per-file typing mode and the `strict`-code diagnostics after the
  ordinary rendering.
- `crates/semantics/tests/typing-imports` holds the package-metadata cases. A
  leading `#namespace ` line forms the case's NAMESPACE source, and a
  `#description ` line forms its DESCRIPTION source. Both are installed as the
  `PackageMetadata` input before rendering. The directive lines stay in the
  analyzed text as ordinary comments, so the expectation offsets are honest.
  An attach fact needs no directive. A `library(data.table)` statement in the
  case source activates the conditional stub namespace through the same scan
  the hosts run.
- `crates/format/tests/format` holds the formatter's golden suite, ported from
  the legacy suite. Each case's source formats to the expected block, and the
  runner re-formats the output to assert idempotence on every case. A case
  whose expectation is a refusal renders the structured `FormatError`.
- `crates/ide/tests/ide` holds the IDE feature fixtures. The case source
  carries one `$0` cursor marker, stripped before analysis, and the
  expectation renders each feature's result at that position: the hover line
  with its absolute range, the definition target range, and the reference
  ranges. A target inside the stub corpus renders the token its range covers,
  as in ``declared in stub source 0 at `print` ``, rather than a byte offset.
  An offset into the shipped corpus shifts whenever an unrelated declaration
  is added, which forced a re-bless on every corpus edit while proving
  nothing. The token proves the range points where it should.

### The retired identity differentials, and the benchmark harness

The rewrite was verified against the frozen previous implementation by a
family of differential suites. They covered typing, scripts, strict mode,
fuzzing, the legacy-corpus sweep, the real-file corpus, and a per-position IDE
comparison with adjudicated divergence ledgers. That program is complete and
retired. The new stack no longer proves equivalence to the oracle. Its own
fixture suites are the semantics contract, and an improvement lands without
the oracle adjudicating it. What remains of the `differential` crate is the
cross-stack benchmark harness in `legacy/differential/tests/test_stats.rs`. It
times the same corpus through both stacks and measures its memory, and it is
kept until the legacy code is deleted.

### The semantics fuzz harness

Fuzzing is pipeline-wide, and it starts at each stage's first commit rather
than being a parser-only concern. `crates/semantics/tests/test_fuzz.rs` runs
generated programs, token soup, and two-file projects through the full
semantic pipeline. The generator is biased toward reference cycles, closures,
annotations, and typing-mode directives. On every input the harness checks
four properties. Nothing panics, which means every salsa fixpoint must
converge. The output is deterministic across fresh databases. The diagnostic
ranges have valid geometry. The analysis is incrementally equivalent, meaning
that the output after editing a file through the salsa setter equals a fresh
database on the edited text, and that editing back restores the original.
`FUZZ_ITERS` scales the budgets. The bounded default runs inside
`cargo test -p semantics`, and the ignored `fuzz_deep` test carries the long
runs. On a panic the harness prints the generating inputs.

### The formatter fuzz harness

`crates/format/tests/test_fuzz.rs` holds the formatter's arm of the same
doctrine. Its inputs are valid-program seeds, byte-level seed mutations, token
soup, random bytes, every fixture case source in the repository, and real
corpus files once they are fetched. On every input it checks four properties.
Nothing panics, because `format` either succeeds or refuses with a structured
error. The output is deterministic. The code is preserved. Formatting is
idempotent, so whenever formatting succeeds, formatting the output again must
succeed and reproduce it byte for byte. The refusal path is part of the
property. A file with an R-grammar syntax error is refused, while an error
raised by the `#:` annotation grammar, which the parser marks `in_annotation`,
only sends the affected block down the verbatim path.

Preservation is the one invariant that can notice the formatter changing code.
Determinism, idempotence, and "the output formats again" all hold for a
formatter that silently drops a statement or misspells a token. Preservation
compares the sequence of non-trivia token kinds and spellings between the
input and the output, with two allowances. Four kinds are excluded entirely,
which are `{`, `}`, `;`, and `#:`, because the formatter may introduce or move
them. It braces a single-statement body, it splits a `;` chain into lines, and
it re-lays out an annotation block across markers. A string is compared by its
content rather than by its text, because the formatter picks the quote
character. A raw string is copied byte for byte, so that one keeps its full
spelling.

Comparing kinds alone is not enough, and the gap is a miscompile rather than a
cosmetic problem. R is case-sensitive, so a formatter that emits the wrong
bytes for an identifier renames the user's variables while preserving every
kind. A stale or off-by-one source range produces exactly that. Measured
against that bug, injected deliberately, the kind-only comparison passed every
arm of this harness. The spelling comparison fails most of them, and it costs
nothing.

The invariant battery itself is exported as `format::check_format_invariants`.
The parser's battery is `syntax::testing::check_parse_invariants`, and the
semantic pipeline's is `semantics::testing::check_semantics_input`. The last
one folds every lint, under an everything-on configuration, into the
rendering, so the lint layer inherits the never-panic, determinism, geometry,
and incremental invariants. The coverage-guided targets below share these
exact contracts. The `format` and `semantics` harnesses additionally carry a
`fuzz_regressions_hold_invariants` battery, which pins the inputs those
targets have broken. `syntax` and `ide` have no such battery yet.

### The IDE fuzz harness

`crates/ide/tests/test_fuzz.rs` sweeps every feature over a sample of byte
offsets in each input. The sample is every token boundary plus a coarse
stride, so an off-boundary position and a mid-character position stay covered.
At each offset nothing may panic, and every range handed to the editor must
lie inside the text it indexes.

Containment alone is a weak oracle. It says a range is somewhere in the file,
not that it is on the right thing. Two further properties therefore hold
wherever the cursor sits on a name. Name identity requires that every rename
edit, every reference, and the definition target cover text spelling that same
name, with backtick quoting normalized, since `` `x` `` and `x` name one
binding. Round trip requires that if the cursor navigates to a declaration,
asking for references from that declaration comes back to the cursor's own
token. Both were measured against an off-by-one injected at the single place
where item-relative ranges are re-anchored to absolute offsets. The
containment-only battery passed, and name identity failed on the first seed.

Completion is sampled once per completion context, which is the kind of token
the cursor sits in or after, rather than at every offset. It is three orders
of magnitude more expensive per call than any other feature here, it asserts
only that a label is non-empty, and what it offers is decided by syntactic
context rather than by the exact byte. A full sweep would re-derive the same
candidate list many times over. Sampling by context halves the harness's wall
clock and keeps the assertion.

### The mined legacy corpus

`crates/syntax/tests/corpus-legacy/*.R.corpus` holds 1,967 distinct R programs
extracted from the frozen stack's fixture suites, and
`syntax::testing::legacy_corpus_sources` reads them. The `syntax`, `format`,
and `semantics` batteries each run the whole set.

They are kept for their inputs, not for their expectations. That stack's
suites hold about 2,830 curated edge cases, but their expected output cannot
be ported. The naming suite renders binding-resolution trees, and the type
suites use an older notation, where `fn(x: ?1) -> ?1` is what the shipping
crates render as `<T> fn(x: T) -> T`. Bulk-blessing them would encode today's
behavior as the contract rather than check it. Case-name overlap with the
current suites is 15 of 138 for ide, and 1 across the whole typecheck suite,
so the corpus was reimplemented rather than ported. Until this arm existed,
nothing in the shipping crates ran a single one of those programs. The harness
that runs them drives them against the frozen oracle.

The invariants need no expectation, which is what makes the inputs usable on
their own. Nothing panics. `syntax` checks the lossless reprint and the tree
geometry, `format` checks the preservation oracle, and `semantics` checks that
a diagnostic range lies inside its file. The semantics arm shares one database
across the whole corpus rather than using the per-input battery, because a
fresh database costs a stub re-parse each time. It therefore asserts what a
shared database can, and leaves determinism and incremental equivalence to the
generated arms. Each arm asserts that the corpus is non-empty, so deleting it
fails loudly instead of passing green on zero cases.

The corpus is regenerated from `legacy/analysis-legacy/tests/**/*.test`. Take
each case's source between its `#---- <id>` header and its `#++++`
expectation, strip the per-file headers that a multi-file case uses, and
deduplicate. The result is committed rather than derived at test time, so it
survives the eventual deletion of that directory.

### The annotation round-trip oracle

`crates/semantics/tests/test_roundtrip.rs` asserts that every type the checker
prints is a type a user can write back. A `#: TYPE` annotation says the
annotated value is compatible with `TYPE`, and the checker has just proved the
value has the type it rendered. Re-declaring an inferred scheme above its own
definition must therefore add no finding.

This is the only test that compares the renderer against the annotation
grammar. Every other suite reads a rendering as a string, so a type that
prints beautifully and parses back as something else is invisible to all of
them. Two live bugs were found this way and fixed. A record field name that is
not a syntactic R name rendered unquoted, so `` list{`a,b`: integer} `` read
back as a different type entirely rather than failing. And `scalar numeric`, a
bound the checker renders, was not one the grammar accepted, which made every
scheme carrying it unwritable.

The test runs over every fixture case source. It skips a case that does not
already check clean, because the oracle is "re-declaring adds no finding",
which needs a baseline with none. It also skips a definition that already
carries a `#:` block, because that is the user's text echoed back rather than
a rendering the checker chose. One database is re-pointed with the salsa
setter rather than rebuilt per probe. A fresh database costs a re-parse of the
whole stub corpus, which is 232 seconds against 4.5 seconds for the same 516
schemes.

`KNOWN_UNWRITABLE` is the escape list for a scheme that does not round-trip.
It is empty, and worth keeping empty. An entry names a case id rather than
skipping by shape, so a new unwritable rendering of any kind fails the test
instead of blending into a category.

### The tree-sitter acceptance differential

`crates/syntax/tests/test_corpus.rs` compares our "this file has errors"
verdict against `tree-sitter-r` as a second opinion, in both directions and
over two input sets.

`corpus_acceptance` runs over the fetched `corpus/` and gates the ours-only
direction, where we reject what tree-sitter accepts. That means a grammar gap.
`in_tree_acceptance` runs over inputs that are always present, which are the
mined legacy corpus plus every fixture case source, and it gates the other
direction, where we accept what tree-sitter rejects.

That second direction is the only thing in the project that bounds the
parse-error count from below. Every other parser invariant bounds it from
above. The cascade guard caps how many errors we report, and nothing asserts
that a broken file reports any at all, while the parser carries a lot of
deduplication and first-wins suppression. Over-suppression is therefore the
live regression risk. This was measured against a deliberately injected
`push_error` that drops a zero-width range. All four `test_fuzz` binaries
stayed green while 24 of 489 broken files went silently clean, and this
differential went from 2 divergences to 23. It costs about 100 ms.

Each direction is gated only where it is meaningful. The ours-only direction
is noise on in-tree input, which is full of `#:` annotations that tree-sitter
reads as comments, and full of deliberately broken sources. The theirs-only
direction is gated there against
`crates/syntax/tests/in-tree-acceptance-allowlist.txt`. An allowlist entry is
adjudicated by running R itself, and it must say which of two things it is:
tree-sitter being wrong, or a known gap in this parser. There is currently one
of each.

### Coverage-guided fuzzing

`fuzz/` is a cargo-fuzz crate, in its own workspace and excluded from the main
one. It holds the libFuzzer targets `parse`, `format`, and `semantics`, each a
thin wrapper over the exported invariant batteries. The `semantics` target
derives its document kind and its incremental edit from the input bytes, so
one byte stream drives the full battery, including the splice cache. It needs
nightly. Run `cargo install cargo-fuzz`, then from `fuzz/`:

```sh
cargo +nightly -Zscript ../scripts/seed-fuzz-corpus.rs   # seed from all fixture case sources
cargo +nightly fuzz run format -- -max_total_time=600 -print_final_stats=1
cargo +nightly fuzz run parse  -- -max_total_time=600
```

The seeder extracts every fixture case's source across the repository into
`fuzz/corpus/{parse,format,semantics}/`. The files are content-addressed, so
re-running only adds. The corpus and the artifacts are gitignored, because a
durable regression belongs in the `REGRESSIONS` battery or in a fixture case,
not in the corpus. On a find, run `cargo +nightly fuzz tmin format <artifact>`
to minimize it. Fix the bug, replay the artifact, and add the minimized input
to `REGRESSIONS`, plus a golden fixture case when the shape deserves a pinned
rendering. A coverage report for judging corpus quality needs the llvm-tools
component. Run `cargo +nightly fuzz coverage format && cargo cov -- show`, as
the [cargo-fuzz coverage guide](https://rust-fuzz.github.io/book/cargo-fuzz/coverage.html)
describes. A deep run is manual or scheduled work, and the bounded in-tree
batteries remain the default-suite gate.

### The lint fixture suites

`crates/semantics/tests/lints/` runs `lints::lint_file` under the default
configuration. `crates/semantics/tests/lints-style/` runs it under
`naming-style = "snake_case"`, `unused-parameter = "warn"`,
`shadows-builtin = "warn"`, and `shadows-namespace = "warn"`, all of which are
off by default. Each case renders every finding as
`start..end severity[code] message`, or as `clean`. Run both with
`cargo test -p semantics --test test_lint_fixtures`.

### The CLI contract suite

`crates/ry/tests/test_cli.rs` drives the real `ry` binary end to end. It
covers diagnostic rendering, with 1-based character positions, snippet
windows, and underlines. It covers JSON Lines output and the documented
exit-code contract, where 0 means no findings, 1 means findings, and 2 means a
usage, configuration, or IO error. It covers configuration discovery and
failure, a per-file `# typing:` directive, a suppression comment, NAMESPACE
validation, a project stub override including its loader-problem reporting,
data-masked evaluation, and the formatter's `--check` and `--diff` and
in-place modes.

### The LSP behavioral suite

`crates/ry/tests/test_lsp.rs` spawns the real `ry server` process and drives
it over LSP stdio with an async-lsp test client. It covers capability
negotiation, which includes position encodings, pull diagnostics, refresh,
label offsets, and snippets. It covers document sync, both incremental and
full-text, an untitled or non-`file:` buffer, and a close-time disk reread. It
covers the two-wave push contract and its settled-superset invariant, and
burst settling under latest-edit-wins cancellation. It covers pull
diagnostics, which includes result ids, unchanged reports, retryable
cancellation, and push suppression. It covers diagnostic refresh on save and
on a configuration change, and the configuration matrix, which includes live
reload, an ancestor configuration, a failure keeping the previous
configuration, and the configuration-file diagnostic. It covers every feature
endpoint, including UTF-16 and UTF-8 range correctness with BMP and non-BMP
content and out-of-bounds safety, semantic tokens for a `#:` body, and serving
an `.Rtypes` or NAMESPACE buffer.

The cancelled-pull test is deterministic through a fault-injection seam. With
the `RY_TEST_DELAY_PULL_MS` environment variable set, the server announces
each diagnostics pull by creating the `RY_TEST_PULL_MARKER` file, then holds
the pull until the cancellation token flips, bounded by the delay, before
computing. The test sends its `didChange` only after the marker appears, so
the edit's flip provably lands while the pull is in flight. The retryable
`SERVER_CANCELLED` response, and the successful retry after it, can therefore
be asserted without sleeping and hoping. These variables exist only for this
test, and a production run never sets them.

### The performance and memory witnesses

`legacy/differential/tests/test_stats.rs` is ignored by default, because it
needs the fetched corpus and a release build. It carries the measurement
instruments, which run one process per stack and report wall time, phase
splits, and resident and peak memory into `target/stats-{new,legacy}.txt`. It
also carries `stats_witness`, the CI-checkable assertion form. That asserts
cold-pass wall time per line, resident bytes per line, and resolve steps per
line, which is the tripwire for a resolve-memoization regression. The budgets
come from the measured gate numbers, with headroom.

## Fixture format

Every suite on the shipping stack runs through one harness,
`syntax::testing::run_fixture_suite`, which the `syntax`, `semantics`,
`format`, and `ide` crates share. A `.test` file holds groups of cases:

```text
#==== group_name
#---- case_name
<input source>
#++++
<expected rendered output>
```

The rules are these.

- `group__case` is the stable test identity. A name must be unique across the
  suite, and a duplicate is rejected rather than silently shadowing another
  case.
- What an expectation shows is the suite runner's contract, whether that is
  schemes, diagnostics, a tree dump, or hover output. Suite-specific behavior
  belongs in the runner, not in the fixture syntax.
- The runner loads only `.test` files, so anything else in a suite directory
  is ignored.

## Focused runs

Run one focused fixture case with `FIXTURE_FILTER` and the suite's test
target. For example:

```sh
FIXTURE_FILTER=group__case cargo test -p semantics --test test_typing_fixtures -- --nocapture
FIXTURE_FILTER=group__case cargo test -p format --test test_format_fixtures -- --nocapture
FIXTURE_FILTER=group__case cargo test -p ide --test test_ide_fixtures -- --nocapture
```

A filter that names no case in any suite fails rather than passing. One test
target drives several suites, so a suite that does not hold the case skips
quietly. An id that no suite holds is a typo, and a run of zero cases would
otherwise report a pass.

The default crate test command while iterating is `cargo test -p semantics`,
which covers analysis behavior. `just gate` runs the whole battery, plus
clippy and a formatting check, before a change lands.

## Blessing expectations

Set `RY_BLESS=1` to rewrite the `#++++` expectation blocks in the source
`.test` files in place, from the actual runner output, instead of failing on a
mismatch:

```sh
RY_BLESS=1 cargo test -p semantics --test test_typing_fixtures
```

Blessing behaves as follows.

- Only the content body of each `#++++` block is rewritten. Directive lines
  and the surrounding spacing are preserved, so a blessed file is
  byte-identical to what a human would have written, and re-running without
  bless passes.
- `FIXTURE_FILTER` still applies, so you can bless a single case.
- Blessing an already-correct suite changes no bytes.

Review every blessed change before you commit it. Bless captures whatever the
runner currently produces, so it records an intentionally wrong outcome when
the implementation is wrong. Fixtures are the desired-semantics contract, not
a regression archive. Accept a changed expectation only when the behavior or
the wording improved on purpose.

## The frozen legacy stack's harnesses

The previous implementation stays in the tree as the cross-implementation
oracle the differential compared against, and as the benchmark baseline. It is
`legacy/analysis-legacy`, `legacy/engine-legacy`, and
`legacy/roughly-legacy`, with its own `legacy/fixtures` harness. Its suites
still run, through `cargo test -p analysis-legacy`, `-p engine-legacy`, and
`-p roughly-legacy`, and the workspace-wide battery covers them. The stack is
frozen. Do not extend its fixtures or its harnesses, and never share code
between the two stacks. Its `fixtures` crate parses the same `Simple` shape as
the shipping harness, plus a `MultiFile` shape, which carries explicit file
paths and grouped workspace edits, that its engine-era suites use.

Beyond fixtures, the legacy engine carries its own differential regression
net. It asserts that engine output equals a from-scratch rebuild over
adversarial edit streams, it checks per-position IDE parity against a
fresh-analysis oracle, and it carries exec-counter and memory witnesses. That
net documents the bar the rewrite's own harnesses were built to meet.

## Testing guidance

- Add or tighten a fixture before you write a parser-local or crate-local unit
  test, unless the behavior is genuinely awkward to express as a fixture.
- When you add a new phase or module, add or extend a fixture suite for that
  phase before you rely on an ad hoc unit test.
- Use the lightest fixture change that captures the failing shape.
- Keep a fixture expectation change deliberate when the output changes.
- Keep typecheck coverage in fixtures, unless the behavior is too low-level or
  too mechanical to express clearly in rendered fixture output.
