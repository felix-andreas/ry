# Decisions: Editor and language server

The language server, editor features, the formatter, lints, the check command's reporting, and the console and debugger.

## Beta semantics: A reported column counts characters, and caret art counts terminal cells

**Status:** built.

- **A column counts characters**: the JSON `column`/`endColumn`, the server's `file:line:column` strings and the CLI report header (`LineIndex::line_column_chars`). A byte column matches no editor and sat the caret right of the code on non-ASCII lines, which R strings carry as soon as they hold a name or unit. The JSON field docs are a stated contract.
- **Underline art counts terminal cells**, because a CJK character or emoji occupies two cells and a character count under-pads. The graphical reporter measures cells.
- **LSP converts to the negotiated encoding** (UTF-16 by default) at the protocol edge, as LSP specifies.

## Beta semantics: The CLI reports through miette

**Status:** built.

- **Shape.** One report type in the CLI (severity, optional code, message, optional cause, optional snippet of a named source and labelled range, nested related reports), drawn by miette's `GraphicalReportHandler`, serves findings, notes and bare failures alike. An error that knows its source implements `miette::Diagnostic` (`ConfigError` carries the config text and toml span). Colour and unicode on a terminal, monochrome unicode when colour is refused, plain ASCII into a pipe. This replaced three hand-rolled renderers with three notions of where a message points. A related location is drawn from its own file, not reduced to a coordinate, and `RelatedNote` resolution runs in the sequential render step, so a parallel worker returns a plain diagnostic.
- **Performance.** The reporter is built once in a `LazyLock`. The snippet source is a custom borrowed `SourceCode`, not `NamedSource`, which owns its text and would copy the whole file into every finding.
- **Two miette gaps patched.** miette counts the header column in bytes, so `read_span` re-counts in characters (else the human header and JSON disagree). It draws every line a range covers, so a range over three lines is clamped to its first line with the reach in the label: a finding on a long item must not print the item.
- **Theme.** A snippet is a window, not a box: unbroken gutter (`vbar_break = vbar`), `ltop = hbar`, `underline = '^'`, and no closing rule (one line saved per finding). The closer is not themeable (miette builds it from `lbot` and `hbar`, which arrows and span brackets also use), so it is filtered out of the rendered string, matching only a line that is nothing but the rule, because a nested report indents its own. miette 7.6 hardcodes the rule's width; `check_draws_the_snippet_as_a_window` fails if an upgrade moves it.
- **Incremental analysis untouched**; the JSON Lines contract is separate.

## Beta semantics: The annotation formatter parses, then pretty-prints

**Status:** built.

A `#:` block is joined, parsed with the real annotation type parser, and pretty-printed with per-bracket hug bits honoring `indent_width`. An opener followed by content on its line stays hugged and its closer mirrors it. On a parse failure the block is left verbatim, so prose is never corrupted. Fully expanded and hugged styles are both fixed points; a mixed shape normalizes.

## Beta semantics: Config subsystem

**Status:** built, except the unknown-key error: an unknown key is a warning and the configuration loads.

- The workspace root comes from `InitializeParams`, never the process working directory.
- Discovery finds the nearest configuration file in an ancestor directory, identically in the LSP and the CLI.
- A config problem surfaces as a diagnostic on the configuration file plus a window message, never as a startup panic. An unknown key is a warning and the configuration loads.
- A config reload triggers a diagnostics refresh. One config struct chain runs end to end.

## The Zed extension versions on its own line

**Status:** superseded by "Releases are built natively per platform, with the runtime beside the binary" ("One version per release"): the `justfile` writes the CLI's version into the Zed manifest and `just release-check` requires the match. Kept for why the Zed extension locates a binary and bundles none.

The Zed extension bundles no binary: it locates `ry` at run time through the LSP settings path, then `PATH`, then the latest GitHub release. The VS Code extension bundles the binary, so its manifest carries the CLI's version with the prerelease suffix stripped, because the marketplace rejects `X.Y.Z-alpha`.

## Editor features for ry's own syntax

**Status:** built.

