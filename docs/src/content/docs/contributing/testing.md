---
title: Testing
description: The fixture-testing contract and suite structure
---

Most of ry is tested with *fixtures*: plain-text files that pair a piece of R with the output ry
should produce for it. A fixture reads well in a diff, a new case takes seconds to add, and it holds
every change, including one an AI agent wrote, to an explicit text contract.

A fixture suite should describe the *desired* semantics as exhaustively as practical, so do not
shape a suite around what the implementation already passes. While a suite is still catching up
with the contract, record the missing coverage explicitly, either in `.agents/memory/backlog.md` or
as a case pinned to the wrong-but-current output with a comment that says so. A gap you have not
recorded looks intentional to the next reader.

Reach for an ordinary Rust test only when the behavior is awkward to express as rendered output.

## Fixture format

Every suite in the shipping crates runs through one harness, `syntax::testing::run_fixture_suite`,
which `syntax`, `semantics`, `format`, and `ide` all share. A `.test` file holds groups of cases:

```text
#==== group_name
#---- case_name
<input source>
#++++
<expected rendered output>
```

- `group__case` is the case's stable identity. It must be unique across the suite, and a duplicate is
  rejected rather than allowed to shadow the other case silently.
- What the expected output shows (schemes, diagnostics, a tree dump, hover text) is up to the suite's
  runner. Behavior specific to one suite belongs in its runner, never in the fixture syntax.
- The runner loads only `.test` files, so anything else in a suite directory is ignored.

### Running one case

To run a single case, set `FIXTURE_FILTER` and pick the suite's test target:

```sh
FIXTURE_FILTER=group__case cargo test -p semantics --test test_typing_fixtures -- --nocapture
FIXTURE_FILTER=group__case cargo test -p format --test test_format_fixtures -- --nocapture
FIXTURE_FILTER=group__case cargo test -p ide --test test_ide_fixtures -- --nocapture
```

One test target can drive several suites, so a suite that does not hold the case skips it quietly.
But a filter that matches no case in *any* suite fails: that is almost certainly a typo, and a run of
zero cases would otherwise report a pass.

While iterating, `cargo test -p semantics` is the default command, since it covers analysis
behavior. Before a change lands, `just gate` runs the whole battery plus clippy and a formatting
check.

### Blessing expectations

With `RY_BLESS=1`, a mismatch does not fail. Instead, the harness rewrites the `#++++` blocks in the
`.test` files in place from the runner's actual output:

```sh
RY_BLESS=1 cargo test -p semantics --test test_typing_fixtures
```

Only the body of each `#++++` block is rewritten. Directive lines and the spacing around them are
kept, so a blessed file is byte-identical to what a person would have written, and a second run
without bless passes. `FIXTURE_FILTER` still applies, so you can bless a single case, and blessing a
suite that is already correct changes no bytes.

Review every blessed change before you commit it. Bless records whatever the runner produces, so if
the implementation is wrong, it faithfully records a wrong outcome. Fixtures are the contract for
the semantics you want, not an archive of what the code once did, so accept a changed expectation
only when the behavior or the wording improved on purpose.

## The fixture suites

- **`crates/syntax/tests/syntax`**: golden lossless trees and syntax errors, rendered by
  `debug_dump`.
- **`crates/syntax/tests/tsr`**: tree-sitter-r's parser corpus, converted to the same format.
- **`crates/syntax/tests/errors`**: golden Elm-style error messages, organized by area (the lexer,
  delimiters, control flow, functions and calls, the `#:` type and directive grammar, and recovery
  locality). Its coverage contract has three parts: every distinct error message the lexer or parser
  can emit has at least one case here, valid but tricky shapes are pinned as `no errors`, and
  recovery cases assert that one broken construct reports once and leaves the rest of the file
  clean.
- **`crates/semantics/tests/naming`**: the name-resolution suite. It records which binding every name
  in an item resolves to, rendered flat in source order. This is the only suite that tests naming
  directly. Everywhere else naming is tested through its consequences, a type that comes out right
  or a diagnostic that does or does not fire, and consequences cannot see a read that resolves to
  the wrong binding while still producing the same type.
- **`crates/semantics/tests/typing`**: the typing suite. Each case runs the full semantic pipeline on
  one package file, with the shipped stubs installed, and renders the exported scheme of every named
  top-level definition (`name: <T: numeric> fn(x: T) -> T`) followed by the file's diagnostics
  (`start..end severity[code] message`, in byte offsets). Cases are grouped by feature, with one
  exception: `programs.R.test` holds whole small programs written the way real projects are
  written. A case earns its place there by exercising several features at once, and a clean case
  with an empty diagnostics block is a promise of zero false positives for that style of code.
