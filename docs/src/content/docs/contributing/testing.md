---
title: Testing
description: The fixture-testing contract and suite structure
---

Most of ry is tested with *fixtures*: plain-text files that pair a piece of R with the output ry
should produce for it. A fixture reads well in a diff, and a new case takes seconds to add. Reach
for an ordinary Rust test only when the behavior is awkward to express as rendered output.

A fixture suite describes the *desired* semantics, not what the implementation happens to pass.
Record a known gap explicitly, in `.agents/memory/backlog.md` or as a case pinned to the
wrong-but-current output with a comment that says so, because an unrecorded gap looks intentional to
the next reader.

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
- A typing or IDE case may hold several files. Each `#~~~~ <path>` line starts one (see
  `syntax::testing::split_files`), and a path under `R/` is a package file while anything else is a
  script. A typing case renders each file's output under an `== <path>` line.
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
behavior. `just gate` runs every test plus clippy and a formatting check.

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

Review every blessed change before you commit it. Bless records whatever the runner produces, wrong
outcomes included, so accept a changed expectation only when the behavior or the wording improved
on purpose.

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
  one package file, or on several, with the shipped stubs installed. It renders the exported scheme
  of every top-level definition (`name: <T: numeric> fn(x: T) -> T`), the type of every other
  top-level statement as a bare line, and then the file's diagnostics
  (`start..end severity[code] message`, in byte offsets). Each test thread reuses one database across
  cases (`semantics::testing::with_fixture_project`), because parsing the stub corpus costs far more
  than checking a case. Cases are grouped by feature, with one exception: `programs.R.test` holds whole small programs
  written the way real projects are written. A case earns its place there by exercising several features at once, and a clean case
  with an empty diagnostics block is a promise of zero false positives for that style of code.
- **`crates/semantics/tests/lowering`**: the HIR each item lowers to, dumped as an indented tree
  under the item's name (or under `# <statement>` for an unnamed item). Argument tags are rendered
  inline, because matching arguments is what a call's lowering has to get right. The suite exists
  because rewrites, such as the native pipe becoming a call, a replacement form, or `local`, would
  otherwise be tested only through the type that falls out at the far end, which cannot tell
  "lowered correctly" from "lowered to the wrong shape but typed the same". A case whose comment
  says `WRONG-BUT-CURRENT` pins a known defect next to the shape it should have, so fixing the
  defect turns the case red.
- **`crates/semantics/tests/typing-scripts`**: the same pipeline over script documents, which have
  one sequential, top-down scope. Besides script semantics, this suite holds the rules for single
  expressions (literals, operators, indexing, lists, and calls to stubbed functions), rendered one
  line per statement.
- **`crates/semantics/tests/annotations`**: one `#:` block per case, where the case's lines are the
  block. It renders what the block lowers to (the kind of coercion, the declared type, the `@new`
  target, and each `@type` or `@alias` definition), followed by its grammar and lowering errors. This
  is the compact contract for the type notation, while the `syntax` suite pins the trees and the
  `errors` suite the wording.
- **`crates/semantics/tests/stubs`**: `.Rtypes` declarations. Each case renders the declarations it
  loads, with overload candidates in order, and the problems that make the loader drop a line, in
  the wording the editor and the override report show.
- **`crates/semantics/tests/typing-inline-off`**: the ordinary rendering with
  `[check] inline-annotations = false`, which pins the `ry-syntax` warning on records, tuples, tags
  and `match`, and shows that literals are still typed while `match` coverage is not checked.
- **`crates/semantics/tests/typing-strict`**: the strict stream. After the ordinary rendering, each
  case appends the file's typing mode and its `strict` diagnostics.
- **`crates/semantics/tests/top-level`**: what a package file's top level runs under strict mode.
  A case lists files with `#~~~~` separators, so `R/` files are library files and `tests/` or root
  files are entries. Each case loads the shipped stubs and renders, per file, every top-level item
  as `definition`, `constant` or `effect`, with ` (pure)` when the item declares its function pure,
  followed by the file's top-level `strict` diagnostics. Run it with
  `cargo test -p semantics --test test_typing_fixtures top_level_fixtures`.