- **The checker records pattern types.** `ItemCheck::pattern_types` maps each `match` pattern to the type it is matched against, a per-item result, so hover and completion in patterns add no project-wide dependency.
- **A tag is a project-wide structural symbol.** `:Ok` in two types is one tag, so references and rename cover every spelling in the project, and go-to-definition goes to its listing in a named type or the stdlib stubs. A tag spelled in `types/std.Rtypes` refuses to rename, because the stub would no longer match.
- **Type parameters navigate to their binder**, scoped to it, so `T` in two functions is two symbols.
- **Semantic tokens are an `ide` query** (`semantic_tokens`, `classify_type_token`); the server only maps `SemanticClass` to its legend. They color only what an R grammar cannot classify (parameters, both declarations and reads that resolve to a parameter binding, call heads, the package of `pkg::name`, the left side of a function assignment, `match`, `=>`, tags), so tokens add facts to the editor's grammar rather than replace it. Range requests are answered, so a large file pays only for the viewport.
- **Member names are `property` or `method` tokens** from syntax alone for `x$name`, `x@name`, record-literal and pattern fields (a called member is a method). R6 members and S4 slots come from the outline's `classify_symbol_call`, so one definition of an R6 member or S4 slot serves outline and highlighting.
- **A quick fix lives on the diagnostic** (`Diagnostic.fix` in `crates/semantics/src/diagnostics.rs`, `push_lint`'s fix argument in `lints.rs`), so `ide` never re-derives a finding. The server reads fixes from what it publishes, after suppressions, so a suppressed finding offers no fix; `ry check --fix` reads the same field.
- **Outline sections and folds come from RStudio section comments** (a comment alone on its line ending in four or more `-`, `=` or `#`), ending at the next header of the same or higher level in the same block, matching RStudio's outline.
- **An inlay hint states what the code does not.** A payload-free tag or `@Name(...)` constructor spells its type, so it gets no hint or "add annotation" action; a vector of records, tuples or tags (`Position[]`) is dropped from a hinted union because no such value exists.
- **A type error on a native literal blames the one differing component** (`native_component_mismatch` in `crates/semantics/src/check.rs`), recursing into nested literals and payloads; with more than one differing component it keeps the whole literal, since no single spot is the cause.
- **Tests:** `crates/ide/tests/ide/records-and-tags.R.test`, `frames.R.test`, the typing suite's `mismatch_blame` group.

## Editor responsiveness at 300,000 lines

**Status:** built.

- **Budget.** Keystroke-to-diagnostics p50 at most 30 ms and p95 at most 100 ms at 300,000 lines, pinned by `crates/semantics/tests/test_perf.rs` (`just perf`, needs the fetched corpus).
- **Per-file name indexes.** `global_names` and `spelled_names` (names after `$`, `@`, `::`) in `crates/ide/src/ide.rs` are tracked per file and derived from the item tree and syntax, so they cannot drift and an edit recomputes one file's entry. Completion ranks with `select_nth_unstable_by` and non-allocating case-insensitive comparison and resolves call shapes only for returned results. References and rename skip a file, then an item, whose text does not contain the name, because every occurrence spells it. Walking every item and file per request cost completion 30 ms p50 and references 300 ms p99 on finbench.
- **Requests preempt idle work** (`CancelHandle::preempt_idle` in `crates/ry/src/server.rs`), never a sync.
- **Project-wide maps fold per-file memos** (`file_definitions`, `file_conditional_writers` in `crates/semantics/src/semantics.rs`), so confirming a map after a keystroke checks about 500 file memos instead of about 56,000 item memos (about 35 ms before any request could answer). `project_arithmetic_classes` reads only `package_definitions`.
- **Durability trap.** A setter that names no durability keeps the field's current one, so every place that creates or rewrites a `SourceFile` names it. Salsa never backdates a memo whose durability drops, so: every host input except the open documents and the file list is `MEDIUM`; `ProjectFiles` is `LOW`, which keeps the maps `LOW` before any first edit (otherwise a file's first edit re-runs every item check, about 2 s on finbench); `didOpen` skips text that matches the disk and lowers the file to `LOW` in idle time. A fold read only by editor requests is re-derived in idle time (`Worker::editor_folds_stale`). `test_incremental.rs` pins that an edit verifies only the edited file's items and a first edit re-checks only its own.
- **Measurement tools must match.** `ry debug analysis-stats` and `crates/ry/examples/keystroke_probe.rs` set the same durabilities as the server, or they measure a different invalidation pattern.

## A parallel startup warm-up in the server

**Status:** built.

- **Shape.** `Worker::warm_up` in `crates/ry/src/server.rs` runs before other idle work: it scans every file's attach facts across all cores and applies the attach union once, then derives the per-file folds behind the project-wide maps (`warm_project_folds`) across all cores. Until it ends the first wave holds only parse-stage diagnostics (lints need the maps), pull diagnostics answer the same, and settled publishes wait; its end refreshes every open file once. The rest of the cold check runs in parallel batches of 64 files; `Worker::in_parallel` serves both. Applying the union once avoids republishing open files on every union change (about six times, with flickering counts). Priming one file at a time derived every item's naming on one thread, about 1.3 s at 300,000 lines before any diagnostics; with the warm-up a file opened at startup gets its first wave in under 15 ms and settled diagnostics about 0.55 s after startup on dsbench.
- **Stopping parallel work.** Threads run on cloned handles that `CancelHandle` cancels, and also poll a queued-job counter between files, because salsa resets a handle's cancellation flag when a top-level query returns (`DbGuard::drop`), dropping a flip that lands at a query's end. Without the counter an open sometimes waited for the whole warm-up.

