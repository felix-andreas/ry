# Decisions: Editor and language server

The language server, editor features, the formatter, lints, the check command's reporting, and the console and debugger.

## Beta semantics: A reported column counts characters, and caret art counts terminal cells

**Status:** built.

The CLI reported byte columns in the rendered header and in `--output json` alike, and padded the
caret by the same byte count. R source carries non-ASCII text as soon as a string holds a name or a
unit. On such a line the number disagreed with every editor, and the caret sat to the right of the
code it accused, sometimes past the end of the line. A byte column served nobody, because a
consumer would have to re-read the file as bytes to use one, and no editor counts that way.

The two jobs need two units.

- **A column counts characters.** `LineIndex::line_column_chars` produces the JSON `column` and
  `endColumn` fields and the server's `file:line:column` strings. The CLI's snippet source
  re-counts miette's byte column the same way for the report header. This is the number a person
  can act on. The JSON field documentation changed with it, and that is a stated contract, so the
  change is recorded here.
- **Underline art counts terminal cells.** A CJK character or an emoji occupies two cells, so a
  character count would under-pad. The graphical reporter measures cells, so the two units never
  have to be reconciled by hand.

The LSP path is untouched. It converts to the negotiated encoding, UTF-16 by default, at the
protocol edge. That is what LSP specifies, and it is not what a byte column was ever meant to
serve.

## Beta semantics: The CLI reports through miette

**Status:** built.

Every user-facing message the CLI printed used to be assembled by hand. `render_human_diagnostic`
computed a gutter width, sliced the finding's first line out of a `LineIndex`, padded a caret row
by terminal cells, truncated a multi-line range with a dim note of its own wording, and printed a
related location as a one-line `= note: ... --> path:line:col` trailer. A second path styled the
`error: ` and `warning: ` prefixes with `console` and printed an underlying I/O failure with a bare
`eprintln!` on the next line. `ConfigParseError` built its own sentence of the form
`in <path> for <key> at line L, column C`. That is three renderers and three notions of where a
message points. Every improvement meant more hand-rolled layout.

The shape now is one report type in the CLI, drawn by miette's `GraphicalReportHandler`. A report
holds a severity, an optional diagnostic code, a message, an optional cause, an optional snippet
made of a named source and a labelled range, and nested related reports. That is enough for a
finding, a companion note and a bare failure alike, so one reporter draws all of them. An error
that knows its own source implements `miette::Diagnostic` itself. `ConfigError` carries the config
text and the toml span, and the CLI hands it to the same entry point. The look follows the
destination: colour and unicode on an attended terminal, monochrome unicode when colour is refused,
and plain ASCII into a pipe or a file.

The effect on each axis:

- Correctness. The underline is the library's, so a multi-byte or wide glyph is handled in one
  audited place. A related location is drawn from its own file instead of being reduced to a
  coordinate. A configuration failure is shown rather than described.
- Simplicity. The caret, gutter and width arithmetic is gone. `RelatedNote` resolution moved out of
  the parallel worker into the sequential render step, so a worker returns a plain diagnostic.
- Performance. The reporter is built once in a `LazyLock` instead of being re-derived per finding.
  The snippet source is borrowed through a custom `SourceCode` implementation rather than a
  `NamedSource`, which owns its text and would copy the whole file into every finding reported
  against it.
- Incremental analysis. Untouched. This is presentation, and the JSON Lines contract is unchanged.

Two things miette does not do here. It counts the header column in bytes, so the borrowed source's
`read_span` re-counts it in characters. Otherwise the human header and the JSON record would
disagree about the same finding. It also draws every line a range covers, so a range spanning more
than three lines is clamped to its first line with the reach stated in the label. A finding on a
long item must not print the item.

The theme is the project's own, not the stock one. A snippet is a window on the source, not a box.
The gutter runs unbroken down every row, with `vbar_break = vbar` in place of the stock dotted row.
The header opens with a plain rule rather than a corner, with `ltop = hbar`. The range is
underlined with carets, with `underline = '^'`. The closing rule under each snippet is dropped,
which saves one line per finding, and a run of findings prints dozens. The closer is the one part
that is not themeable. miette builds it from `lbot` and `hbar`, and the cause-chain arrows and the
multi-line span brackets draw from those too, so it is filtered out of the rendered string instead.
The filter matches only a line that is nothing but the rule, because a nested report indents its
own in the parent's colour.

The README shows its diagnostics as the plain ASCII `ry check` prints into a pipe, not as an image:
GitHub strips ANSI escapes from a code block, and an image rendered from them is one more artifact
that nothing checks and that loses the text for search and screen readers.

The snippet-closing-rule width is read off miette 7.6, which hardcodes it.
`check_draws_the_snippet_as_a_window` fails if an upgrade moves it, which was verified by changing
the constant.

## Beta semantics: The annotation formatter parses, then pretty-prints

**Status:** built.

The formatter used to re-indent a `#:` block line by line, with no bracket matching, and it never
split a line. It now joins the block, parses it with the real annotation type parser, and
pretty-prints the result with per-bracket hug bits, honoring `indent_width`. An opener followed by
content on its own line stays hugged, and its closer mirrors it. On a parse failure the block is
left verbatim, which also ends prose corruption and removed the drifted duplicate tokenizer. Both
the fully expanded style and the hugged style are stable fixed points, and a mixed shape
normalizes.

## Beta semantics: Config subsystem

**Status:** built, except the unknown-key error: an unknown key is a warning and the configuration loads.

The workspace root comes from `InitializeParams` and never from the process working directory.
Discovery finds the nearest configuration file in an ancestor directory, identically in the LSP and
in the CLI. An unknown key is an error with a toml span, surfaced as a diagnostic on the
configuration file plus a window message, and never as a startup panic. A config reload triggers a
diagnostics refresh. One config struct chain runs end to end, replacing four parallel
representations.

## The Zed extension versions on its own line

**Status:** superseded by "Releases are built natively per platform, with the runtime beside the binary" ("One version per release"): the `justfile` writes the CLI's version into the Zed manifest and `just release-check` requires the match. Kept for why the Zed extension locates a binary and bundles none.