- **`crates/semantics/tests/lowering`**: the HIR each item lowers to, dumped as an indented tree
  under the item's name (or under `# <statement>` for an unnamed item). Argument tags are rendered
  inline, because matching arguments is what a call's lowering has to get right. Think of it as the
  parse-tree dump one layer down: the `syntax` suite pins what the parser built, and this suite pins
  what lowering made of it. It exists because rewrites, such as the native pipe becoming a call, a
  replacement form, or `local`, would otherwise be tested only through whatever type falls out at
  the far end, which cannot tell "lowered correctly" from "lowered to the wrong shape but happened to
  type the same". A case whose comment says `WRONG-BUT-CURRENT` pins a known defect next to the shape
  it should have, so fixing the defect turns the case red. `.agents/memory/backlog.md` lists the open
  ones.
- **`crates/semantics/tests/typing-scripts`**: the same pipeline over script documents, which have
  one sequential, top-down scope.
- **`crates/semantics/tests/typing-strict`**: the strict stream. After the ordinary rendering, each
  case appends the file's typing mode and its `strict` diagnostics.
- **`crates/semantics/tests/typing-imports`**: package metadata. A leading `#namespace ` line supplies
  the case's `NAMESPACE` source and a `#description ` line its `DESCRIPTION` source, and both are
  installed as the `PackageMetadata` input before rendering. The directive lines stay in the analyzed
  text as ordinary comments, so expected offsets are honest. Attaching a package needs no directive:
  a `library(data.table)` statement in the source activates the conditional stub namespace through
  the same scan the hosts run.
- **`crates/semantics/tests/lints`** and **`lints-style`**: `lints::lint_file`, first under the
  default configuration and then under `naming-style = "snake_case"`, `unused-parameter = "warn"`,
  `shadows-builtin = "warn"`, and `shadows-namespace = "warn"`, all of which are off by default. Each
  case renders every finding as `start..end severity[code] message`, or `clean`. Run both with
  `cargo test -p semantics --test test_lint_fixtures`.
- **`crates/format/tests/format`**: the formatter's golden suite. Each case's source must format to
  the expected block, and the runner formats that output again to assert idempotence on every case.
  When the expected result is a refusal, the case renders the structured `FormatError`.
- **`crates/ide/tests/ide`**: the editor features. The source carries one `$0` cursor marker, which
  is stripped before analysis, and the expectation renders each feature's answer at that position:
  the hover line with its absolute range, the definition's target range, and the reference ranges.
  A target inside the stub corpus renders the token its range covers, as in
  ``declared in stub source 0 at `print` ``, rather than a byte offset. An offset into the shipped
  corpus shifts whenever someone adds an unrelated declaration, which forced a re-bless on every
  corpus edit and proved nothing, whereas the token proves the range points at the right thing.

## Fuzzing and property tests

Fixtures check the answers we thought to write down. The harnesses in this section check properties
that must hold for *every* input, and then throw a great many inputs at them. Fuzzing covers the
whole pipeline and starts with each stage's first commit; it is not a parser-only concern.

The batteries of invariants are exported, so that the in-tree harnesses and the coverage-guided
targets share exactly the same contracts: `syntax::testing::check_parse_invariants` for the parser,
`format::check_format_invariants` for the formatter, and `semantics::testing::check_semantics_input`
for the semantic pipeline. The last one folds every lint, under a configuration that turns them all
on, into its rendering, so the lint layer inherits the same invariants. The `format` and `semantics`
harnesses also carry a `fuzz_regressions_hold_invariants` battery that pins every input the
coverage-guided targets have ever broken. `syntax` and `ide` have no such battery yet.

### The semantic pipeline

`crates/semantics/tests/test_fuzz.rs` runs generated programs, token soup, and two-file projects
through the full semantic pipeline, with the generator biased toward reference cycles, closures,
annotations, and typing-mode directives. On every input it checks four properties:

- **Nothing panics**, which among other things means every salsa fixpoint converges.
- **The output is deterministic** across fresh databases.
- **Diagnostic ranges are well formed.**
- **Incremental equals fresh.** Editing a file through the salsa setter gives the same output as a
  fresh database on the edited text, and editing it back restores the original.

`FUZZ_ITERS` scales the budgets. A bounded default runs inside `cargo test -p semantics`, the
ignored `fuzz_deep` test carries the long runs, and on a panic the harness prints the inputs that
produced it.

### The formatter