- **`crates/semantics/tests/typing-imports`**: package metadata. A leading `#namespace ` line supplies
  the case's `NAMESPACE` source and a `#description ` line its `DESCRIPTION` source, and both are
  installed as the `PackageMetadata` input before rendering. The directive lines stay in the analyzed
  text as ordinary comments, so expected offsets are honest. Attaching a package needs no directive:
  a `library(data.table)` statement in the source activates the conditional stub namespace through
  the same scan the hosts run.
- **`crates/semantics/tests/lints`**, **`lints-style`** and **`lints-object-systems-off`**:
  `lints::lint_file`, first under the default configuration, then under `naming-style = "snake_case"`,
  `unused-parameter = "warn"`, `shadows-builtin = "warn"`, and `shadows-namespace = "warn"`, all of
  which are off by default, and last under `[check] object-systems = false`. Each case renders every
  finding as `start..end severity[code] message`, or `clean`. Run all three with
  `cargo test -p semantics --test test_lint_fixtures`.
- **`crates/semantics/tests/load_index`**: the package-load index. A case lists `R/` files, and
  optionally `NAMESPACE` and `DESCRIPTION`, with `#~~~~` separators, and renders the package's name,
  the `NAMESPACE` directives a load honors, and, per file, each item's position and whether it is
  bound lazily (as a function or, in a strict file, a constant) or runs eagerly, with what it writes,
  what it reads while running, and the registrations it makes. Run it with `cargo test -p semantics --test test_load_index_fixtures`.
- **`crates/loader/tests/reload`**: reload plans. A file headed `#~~~~ before <path>` or
  `#~~~~ after <path>` exists on that side only, so the same path on both sides is an edit and one
  side alone is an addition or a deletion, while a plain `#~~~~ <path>` is unchanged. The case loads
  the `before` side, then renders the plan that takes it to the `after` side: what is bound, run,
  removed, unregistered, and re-registered. Run it with
  `cargo test -p loader --test test_reload_fixtures`.
- **`crates/format/tests/format`**: the formatter's golden suite. Each case's source must format to
  the expected block, and the runner formats that output again to assert idempotence on every case.
  When the expected result is a refusal, the case renders the structured `FormatError`.
- **`crates/ide/tests/ide`**: the editor features. The source carries one `$0` cursor marker, which
  is stripped before analysis, and the expectation renders each feature's answer at that position:
  the hover line with its absolute range, the definition's target range, and the reference ranges.
  A target inside the stub corpus renders the token its range covers, as in
  ``declared in stub source 0 at `print` ``, rather than a byte offset, because an offset shifts
  whenever an unrelated declaration is added, while the token proves the range is right.

## Fuzzing and property tests

Fixtures check the answers someone wrote down. The harnesses in this section check properties that
must hold for *every* input, over a great many inputs. Every pipeline stage gets fuzz coverage from
its first commit.

The batteries of invariants are exported, so that the in-tree harnesses and the coverage-guided
targets share exactly the same contracts: `syntax::testing::check_parse_invariants` for the parser,
`format::check_format_invariants` for the formatter, and `semantics::testing::check_semantics_input`
for the semantic pipeline. The last one folds every lint, under a configuration that turns them all
on, into its rendering, so the lint layer inherits the same invariants. The `format` and `semantics`
harnesses, and those of `syntax` and `ide`, also carry a `fuzz_regressions_hold_invariants` battery
that pins every input the coverage-guided targets have ever broken.

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

Comparing kinds alone would miss a miscompile: a stale or off-by-one source range makes the
formatter emit the wrong bytes for an identifier, which renames a variable while preserving every
kind. An injected bug of exactly that shape passed a kind-only comparison and fails the spelling
comparison.

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

An off-by-one injected where item-relative ranges convert back to absolute offsets passes the
containment check and fails name identity on the first seed.

Completion is sampled once per completion context (the kind of token the cursor sits in or after)
rather than at every offset, because it costs far more per call than any other feature and its
answer depends on the syntactic context rather than the exact byte.

### The program corpus

`crates/syntax/tests/programs/*.R.corpus` holds 1,967 distinct R programs, read through
`syntax::testing::program_corpus_sources`. The `syntax`, `format`, and `semantics` batteries each
run the whole set. The corpus holds inputs only, with no expected output.