Three shipped artifacts carry a version, and only two of them derive from the workspace
`Cargo.toml`. The CLI is the source of truth. The VS Code extension bundles that binary, so its
manifest carries the same number with the prerelease suffix stripped, because the marketplace
rejects `X.Y.Z-alpha`. That is a mechanical derivation.

The Zed extension bundles nothing. It locates a binary at run time, through the LSP settings path,
then `PATH`, then the latest GitHub release. Its version therefore describes the extension's own
code and says nothing about the CLI. That was settled once and still failed to hold. The release
recipe stopped bumping it, the number was hand-realigned to the CLI's twice anyway, and the second
time a test was added to mandate the alignment. A version that must be manually resynchronized to a
number it has no relationship with is the duplication, not the cure.

`editors/zed/extension.toml` is now a plain semver line of its own, at `0.1.0`. It restarted
because the extension has never been published to Zed's registry and nothing constrains its
history. The wasm crate's `Cargo.toml` version tracks that manifest rather than the workspace, and
neither inherits `version.workspace`. It is bumped by hand when the extension changes. The
release-metadata test asserts the VS Code derivation only, and its module documentation states why
the Zed manifest is absent, because that test is what would otherwise re-couple them. The
prerelease suffix is dropped for good, because `-alpha` and `-beta` name the CLI's release channel,
and an extension that only locates a binary cannot be in one.

Each artifact now has one number with one owner. A Zed release no longer implies a CLI release, or
the reverse. The recurring commit that realigned the stale Zed extension version has no reason to
exist.

## Editor features for ry's own syntax

**Status:** built.

- **The checker records pattern types.** `ItemCheck::pattern_types` maps each `match` pattern to
  the type of the value it is matched against, filled while the arms are checked. Before this the
  IDE had no type for a pattern, so hover and completion there had nothing to read. It is a
  per-item query result, so it adds no project-wide dependency.
- **A tag is a project-wide structural symbol.** Tags have no single declaration: `:Ok` in one type
  and `:Ok` in another are the same tag. References and rename therefore cover every spelling of the
  name in the project, and go-to-definition goes to its listing in a named type definition, or in
  the standard library stubs. A tag the standard library
  spells (in `types/std.Rtypes`) refuses to rename, because the stub would no longer match.
- **Type parameters navigate to their binder**, scoped to the definition or function that binds
  them, so `T` in two functions is two symbols.
- **Semantic tokens are an `ide` query** (`semantic_tokens`, `classify_type_token`), and the server
  only maps `SemanticClass` to its legend. Before this the classification lived in the server and
  covered annotations only, which left `match`, `=>` and tags uncolored.
- **Semantic tokens also color R names an R grammar cannot classify**: parameters (declarations and
  reads that resolve to a parameter binding), call heads as functions, the package of `pkg::name` as a
  namespace, and the left side of a function assignment. Everything else stays with the editor's R
  grammar, so tokens add facts rather than replace the grammar. The server also answers range
  requests, so a large file only pays for the viewport.
- **Member names are `property` or `method` tokens.** `x$name` and `x@name` decide from the
  syntax alone (a called member is a method), and so do record-literal and pattern fields. R6
  members and S4 slots are named by call arguments, so `semantic_tokens` reads them from the
  outline's `classify_symbol_call` rather than a second classifier: one definition of what an R6
  member or S4 slot is serves the outline and the highlighting.
- **A quick fix lives on the diagnostic** (`Diagnostic.fix` in `crates/semantics/src/diagnostics.rs`,
  `push_lint`'s fix argument in `lints.rs`). Before this, a fix would have had to re-derive the
  finding in `ide`, a second source of truth that drifts. The server reads fixes from the diagnostics
  it publishes, after suppressions, so a suppressed finding offers no fix. `ry check --fix` can reuse
  the same field.
- **Outline sections and folds come from RStudio section comments** (a comment alone on its line
  ending in four or more `-`, `=` or `#`). A section ends at the next header of the same or a higher
  level in the same block, which matches RStudio's outline, so files written there look the same.
- **An inlay hint states what the code does not.** A payload-free tag or an `@Name(...)` constructor
  already spells its type, so it gets no hint and no "add annotation" action, and a vector of
  records, tuples or tags (`Position[]`) is dropped from a hinted union because no such value exists.
- **A type error on a native literal blames the one differing component** (`native_component_mismatch`
  in `crates/semantics/src/check.rs`), recursing into nested literals and tag payloads, and keeps the
  whole literal when more than one component differs, because then no single spot is the cause.
- **Impact:** every feature reads per-item query results, so incremental analysis is unchanged; the
  fixtures in `crates/ide/tests/ide/records-and-tags.R.test`, `frames.R.test` and the typing suite's
  `mismatch_blame` group pin the behavior.

## Editor responsiveness at 300,000 lines

**Status:** built.

- **Previous shape.** Completion walked every item in the project per request, reading each item's
  salsa fields, re-walked every file's syntax tree for untyped `$` fields, resolved a scheme (and
  so possibly a type check) for every matching global, and sorted every candidate with an
  allocated key before keeping 128. References for a global named and lowered every item in the
  project. On finbench that was 30 ms p50 for completion and 300 ms p99 for references.
- **Target shape.** Two per-file tracked indexes in `crates/ide/src/ide.rs`: `global_names` (the
  top-level names a file writes, with their item) and `spelled_names` (the names spelled after
  `$`, `@` and `::`). They are derived from the item tree and the syntax, so they cannot drift, and
  an edit recomputes only the edited file's entry. Ranking selects the top results with
  `select_nth_unstable_by` and compares labels case-insensitively without allocating, and call
  shapes are resolved only for the results returned. References and rename skip a file, then an
  item, whose source text does not contain the name, because every occurrence spells it.
- **Requests preempt idle work.** A request cancels the idle unit in flight once the request is
  queued (`CancelHandle::preempt_idle` in `crates/ry/src/server.rs`); it never cancels a sync.
- **Durability.** A setter that names no durability keeps the field's current one, so the old
  `didOpen` rewrote every opened file at `MEDIUM` and invalidated every memo in the project. It now
  skips text that matches the disk and lowers the file to `LOW` in idle time, because salsa never
  backdates a memo whose durability drops, and the first `LOW` write therefore re-derives the
  file's dependents once.