`crates/format/tests/test_fuzz.rs` feeds the formatter valid-program seeds, byte-level mutations of
those seeds, token soup, random bytes, every fixture case source in the repository, and real corpus
files once they are fetched. It checks four properties: nothing panics (the formatter either
succeeds or refuses with a structured error), the output is deterministic, the code is preserved,
and formatting is idempotent, meaning that formatting a successful output again succeeds and
reproduces it byte for byte.

The refusal path is part of the contract. A file with a syntax error in its R is refused, while an
error in a `#:` annotation, which the parser marks `in_annotation`, only sends the affected block
down the verbatim path.

**Preservation** is the one property that can catch the formatter changing your code. Determinism,
idempotence, and "the output formats again" all hold for a formatter that silently drops a
statement or misspells a token. Preservation compares the sequence of non-trivia token kinds *and
spellings* between input and output, with two allowances:

- Four kinds are ignored entirely (`{`, `}`, `;`, and `#:`), because the formatter may add or move
  them: it braces single-statement bodies, splits `;` chains onto separate lines, and re-lays out
  annotation blocks.
- A string is compared by its content rather than its text, because the formatter chooses the quote
  character. Raw strings are copied byte for byte, so they keep their full spelling.

Comparing kinds alone would not be enough, and the gap would be a miscompile, not a cosmetic slip.
R is case-sensitive, so a formatter that emits the wrong bytes for an identifier renames your
variables while preserving every kind, and a stale or off-by-one source range produces exactly
that. When that bug was injected on purpose, a kind-only comparison passed every arm of the
harness; comparing spellings fails most of them, at no extra cost.

### The editor features

`crates/ide/tests/test_fuzz.rs` sweeps every feature over a sample of byte offsets in each input:
every token boundary plus a coarse stride, so positions off token boundaries and in the middle of
multi-byte characters are covered too. At each offset nothing may panic, and every range handed to
the editor must lie inside the text it indexes.

Containment alone is a weak check, though: it says a range is somewhere in the file, not that it is
on the right thing. Wherever the cursor sits on a name, two stronger properties hold as well:

- **Name identity**: every rename edit, every reference, and the definition target cover text that
  spells the same name (with backtick quoting normalized, since `` `x` `` and `x` are one binding).
- **Round trip**: if the cursor navigates to a declaration, asking for references from that
  declaration leads back to the cursor's own token.

Both were tested against an off-by-one injected at the single place where item-relative ranges are
converted back to absolute offsets. The containment-only battery passed; name identity failed on
the first seed.

Completion is sampled once per completion context (the kind of token the cursor sits in or after)
rather than at every offset. It costs three orders of magnitude more per call than any other feature
here, the harness only asserts that each label is non-empty, and what completion offers depends on
the syntactic context rather than the exact byte, so a full sweep would compute the same list over
and over. Sampling by context halves the harness's running time and keeps the assertion.

### The mined legacy corpus

`crates/syntax/tests/corpus-legacy/*.R.corpus` holds 1,967 distinct R programs extracted from the
frozen stack's fixture suites, read through `syntax::testing::legacy_corpus_sources`. The `syntax`,
`format`, and `semantics` batteries each run the whole set.

The programs are kept for their inputs, not their expectations. The old suites hold about 2,830
curated edge cases, but their expected output cannot be ported: the naming suite renders
binding-resolution trees, and the type suites use an older notation, in which
`fn(x: ?1) -> ?1` is what the shipping crates render as `<T> fn(x: T) -> T`. Bulk-blessing them
would simply declare today's behavior correct instead of checking it. The suites were reimplemented
rather than ported (only 15 of 138 case names overlap for the IDE, and just 1 across the whole
typecheck suite), so until this corpus existed, nothing in the shipping crates ran a single one of
those programs.

What makes the inputs usable on their own is that the invariants need no expected output: nothing
panics, `syntax` checks the lossless reprint and the tree geometry, `format` checks preservation,
and `semantics` checks that every diagnostic range lies inside its file. The semantics arm shares
one database across the whole corpus instead of using the per-input battery, because a fresh
database re-parses the stubs every time. It therefore asserts only what a shared database can, and
leaves determinism and incremental equivalence to the generated inputs. Every arm also asserts that
the corpus is non-empty, so deleting it fails loudly instead of passing on zero cases.

To regenerate the corpus from `legacy/analysis-legacy/tests/**/*.test`, take each case's source
between its `#---- <id>` header and its `#++++` expectation, strip the per-file headers that
multi-file cases use, and deduplicate. The result is committed rather than derived at test time, so
it will survive the eventual deletion of that directory.

### The annotation round trip