## A fix says whether it may be applied unseen

**Status:** built.

- `Fix.applicability` (`diagnostics.rs`) is `Safe` when the edit keeps what the code means (lint rewrites, syncing a `#:` comment to the governing inline type, removing an unreachable arm) and `Unsafe` when it guesses intent or silences a finding without handling it (spelling suggestions, `stop()` arms). ruff's model. `ry check --fix`, the preferred quick fix and `source.fixAll` apply only safe fixes, because nobody reviews them; `--unsafe-fixes` and a click opt into the rest. CLI and server share `applicable_fixes`/`apply_fixes`.
- `ry check --fix` repeats until no fix changes a file (at most `MAX_FIX_PASSES`), because overlapping fixes are skipped within a pass and one fix can expose another. It refuses a file whose disk text no longer matches the analyzed text, and leaves a file with a syntax error alone.
- Safe means safe in context: `boolean-shorthand` holds only while the file binds no `T`/`F` (the lints' `unless_redefined` channel), and the `#:` sync fix is withheld when the inline type names a type parameter the comment would not bind.
- The server keeps each open document's settled fix-bearing findings with their text (`published_fixes` in `server.rs`), so `codeAction` per cursor move costs no analysis and offers exactly the findings on screen.

## Parameter hints and call hierarchy follow R's own call rules

**Status:** built.

- Parameter-name inlay hints map arguments with the checker's own `argument_targets` (`check.rs`), so hint and type check never disagree. Hints stop at a forwarded `...`, skip a piped value, show only with at least two positional arguments, and skip one-letter parameters and arguments whose text already names the parameter (`years`, `x$years`): a hint repeating the screen is noise.
- The call hierarchy covers project top-level functions only (`package_definitions`, already per-name tracked). Only call heads count, so passing `f` to `lapply` is a reference. A top-level-statement caller is grouped as "(top level)". The server re-resolves a function item from its uri and selection start instead of serializing ids into `data`, so a stale item never names a wrong function; a "(top level)" item does not re-resolve.

## The debugger lives inside the interpreter

**Status:** built, except the debugger-only source references for package code, which "Lazy definitions keep source references" supersedes.

- **Shape.** The DAP adapter is `interp::debugger`, inside the interpreter, not a separate attaching process. `ry debug-adapter` (`crates/ry/src/debug_adapter.rs`) only frames stdio messages and reaches it through libR's `ry_debug_adapter`, because the interpreter lives in `libR.so`. A reader thread feeds a channel so the runtime polls for `pause` between statements without blocking on stdin.
- **Hooks.** Statements are located by the srcrefs `{` already sets (`Op::SetSrcref`, `do_begin`/`eval_all`). Without an adapter the hook costs one `Option` check per statement. `SetSrcref` carries a flag because the one restoring the outer srcref after a block starts no statement and must not stop. `browser()` and the uncaught-error handler stop through the same entry.
- **Rooting.** Every object the variables view hands out a reference for is rooted until resume, because the debug console can drop its last other reference.
- **Stdout.** The protocol owns stdout, so the adapter keeps a duplicate and points fd 1 at stderr; native `printf` cannot corrupt the protocol.
- **Tests:** `frames` `test_debugger` (transcripts and a seeded random client) and the `debug-*.scenario` journeys, which run the real `ry debug-adapter`.

## Range formatting is a cut of the whole-file format

**Status:** built.

- `format::format_range` formats the whole file, pairs source and formatted trees statement by statement, and returns the formatted text of the touched statements. Formatting a selection on its own let selection and file disagree (indentation, auto-bracing, comment attachment); with the formatted file as the single source of truth, restriction and additivity hold by construction, checked by `check_range_format_invariants` (also a fuzz target). A whole-file format per request is cheap next to analysis.
- On-type formatting reuses it: a line break formats the finished line, `}` the construct it closes. Both are advertised unconditionally; the old experimental-features config key parses with a warning.

## Bug lints are on by default, style lints are opt-in

**Status:** built.

- **lintr parity is not a goal.** A lint is added only when it catches real bugs (every corpus hit of `length-test` was a bug) or fits the lint architecture cheaply; lints already ported stay, and nobody ports lintr's list for completeness.
- **On by default** when the flagged code behaves differently from how it reads (`equals-na`, `vector-logic`, `length-sequence`, `class-equals`, `trailing-comma`, `T`/`F`, `unreachable-code`, `sprintf-arguments`, `length-test`; a CRAN corpus sweep found only true positives for the last three). **Off** when it only flags taste or speed (`redundant-return`, `commented-code`, naming, shadowing and the other lintr ports). lintr turns both on, which teaches users to ignore it; ry's defaults must stay clean on a tidy codebase.
- `duplicate-argument` is opt-in: a callee collecting `...` keeps repeats on purpose (cli bullets, HTML `class`), and a syntax lint cannot see formals. With `[check] typing = true` a known function's repeated formal is already a `type-mismatch` (`named_argument_mismatch` in `check.rs`).
- `implicit-assignment` is opt-in because `while ((line <- f()) != "")` is idiomatic. `line-length` is the only layout linter ported, because the formatter never splits lines; `ry format` owns all other layout. lintr's `cyclocomp`, `object_length`, `nested_ifelse` were skipped as low value without a fix.
- A fix that changes behavior on purpose (`x == NA` to `is.na(x)`, `&` to `&&`) is unsafe even when almost certainly intended, because `--fix` applies safe fixes unseen. `redundant-return`'s fix is safe: `return()` keeps its argument's visibility, so `return(x <- 1)` and a trailing `x <- 1` are both invisible.
- `trailing-comma` fires inside rlang's dynamic dots too (harmless there; the safe fix only removes the comma), because stub `@masked` also covers data.table verbs, whose `...` is not rlang's.
- **lintr compatibility.** `# nolint` comments are honored (`apply_suppressions` in `crates/ry/src/diagnostics.rs`; lintr names map through `lintr_codes`, keep it in step when porting), because 15 packages in the CRAN corpus carry them. A bare `# nolint` keeps syntax errors. A root `.lintr`'s `linters` field fills only levels `ry.toml` leaves at `Default` (`apply_lintr` in `crates/ry/src/config.rs`), so `ry.toml` always wins; an unreadable field is a warning, because the file is lintr's. Only the `.lintr` at the project root (nearest `ry.toml`, `roughly.toml` or `DESCRIPTION`) applies, so the answer never depends on path spelling. `lint_levels_mut` in `lints.rs` is the one name-to-field table, pinned by a unit test.
- **Cost.** Call lints are gated by `callee_may_lint`, so common callees cost nothing while their opt-in lints are off. A replacement that binds more loosely goes through `in_place_of`, which parenthesizes unless the position stands alone. Lint fixtures render each fix (`safe fix a..b "text"`), so range and text are contract.

## `ry check` keeps an on-disk record of its findings

**Status:** built.

- **Why a record.** On 4 cores a cold check of the 300,000-line finance bench takes about 1.45 s wall and 5 CPU seconds, with syntax facts, naming and the item check each using 3.6 to 3.8 cores and only setup and releasing trees (0.05 s each) serial, so more parallelism has no headroom: a cold run under 1 s needs about a third less CPU work. Salsa 0.28 neither exposes its dependency graph nor persists memos without serde on every value, including the global `Ty` table, so the record stores findings, not memos.
- **Shape.** `.ry_cache/check.json` (`crates/ry/src/check_cache.rs`); details in `docs/src/content/docs/development/architecture.md`, "The check cache". Per file: content fingerprint, surface fingerprint, the files its check read through interface edges, and the `NAMESPACE` imports it uses. The whole record is keyed by the ry binary's bytes, the configuration, `NAMESPACE`, `DESCRIPTION`, data files and stubs. No changed file: every finding replays. Every changed file keeps its surface: only changed files and dependents are checked. Otherwise all files. No `ry.toml` or `DESCRIPTION`, no record.
- **Why the surface rule is sound.** A file's check reads other files without an interface edge only through name-keyed project-wide maps, which fold per-file shares, so the surface fingerprints each share as `diagnostics::project_fold_shares` spells it, beside the text outside function bodies. Text alone was not enough: a `UseMethod` or `#: @type` inside a body feeds the S3-generic and type-declaration folds (reproduced as stale findings). Inferred schemes are covered by the recorded interface dependencies. On all three benches a file's dependency closure has median size 0 and maximum 1.
- **Measured** on the finance bench: replay 0.10 s (57 MB peak), a body edit 0.45 s, a cold run writing the record 0.1 s more than `--no-cache`; the other benches replay in 0.09 s and 0.07 s.
- **Floor of a partial run.** It still parses every file, because the project-wide folds read every item tree, warms those folds on all cores, and keeps its trees: releasing walks every item serially and cost more than it saved.
- **Checked.** With invariants on, every reuse is compared against a fresh run and panics on a difference; `cached_checks_match_fresh_checks_across_seeded_edits` in `cli.rs` drives it with seeded body and signature edits, `<<-`, `library`, `assign`, `setClass`, `UseMethod` and S3 methods, `#: @type` in and outside bodies, script files, and added and removed files. Output is byte-identical to `--no-cache`.

## The data-table lint is opt-in and marks only constructors

**Status:** built.

- **Shape.** `[lint] data-table` (`data_table_lint` in `lints.rs`) flags `data.table()`, `fread()`, `as.data.table()`, `setDT()`, bare or qualified, and names the DataFrame equivalent with a link to `reference/from-data-table.md`. A bare call is dropped when the file defines that name, as in `object-system`.
- **Why constructors.** As `object-system` flags definitions rather than every `new()`: every data.table operation needs a table first, so constructors mark where data.table enters without flagging each query.
- **Why off.** S4/R6 warn by default by user mandate; data.table has no such mandate, the benchmark code bases use it everywhere, and migrating is optional.

## The language server reads the check record

**Status:** built.

- **Shape.** At initialize the worker loads `.ry_cache/check.json` keyed by `check_cache::key` (the same function `ry check` uses; it canonicalizes the root and config directory, because `ry check .` and the server spell them differently), runs `check_cache::plan` over `collect_r_files` (hashing loaded files from the database, others from disk), and restores reused findings (`Worker::restore_recorded_diagnostics`). `fast_diagnostics` serves them until the settled wave, so a large project does not look clean for seconds at startup. Any input change clears them all, because one file's change can change another's findings.
- **Why not per-file invalidation.** It would need the dependency closure at every edit, and after the first edit the live analysis is warm anyway.
- **Known difference.** The server analyses package files and open documents while `ry check` analyses every file, so a script-dependent finding can differ until the settled wave.
- **Identity.** `WorkerSeed::executable` is `current_exe` in production; `Simulation::start_as` passes the scenario's `ry` binary, since the simulated server runs inside the test binary. `ClientOptions::cold` starts a simulated session before the warm-up, and the seeded sessions draw it.

## Literate documents keep their offsets, and the server keeps their original text

**Status:** built.

- **Analysis** sees `.Rmd`/`.qmd`/`.Rnw` through `syntax::literate`, which blanks prose to spaces byte for byte, so every offset is the document's own and no range table exists to drift. A document's chunks form one script.
- **Positions.** The server keeps the original text (`Worker::literate_documents`) and converts LSP positions against it, because a blanked multibyte character has a different UTF-16 width. `did_change` applies edits to the original and re-blanks the whole buffer; conversion is a linear scan, so per-edit conversion would add a second source of truth for no measurable gain.
- **Formatting** is per chunk (`format::format_chunks`), one edit per changed chunk, so prose, fences and inline code are never rewritten. An indented chunk (in a list item) is skipped, because re-indenting it would need the list's indentation. Range formatting is off for literate files.
- **Completion** answers only inside an R region (`syntax::literate::r_regions`). Only reference-count code lenses remain; "Run file" and "Run test" mean nothing for a report.
- **VS Code** matches literate files by glob, because their language id depends on which extension is installed.

## A running console is a debug adapter, a data source, and a live-value source on one socket

**Status:** built.

- **Transport.** `ry run` listens on a Unix socket in `$TMPDIR/ry-<uid>` (0700, owner and mode verified by both ends) and writes `console-<pid>.json` naming its cwd and socket, so discovery is a directory read. One accept thread, a reader thread per connection queueing events, the interpreter thread answering (`interp::console_server`). Framing is DAP's `Content-Length`, one parser for debugger, viewer and language server. Each connection's queue is bounded at 64 and the socket thread blocks, so a flooding client stalls on its own socket buffer and nothing is dropped.
- **Prompt input.** A persistent input thread (`console_server::read_line`) owns the console reader and reads one line per prompt that asks, while the interpreter thread answers requests. A line stays owed until the interpreter takes it, so a prompt whose wait ended early (an editor took a `Browse[1]>` stop over) hands it to the next prompt instead of reading ahead; that next prompt is written to the terminal, since the thread still shows the old one. A per-prompt scoped thread could not, because its scope had to join before the prompt returned. A line typed while a command runs goes to `STASHED_INPUT` and the queue keeps draining, so a `pause` behind it is answered.
- **While a command runs**, `poll_interrupt` services the queue: debugger requests are answered, anything that would evaluate R is refused as busy, because it would interleave with the user's code. `browser()` in R code run for an editor request at the idle prompt does not stop (`SERVING_REQUEST`), because the top-level read owns the terminal.
- **`browser()` without an editor drives the same adapter from the terminal**: `Adapter::for_console` over `console_server::Browse` turns GNU R's browser commands into DAP requests (`c` disconnect, `n`/`s`/`f` next/stepIn/stepOut, `Q` terminate, other lines `evaluate` with context `ryBrowse`), so stepping rules exist once. An editor `initialize` at the prompt takes over the transport and `configurationDone` re-sends `stopped`. An editor attaching while a Browse expression runs keeps its adapter (`with_adapter` drops the finished stop's adapter instead of writing it back). The adapter ends with its command while the terminal drives it (`Adapter::is_browse`) but stays attached once an editor took over. A forced `browser()` stop needs no srcref; only error stops hide sourceless frames. While stopped, the debugger's own receive loop answers other connections inline. Scripts install no reader, so `browser()` is a no-op there, as in non-interactive GNU R.
- **Attach** installs the launch path's `debugger::Adapter` with `console = true` (disconnect detaches) and reloads workspace packages with srcrefs so package breakpoints hit. One debugger connection at a time. `debug`/`debugonce` set closure gp bits and enter `Mode::FunctionEntry`, stopping with reason "function breakpoint"; prompt code has no file, so its frames get a `sourceReference` the client fetches with `source`.
- **Data viewer.** `ryView` evaluates an expression (in the stopped frame, else the global env) and registers a view that preserves the object, `ryTable` returns one page of at most 1000 formatted rows, `ryRelease` drops it, at most 32 views live: data never leaves the runtime whole. Variables carry `evaluateName` so "View Data" names the exact value. `View(x)` sends a `ryView` event to connections that sent `rySubscribe`, which VS Code sends when it opens or attaches to a console.
- **Viewer sort and filter are row numbers, not copies.** The view caches the zero-based rows a `ryTable` request's `filter`/`sort` pair leaves (`viewer::Order`), recomputing only when the pair changes. A `DataFrame` computes them in the table engine (`table::ops::view_rows`, `sort_frame`'s radix path on a frame of the key and row numbers) and gathers pages with `take_rows`; an R object uses `grepl()` and `order(na.last = FALSE)` in a base-scoped environment. Both put `NA` first in either direction, keep ties in row order and let `NA` match no search, so both engines show the same rows.
- **Live values in the language server** come from `ryGlobals` (names, class, summary, columns; promises unforced) with a 150 ms timeout (`ry::console_link`). Analysis stays the source of truth: the console only adds hover lines and completion items analysis lacks, and the simulator disables the link to stay deterministic.

