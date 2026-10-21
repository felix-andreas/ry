# Memory

Cross-session knowledge base for the agents building ry, in three horizons. **Short-term** holds the current focus, so prune an item once it is resolved. **Mid-term** holds active priorities and debt. **Long-term** holds durable, non-obvious traps and their rationale. Every entry is context-free and timeless, and points at code or docs instead of restating them.

The companion files are `backlog.md` (open work, plus a one-line shipped ledger at the bottom), `decisions.md` (settled decisions and why), `test-user-reports.md` (closed simulated-user findings), and `worklog.md` (chronological, appended by a routine). The specs live in the docs site: `reference/type-system.md` is the typing contract, `type-checking/tour.md` introduces it, and `contributing/{architecture,structure,testing,authoring-stubs}.md` cover the internals. Unsettled design work goes to `docs/…/contributing/design/` (out of the sidebar, listed on its index page), never here.

## Short-term

- **Cleanup program (user-directed, one PR per step, and a step may split):**
  1. Done: legacy ported and deleted.
  2. Done: comments cut to short non-obvious "why" comments, and the agent docs trimmed.
  3. Next: shrink the code in several passes. Simple and data-oriented, and duplication is acceptable.
  4. Docs passes for density that still introduce one concept after another (style reference: vxlang.org/docs/tour.html).
  5. At the local maximum, consider a green-field rewrite.

  Keep `roughly.toml` support and the REPL (user directives). Other old-name fallbacks, the perf and debug probes, and the fuzz scaffolding may shrink.
- **Next release: update the install docs.** `installation.md`, `getting-started.md` and the README pin 0.3.1-beta, whose Linux binary (also bundled in the VS Code Linux extension 0.3.1) has a `/nix/store` loader and starts only under Nix. Once a release ships the glibc 2.17 binary, point the examples at it and drop the caveats.
- **Docs pages follow the writing rules in `AGENTS.md`, and a page that reads like a telegram is as much a regression as dense prose.** Read a page aloud when you edit it, and keep a reason inside its sentence rather than splitting it off. Before rewriting a page, run an adversarial reader subagent with the owner's objections as the rubric (too abstract, information the reader does not need, a term used before it is introduced, copy about the tool instead of what the reader gets). That review found 35 false claims on the user pages and 14 on the contributing pages.
- **A new em-dash in a diagnostic or a comment is a regression.** Two survive on purpose as test data (`literate.rs`, `test_cli.rs`), and `worklog.md` keeps its original wording.
- **Validate a docs link against the ids in the built HTML** (`docs/dist/**/index.html` after `cd docs && bun run build`), never against a hand-rolled slug, because two headings that differ only in punctuation slug to the same id and the second silently becomes `-1`.
- The landing-page hero animation is user-owned, and `NOTES.md` is human-maintained. Never edit either.

## Mid-term

- **False reports on correct R come first.** `backlog.md` ("Open: false reports and gaps found by the documentation review") lists them worst first, such as inferred numeric parameters refusing mixed `integer`/`double`, `pkg::name` not activating a conditional namespace, and positional append on `list()`. Each is listed on `type-checking/limitations.md`, so a fix also deletes its line there.
- **The adoption reviews heading `backlog.md` come next**, each with a minimal repro. Re-run a filed finding before ranking work off it, because two of four claims in one entry were already stale. Add a control in the same file (an obviously undefined name), so "no finding" cannot mean "the check is off".
- **User direction (standing):** full ownership with no check-in gates, driving toward rust-analyzer quality and an empty `backlog.md` §Open. Work lands through PRs on the session branch. A slice is the docs contract, the implementation, fixtures, the full gate (`just gate`), and the memory and backlog update. Treat a "landed" claim in an old note with suspicion: the verified state is `backlog.md` plus the fixtures.
- **The language is `ry`** (crate `crates/ry`, published `ry-lang`, config `ry.toml`, suppression `# ry: allow(...)`, env `RY_*`). The former spellings (`roughly.toml`, `# roughly: allow`, `ROUGHLY_*`, extension `roughly.*`) still fall back, and `roughly.toml` must stay.
- **The type system admits only what is fast to check, which means Hindley-Milner (user directive).** `.Rtypes` overloading is the one sanctioned exception. Traits, typeclasses and general subtyping are declined, not deferred, so evaluate every type-system proposal against `decisions.md` first. Open questions live in `contributing/design/open-questions.md`.
- **CI gates only `crates/ry`** (`default-members`), so no fixture or fuzz suite runs in CI. The widened workflow is staged in `.github/pending-ci.yml` for a human `git mv`, because the agent token lacks workflow scope. Run `just gate` locally per slice, and never read a green CI badge as "the fixtures pass".
- **Debt queue:** `backlog.md` §abstraction & duplication review and §test & fuzz architecture review. Both list items deliberately not worth fixing, so respect those.

## Long-term

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
- **The parser's acceptance gate is hard:** zero unadjudicated divergences against tree-sitter, with R as the referee.

### IDE and server

- **Cursor position model:** the smallest end-inclusive expression wins, with a `NameRef` winning a tie, so the declaration scan stays a fallback for every binding kind. Which token declares an item has one answer, `semantics::item_name_range`, and `:=` is never a definition.
- **The server** is one async-lsp frontend thread plus one worker owning the database. Edits flip salsa's cancellation token, and later jobs use a fresh storage handle. Host assembly (config gating, typing modes, suppressions, lints) is shared with the CLI in `crates/ry/src/diagnostics.rs`.
- **Coherence failures panic** (document sync), and IDE lookups never panic.
- **CLI reporting is miette's** (`crates/ry/src/cli.rs`). Its header column counts bytes, so `read_span` re-counts characters. A range over several lines clamps to the first, and sources are borrowed (`NamedText`), never `NamedSource`.
- **The IDE fixtures print the token a range covers**, not a byte offset, so a stub addition never forces a re-bless. If one fails after a corpus change, the range is genuinely wrong.

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
