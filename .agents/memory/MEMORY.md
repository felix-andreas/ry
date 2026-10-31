# Memory

Cross-session knowledge base for the agents building ry, in three horizons. **Short-term** holds the current focus, so prune an item once it is resolved. **Mid-term** holds active priorities and debt. **Long-term** holds durable, non-obvious traps and their rationale. Every entry is context-free and timeless, and points at code or docs instead of restating them.

`.agents/orchestrator.md` is the brief for the orchestrating session only, and every other agent ignores it. The companion files are `backlog.md` (open work, plus a one-line shipped ledger at the bottom), `decisions.md` (settled decisions and why), `test-user-reports.md` (closed simulated-user findings), and `worklog.md` (chronological, appended by a routine). The specs live in the docs site: `reference/type-system.md` is the typing contract, `type-checking/` introduces it one short page per idea, and `contributing/{architecture,structure,testing,development,authoring-stubs}.md` cover the internals. Unsettled design work goes to `docs/…/contributing/design/` (out of the sidebar, listed on its index page), never here.

## Short-term

- **Docs pages follow the writing rules in `AGENTS.md`, and a page that reads like a telegram is as much a regression as dense prose.** Read a page aloud when you edit it, and keep a reason inside its sentence rather than splitting it off. Before rewriting a page, run an adversarial reader subagent with the owner's objections as the rubric (too abstract, information the reader does not need, a term used before it is introduced, copy about the tool instead of what the reader gets). That review found 35 false claims on the user pages and 14 on the contributing pages.
- **A new em-dash in a diagnostic or a comment is a regression.** Two survive on purpose as test data (`literate.rs`, `test_cli.rs`), and `worklog.md` keeps its original wording.
- **Validate a docs link against the ids in the built HTML** (`docs/dist/**/index.html` after `cd docs && bun run build`), never against a hand-rolled slug, because two headings that differ only in punctuation slug to the same id and the second silently becomes `-1`.
- The landing-page hero animation is user-owned, and `NOTES.md` is human-maintained. Never edit either.

## Mid-term