- **Project-wide maps fold per-file memos.** `package_definitions`, `conditional_slot_items`,
  `interface_sccs` and their test-file twins read every item's projections directly. Once the
  edited file was `LOW`, each map was too, and confirming it after a keystroke checked about 56,000
  item memos, about 35 ms before any request could answer. They now read `file_definitions` and
  `file_conditional_writers` (`crates/semantics/src/semantics.rs`; `interface_sccs` is gone), so
  the check touches about 500 file memos and every untouched one passes on durability.
  `project_arithmetic_classes` needed no change: it reads only `package_definitions`.
- **Every host input but the open documents and the file list is `MEDIUM`.** The server and
  `analysis-stats` created the stubs, settings, metadata and data files at the default `LOW`, and
  nearly every memo reads the stubs, so no memo was ever `MEDIUM` and the file durabilities had no
  effect. With those inputs at `MEDIUM`, the maps must be `LOW` before any file's first edit, since
  a memo whose durability drops cannot backdate and the drop would re-execute every item check
  (about 2 s on finbench). `ProjectFiles` is therefore `LOW`, which keeps the maps `LOW`.
  `test_incremental.rs` pins both: an edit verifies only the edited file's items, and a file's
  first edit re-checks only its own.
- **Impact.** Settled requests are about 1 ms p50 except completion at about 4 ms, and a request
  right after a keystroke now costs about the same (0.5 ms hover p50 in `analysis-stats` on
  finbench, from 30 ms). Edited-file diagnostics fell from 36 ms to 1.5 ms median.

- A setter that names no durability keeps the field's current one, so every place that creates or rewrites a `SourceFile` names it. Opening a file lowers it to `LOW`, which re-runs once every fold that reads it. A fold that only editor requests read is re-derived in idle time (`Worker::editor_folds_stale`) so the first request after the open does not pay for it.
- `ry debug analysis-stats` and `crates/ry/examples/keystroke_probe.rs` set the same durabilities as the server, or their numbers describe a different invalidation pattern.
- The budget is a keystroke-to-diagnostics p50 of at most 30 ms and a p95 of at most 100 ms at 300,000 lines, pinned by `crates/semantics/tests/test_perf.rs` (`just perf`, which needs the fetched corpus).

## A parallel startup warm-up in the server

**Status:** built.

- **Previous shape.** The server primed the workspace one file at a time in idle units, and each
  unit also scanned the file's `library()` calls and applied the grown attach union at once. The
  first file's check derived every item's naming for the project-wide maps on one thread, about
  1.3 s on a 300,000-line project, and the first wave's lints read those maps, so a file opened at
  startup waited that long for any diagnostics. Each union change re-ran the open files'
  diagnostics, so an open file was republished about six times, with counts that flickered.
- **Target shape.** `Worker::warm_up` in `crates/ry/src/server.rs` runs before any other idle
  work: it scans every file's attach facts across all cores and applies the union once, then
  derives the per-file folds behind the project-wide maps (`warm_project_folds`) across all cores.
  It used to name every item as well, which the next record removed. Until it ends, the first wave
  holds only parse-stage diagnostics (the lints need the maps), pull diagnostics answer the same,
  and settled publishes wait; its end refreshes every open file. The rest of the cold check runs
  in parallel batches of 64 files. `Worker::in_parallel` is the one helper for both.
- **Stopping parallel work.** The threads run on cloned handles whose tokens `CancelHandle`
  cancels too, and they also poll a queued-job counter between files, because salsa resets a
  handle's cancellation flag when a top-level query returns (`attach.rs`, `DbGuard::drop`), which
  drops a flip that lands at a query's end. Without the counter an open sometimes waited for the
  whole warm-up.
- **Impact.** On dsbench a file opened at startup gets its first wave in under 15 ms (from about
  1.3 s) and its settled diagnostics about 0.55 s after startup (from 1.3 to 1.9 s), published
  once instead of six times. Analysis is unchanged; only the order and parallelism of work moved.

## A fix says whether it may be applied unseen

**Status:** built.

- `Fix.applicability` (`diagnostics.rs`) is `Safe` when the edit keeps what the code means (the lint
  rewrites, syncing a `#:` comment to the inline type that governs, removing an unreachable arm) and
  `Unsafe` when it guesses intent or silences a finding without handling it (spelling suggestions,
  `stop()` arms for missing cases). It follows ruff's model. `ry check --fix`, the editor's preferred
  quick fix and the `source.fixAll` action apply only safe fixes, because nobody reviews those edits;
  `--unsafe-fixes` and a click opt into the rest. The fix lives on the diagnostic, so the CLI and the
  server share one source of truth (`applicable_fixes`, `apply_fixes`).
- `ry check --fix` re-analyzes and repeats until no fix changes a file (at most `MAX_FIX_PASSES`),
  because overlapping fixes are skipped within a pass and one fix can expose another. It refuses
  to write a file whose text on disk no longer matches the analyzed text, and leaves a file with a
  syntax error alone.