`crates/semantics/tests/test_roundtrip.rs` asserts that every type the checker prints is a type you
could write back. A `#: TYPE` annotation says the annotated value is compatible with `TYPE`, and the
checker has just proved the value has the type it rendered, so pasting an inferred scheme above its
own definition must add no finding.

No other test compares the renderer with the annotation grammar. Every other suite reads a rendered
type as a string, so a type that prints beautifully but parses back as something else is invisible
to all of them. This test found two real bugs. A record field whose name is not a syntactic R name
was rendered unquoted, so `` list{`a,b`: integer} `` read back as a different type entirely instead
of failing. And `scalar numeric`, a bound the checker renders, was not one the grammar accepted,
which made every scheme carrying it impossible to write down.

The test runs over every fixture case source. It skips cases that do not already check clean,
because "re-declaring adds no finding" needs a baseline with no findings, and it skips definitions
that already carry a `#:` block, because those render the user's own text rather than a type the
checker chose. It re-points one database with the salsa setter instead of building a new one per
probe: a fresh database re-parses the whole stub corpus, which took 232 seconds against 4.5 seconds
for the same 516 schemes.

`KNOWN_UNWRITABLE` lists schemes that are allowed not to round-trip. It is empty, and worth keeping
that way. Its entries name case ids rather than skipping by shape, so a new unwritable rendering of
any kind fails the test instead of blending into a known category.

### Tree-sitter as a second opinion

`crates/syntax/tests/test_corpus.rs` compares our verdict on "does this file have errors?" with
`tree-sitter-r`'s, in both directions and over two sets of inputs:

- `corpus_acceptance` runs over the fetched `corpus/` and gates the direction where *we* reject what
  tree-sitter accepts, which means a gap in our grammar.
- `in_tree_acceptance` runs over inputs that are always present (the mined legacy corpus plus every
  fixture case source) and gates the other direction, where we accept what tree-sitter rejects.

That second direction is the only thing in the project that bounds the number of parse errors from
below. Every other parser invariant bounds it from above: the cascade guard caps how many errors we
report, but nothing else asserts that a broken file reports any at all. Since the parser does a lot
of deduplication and first-one-wins suppression, over-suppression is the regression most likely to
slip in. To measure this, a `push_error` that drops zero-width ranges was injected on purpose. All
four `test_fuzz` binaries stayed green while 24 of 489 broken files went silently clean, and this
differential jumped from 2 divergences to 23. It costs about 100 ms.

Each direction is gated only where it means something. Rejecting what tree-sitter accepts is noise
on in-tree input, which is full of `#:` annotations (comments, to tree-sitter) and of deliberately
broken sources. Accepting what tree-sitter rejects is gated there against
`crates/syntax/tests/in-tree-acceptance-allowlist.txt`. Each allowlist entry is adjudicated by
running R itself, and must say which of two things it is: tree-sitter being wrong, or a known gap in
this parser. There is currently one of each.

### Coverage-guided fuzzing

`fuzz/` is a cargo-fuzz crate, in its own workspace and excluded from the main one. It holds the
libFuzzer targets `parse`, `format`, and `semantics`, each a thin wrapper around the exported
invariant batteries. The `semantics` target derives its document kind and an incremental edit from
the input bytes, so a single byte stream drives the full battery, the splice cache included.

It needs nightly. Run `cargo install cargo-fuzz`, then from `fuzz/`:

```sh
cargo +nightly -Zscript ../scripts/seed-fuzz-corpus.rs   # seed from all fixture case sources
cargo +nightly fuzz run format -- -max_total_time=600 -print_final_stats=1
cargo +nightly fuzz run parse  -- -max_total_time=600
```

The seeder copies every fixture case's source in the repository into
`fuzz/corpus/{parse,format,semantics}/`. Files are content-addressed, so running it again only adds.
The corpus and the artifacts are gitignored, because a durable regression belongs in the
`REGRESSIONS` battery or in a fixture case, not in the corpus.