## A parameter and its named arguments are one editor binding

**Status:** built.

- `ide::target_at` maps a named argument (`f(rate = 1)`) to the parameter's `Target::Slot` in the callee (resolved as any name read, `expression_target`), and a parameter slot's occurrences add the named arguments at every call of its function, so renaming a parameter renames every `rate =`. Calls come from the function binding's own occurrences (the global walk for a top-level function, the slot walk for a local one), so another function with a same-named parameter is untouched.
- Nothing new is stored: call sites derive on request from existing occurrence walks, so no project-wide map gains a dependency.
- A library callee's named argument goes to the stub's function, with the parameter's signature-help label on hover. Partial matching is not followed, on purpose: a rename would guess.

## R's dynamic help server runs on the interpreter thread, only while R waits

**Status:** built.

- **Shape.** `interp::help_server` runs an accept thread and one thread per connection that only parse HTTP and queue an exchange; the R handler (`tools:::httpd`, or `tools:::.httpd.handlers.env[[name]]` for `/custom/<name>`) runs on the interpreter thread. No R object crosses a thread. The handler runs as under GNU R's `R_ToplevelExec`: the waiting code's handlers and restarts are parked in `Interp::suspended_handlers` (where the collector sees them), and a Ctrl-C during it reaches the waiting code after the response. `stopHTTPD` shuts every open connection down. Request heads are bounded at 1 MiB, bodies at 64 MiB, and idle connections close.
- **When requests are answered** follows GNU R, which serves help only from its event loop: at the console prompt (woken through `console_server::wake`), in `Sys.sleep`, in `process.events`, and during an HTTP download on its transfer thread (so a session can fetch its own help via `url()`). A running command never interleaves with a handler. Off Unix the console idle loop does not serve yet.
- **Wire format** (percent-decoding that stops at the first malformed escape, the query vector, the raw body with its `content-type`, the `Request-Method:` header block, error 500 forms, status reason "Code N", HEAD, HTTP version echo, `startHTTPD` returns 0/-1/-2) was probed against GNU R and is pinned by `crates/oracle/cases/tools__help_server.R`. Rd pages render through R's own `tools` code: every Rd file of 14 default packages through `Rd2txt`, `Rd2HTML`, `Rd2latex` and `Rd2ex` matched GNU R byte for byte apart from timestamps.