- Safe means safe in context, not by syntax alone: `boolean-shorthand` holds only while the file
  binds no `T`/`F` (it rides the lints' `unless_redefined` channel), and the `#:` comment fix is
  withheld when the inline type names a type parameter the comment would not bind.
- The server keeps each open document's settled fix-bearing findings with the text they were computed
  for (`published_fixes` in `server.rs`). `codeAction` reuses them while the text matches, so a
  request per cursor move costs no analysis, and the offered fixes are the findings on screen.

## Parameter hints and call hierarchy follow R's own call rules

**Status:** built.

- Parameter-name inlay hints (`inlay_hints` in `ide.rs`) map arguments to parameters with the
  checker's own `argument_targets` (`check.rs`), so the hint and the type check never disagree on
  where an argument goes. Hints stop at a forwarded `...` and skip a piped value. They show only in calls with at least two positional arguments, and skip one-letter
  parameters and arguments whose text already names the parameter (`years` or `x$years`), because a
  hint that repeats what is on screen is noise.
- The call hierarchy is limited to project top-level functions (the `package_definitions` index),
  because those are the items a reader navigates between and the index is already per-name tracked.
  Only call heads count as calls, so passing `f` to `lapply` is a reference, not a call. A caller
  that is a top-level statement is grouped as "(top level)". The server re-resolves a function item from
  its uri and selection start instead of serializing ids into `data`, so a stale item never names
  a wrong function; a "(top level)" item does not re-resolve, because its start is whatever the
  statement calls first.

## The debugger lives inside the interpreter

**Status:** built, except the debugger-only source references for package code, which "Lazy definitions keep source references" supersedes.

- **Previous shape**: none; `browser()` was the only way to stop R code, and only in a console.
- **Shape**: the Debug Adapter Protocol adapter is `interp::debugger`, inside the interpreter, not
  a separate process attaching to it. `ry debug-adapter` (`crates/ry/src/debug_adapter.rs`) only
  frames messages on stdio and reaches the adapter through libR's `ry_debug_adapter`, because the
  interpreter lives in `libR.so`. A reader thread feeds a channel so the runtime can poll for a
  `pause` between statements without blocking on stdin.
- **Hooks**: statements are located by the srcrefs `{` already sets (`Op::SetSrcref` in
  compiled code, `do_begin`/`eval_all` in the tree walker). Without an attached adapter the hook
  costs one `Option` check per statement. `SetSrcref` carries a flag, because the one that
  restores the outer srcref after a block starts no statement and must not stop. `browser()` and
  the uncaught-error handler stop through the same entry point.
- **Rooting**: every object the variables view hands out a reference for is rooted until the
  program resumes, because the debug console can drop its last other reference.
- **Stdout**: the protocol owns stdout, so the adapter keeps a duplicate of it and points fd 1 at
  stderr; native code calling `printf` cannot corrupt the protocol.
- **Package code**: the loader lowers lazy definitions without srcrefs for speed, and only while a
  debugger is attached does it lower against a whole-file srcfile (`interp::lower::file_srcfile`),
  held as the tree handle's protected value. Loading stays as fast as before outside the debugger.
- **Testing**: `interp` `test_debugger` (transcripts and a seeded random client) and the
  `debug-*.scenario` journeys, which run the real `ry debug-adapter`.

## Range formatting is a cut of the whole-file format

**Status:** built.

- Previously the formatter had only a whole-file entry point, and an unfinished range formatter
  formatted the selected text on its own, so a selection and the file could disagree about a line
  (indentation, a brace added by auto-bracing, a comment's attachment).
- Range formatting (`format::format_range`) formats the whole file, pairs source and formatted trees
  statement by statement, and returns the formatted text of the statements the selection touches.
  The formatted file is the single source of truth, so restriction and additivity hold by
  construction and the range invariant battery (`check_range_format_invariants`, also a fuzz target)
  can check them. Cost is a whole-file format per request, which is cheap next to analysis.
- On-type formatting reuses it: a line break formats the line just finished, `}` the construct it
  closes. The language server advertises both unconditionally; the experimental-features registry
  that gated them was removed, and its config key still parses with a warning.

## Bug lints are on by default, style lints are opt-in

**Status:** built.

- lintr parity is not a goal. A lint is added only when it catches real bugs (as `length-test`
  did: every corpus hit was a bug) or when it fits the existing lint architecture and costs little.
  Lints already ported stay; nobody works through lintr's list for completeness.
- A lint is on by default when the code it flags behaves differently from how it reads
  (`equals-na`, `vector-logic`, `length-sequence`, `class-equals`, `trailing-comma`, `T`/`F`), and
  off when it only flags taste (`redundant-return`, `commented-code`, naming, shadowing). lintr turns
  both kinds on, which teaches users to ignore it; ry's defaults must stay clean on a tidy codebase.
- A fix that changes behavior on purpose (`x == NA` to `is.na(x)`, `&` to `&&`) is unsafe even when
  it is almost certainly what the author meant, because `ry check --fix` applies safe fixes unseen.
  `redundant-return` is safe: `return()` keeps its argument's visibility, so
  `return(x <- 1)` and a trailing `x <- 1` are both invisible.
- The lint fixtures render each fix (`safe fix a..b "text"`), so a fix's range and text are part of
  the contract.
- `unreachable-code`, `sprintf-arguments` and `length-test` are on by default (each is a bug; a
  CRAN corpus sweep found only true positives). `duplicate-argument` is opt-in although a repeated name is often a
  bug, because a callee that collects `...` keeps both on purpose (cli bullets in `c()`, repeated
  HTML `class` attributes) and a syntax lint cannot see the callee's formals. The semantic
  version already exists: with `[check] typing = true`, a call that gives a known function's
  formal twice is a `type-mismatch` (`named_argument_mismatch` in `check.rs`), so no second
  semantic lint is needed.
- The other lintr ports (`any-is-na`, `paste-sep`, `lengths`, `literal-coercion`,
  `redundant-equals`, `single-quotes`, `testthat-expectation`, `condition-message`,
  `comparison-negation`, `outer-negation`, `fixed-regex`, `which-grepl`, `length-levels`,
  `browser-call`, `numeric-leading-zero`, `line-length`, `implicit-assignment`, `while-true`,
  `scalar-in`, `order-subset`, `matrix-apply`, `empty-assignment`, `unnecessary-lambda`,
  `yoda-test`, `nzchar`, `sample-int`, `unnecessary-concatenation`) are opt-in taste or speed lints. `implicit-assignment` is opt-in because
  `while ((line <- f()) != "")` is idiomatic R; lintr's `cyclocomp`, `object_length` and
  `nested_ifelse` were skipped as low value without a fix.
  `line-length` is the only lintr layout linter ported, because the formatter never splits a
  line, so nothing else keeps lines short. lintr's whitespace and
  layout linters are not ported, because `ry format` owns layout.
- `trailing-comma` stays on inside rlang's dynamic dots (`mutate(d, y = 1, )`), where the comma
  is harmless: it is a warning whose safe fix only removes the comma, and stub `@masked` cannot
  mark dynamic dots because it also covers data.table verbs, whose `...` is not rlang's.
- lintr's `# nolint` comments are honored (`apply_suppressions` in `crates/ry/src/diagnostics.rs`),
  because 15 packages in the CRAN corpus carry them and the mandate is that existing code keeps
  working. A bare `# nolint` keeps syntax errors, and lintr names map to ry codes in `lintr_codes`;
  keep that table in step when a lint is ported. A `.lintr` file's `linters` field is read too
  (`apply_lintr` in `crates/ry/src/config.rs`): it fills only the levels `ry.toml` leaves at
  `Default`, so `ry.toml` always wins, and an unreadable field is a warning, never an error,
  because the file is lintr's. Only the `.lintr` at the project root (nearest `ry.toml`,
  `roughly.toml` or `DESCRIPTION`) applies, so the answer never depends on how a path is spelled.
  `lint_levels_mut` in `lints.rs` is the one name-to-field table, and
  a unit test pins it against the struct's fields.
- Call lints are gated in the green pre-check by `callee_may_lint`, which knows which enabled lint
  reads each bare callee, so common callees (`length`, `stop`, `which`) cost nothing while their
  opt-in lints are off. A fix whose replacement binds more loosely than what it replaces goes
  through `in_place_of`, which adds parentheses unless the position stands alone.

## `ry check` keeps an on-disk record of its findings

**Status:** built.

- **Measured first.** On 4 cores, a cold check of the 300,000-line finance bench takes about 1.45 s
  wall and 5 CPU seconds. Every pass already spreads over the cores: deriving the syntax facts used
  3.8 cores, naming 3.6, and the item check 3.7. Releasing the trees (0.05 s) and setup (0.05 s) are
  the only serial parts. More parallelism therefore has no headroom on this machine, and a cold run
  under 1 s needs about a third less CPU work, not more threads.
- **Previous shape.** Each run started from nothing. The salsa memos live only in memory, and
  salsa 0.28 neither exposes its dependency graph nor persists memos without serde on every value,
  including the global `Ty` table.
- **Target shape.** `.ry_cache/check.json` (`crates/ry/src/check_cache.rs`) records the findings,
  not the memos. Each file's record holds its content fingerprint, its surface fingerprint (the
  text without top-level function bodies, unless a body can write or declare outside its frame),
  the files its check read through interface edges, and the `NAMESPACE` imports it uses. The whole
  record is keyed by the ry binary (hashed by its bytes), the configuration, `NAMESPACE`,
  `DESCRIPTION`, the data files and the stubs. With no changed file, every finding replays. When
  every changed file keeps its surface, only the changed files and their dependents are checked.
  Anything else checks all files. A directory with no `ry.toml` or `DESCRIPTION` gets no record.
  The architecture page's "The check cache" section has the details.