The inputs are usable on their own because the invariants need no expected output: nothing
panics, `syntax` checks the lossless reprint and the tree geometry, `format` checks preservation,
and `semantics` checks that every diagnostic range lies inside its file. The semantics arm shares
one database across the whole corpus instead of using the per-input battery, because a fresh
database re-parses the stubs every time. It therefore asserts only what a shared database can, and
leaves determinism and incremental equivalence to the generated inputs. Every arm also asserts that
the corpus is non-empty, so deleting it fails loudly instead of passing on zero cases.

### The annotation round trip

`crates/semantics/tests/test_roundtrip.rs` asserts that every type the checker prints is a type you
could write back. A `#: TYPE` annotation says the annotated value is compatible with `TYPE`, and the
checker has just proved the value has the type it rendered, so pasting an inferred scheme above its
own definition must add no finding.

No other test compares the renderer with the annotation grammar: every other suite reads a
rendered type as a string, so a type that prints well but parses back as something else is
invisible to them.

The test runs over every fixture case source. It skips cases that do not already check clean,
because "re-declaring adds no finding" needs a baseline with no findings, and it skips definitions
that already carry a `#:` block, because those render the user's own text rather than a type the
checker chose. It re-points one database with the salsa setter instead of building a new one per
probe, because a fresh database re-parses the whole stub corpus.

`KNOWN_UNWRITABLE` lists schemes that are allowed not to round-trip. It is empty, and worth keeping
that way. Its entries name case ids rather than skipping by shape, so a new unwritable rendering of
any kind fails the test instead of blending into a known category.

### Tree-sitter as a second opinion

`crates/syntax/tests/test_corpus.rs` compares our verdict on "does this file have errors?" with
`tree-sitter-r`'s, in both directions and over two sets of inputs:

- `corpus_acceptance` runs over the fetched `corpus/` and gates the direction where *we* reject what
  tree-sitter accepts, which means a gap in our grammar.
- `in_tree_acceptance` runs over inputs that are always present (the program corpus plus every fixture
  case source) and gates the other direction, where we accept what tree-sitter rejects.

That second direction is the only check that bounds the number of parse errors from below. Every
other parser invariant bounds it from above, so without it an over-suppressing parser, the
regression most likely to slip in given how much deduplication the parser does, would pass every
fuzz battery while broken files went silently clean.

Each direction is gated only where it means something. Rejecting what tree-sitter accepts is noise
on in-tree input, which is full of `#:` annotations (comments, to tree-sitter) and of deliberately
broken sources. Accepting what tree-sitter rejects is gated there against
`crates/syntax/tests/in-tree-acceptance-allowlist.txt`. Each allowlist entry is adjudicated by
running R itself, and must say which of two things it is: tree-sitter being wrong, or a known gap in
this parser.

Inline type annotations are ry syntax that R rejects by design, so tree-sitter judges the plain R
program a source erases to (`syntax::erase_inline_annotations`), not the source itself. That makes
the test a check on erasure too: an annotated fixture whose erased text tree-sitter rejects fails it.
`check_parse_invariants` pins erasure from the other side, for every fuzz input that parses cleanly:
the erased text parses cleanly, holds no inline annotation, and erases to itself.

### The runtime

The runtime is tested against models and against GNU R rather than with fixtures. The oracle's
cases (`crates/oracle/cases/`, [Corpus and oracle](/contributing/corpus-and-oracle)) hold the
R-level behavior, and `crates/interp/tests/test_fuzz.rs` fuzzes lowering and checks that the
tier-0 bytecode agrees with the evaluator. `crates/jit/tests/test_tiers.rs` does the same one tier
up: it runs random loop, branch and vector programs as closure bodies under tier 0 and under the JIT
forced on (`jit::Mode::Force`, which compiles a body on its second call and a loop on its first
backward jump), and every call must give the same value or error. Most generated programs must run
to a value, so a generator that drifts into erroring early fails the test instead of passing
without comparing anything. The test interpreter has no base R closures (`numeric`, `as.list`,
`withCallingHandlers`), so programs use builtins, and each hand-written seed states whether it is
meant to fail. Behavior that needs base R, such as a warning handler watching a compiled loop,
goes in the oracle case `bytecode__hot_loops.R`. `crates/interp/tests/test_linalg.rs` pins base R's linear
algebra to GNU R's results bit for bit. `crates/table/tests/test_table.rs` checks the table engine
against values read from data.table, and `test_altrep.rs` there checks the columns as ALTREP
vectors (feature `runtime`). `crates/heap/tests/test_gc.rs` builds random
object graphs (vectors, lists, interned strings, environments), drops roots, stores young objects
into old ones and collects at random points, with and without collection stress, and after every
collection checks each object the model still reaches. It also re-interns every string ever made,
so an intern-table entry that outlived its string shows up. A disabled write barrier, a frame whose
cells are not traced, and a minor collection that keeps dead strings in the table each turn it red.