## Completion, signature help and import fixes reach untyped and installed code

**Status:** built, except installed formals passed as a callback, which "Packages without stubs are read from the R library by the host, lazily" supersedes.

- **Untyped project functions and R6 methods** (`fields::field_function`) read formals and default text from the HIR and syntax; a function with a type or stub scheme keeps the typed signature.
- **Use counts are chunked.** `chunk_uses` counts 32 files per tracked query and completion sums the chunks, so an edit recomputes one chunk; one query over all files re-summed every file per keystroke (p99 about 60 ms on finbench).
- **Import fixes read the export manifests, not `stubs()`**, because `stubs()` skips conditional and qualified-only namespaces the project never uses, which are exactly the ones an "unresolved name" fix must suggest. `stubs::exporting_namespaces` indexes `types/*.exports` by name; the fix offers `pkg::name` and, outside a package, `library(pkg)` after the last top-level attach.
- **Header inlay hints are the `inline_annotation_edits`** the "Add inferred type annotation" action inserts, so hint and inserted text cannot disagree.

## Any save refreshes every open document's diagnostics

**Status:** built.

- **Shape.** `Worker::did_save` (`crates/ry/src/server.rs`) refreshes every open document, whether or not the saved path is open.
- **Why.** Saving `NAMESPACE`, a stub or an unshown file changes what other documents see. Pinned by `any_save_republishes_documents_that_an_edit_elsewhere_changed`.