- **Why the surface rule is sound.** One file's check reads other files without an interface edge
  only through name-keyed project-wide maps. Those are folds of per-file shares, so the surface
  fingerprints each share as `diagnostics::project_fold_shares` spells it, beside the text outside
  function bodies. A text rule alone was not enough: a `UseMethod` call or a `#: @type` inside a
  body feeds the S3-generic and type-declaration folds, and a review reproduced both as stale
  findings before the shares were added. Inferred schemes are the remaining body-dependent fact,
  and the recorded interface dependencies cover them. On all three benches, a file's dependency
  closure has a median size of 0, a 90th percentile of 1 and a maximum of 1.
- **Measured.** On the finance bench, a replay takes 0.10 s (57 MB peak), a body edit 0.45 s, and a
  cold run with the record written 0.1 s more than `--no-cache`. The data-science and biostatistics
  benches replay in 0.09 s and 0.07 s. The output is byte-identical to a run with `--no-cache`.
- **Floor of a partial run.** It still parses every file, because the project-wide folds read every
  file's item tree. It warms those folds on all cores, and keeps its trees instead of releasing
  them: releasing walks every item serially and cost more than it saved.
- **Checked.** With invariants on, every run that reuses findings is compared against a fresh run
  and panics on a difference. A seeded simulation drives that comparison with body and signature
  edits, `<<-`, `library`, `assign`, `setClass`, `UseMethod` and S3 methods, `#: @type` in and
  outside bodies, script files, and added and removed files
  (`cached_checks_match_fresh_checks_across_seeded_edits` in `cli.rs`).

## The data-table lint is opt-in and marks only constructors

**Status:** built.

- **Shape.** `[lint] data-table` (off by default, `data_table_lint` in `lints.rs`) flags
  `data.table()`, `fread()`, `as.data.table()` and `setDT()`, bare or `data.table::`, and names the
  DataFrame equivalent with a link to `reference/from-data-table.md`. A bare call is dropped when the
  file defines that name, exactly as `object-system` does.
- **Why constructors.** Like the object-system lint, which flags definitions rather than every
  `new()`, every data.table operation needs a table first, so the constructors mark where
  data.table enters the code without flagging each query.
- **Why off.** S4/R6 warn by default by user mandate; data.table has no such mandate, the
  benchmark code bases import it everywhere, and the guide says migrating is optional.

## The language server reads the check record

**Status:** built.

- **Previous shape.** The server started from nothing. Until the startup warm-up ended, opening a file
  showed only its parse-stage findings, so a large project looked clean for seconds.
- **Target shape.** At initialize the worker loads `.ry_cache/check.json` and keys it with
  `check_cache::key`, the one function `ry check` uses too. The key canonicalizes the root and the
  configuration's directory, because `ry check .` and the server spell them differently. The worker
  then runs `check_cache::plan` over `collect_r_files`, hashing loaded files from the database and
  reading the others from disk, and restores the findings of every loaded file the plan reuses
  (`Worker::restore_recorded_diagnostics`). `fast_diagnostics` serves them until the settled wave
  replaces them. Any input change clears them all, because one file's change can change another's
  findings.
- **Why not per-file invalidation.** Partial invalidation would need the dependency closure at every
  edit. The record is only a cold-start aid: after the first edit the live analysis is warm anyway.
- **Known difference.** The server analyses the package files and the open documents while `ry check`
  analyses every file, so a finding that depends on a script can differ until the settled wave.
- **Identity.** A record is keyed by the binary that wrote it. `WorkerSeed::executable` is
  `current_exe` in production; `Simulation::start_as` passes the scenario's `ry` binary, since the
  simulated server runs inside the test binary. `ClientOptions::cold` starts a simulated session
  before the warm-up, and the seeded sessions draw it.

## Literate documents keep their offsets, and the server keeps their original text

**Status:** built.