`crates/libr/tests/test_fuzz_native.rs` fuzzes the native boundary itself. It nests every
non-local exit native code can set up (`Rf_eval`, `R_tryEval`, `R_ToplevelExec`, `R_tryCatchError`,
`R_UnwindProtect`, `R_ExecWithCleanup` and a cpp11-style C++ unwind) around values and errors, and
checks the outcome, which cleanups ran, and that every C context, protection and `R_alloc` block was
released. A second battery drives random API sequences under collection stress against a model.

`crates/oracle/tests/test_differential.rs` runs whole generated R programs (`oracle::fuzz::program`)
under GNU R, ry's tier 0 and ry's JIT, and under collection stress against ry unstressed, and every
engine must print the same output, warnings and errors. The generator covers loops shaped for the
JIT, closures, S3 dispatch, conditions and `on.exit`, environments, attributes, `NA` and overflow,
and about one program in eight ends on a deliberate error. `cargo run -p oracle -- fuzz --seed N
--count K --engines ry-tier0,ry-jit,ry-gctorture` runs any range, prints each divergent program and
its replay command, and `just fuzz-differential` and `just gc-torture` are the long runs.
`RY_GC_STRESS=1` stresses from the script's start and `RY_GC_STRESS=startup` from the first
allocation, which is too slow for a debug build today.

### The table engine

`crates/table/tests/test_table.rs` drives the engine from Rust. Every expected value that encodes
data.table behavior was read from data.table under GNU R. Its differential fuzz generates random
tables and queries and checks the native kernels against the Polars plans
(`query::execute_with_plans_only`), which serve as the reference. `FUZZ_ITERS` raises the
iteration count; run it in release, as `FUZZ_ITERS=3000 cargo test --release -p table fuzz`. The
fuzz tables are small, so the kernels' parallel paths get large-table tests of their own.

`tools/bench/table/run.sh <dir> [rows]` runs the db-benchmark questions under data.table and the
engine, each printing rows, columns and a checksum, so the answers compare line by line.

### Coverage-guided fuzzing

`fuzz/` is a cargo-fuzz crate, in its own workspace and excluded from the main one. Each target is a
thin wrapper around a battery its crate exports, which the crate's in-tree harness runs too:

| Target | Battery |
| --- | --- |
| `parse` | `syntax::testing::check_parse_invariants` |
| `format` | `format::check_format_invariants` |
| `semantics` | `semantics::testing::check_semantics_input` |
| `ide` | `ide::testing::check_ide_invariants` |
| `position` | `ry::position::check_position_invariants` |
| `config` | `ry::config::check_config_invariants` |
| `lsp` | the language-server simulator, `run_session`, reading its choices from the input |
| `load_index`, `reload` | `loader::testing::check_load_index_input`, `check_random_reload` |
| `sniff` | `sniff::check_sniff_invariants` |
| `csv` | `table::io::check_csv_input` |
| `interp_lower`, `interp_unserialize`, `bytecode` | `interp::testing::check_lowering_input`, `check_unserialize_input`, `check_bytecode_agreement` |
| `unserialize` | `R::serialize::check_unserialize_input` |

The `semantics` target derives its document kind and an incremental edit from the input bytes, so a
single byte stream drives the full battery, the splice cache included. The generator-driven targets
(`lsp`, `reload`, `bytecode`) read the generator's choices from the input through
`syntax::testing::ByteStream`, so a byte mutation is a mutation of the program or session. The
runtime targets link polars, so `cargo fuzz build --no-default-features` builds only the analysis
targets on a machine with little memory.