When the fuzzer finds something, minimize it with `cargo +nightly fuzz tmin format <artifact>`, fix
the bug, replay the artifact, and add the minimized input to `REGRESSIONS`, plus a golden fixture
case when the shape deserves a pinned rendering. To judge corpus quality, you can get a coverage
report (it needs the llvm-tools component) with
`cargo +nightly fuzz coverage format && cargo cov -- show`, as the
[cargo-fuzz coverage guide](https://rust-fuzz.github.io/book/cargo-fuzz/coverage.html) describes.
Deep runs are manual or scheduled work; the bounded in-tree batteries remain the gate in the default
suite.

## End-to-end suites

### The CLI

`crates/ry/tests/test_cli.rs` drives the real `ry` binary end to end. It covers:

- diagnostic rendering, with 1-based character positions, snippet windows, and underlines;
- JSON Lines output;
- the exit-code contract: 0 for no findings, 1 for findings, and 2 for a usage, configuration, or IO
  error;
- finding the configuration, and failing to;
- per-file `# typing:` directives and suppression comments;
- `NAMESPACE` validation, and project stub overrides including their loader-problem reports;
- data-masked evaluation;
- the formatter's `--check`, `--diff`, and in-place modes.

### The language server

`crates/ry/tests/test_lsp.rs` spawns the real `ry server` process and drives it over stdio with an
async-lsp test client. It covers:

- capability negotiation: position encodings, pull diagnostics, refresh, label offsets, and
  snippets;
- document sync, both incremental and full-text, including untitled and non-`file:` buffers and
  re-reading from disk on close;
- the two-wave push contract, its settled-superset invariant, and how a burst of edits settles under
  latest-edit-wins cancellation;
- pull diagnostics: result ids, unchanged reports, retryable cancellation, and push suppression;
- diagnostic refresh on save and on a configuration change;
- configuration: live reload, a configuration in an ancestor directory, a broken configuration
  keeping the previous one in force, and the diagnostic on the configuration file itself;
- every feature endpoint, including correct UTF-16 and UTF-8 ranges over BMP and non-BMP text, and
  safety for out-of-bounds positions;
- semantic tokens for a `#:` body, and serving `.Rtypes` and `NAMESPACE` buffers.

The cancelled-pull test is made deterministic by a fault-injection seam. When the
`RY_TEST_DELAY_PULL_MS` environment variable is set, the server announces each diagnostics pull by
creating the `RY_TEST_PULL_MARKER` file, then holds the pull (for at most the delay) until the
cancellation token flips. The test sends its `didChange` only after the marker appears, so the
cancellation provably lands while the pull is in flight, and the retryable `SERVER_CANCELLED`
response and the successful retry after it can be asserted without sleeping and hoping. These
variables exist only for this test; a production run never sets them.

## Performance and memory

`legacy/differential/tests/test_stats.rs` holds the measurement instruments. They are ignored by
default because they need the fetched corpus and a release build. Each instrument runs one process
per stack and writes wall time, a split by phase, and resident and peak memory into
`target/stats-{new,legacy}.txt`.

The same file carries `stats_witness`, the assertion form that CI can check. It asserts the
cold-pass wall time per line, resident bytes per line, and resolve steps per line (the tripwire for a
regression in resolve memoization), against budgets taken from measured numbers plus headroom.

## The legacy stack

The rewrite was verified against the frozen previous implementation by a family of differential
suites covering typing, scripts, strict mode, fuzzing, the legacy corpus, a corpus of real files,
and a per-position comparison of IDE answers, each with a ledger of adjudicated divergences. That
program is complete and retired. The new stack no longer has to prove equivalence to the old one:
its own fixture suites are the semantics contract, and an improvement lands without the old
implementation having to agree. All that remains of the `differential` crate is the cross-stack
benchmark in `legacy/differential/tests/test_stats.rs`, which runs the same corpus through both
stacks and measures time and memory. It stays until the legacy code is deleted.

The legacy stack itself (`legacy/analysis-legacy`, `legacy/engine-legacy`, and
`legacy/roughly-legacy`, with its own `legacy/fixtures` harness) still has its suites, which run
through `cargo test -p analysis-legacy`, `-p engine-legacy`, and `-p roughly-legacy` and are part of
the workspace battery. It is frozen: do not extend its fixtures or harnesses, and never share code
between the two stacks. Its `fixtures` crate reads the same simple shape as the shipping harness,
plus a multi-file shape, with explicit file paths and grouped workspace edits, that its engine-era
suites use.

Beyond fixtures, the legacy engine carries its own differential safety net: engine output must
equal a from-scratch rebuild over adversarial edit streams, IDE answers must match a fresh analysis
at every position, and counters and memory witnesses bound the work. It documents the bar the new
stack's harnesses were built to meet.

## Guidelines

- Add or tighten a fixture before writing a parser-local or crate-local unit test, unless the
  behavior is genuinely awkward to express as a fixture.
- When you add a new phase or module, add or extend a fixture suite for it before relying on ad hoc
  unit tests.
- Use the lightest fixture change that captures the failing shape.
- Change a fixture expectation deliberately, never as a side effect.
- Keep type-checking coverage in fixtures, unless the behavior is too low-level or mechanical to
  express clearly as rendered output.