- **Previous shape.** `check` read `.Rmd`/`.qmd`/`.Rnw` through `syntax::literate`, which blanks prose to spaces byte for byte, but the server and `ry fmt` skipped them.
- **Analysis.** The engine still sees only the blanked text, so every byte offset is the document's own and no range table exists to drift. The chunks of one document form one script.
- **Positions.** The server keeps the original text per literate file (`Worker::literate_documents`) and converts LSP positions against it, because a blanked multibyte character has a different UTF-16 width. Each `did_change` applies the edits to the original and converts the whole buffer again; conversion is a linear scan, so per-edit conversion would add a second source of truth for no measurable gain.
- **Formatting** is per chunk (`format::format_chunks`), returning one edit per changed chunk, so prose, fences and inline code are never rewritten. An indented chunk (inside a list item) is skipped, because re-indenting it would need the list's indentation. Range formatting is off for literate files.
- **Completion** answers only inside an R region (`syntax::literate::r_regions`), so typing prose shows nothing. Code lenses other than reference counts are dropped, because "Run file" and "Run test" mean nothing for a report.
- **VS Code** matches literate files by glob, because their language id depends on which extension is installed.

## A running console is a debug adapter, a data source, and a live-value source on one socket

**Status:** built.

- **Previous shape.** The debugger existed only as `ry debug-adapter`, which launches a fresh script; a `ry run` console had no editor connection, so `debug()`, `View()` and the editor's hover knew nothing of the session.
- **Transport.** `ry run` listens on a Unix socket in `$TMPDIR/ry-<uid>` (0700, owner and mode verified by both ends) and writes `console-<pid>.json` naming its cwd and socket. Discovery is a directory read, so nothing extra runs at start-up beyond one accept thread; a reader thread per connection queues events, and the interpreter thread answers them (`interp::console_server`). Framing is DAP's `Content-Length`, so one parser serves the debugger, the viewer and the language server.
- **When requests are answered.** At the prompt, input is read on a persistent input thread (`console_server::read_line`) while the interpreter thread answers every request. The thread owns the console's reader and reads one line per prompt that asks; a line stays owed until the interpreter takes it, so a prompt whose wait ended early (an editor took a `Browse[1]>` stop over) hands its pending line to the next prompt instead of reading ahead; that next prompt is then written to the terminal, since the thread is still showing the old one. A line typed while a command runs goes to `STASHED_INPUT` and the queue keeps draining, so an editor's `pause` behind it is still answered. `browser()` inside R code run for an editor's request at the idle prompt does not stop (`SERVING_REQUEST`), because the top-level read owns the terminal. An editor that attaches while a Browse stop's expression runs keeps its adapter: `with_adapter` drops the finished stop's adapter instead of writing it back. A per-prompt scoped helper thread could not do that, because the scope had to join before the prompt returned. Every request queue is bounded at 64 queued requests per connection: the socket thread blocks, so a flooding client stalls on its own socket buffer and nothing is dropped.
- **Viewer sort and filter are row numbers, not copies.** A `ryTable` request carries `filter` and `sort`; the view caches the zero-based row numbers they leave (`viewer::Order`) and pages through them, recomputing only when the pair changes. A ry `DataFrame` computes them in the table engine (`table::ops::view_rows`, which reuses `sort_frame`'s radix path on a two-column frame of the key and the row numbers) and gathers a page with `take_rows`; an R object computes them with `grepl()` and `order(na.last = FALSE)` in a base-scoped environment. Both put `NA` first in either direction, keep ties in row order, and let `NA` match no search, so the two engines show the same rows.
- **`browser()` with no editor drives the same adapter from the terminal.** An interactive console installs `Adapter::for_console` over `console_server::Browse`, a transport that turns GNU R's browser commands into DAP requests (`c` is a disconnect, `n`/`s`/`f` are next/stepIn/stepOut, `Q` is terminate, other lines are an `evaluate` with context `ryBrowse`, which autoprints) and renders responses as text. Stepping rules therefore exist once. An editor `initialize` arriving at the prompt replaces the terminal inside the transport, and `configurationDone` re-sends the `stopped` event. The adapter ends with its command while the terminal drives it (`Adapter::is_browse`), but stays attached once an editor took over. A forced `browser()` stop needs no srcref, and only error stops hide sourceless frames. Scripts (`ry run file.R`) install no reader, so `browser()` stays a no-op there, as in non-interactive GNU R. While a command runs, `poll_interrupt` services the queue: debugger requests are answered (pause, breakpoints), anything that would evaluate R is refused as busy, because it would interleave with the user's code. While stopped, the debugger's own receive loop answers other connections inline.
- **Attach** installs the same `debugger::Adapter` the launch path uses, with `console = true` so disconnect detaches instead of exiting, and reloads the workspace packages with srcrefs so package breakpoints hit. Only one debugger connection at a time.
- **`debug`/`debugonce`** set closure gp bits and enter `Mode::FunctionEntry`, stopping with reason "function breakpoint". Code typed at the prompt has no file, so its frames get a `sourceReference` the client fetches with `source`.
- **Data viewer.** `ryView` evaluates an expression (in a frame when stopped, else in the global env) and registers a view that preserves the object; `ryTable` returns one page of formatted rows, at most 1000; `ryRelease` drops it, and at most 32 views live at once. The data never leaves the runtime whole, so a million-row table costs one page. Variables carry `evaluateName` so the editor's "View Data" names the exact value. `View(x)` sends a `ryView` event to connections that sent `rySubscribe`; VS Code subscribes when it opens or attaches to a console.
- **Live values in the language server** come from `ryGlobals` (names, class, summary, columns, promises unforced) with a 150 ms timeout (`ry::console_link`). Analysis stays the source of truth: the console only adds hover lines and completion items analysis lacks, and the simulator disables the link to stay deterministic.

## A parameter and its named arguments are one editor binding

**Status:** built.

- Before, a named argument (`f(rate = 1)`) was no navigation target: hover widened to the whole
  call, goto and references found nothing, and renaming the parameter left every call site's
  `rate =` behind, so the rename broke the code.
- Now `ide::target_at` maps a named argument to the parameter's `Target::Slot` in the callee's
  item (the callee resolves the way any name read does, `expression_target`), and the occurrences
  of a parameter slot add the named arguments at every call of its function. Those calls come
  from the function binding's own occurrences (the global walk for a top-level function, the slot
  walk for a local one), so a different function with a parameter of the same name is untouched.