## A call-hierarchy item answers only in its own document

**Status:** built.

- **Shape.** Incoming and outgoing calls answer nothing if the item's name resolves to a function in another file (`server.rs`, the hierarchy item lookup).
- **Why.** An item's call ranges are relative to its own document; an edit can move the definition its name resolves to, and answering from the other file returns ranges in the wrong text.

## The console test waits on a count of handled closes

**Status:** built.

- **Shape.** `console_server::closed_connections` and `wait_for_closed_connections` (`crates/interp/src/console_server.rs`) count finished debugger-connection closes behind a mutex and condition variable; `test_console_server` waits on its count.
- **Why.** Polling with a wall-clock budget fails on a loaded machine and breaks the rule that no harness reads the clock.

## Refactorings refuse rather than guess, and a seeded property check holds them to it

**Status:** built.

- `ide::refactors` (`crates/ide/src/refactor.rs`): extract variable, extract function, inline variable, annotation conversion, each offered only where the rewrite keeps evaluation. Extract variable refuses an expression evaluated repeatedly, conditionally or never (loop or function bodies, `if` branches, the right of `&&`/`||`) and code under a formula or quoting call (`is_captured`). Extract function refuses `return()`, `<<-`, `...` and frame-inspecting calls. Inline refuses when a read name is reassigned before the last use or a call would run more than once; a value that assigns is never inlined (`x <- y <- 0`).
- `argument_evaluation` classifies calls: *captured* (quoting, data masking such as `with()` and dplyr verbs, the index of `[` for data.table) blocks every refactoring; *guarded* (condition handlers, `switch`, `withr::with_*`) blocks hoisting but allows extracting a function called in place. `evaluated_first` is the one rule for hoisting and for inlining a value with a call: every node between the expression and its statement evaluates it exactly once, and nothing before it in the statement calls or assigns. Extract function treats a read anywhere in the enclosing loop as later code, because the next pass runs it. The property check cannot see these (the result parses and resolves), so each is pinned in `crates/ide/tests/refactor`.
- Text that parses as one expression only in its old context is parenthesized (`needs_grouping`: a `?`, or a reparse that is not exactly one node), because `value <- d?f` and multi-line operand chains change meaning when moved.
- Two edits at one offset are merged, because LSP clients disagree on the order of same-offset inserts (a definition inserted at the statement start plus a replacement there).
- `check_refactor_invariants` (driven by `crates/ide/tests/test_fuzz.rs`) applies every action at every node range and asserts determinism, non-overlapping edits, a clean reparse and no new unresolved names. It runs without stubs (one seed took 17 s instead of 1.3 s with them, and unresolved-name reporting does not need them). Parse and resolution invariants run only on input that parsed cleanly, because mutated input already has errors the rewrite cannot be blamed for.