- **Scope (user directive): ry becomes a full, backwards-compatible reimplementation of R, extended with ry's typing.** `AGENTS.md` ("Goals" and "Process") holds the mandate, the benchmark dependency set, and the process rules. The top priority is designing the R C API layer (`SEXP`, memory protection, `.Call`), because most benchmark dependencies ship compiled code against it, so it constrains the architecture more than the JIT does. Inline annotations, the comment-agreement warning and strict mode's untyped-parameter rule have shipped (`decisions.md`, "How inline annotations are built"), and so has lazy package loading on the GNU R backend (`ry load`, `ry repl`, `load_all()`; `decisions.md`, "the package-development loader"; gaps in `backlog.md`, "Open: the package loader"). Then come the runtime, the typed table type that must outperform data.table, and a JIT on the level of Bun. Progress is measured on three synthetic code bases (finance, data science, biostatistics) of about 300,000 lines in about 500 files each.
- **Runtime status: the object model, collector and the interpreter-independent C API exist.** `crates/heap` and `crates/libr` (`libR.so`, headers in `include/`) cover the measured API surface, ALTREP, connections, serialization, Rmath and the numerical routines, with BLAS and LAPACK header-only against the system libraries; `crates/rcmd` builds and installs packages with `R CMD INSTALL` semantics against ry's R home. Evaluation goes through the `libr::eval::Evaluator` hook the interpreter installs. `decisions.md` ("the runtime's object model, collector and native boundary", "the full C API surface and package installation") has the settled shape; `backlog.md` ("Open: the runtime") the gaps.
- **The runtime design draft is `contributing/design/r-runtime.md`**, with the measured C API usage of the benchmark closure in `tools/research/data/` (regenerate with `tools/research/r_api_usage.py`). Its main conclusions: ship an ABI-compatible `libR` covering the whole R 4.6 public header set (the closure uses 792 API names plus 150 BLAS and LAPACK routines, and covering half of its 121 compiled packages takes the 495 most used names), and present as R 4.6, including `R_SVN_REVISION` of at least 89194, so packages take their modern-API branches instead of non-API fallbacks. `contributing/design/prior-art.md` records which ideas from Roc, Bend, goose and Vx the runtime adopts.
- **The corpus, oracle and benchmark generator exist** (`contributing/corpus-and-oracle.md`): `tools/corpus/install.R` builds the GNU R library of the benchmark closure, the `oracle` crate runs files under GNU R and a second engine (`Engine` trait, stub until the runtime exists), and `tools/bench/generate.py` writes the three synthetic packages from R templates. Next for this area: plug the runtime into `ProcessEngine` (a command taking `<script> <report>` and writing the driver's JSON report), grow `crates/oracle/cases/`, run the generated packages' tests through ry, and check the generated code with `ry check` for false reports.
- **The runtime packaging draft is `contributing/design/packaging.md`** (decision record "packages and R versions in the runtime"): one installed package directory serves GNU R 4.6 and ry's runtime, ry's per-package data lives in its own content-keyed cache, runtime selection is explicit, and the analysis reads installed `NAMESPACE`/`.rdx` instead of generated manifests. A package manager of ry's own is deliberately left open.
- **Process (user direction, see `AGENTS.md` "Process"):** full ownership with no check-in gates, and velocity first, so main may be broken and CI need not be awaited. Small tasks go from the orchestrator to an in-session worker that pushes to main, and large tasks go to cloud sessions that deliver reviewed pull requests. Treat a "landed" claim in an old note with suspicion: the verified state is `backlog.md` plus the fixtures.
- **Analysis debt that predates the scope.** The next two items still matter as the semantics contract, but they rank behind the reimplementation.
- **False reports on correct R.** `backlog.md` ("Open: false reports and gaps found by the documentation review") lists them worst first, such as inferred numeric parameters refusing mixed `integer`/`double`, `pkg::name` not activating a conditional namespace, and positional append on `list()`. Each is listed on `type-checking/limitations.md`, so a fix also deletes its line there.
- **The adoption reviews heading `backlog.md` come next**, each with a minimal repro. Re-run a filed finding before ranking work off it, because two of four claims in one entry were already stale. Add a control in the same file (an obviously undefined name), so "no finding" cannot mean "the check is off".
- **The language is `ry`** (crate `crates/ry`, published `ry-lang`, config `ry.toml`, suppression `# ry: allow(...)`, env `RY_*`). The former spellings (`roughly.toml`, `# roughly: allow`, `ROUGHLY_*`, extension `roughly.*`) still fall back, and `roughly.toml` must stay.
- **The type system admits only what is fast to check, which means Hindley-Milner (user directive).** `.Rtypes` overloading is the one sanctioned exception. Traits, typeclasses and general subtyping are declined, not deferred, so evaluate every type-system proposal against `decisions.md` first. Open questions live in `contributing/design/open-questions.md`.
- **CI gates only `crates/ry`** (`default-members`), so no fixture or fuzz suite runs in CI. The widened workflow is staged in `.github/pending-ci.yml` for a human `git mv`, because the agent token lacks workflow scope. `just gate` runs every suite locally (it does not block landing while main may be broken), and a green CI badge never means "the fixtures pass".
- **Debt queue:** `backlog.md` §abstraction & duplication review and §test & fuzz architecture review. Both list items deliberately not worth fixing, so respect those.

## Long-term

### Runtime traps

- **A Rust function that allocates while it holds a handle nothing else roots must root it first** (`heap.root`, or the `keep` list of `Heap::allocate`). Collection stress is the only thing that shows it, so run `crates/libr/tests/test_fuzz_native.rs` with a large `FUZZ_ITERS` after touching allocation paths.
- **Native code must never be called from Rust except through a C trampoline in `native.c`**, and a callback native code hands to the API (`R_UnwindProtect` and friends) must run from C, so a `longjmp` or a C++ exception never crosses a Rust frame. An exported API function runs its body through `boundary::enter`.
- **A data symbol exported from `native.c` is marked `RY_EXPORT_DATA`**, not `RY_EXPORT`; `build.rs` reads both markers into the version script, and a function-pointer variable is named by the `(*name)` inside its declarator.
- **The corpus tests (`test_corpus_binaries`, `test_corpus_calls`, `RY_SOURCE_PACKAGES` in `test_install`) need `.corpus/library`** from `tools/corpus/install.R` and skip without it. Re-record `tests/data/corpus_calls/cases.rds` under `LC_ALL=C.UTF-8`, or string routines record C-locale results.
- **`cargo test` writes the fresh `libR.so` to `target/<profile>/deps`**; the copy in `target/<profile>` is only refreshed by `cargo build`, so a harness that loads it tests stale code.
- **A C function native code calls must be marked `RY_EXPORT` in `native.c`**, because rustc's version script hides C symbols in a cdylib and `build.rs` exports only the marked ones.
- **Translate a C `do { ... } while (a > b)` that can meet NaN as `if !(a > b) { break }`**, never `if a <= b { break }`: a NaN comparison is false, so the C loop ends and the inverted one spins forever (`rmath.rs`).
- **Never copy GNU R's GPL code into the repository** (`decisions.md`, "runtime licensing"). Headers and implementation are written independently; vendored test packages carry their own license (brio is MIT).

### Performance traps

- **A per-item query must never scan a per-file or per-project list.** Compute once per file in a memoized query and probe per item. `item_spans` is the single source of top-level item identity, so never re-derive the classification. Such a quadratic has two factors (items times names looked up), so vary both axes when measuring, because either axis alone looks linear.
- **A per-item read that joins over a project-wide list needs a cap**, not just memoization (`CONDITIONAL_SLOT_JOIN_CAP`, widening to `Unknown`).
- **A check reads per-name firewalls, never a project-wide map** (`package_name`, `interface_group`, `frame_binder`). `crates/semantics/tests/test_incremental.rs` pins it by counting query executions.
- **Never call `Checker::infer` twice on one expression.** It has no memo, so a double walk made operator chains cost 2^depth and reported each finding 2^depth times. The fixtures dedup, so they cannot see it. Thread the `Ty` you already have.
- **`GlobalEnv` facts are borrowed per-file values** (`&'db`). Returning them owned makes every item pay for the whole project.
- **Hot tracked queries returning maps or vectors use `returns(ref)`**, because `returns(clone)` deep-clones per call.
- **A type is an interned DAG, so walk it with a memo**, never as a tree (`substitute_rigid` went from 278M to 2M calls).
- **A composite type past `TYPE_SIZE_CEILING` records as `Unknown`** in `Checker::record`. `types::type_size` counts paths, not distinct nodes, because a consumer walking a type pays the tree it denotes. Without the ceiling, a record whose fields all return that record grew tenfold per level and one file never finished.
- **A write inside a call-argument block binds at top level**, so `test_that("x", { out <- f() })` publishes `out` package-wide, which is why a package's tests can dominate its cost. Whether R binds outward depends on the callee: `try`, `suppressWarnings` and `invisible` do, while `local` and `test_that` do not.
- **Measurement traps.** Alternate the two binaries run by run, because sequential blocks showed a fake 2x win. Measure the analysis path with `--output json`, because the human reporter is itself quadratic in findings. Compare corpus outputs as unique sorted records, not line counts (baselines: data.table 4077, dplyr 675, ggplot2 1091, shiny 1038). Every in-container timing is effectively a two-core number or worse.
- **Budgets:** keystroke-to-diagnostics p50 at or under 30 ms and p95 at or under 100 ms at 300K LoC, pinned by `crates/semantics/tests/test_perf.rs` (`just perf`, which needs the fetched corpus). For reference, 794K lines across 69 CRAN packages check cold in about 14 s at about 1.3 KiB resident per line, with a keystroke p50 of about 12 ms.

### Semantics traps

- **`GlobalEnv` has no default method bodies, on purpose.** The SCC fixpoint's view is a second implementor, and a defaulted method silently gives it a wrong answer (this once collapsed every recursive function using `Date + 1` to `Unknown`). Test such facts with a recursive and non-recursive fixture pair.
- **Dispatch and the numeric constraint must read the same class scope** (stubs plus project), so a package's own `+.Money` makes `Money` arithmetic.
- **A read recorded by name instead of by expression id is invisible to the slot model**, which is why `%op%` reads go through `mark_slot_read`.
- **One structural map rewrites types: `types::map_child_types`** (reads use `for_each_child_type` and `Parameter::types`). A second hand-written walk once skipped a parameter's default and leaked an inference variable across the item boundary, so new `TyKind`, `FunctionType` or `Parameter` members go there and nowhere else.
- **Every type the checker prints must parse back** (`test_roundtrip.rs`), so a new constraint, `TyKind` or rendering includes its grammar side. A function member of a union renders parenthesized.
- **Every mismatch report goes through `Checker::type_mismatch`**, which narrows record and function mismatches to the failing field or position and narrows the range with the message.
- **A finding underlines the smallest expression its message is about.** Thread narrower ranges (`name_range` on fields and call arguments), keep the whole type as the verdict, and use the leaves only to place it.
- **Every finding inside an item is reported**, and only the export is cut to `Unknown` when the item fails.
- **A `#:` annotation declares parameter types, never the parameter list.** The export is always the formals, and `check_declared_function` owns the reconciliation, so a call is never blamed for an annotation's mistake.
- **Do not `table.resolve` a type you display as written**, because resolve expands aliases.
- **One `TypeRenderer` spans everything that shares variable names** (a signature, both sides of a mismatch).
- **Immediate reads resolve against the nearest earlier writer in the file, while deferred reads (closures) differ by kind.** A package closure sees the project-wide winner and a script closure sees the settled file frame. Getting this backwards breaks forward references from function bodies.
- **`"x" <- 1` binds `x`** through the name-target arm, not `replacement_base`, which reads the base first.
- **A fixture cannot exercise anything keyed on `PackageMetadata`**, so those cases belong in `crates/ry/tests/test_cli.rs`. Read what a bless recorded before believing it covers the scenario.

### Parser and annotation traps

- **Recovery stops at the statement boundary when the next line assigns** (`starts_new_statement`). `=` does not count, because inside a list it is a named argument. The check scans backwards, because the newline was consumed with the previous element.
- **One mistake, one finding.** A lexer `ERROR_TOKEN` is already diagnosed and suppresses its statement's boundary report, and the `#:` grammar reports once per region (`error_unclosed` lets one unclosed opener through). Never report a consequence beside its cause, and blame the incomplete construct rather than where the search stopped.
- **An unterminated quote ends at its first line break**, so typing a quote does not take the rest of the file out of analysis.
- **`#:` block-form rules are about lines, and a stitched line's marker may sit inside a nested node**, so count lines over all of the block's tokens. A `<T>` binder list is a prefix of the next type, not an item. A refused region is an `ERROR` node, and a block containing one carries no typing payload.
- **A diagnostic code names whose grammar broke**: `syntax-error` for R and `annotation` for `#:` (`SyntaxError::in_annotation`).
- **Formatter:** trailing comments attach inside expression nodes, so placement decisions are token-level (`last_token_before`). Line math uses `significant_range`, and the formatter refuses only on R-grammar errors.
- **Golden error suite** (`crates/syntax/tests/errors`): a new parser error path adds its case in the same change.
- **Inline type grammar defers its trivia** (`Parser::deferred_trivia`): a new `ann_*` path must emit through `bump`, `start` or `checkpoint`, which flush it, never through the builder directly, or trivia lands on the wrong side of a node.
- **An annotated binding is recognized only at a statement start** (`Parser::statement`), because R reads `name:TYPE <- v` as `` `:<-` ``; everywhere else `x:y` stays a range. Tree-sitter judges the erased text (`erase_inline_annotations`), so an annotated fixture needs no allowlist entry.
- **Both annotation spellings lower into one `Annotation`** (`lower_annotation`, `lower_inline_annotation`, `merge_annotations`). A new annotation feature goes into that shared lowering, never into a checker path that knows the spelling.
- **The parser's acceptance gate is hard:** zero unadjudicated divergences against tree-sitter, with R as the referee.

### IDE and server

- **Cursor position model:** the smallest end-inclusive expression wins, with a `NameRef` winning a tie, so the declaration scan stays a fallback for every binding kind. Which token declares an item has one answer, `semantics::item_name_range`, and `:=` is never a definition.
- **The server** is one async-lsp frontend thread plus one worker owning the database. Edits flip salsa's cancellation token, and later jobs use a fresh storage handle. Host assembly (config gating, typing modes, suppressions, lints) is shared with the CLI in `crates/ry/src/diagnostics.rs`.
- **Coherence failures panic** (document sync), and IDE lookups never panic.
- **CLI reporting is miette's** (`crates/ry/src/cli.rs`). Its header column counts bytes, so `read_span` re-counts characters. A range over several lines clamps to the first, and sources are borrowed (`NamedText`), never `NamedSource`.
- **The IDE fixtures print the token a range covers**, not a byte offset, so a stub addition never forces a re-bless. If one fails after a corpus change, the range is genuinely wrong.

### Package loader

- **A plan is backend-neutral; only `gnur.rs`/`runtime.R` know GNU R.** Never put R-specific logic in the diff (`loader::diff`), or the runtime backend cannot reuse it.
- **Every reload rule must keep `test_reload_fuzz.rs` green**, which compares a reload against a fresh load in a model. Hand-reasoned rules missed swapped writers, `f <- memoise(f)` wrapping twice, and a fixpoint that stopped before the changed-name set settled.
- **R traps the GNU R backend hit:** `environment<-` strips a closure's bytecode (cache the whole environment instead); `getOption("keep.source.pkgs")` is `FALSE` by default, so it must not shadow `keep.source`; `.Internal(...)` arguments are evaluated, so a language argument needs `quote()` (`.ry_internal`); `ls()` sorts, so hot paths use `names(env)`; the attached package environment keeps names the namespace dropped unless every sync removes stale ones (S4 `.__C__` metadata).
- **Measure loaders with the same `keep.source`.** `ry run` and `ry repl` are interactive, so source references are on, while `Rscript` runs others without them; that alone is 10 to 20% of a cold load. `tools/loader-bench` is the harness.

### Testing

- **An oracle is trusted only once an injected bug turns it red.** Determinism, idempotence and round-trip are self-consistency checks that a uniformly wrong answer satisfies, and three batteries here stayed green over real mutants.
- **Give a differential oracle inputs that always exist.** The tree-sitter acceptance test never ran while it pointed only at the gitignored corpus, and the test runner hid its skip note.
- **A foreign renderer's expectations are never bulk-blessed.** Adjudicate each case against the typing reference.
- **The fuzz batteries' cost is re-parsing the stub corpus per fresh database**, not the iteration count.
- **A gate must fail on its own exit status.** `cmd | grep …; echo OK` always passes, and a background job's output file can be read mid-write.
- **The REPL pty suite flakes under concurrent load** (one failure, and a suite wall time of 41 s instead of 4 to 6 s). Re-run it serially before believing it. A real desync reproduces and takes about 90 s.

### REPL and environment

- **The REPL never feeds R input through the console to set state.** Startup work goes through `RApi::eval` between `setup_Rmainloop()` and `run_Rmainloop()`. `contributing/design/repl.md` has the details, including the Windows recipe (`GA_initapp` lives in `Rgraphapp.dll`) and the pty harness requirements (answer the cursor-position query, serialize the sessions). The pty suite is verified on Windows against R 4.5.2 and on Unix.
- **The Linux release binary links glibc 2.17 through zig** with the standard `/lib64` loader, so it can `dlopen` R, and the flake's `default` is a native nixpkgs build for Nix users. The sandbox has no `/lib64` loader, so release derivations run no tests and the native package carries the suite. `allowedReferences = [ ]` fails a release build that keeps a store path. `decisions.md` has the rationale, including why not static musl.
- **A windows-gnu cross-link needs a synthesized `synchronization` import library**, which the flake's windows `preBuild` makes with `zig dlltool`. A crane deps-only build cannot catch a breakage here, because the dummy sources never reference reedline.
- **R installs in agent containers** (`apt-get install -y --no-install-recommends r-base-core`, or the CRAN apt repo for a current R). `fs` needs `libuv1-dev`, and the tidyverse needs `libcurl4-openssl-dev libharfbuzz-dev libfribidi-dev libfreetype-dev libpng-dev libjpeg-dev`. Free disk first (`target/debug/incremental`).
- **Export manifests** (`types/*.exports`) come from a live session (`Rscript scripts/export-manifests.R`) on an R at least as new as the recorded one, because a hand-written or older-R manifest turns real exports into false `unresolved` findings. Meta-packages expand through `stubs::META_PACKAGE_MEMBERS`.
- **Stubs** are declaration-only `.Rtypes` files in `types/`, using the `#:` grammar with the loader in `semantics/src/stubs.rs`. They are a set-once salsa input, and project `stubs/*.Rtypes` files override them. An overload set ends with an `Any` fallback.
- **Landing-page and README samples are captured from the running tools**, never hand-edited, and the landing markup renders through `set:html` because Astro strips whitespace-only content. A changed message or blame range means re-capturing them (`backlog.md` lists the pages, and `scripts/render-diagnostic-svg.rs` makes the README image).

### Conventions

- **Verify with CI's toolchain:** run `rustup update stable` first, and check for a formatting diff rather than an exit code piped through `head`. The nightly-only `rustfmt.toml` keys warn on stable.
- **Exclude exactly `zed_ry`** (`--workspace --exclude zed_ry`), because cargo only warns on a misspelled `--exclude` and builds the crate anyway.
- **`[check] typing` defaults off**, so CLI probes need `[check]\ntyping = true` in `ry.toml`, while `[check] unused` defaults on. To read an expression's inferred type, annotate its binding with a type it cannot have and read the `found` half of the mismatch.
- **`reference/formatting-rules.md` is generated** from `crates/format/tests/formatter.template.md` (`RY_BLESS=1 cargo test -p format --test test_format_docs`), so a site-wide link rewrite must include the template.
- **Scratch output goes to absolute paths.** `cd dir && job &` backgrounds the whole compound, so later commands run in the repo and `git add -A` then commits their output. Read `git status` before `git add -A`.
- **The `fuzz/` lockfile is gitignored**, and a fresh resolution can pick a `salsa-macro-rules` that does not match the pinned `salsa`. Seed it from the workspace lock.
- **Docs content:** state every rule with its reason, motivate a design with its technical reason, explain a thing once where it is introduced, tabulate it once in Reference, and link elsewhere. Never use a capability before the page introduces it. The fixed terms are finding, diagnostic code, annotation, check (the command), code analysis (the capability), the language server, and project.
- **Verify every docs claim against the binary**, even for a prose-only edit. A contradiction between two sections of one spec is invisible to every gate (the reference once said `!` takes only `logical` while the checker coerces numbers), so read a page end to end when you touch it.
- **Docs site structure:** four sidebar groups by purpose (Introduction, Type checking, Reference, Contributing), and no how-to guides (owner decision). The installation page is deliberately absent from the sidebar. Breadcrumbs come from the sidebar (`docs/src/components/PageTitle.astro`).
- **UI:** no `border-l` accent rail on rounded elements.