- Nothing new is stored: call sites are derived on request from the existing occurrence walks, so
  no project-wide map gains a dependency and incremental behavior is unchanged.
- A library callee has no parameter site, so its named argument goes to the stub's function and
  hover shows the signature-help label of the parameter. Partial matching is not followed, on
  purpose: it would make a rename guess.

## R's dynamic help server runs on the interpreter thread, only while R waits

**Status:** built.

- **Previous shape.** `startHTTPD` failed on purpose, so `help(help_type = "html")`, `help.start()` and `tools::startDynamicHelp()` reported that the help server was not implemented.
- **Shape.** `interp::help_server` binds the listener and runs an accept thread plus one thread per connection. Those threads only parse HTTP and queue an exchange; the R handler (`tools:::httpd`, or `tools:::.httpd.handlers.env[[name]]` for `/custom/<name>`) runs on the interpreter thread, which answers the queue and hands the response back over a channel. No R object crosses a thread. The handler runs as under GNU R's `R_ToplevelExec`, with none of the waiting code's condition handlers or restarts in reach (they are parked in `Interp::suspended_handlers`, where the collector sees them), and a Ctrl-C during it reaches the waiting code once the response is sent. `stopHTTPD` shuts every open connection down, and the server bounds request heads (1 MiB) and bodies (64 MiB) and closes idle connections.
- **When requests are answered** follows GNU R, which serves help only from its event loop: at the console prompt (the `console_server` idle loop, woken through `console_server::wake`), in `Sys.sleep`, in `process.events`, and while an HTTP download runs on its transfer thread, so a session can fetch its own help pages through `url()` or `download.file`. A running command never interleaves with a handler. Off Unix the console idle loop does not serve yet.
- **Wire format.** Request decoding (path percent-decoding that stops at the first malformed escape, the query vector, the raw body with its `content-type`, the `Request-Method:` header block) and the response rules (error 500 forms, status reason "Code N", HEAD, HTTP version echo, `startHTTPD` return codes 0, -1 and -2) were probed against GNU R and are pinned by `crates/oracle/cases/tools__help_server.R`.
- **Impact.** The handler runs exactly as on GNU R, so every Rd page renders through R's own `tools` code; a sweep of every Rd file in 14 default packages through `Rd2txt`, `Rd2HTML`, `Rd2latex` and `Rd2ex` matches GNU R byte for byte apart from the render timestamps.

## Completion, signature help and import fixes reach untyped and installed code

**Status:** built, except installed formals passed as a callback, which "Packages without stubs are read from the R library by the host, lazily" supersedes.

- **Installed formals are a callback.** `ide::completion_with` and `ide::signature_help_with` take `&mut dyn FnMut(&PackageTarget) -> Option<Vec<Formal>>`; the server and `analysis-stats` pass `InstalledPackages::formals`, which parses the definition out of the generated package source (`crates/ry/src/installed.rs`). Reading the R library is I/O and not salsa-tracked, so it stays out of `ide`. Untyped project functions and R6 methods (`fields::field_function`) read formals and default text from the HIR and syntax instead, and a function with a type or stub scheme keeps the typed signature.
- **Use counts are chunked.** Completion ranks by project use counts. One tracked query over all files re-summed every file per keystroke (p99 about 60 ms on finbench), so `chunk_uses` counts 32 files per tracked query and completion sums the chunks: an edit recomputes one chunk.
- **Import fixes read the export manifests, not `stubs()`.** `stubs()` skips conditional and qualified-only namespaces the project never uses, which are exactly the ones an "unresolved name" fix must suggest. `stubs::exporting_namespaces` indexes every manifest (`types/*.exports`) by name. The fix offers `pkg::name` and, outside a package, `library(pkg)` after the last top-level attach call.
- **Header inlay hints reuse the annotate edits.** A function literal's hints are the `inline_annotation_edits` the "Add inferred type annotation" action inserts, so the hint and the inserted text cannot disagree.

## Any save refreshes every open document's diagnostics

**Status:** built.

- **Shape.** `Worker::did_save` (`crates/ry/src/server.rs`) refreshes every open document, whether or not the saved path is open.
- **Why.** A save of `NAMESPACE`, a stub, or a file no buffer shows changes what other documents see, and the server had skipped the refresh for a path it did not hold open, so open documents kept stale diagnostics until their next edit. The simulator found it; `any_save_republishes_documents_that_an_edit_elsewhere_changed` pins it.

## A call-hierarchy item answers only in its own document

**Status:** built.

- **Shape.** Incoming and outgoing calls resolve the item and answer nothing if the name resolves to a function in another file (`server.rs`, the hierarchy item lookup).
- **Why.** An item's call ranges are relative to its own document; an edit can move the definition its name resolves to, and answering from the other file returned ranges in the wrong text.

## The console test waits on a count of handled closes

**Status:** built.

- **Shape.** `console_server::closed_connections` and `wait_for_closed_connections` (`crates/interp/src/console_server.rs`) count the debugger connections the console has finished closing, behind a mutex and condition variable. `test_console_server` waits for its own count instead of polling with a sleep.
- **Why.** Polling bounded by a wall-clock budget fails on a loaded machine and breaks the rule that no harness reads the clock.

## Refactorings refuse rather than guess, and a seeded property check holds them to it

**Status:** built.

- `ide::refactors` (`crates/ide/src/refactor.rs`) offers extract variable, extract function,
  inline variable and annotation conversion. Each one is offered only where the rewrite keeps
  evaluation: extract variable refuses a statement that would evaluate the expression repeatedly,
  conditionally or never (loop or function bodies, `if` branches, the right of `&&`/`||`), and
  `is_captured` refuses code under a formula or a quoting call. Extract function refuses `return()`,
  `<<-`, `...` and frame-inspecting calls. Inline refuses when a read name is reassigned before the
  last use or a call would run more than once.