## Call-head completion ranks by call counts, without test files

**Status:** built.

- **Shape.** The per-file name-use fold (`SpelledNames`) also counts call-head uses (direct or `pkg::name`). A call-head completion ranks project S3 methods (a `generic.class` beside a `generic` candidate) last, then by calls, then spellings. Ranking by spellings let testthat's `expect_*` beat the package's own functions at two characters, and S3 methods (`print.purrr_rate`) crowd out functions. Call counts leave out test files unless the cursor is in one (`chunk_uses` takes `with_tests`); spelling counts keep them, because tests show which `pkg::` members a project uses (dropping them cost httr2's `pkg::name` role 9 points). Two characters on call heads reached 72% on httr2 and 76% on purrr.
- **Rejected:** ranking by words shared with the enclosing function (`check_rate` in code that has a `rate`); it gained a point or two at half the name but lost six to eight points at two characters, where long names share some word with almost anything.

## Call hierarchy and workspace symbols are measured against the reference oracle

**Status:** built.

- `ry debug analysis-stats` checks incoming and outgoing calls of sampled project top-level functions (defined exactly once) against the independent lexical resolver that checks references: every oracle global call-head site is an expected call, grouped by enclosing top-level definition. Workspace symbols are checked by each sampled name and a half-length prefix (at least 3 characters); a prefix matching more names than a search lists is left out, since a user types on.
- Workspace search reads a memoized per-file list (`searchable_symbols`), because rebuilding every outline per query took about half a second at 300,000 lines.

## Completion pushes down names whose declared type cannot fit, and never promotes

**Status:** built.

- **Shape.** `expected_type` in `crates/ide/src/ide.rs` reads what the cursor's context wants: logical for an `if`/`while` condition and operands of `!`, `&&`, `||`; for an argument, the parameter type its global or stub callee *declares*. Declared, not the call's instance, because the instance took its parameter types from the argument being typed (`print(i)` made the slot `integer` and pushed `iconv` down). It unions the parameter across every stub overload, because the argument picks the overload only once written (`lapply`'s first overload takes a named list, which pushed `seq_along` down). Comparison is by coarse value shape (`type_mismatch`) with logical-integer-double widening; a known mismatch sorts after fits within the same match score and source group. Inferred function types are ignored on both sides: they say how a body ends, not what callers may pass.
- **Rejected:** promoting candidates that fit; inferred-`logical` helpers then outranked `is.na`, and two characters on call heads lost 2 points on purrr and 1.3 on R6. Keeping it to push-down is neutral on httr2, purrr and R6 (+0.3 on finbench) at about 2 ms median completion on packages and 8 ms on finbench; the gain is in annotated code. Pinned by the `completion_expected_type` fixtures.

## The console's crossterm is patched to drain the terminal, and R reads console input one line per chunk

**Status:** built.

- **Patch.** Upstream crossterm's mio reader returns after the first event it parses from an edge-triggered read, leaving the rest of a paste buffered until the next edge; reedline then waits its two-second cursor-position timeout. `patches/crossterm` drains the terminal before returning an event. The `use-dev-tty` reader is not a substitute: its `poll(Duration::ZERO)` reports no event while bytes are buffered, which breaks reedline's batching and `take_editor_events`. Bracketed paste is on.
- **One line per chunk.** R discards the rest of its input buffer when a command errors, so the console hands R one line per read (`take_chunk` in `crates/repl/src/console.rs`) and keeps the rest in `pending`. Code a script or hook queues goes as one unit. A read below top level (`readline()`, `menu()`, the browser) is answered from the console, never from `pending`; Ctrl-C clears `pending`.
- **Preflight.** `InputFilter::discard` retracts a refused command, so the session's analysis never holds a command R did not run. A command counts as ry syntax when it holds an annotation, a `typing:` directive, a ry literal or a `Module.function` name that a native namespace declares, so `Sys.time` and `Ops.money` do not (`crates/ry/src/preflight.rs`).
- **Tests.** `crates/ry/tests/test_repl_e2e.rs` pins multi-line paste, an error mid-paste, a nested read during a paste, Ctrl-C with input pending, and a refused command.

## An editor target is what the code means, and rename refuses what it cannot trace

**Status:** built.

- **A top-level loop variable is the global of its name.** `for (x in xs) ...` at the top level leaves `x` bound for the statements after it, so definition, references and rename treat it like a top-level assignment (`binds_global` in `crates/ide/src/ide.rs`). As a separate symbol, the read after the loop was left behind by a rename.
- **A global's references skip reads that mean something else.** A bare name in a frame query that names a column reads the column (`frame_column_reads`), and a call that passes a binding holding no function reaches the library's function (`CalleeTarget::Library`, or `CalleeTarget::Project` resolved through `non_local_target`, because the checker's global scope holds the library's functions too). Neither is an occurrence of the project's global.
- **Rename refuses a field it cannot trace.** A field with a same-named string subscript (`x[["name"]]`) or an unbound bare name in a subscript (`frame[, name]`) is refused (`SpellingKind::Untraced` in `crates/ide/src/fields.rs`), in line with "Refactorings refuse rather than guess". Carrying the rename through those reads is in the backlog.
- **Selection ranges always cover the cursor.** A cursor on a string's quote or in a block's indentation starts from the smallest node that covers it; an editor drops an answer whose first range does not hold the position.
- **Why.** Each was a rename or a reference list that left the code meaning something else, found by the IDE battery's rename check (`testing.md`, "The editor features") and pinned in `crates/ide/tests/ide/rename_ported.R.test` and `tests/structure/selection.R.test`.