It needs nightly, which the repository's pinned toolchain already is. Run `cargo install
cargo-fuzz`, then from `fuzz/`:

```sh
cargo -Zscript ../scripts/seed-fuzz-corpus.rs   # seed from all fixture case sources
cargo fuzz run format -- -max_total_time=600 -print_final_stats=1
cargo fuzz run parse  -- -max_total_time=600
```

The seeder copies every fixture case's source in the repository into
`fuzz/corpus/{parse,format,semantics}/`. Files are content-addressed, so running it again only adds.
The corpus and the artifacts are gitignored, because a durable regression belongs in the
`REGRESSIONS` battery or in a fixture case, not in the corpus.

When the fuzzer finds something, minimize it with `cargo fuzz tmin format <artifact>`, fix
the bug, replay the artifact, and add the minimized input to `REGRESSIONS`, plus a golden fixture
case when the shape deserves a pinned rendering. To judge corpus quality, you can get a coverage
report (the pinned toolchain includes the llvm-tools component it needs) with
`cargo fuzz coverage format && cargo cov -- show`, as the
[cargo-fuzz coverage guide](https://rust-fuzz.github.io/book/cargo-fuzz/coverage.html) describes.
Deep runs are manual or scheduled work; the bounded in-tree batteries remain the gate in the default
suite.

### Reloading

`crates/loader/tests/test_reload_fuzz.rs` generates packages from statements that define, wrap,
read and rewrite a small pool of names, edits them at random, and checks that applying the reload
plan to the loaded package gives what a fresh load of the edited package gives. A model executes
both sides: a lazy binding's value is its definition's text, and a statement stores its text plus
the values it read, so a statement that should have rerun shows up as a stale value. The same test
checks that the incrementally derived index equals one built from scratch, and a second test feeds
the index arbitrary bytes and asserts its positions stay inside the text. Removing either of the
diff's ordering rules turns it red within a few hundred seeds.

## End-to-end suites

### The CLI

`crates/ry/tests/test_cli.rs` drives the real `ry` binary end to end. It covers:

- diagnostic rendering, with 1-based character positions, snippet windows, and underlines;
- JSON Lines output;
- the [exit-code contract](/reference/cli#exit-codes);
- finding the configuration, and failing to;
- per-file `# typing:` directives and suppression comments;
- `NAMESPACE` validation, and project stub overrides including their loader-problem reports;
- data-masked evaluation;
- the formatter's `--check`, `--diff`, and in-place modes;
- `ry run` and `ry R` without R: help output, usage errors, a missing script, and strict mode
  refusing a script before any R starts.

`crates/ry/tests/test_command_line.rs` property-tests and fuzzes argument parsing for `ry run`,
`ry R` and the `ry repl` alias against a model: everything after the script reaches it verbatim,
options resolve to the documented runtime and mode, and no argument vector panics the parser.
`test_repl_e2e.rs` runs every entry point on both runtimes: script arguments, `-i`, piped consoles,
and the earlier spellings (`--runtime`, `RY_RUNTIME`, `ry repl`).

### Docs examples

`crates/ry/tests/test_docs.rs` runs the examples in the docs through the real `ry` binary, so a page
that shows stale output fails the suite. A block opts in with a marker: the word `ry` after the
fence's language, then the command and its options. Expressive Code ignores the extra words, so the
site shows a plain block, and the pages' Markdown versions drop the marker. Here an output block
follows a tab group:

````md
<Tabs syncKey="annotation-style">
<TabItem label="Inline">

```r
greet <- function(name: character, greeting: character = "hello") -> character {
  paste(greeting, name)
}
greet("Ada", greting = "hi")
```

</TabItem>
<TabItem label="Comments">

```r
#: fn(name: character, [greeting]: character) -> character
greet <- function(name, greeting = "hello") {
  paste(greeting, name)
}
greet("Ada", greting = "hi")
```

</TabItem>
</Tabs>

```text ry check typing
type-mismatch

  x this function has no parameter `greting`. Did you mean `greeting`?
```
````