- Calls that do not evaluate their arguments once, in order, in the caller are classified by
  `argument_evaluation`: *captured* (quoting, data masking such as `with()` and `dplyr` verbs,
  and the index of a `[`, for data.table) blocks every refactoring; *guarded* (condition
  handlers, `switch`, `withr::with_*`) blocks hoisting a variable out but allows extracting a
  function that is called in place. `evaluated_first` is the one rule for hoisting and for
  inlining a value with a call: every node between the expression and its statement evaluates it
  exactly once, and nothing before it in the statement calls or assigns. A value that assigns is
  never inlined (`x <- y <- 0`). Extract function treats a read anywhere in the enclosing loop as
  later code, because the next pass runs it. These came from review, and the property check
  cannot see them (the result parses and resolves), so each is pinned in
  `crates/ide/tests/refactor`.
- Text that only parses as one expression inside its old context is parenthesized
  (`needs_grouping`: a `?`, or a reparse that is not exactly one node), because `value <- d?f`
  and a multi-line operand chain change meaning when moved out.
- Two edits at one offset are merged into one: LSP clients disagree on the order of same-offset
  inserts, so a definition inserted at the statement start plus a replacement there must be one
  edit.
- `ide::testing::check_refactor_invariants` (driven by `crates/ide/tests/test_fuzz.rs`) applies
  every action at every node range and asserts determinism, non-overlapping edits, a clean reparse
  and no new unresolved names. It runs without stubs: with them one seed took 17 s instead of
  1.3 s, and unresolved-name reporting does not need them. The parse and resolution invariants run
  only on input that parsed cleanly, because mutated input already has errors the rewrite cannot
  be blamed for.

## Call-head completion ranks by call counts, without test files

**Status:** built.

- **Before.** Completion ranked equally good matches by how often the name is spelled in the
  project, so at two characters on a call head testthat's `expect_*` (spelled in every test)
  beat the package's own functions, and S3 methods (`print.purrr_rate`) crowded out functions.
- **Shape.** The per-file name-use fold (`SpelledNames`) also counts names in call-head position
  (directly or as `pkg::name`). A call-head completion ranks project S3 methods (a `generic.class`
  beside a `generic` candidate) last, then by calls, then spellings. Call counts leave out test
  files unless the cursor is in one (`chunk_uses` takes `with_tests`); spelling counts keep them,
  since tests show which `pkg::` members a project uses (dropping them cost httr2's `pkg::name`
  role 9 points). Two characters on call heads went from 68% to 72% on httr2 and 74% to 76% on
  purrr.
- **Rejected:** ranking by words shared with the enclosing function (`check_rate` in code that
  has a `rate`). It gained a point or two at half the name and lost six to eight at two
  characters, where long names share some word with almost any function.

## Call hierarchy and workspace symbols are measured against the reference oracle

**Status:** built.

- `ry debug analysis-stats` checks incoming and outgoing calls of a sample of project top-level
  functions (defined exactly once) against the independent lexical resolver that already checks
  references: every oracle global site that is a call head is an expected call, grouped by the
  enclosing top-level definition. Workspace symbols are checked by querying each sampled
  definition's exact name and a half-length prefix (at least 3 characters); a prefix that
  matches more names than a search lists is left out of the share, since a user types on.
- Workspace search reads a memoized per-file list (`searchable_symbols`), because rebuilding
  every file's outline per query took about half a second on finbench.

## Completion pushes down names whose declared type cannot fit, and never promotes

**Status:** built.

- **Before.** Completion ranked equally good matches by source and use counts only, so in
  `if (is_|)` a helper declared `-> character` ranked beside one declared `-> logical`.
- **Shape.** `expected_type` in `crates/ide/src/ide.rs` reads what the code around the cursor
  wants: a logical for an `if`/`while` condition and an operand of `!`, `&&` or `||`, and for an
  argument the parameter type its global or stub callee *declares*. It reads the declared
  scheme, not the call's instance, because the instance took its parameter types from the
  argument being typed (`print(i)` made the slot `integer` and pushed `iconv` down), and it
  unions the parameter across every stub overload, because the argument picks the overload only
  once it is written (`lapply`'s first overload takes a named list, which pushed `seq_along`
  down). A candidate is compared by coarse value shape (`type_mismatch`), with R's
  logical-to-integer-to-double widening allowed, and a known mismatch sorts after the fits within
  the same match score and source group. Inferred function types are ignored on both sides,
  since they say how a body ends, not what callers may pass.
- **Rejected:** promoting candidates that fit. Project helpers with an inferred `logical` result
  then outranked `is.na` and friends, and two characters on call heads lost 2 points on purrr and
  1.3 on R6.
- **Impact.** Neutral on httr2, purrr and R6 and +0.3 points on finbench at two characters,
  with completion still about 2 ms median on the packages and 8 ms on finbench; the gain is in
  annotated code, which the `completion_expected_type` fixtures pin.

## The console's crossterm is patched to drain the terminal, and R reads console input one line per chunk

**Status:** built.

- **Previous shape.** The console used upstream crossterm, whose mio reader returns after the first event it parses from an edge-triggered read. The rest of a paste stayed in the reader's buffer until the next readiness edge. reedline then asked for the cursor position and waited its two-second timeout, so a pasted block stalled.
- **Target shape.** `patches/crossterm` drains the terminal before it returns an event. The `use-dev-tty` reader is not a substitute: its `poll(Duration::ZERO)` reports no event while bytes are buffered, which breaks reedline's event batching and `take_editor_events`. The console enables bracketed paste (`use_bracketed_paste(true)`).
- **One line per chunk.** R discards the rest of its input buffer when a command raises an error, so the console hands R one line per read (`take_chunk` in `crates/repl/src/console.rs`) and keeps the rest in `pending`. Code a script or a hook queues goes to R as one unit. A read below top level (`readline()`, `menu()`, the browser) is answered from the console, never from `pending`, and Ctrl-C clears `pending`.
- **Preflight.** `InputFilter::discard` retracts a command the preflight refused, so the session's analysis never holds a command R did not run. The preflight counts a command as ry syntax when it holds an annotation, a `typing:` directive, a ry literal or a qualified `Module.function` name. A `Module.function` counts only when a native namespace declares that function, so `Sys.time` and the S3 method `Ops.money` do not (`crates/ry/src/preflight.rs`).
- **Tests.** `crates/ry/tests/test_repl_e2e.rs` pins a multi-line paste, an error in the middle of a paste, a nested read during a paste, Ctrl-C with input pending, and a refused command.