A marked `text` block is the output of the nearest `r` block above it, or of every tab of the
nearest tab group above it, and each tab must produce it on its own. The code runs in a throwaway
project of one `ry.toml` and one file, `main.R` unless the output's snippet names another file, as
`--[R/both.R:2:31]` does. A tab group that no output block follows must give the same findings in
every tab, with typing on. An unmarked output block is not tested.

| Word | Meaning |
| --- | --- |
| `check` / `run` | Run `ry check`, or `ry run main.R`. |
| `typing`, `strict` | Set that `[check]` key to `true`. Type checking is off unless a block says `typing`. |
| `KEY=VALUE` | Any other `[check]` key, written as in `ry.toml`, such as `object-systems=false`. |
| `runtime=ry` / `runtime=gnu-r` | Which runtime `run` uses; GNU R by default, as on the command line. |
| `exact` | Compare the whole output instead of an excerpt. |
| `continue` | Run the page's earlier `r` blocks first, as one session, and compare only this block's output. A `check` takes every earlier block; a `run` takes only those whose output the page checks, so an illustration that needs a package the machine lacks does not stop it. |
| `tab=LABEL` | Only that tab produces this output, for the rare example whose spellings legitimately differ. |
| `dput` | On an `r` block with `#>` output, print each result with `dput()` instead of `print()`. |

An `r` block can also hold its own output, as `#>` lines after the code that prints it, with a marker
such as ```` ```r ry run runtime=ry dput ````. Each run of code before a `#>` group shows its last
value, and the comparison is always exact. A page's `#>` blocks share one R process, each in its
own `local()` environment, because a runtime start costs seconds and the standard library reference
has over a hundred of them.

An excerpt matches when every non-blank line of it equals a line of the real output, both trimmed of
surrounding whitespace, in the page's order. Lines the page leaves out, such as the summary, a
source snippet, or other findings, are free. A message the page wraps by hand matches one output
line when its lines are joined with single spaces. This rule lets a page show the part of the
output that matters while still pinning every word it does show. An exact block matches when the
whole output agrees, ignoring trailing whitespace and the project's temporary directory. Both
compare standard output and standard error as one stream, interleaved as a terminal shows them,
with color off.

`just docs-test [PAGE]` runs the pages whose path contains `PAGE`, and `just docs-bless [PAGE]`
rewrites the output of exact and `#>` blocks from the real tool. An excerpt is curated by hand, so
bless leaves it alone: the failure prints the full output to choose the lines from. A `run` block
is skipped with a note when GNU R is missing, or for `runtime=ry`, when `libR.so` is not built
beside `ry`. The landing page's samples in `docs/src/pages/index.astro` are checked as excerpts with
typing on, without markers.

### Package loading

`crates/ry/tests/test_load.rs` runs `ry load` and plain `Rscript` against a real GNU R and checks
the loaded result: `Collate` order, imports, S3 and S4 registration, `Depends`, `sysdata.rda`,
source references, reloads that rebind, rerun and remove, undoing deleted S4 definitions, the error
locations, and the `ry load --r-entry` entry point. `test_repl_e2e.rs` checks that `ry R` loads
the package it starts in and reloads an edit before the next command. Both skip without R.

### Native packages

The `libr` tests load `libR.so` the way an R host does, build packages from source with the system C
and C++ compilers, load them with `dyn.load` (which runs `R_init_<pkg>`), and call their routines
through `.Call`, all through the exported C API. The shared harness is `crates/libr/tests/harness.rs`.
The packages are brio from the benchmark closure, vendored in `tests/packages/brio`, and
`tests/packages/rytest`, written for these tests.

- `test_native.rs` builds both against ry's headers and checks the results, collection stress, the
  handle scope, `PROTECT` imbalance, errors unwinding through C and C++ frames, `R_alloc` release,
  finalizers, coercion, and the random number generator against GNU R's output.
- `test_gnu_r_binaries.rs` builds both with GNU R's `R CMD SHLIB`, against GNU R's headers and
  linked to GNU R's `libR.so`, and runs them under ry's. It runs in a process of its own, so the two
  builds of one package never share a session.
- `test_headers.rs` compiles every header as C11 and C++17 with warnings as errors, checks with
  the compiler that every declared name is exported, runs BLAS and LAPACK calls through the
  headers, and compares the graphics structs' layout with GNU R's.
- `test_altrep.rs` and `test_altrep_api.rs` drive the `altreptest` package's classes of every
  kind and base R's own classes, against a report recorded from GNU R.
- `test_serialize.rs` and `test_serialize_native.rs` read and write GNU R's recorded streams byte
  for byte in every format and version, read installed lazy-load databases, and fuzz the reader.
- `test_connections.rs` runs one connection scenario list in GNU R and in ry and compares the
  transcripts; `test_connections_gnu_binary.rs` repeats it with the `ryconn` package built by GNU R.
- `test_install.rs` assembles ry's R home and installs packages with `R CMD INSTALL` semantics.
  With `RY_SOURCE_PACKAGES` set to a directory of source tarballs, it builds each one against ry's
  headers and reports the outcome.
- `test_corpus_binaries.rs` loads every compiled package of the installed corpus
  (`RY_CORPUS_LIBRARY`, default `.corpus/library`) under ry, each in a child process with its
  dependencies, and writes `target/tmp/corpus-binaries.tsv`.
- `test_corpus_calls.rs` replays native routine calls that `tests/data/corpus_calls/record.R`
  recorded under GNU R, reading arguments and results with ry's unserializer, and requires every
  routine whose library loads to return what it returned under GNU R.

The two corpus suites pass without a corpus and report that they skipped.

These suites need a C and C++ toolchain, and GNU R for the second suite and the generator check.
`RY_SKIP_GNU_R=1` skips the parts that need GNU R, and without it a missing R is a failure.

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
response and the successful retry after it can be asserted without sleeping and hoping. Nothing
else sets these variables.

`crates/ry/tests/test_simulation.rs` runs the in-process language-server simulator: random editor
sessions under a seeded schedule, checked at random points against a server started fresh on the same
disk and buffers. `crates/ry/tests/test_scenarios.rs` runs the scripted user journeys in
`crates/ry/tests/scenarios/`. [Robustness](/contributing/robustness/) documents both, including the
scenario format and the replay variables.

## Incrementality

A fixture checks what an edit produces, but not how much work it took to get there.
`crates/semantics/tests/test_incremental.rs` checks the work: `semantics::testing::ProbeDatabase`
logs every query execution, and `checked_since` names the items whose `item_check` ran. The tests
pin these contracts:

- A body edit that keeps the item's scheme re-checks the edited item alone.
- A body edit that changes the scheme also re-checks the items that read it.
- A new definition re-checks only the items that read its name.
- An inserted statement re-checks only the reads it intercepts.
- A body edit re-derives one item's load facts and its own file's index layer, a `NAMESPACE` edit
  re-derives no item, and a statement that only moved keeps its load facts.

The same file checks that a chain of re-exports resolves to its base, that a cycle of them pins to
`Unknown`, and that a chain of 4,096 links resolves without overflowing the analysis thread's stack.
In the IDE harness, `point_queries_recheck_only_what_an_edit_changed` holds the features that run on
every keystroke to the same rule.

## Performance and memory

`crates/semantics/tests/test_perf.rs` asserts performance budgets over the real-file corpus. It is
ignored by default, because it needs the fetched corpus and a release build, and CI does not fetch
the corpus:

```sh
cargo test -p semantics --release --test test_perf -- --ignored --nocapture
```

`just perf` runs the same command. Its budgets are measured numbers plus headroom, and cover the
cold-pass wall time per line, resolve steps per line (the tripwire for a regression in resolve
memoization), resident bytes per line, and the p50 and p95 latency from a keystroke to fresh
diagnostics on the corpus's largest package. It prints what it measured, so it doubles as the
corpus-wide measurement, while `ry debug analysis-stats` diagnoses one workspace phase by phase.

## Guidelines

- Add or tighten a fixture before writing a parser-local or crate-local unit test.
- A new phase or module gets a fixture suite before it relies on ad hoc unit tests.
- Use the lightest fixture change that captures the failing shape.
- Change a fixture expectation deliberately, never as a side effect.
