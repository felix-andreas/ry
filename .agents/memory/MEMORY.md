# Memory

Cross-session knowledge base for the agents building ry. Three horizons: **Short-term** (current focus; prune once resolved), **Mid-term** (active priorities and debt), **Long-term** (durable, non-obvious traps and rationale). Every entry is context-free, timeless and terse; point at code or docs instead of restating them.

Companion files: `backlog.md` (open work, plus a one-line shipped ledger at the bottom), `decisions.md` (settled decisions and why), `test-user-reports.md` (closed simulated-user findings), `worklog.md` (chronological, appended by a routine). Specs live in the docs site: `reference/type-system.md` (typing contract), `contributing/{architecture,structure,testing,authoring-stubs}.md`. Unsettled design work goes to `docs/…/contributing/design/` (out of the sidebar, listed on its index page), never here.

## Short-term

- **Cleanup program (user-directed, one PR per step):** (1) DONE: legacy ported and deleted. (2) IN PROGRESS: cut comments to short non-obvious "why" comments; trim agent docs. (3) Shrink code in several passes: simple, data-oriented, duplication acceptable. (4) Docs passes for density, one concept after another (style reference: vxlang.org/docs/tour.html). (5) At the local maximum, consider a green-field rewrite. Keep `roughly.toml` support and the REPL (user directives); other old-name fallbacks, perf/debug probes and fuzz scaffolding may shrink.
- **Next fixes come from the adoption reviews** heading `backlog.md`, each with a minimal repro. Re-run a filed finding before ranking work off it: two of four claims in one entry were already stale. Always add a control in the same file (an obviously undefined name), so "no finding" cannot mean "check is off".
- The landing-page hero animation is user-owned; `NOTES.md` is human-maintained. Never edit either.

## Mid-term

- **User direction (standing):** full ownership, no check-in gates; drive toward rust-analyzer quality and an empty `backlog.md` §Open. Work lands through PRs on the session branch. A slice = docs contract + implementation + fixtures + full gate (`just gate`) + memory/backlog update.
- **The language is `ry`** (crate `crates/ry`, published `ry-lang`, config `ry.toml`, suppression `# ry: allow(...)`, env `RY_*`). Former spellings (`roughly.toml`, `# roughly: allow`, `ROUGHLY_*`, extension `roughly.*`) still fall back today; `roughly.toml` must stay.
- **The type system admits only what is fast to check: Hindley–Milner (user directive).** `.Rtypes` overloading is the one sanctioned exception. Traits/typeclasses and general subtyping are declined, not deferred. Evaluate every type-system proposal against `decisions.md` first. Open questions: `contributing/design/open-questions.md`.
- **CI gates only `crates/ry`** (`default-members`), so no fixture or fuzz suite runs in CI. The widened workflow is staged in `.github/pending-ci.yml` for a human `git mv` (the agent token lacks workflow scope). Run `just gate` locally per slice and never read a green CI badge as "the fixtures pass".
- **Debt queue:** `backlog.md` §abstraction & duplication review and §test & fuzz architecture review. Both list items deliberately not worth fixing; respect those.

## Long-term

### Performance traps

- **A per-item query must never scan a per-file or per-project list.** Compute once per file in a memoized query and probe per item. Quadratics here have two factors (items × names looked up), so vary both axes when measuring; one axis alone looks linear.
- **A per-item read that joins over a project-wide list needs a cap**, not just memoization (`CONDITIONAL_SLOT_JOIN_CAP`, widening to `Unknown`).
- **A check reads per-name firewalls, never a project-wide map** (`package_name`, `interface_group`, `frame_binder`); `crates/semantics/tests/test_incremental.rs` pins it by counting query executions.
- **Never call `Checker::infer` twice on one expression.** It has no memo: a double walk made operator chains cost 2^depth and multiplied each finding 2^depth times (fixtures dedup, so they cannot see it). Thread the `Ty` you already have.
- **`GlobalEnv` facts are borrowed per-file values** (`&'db`); returning them owned makes every item pay for the whole project.
- **Hot tracked queries returning maps/vecs use `returns(ref)`**; `returns(clone)` deep-clones per call.
- **A type is an interned DAG: walk it with a memo**, never as a tree (`substitute_rigid` went from 278M to 2M calls).
- **A write inside a call-argument block binds at top level** (`test_that("x", { out <- f() })` publishes `out` package-wide), which is why a package's tests can dominate its cost. Whether R binds outward depends on the callee: `try`/`suppressWarnings`/`invisible` do, `local`/`test_that` do not.
- **Measurement traps:** alternate the two binaries run by run (sequential blocks showed a fake 2× win); measure the analysis path with `--output json` (the human reporter is itself quadratic in findings); compare corpus outputs as unique sorted records, not line counts; every in-container timing is effectively ≤2 cores.
- **Budgets:** keystroke-to-diagnostics p50 ≤ 30 ms / p95 ≤ 100 ms at 300K LoC, pinned by `crates/semantics/tests/test_perf.rs` (`just perf`, needs the fetched corpus). Reference: 794K lines / 69 CRAN packages cold in ~14 s at ~1.3 KiB resident per line; keystroke p50 ~12 ms.

### Semantics traps

- **`GlobalEnv` has no default method bodies, on purpose.** The SCC fixpoint's view is a second implementor; a defaulted method silently gives it a wrong answer (this once collapsed every recursive function using `Date + 1` to `Unknown`). Test such facts with a recursive/non-recursive fixture pair.
- **Dispatch and the numeric constraint must read the same class scope** (stubs plus project): a package's own `+.Money` makes `Money` arithmetic.
- **A read recorded by name instead of by expression id is invisible to the slot model** (`%op%` reads go through `mark_slot_read`).
- **One structural map rewrites types: `types::map_child_types`** (reads: `for_each_child_type`, `Parameter::types`). A second hand-written walk once skipped a parameter's default and leaked an inference variable across the item boundary. New `TyKind`/`FunctionType`/`Parameter` members go there and nowhere else.
- **Every type the checker prints must parse back** (`test_roundtrip.rs`); a new constraint, `TyKind` or rendering includes its grammar side. A function member of a union renders parenthesized.
- **Every mismatch report goes through `Checker::type_mismatch`**, which narrows record/function mismatches to the failing field or position and narrows the range with the message.
- **A finding underlines the smallest expression its message is about.** Thread narrower ranges (`name_range` on fields and call arguments); keep the whole type as the verdict and use leaves only to place it.
- **Every finding inside an item is reported; only the export is cut to `Unknown`** when the item fails.
- **A `#:` annotation declares parameter types, never the parameter list.** The export is always the formals; `check_declared_function` owns the reconciliation, and a call is never blamed for an annotation's mistake.
- **Do not `table.resolve` a type you display as written**: resolve expands aliases.
- **One `TypeRenderer` spans everything that shares variable names** (a signature, both sides of a mismatch).
- **Immediate reads resolve against the nearest earlier writer in the file; deferred reads (closures) differ by kind**: a package closure sees the project-wide winner, a script closure the settled file frame. Getting this backwards breaks forward references from function bodies.
- **`"x" <- 1` binds `x`** via the name-target arm, not `replacement_base` (which reads the base first).
- **A fixture cannot exercise anything keyed on `PackageMetadata`**; those cases belong in `crates/ry/tests/test_cli.rs`. Read what a bless recorded before believing it covers the scenario.

### Parser and annotation traps

- **Recovery stops at the statement boundary when the next line assigns** (`starts_new_statement`), and `=` does not count (it is a named argument inside a list). The check scans backwards because the newline was consumed with the previous element.
- **One mistake, one finding.** A lexer `ERROR_TOKEN` is already diagnosed and suppresses its statement's boundary report; the `#:` grammar reports once per region (`error_unclosed` lets one unclosed opener through). Never report a consequence beside its cause, and blame the incomplete construct, not where the search stopped.
- **An unterminated quote ends at its first line break**, so typing a quote does not take the rest of the file out of analysis.
- **`#:` block-form rules are about lines, and a stitched line's marker may sit inside a nested node**: count lines over all of the block's tokens. A `<T>` binder list is a prefix of the next type, not an item. A refused region is an `ERROR` node, and a block containing one carries no typing payload.
- **A diagnostic code names whose grammar broke**: `syntax-error` for R, `annotation` for `#:` (`SyntaxError::in_annotation`).
- **Formatter:** trailing comments attach inside expression nodes, so placement decisions are token-level (`last_token_before`); line math uses `significant_range`; the formatter refuses only on R-grammar errors.
- **Golden error suite** (`crates/syntax/tests/errors`): a new parser error path adds its case in the same change.

### IDE and server

- **Cursor position model:** smallest end-inclusive expression with a NameRef tie-win, so the declaration scan stays a fallback for every binding kind. Which token declares an item has one answer, `semantics::item_name_range`; `:=` is never a definition.
- **The server** is one async-lsp frontend thread plus one worker owning the database; edits flip salsa's cancellation token and later jobs use a fresh storage handle. Host assembly (config gating, typing modes, suppressions, lints) is shared with the CLI in `crates/ry/src/diagnostics.rs`.
- **Coherence failures panic** (document sync); IDE lookups never panic.
- **CLI reporting is miette's** (`crates/ry/src/cli.rs`): its header column counts bytes, so `read_span` re-counts characters; ranges over several lines clamp to the first; sources are borrowed (`NamedText`), never `NamedSource`.

### Testing

- **An oracle is trusted only once an injected bug turns it red.** Determinism, idempotence and round-trip are self-consistency checks a uniformly wrong answer satisfies; three batteries here were green over real mutants.
- **Give a differential oracle inputs that always exist** (the tree-sitter acceptance test never ran while it pointed only at the gitignored corpus, and its skip note was hidden by the test runner).
- **A foreign renderer's expectations are never bulk-blessed**; adjudicate each case against the typing reference.
- **The fuzz batteries' cost is re-parsing the stub corpus per fresh database**, not iteration count.
- **A gate must fail on its own exit status**: `cmd | grep …; echo OK` always passes, and a background job's output file can be read mid-write.
- **The REPL pty suite flakes under concurrent load** (one failure, suite wall time 41 s instead of 4–6 s); re-run serially before believing it. A real desync reproduces and takes ~90 s.

### REPL and environment

- **The REPL never feeds R input through the console to set state.** Startup work goes through `RApi::eval` between `setup_Rmainloop()` and `run_Rmainloop()`. Details, including the Windows recipe (`GA_initapp` lives in `Rgraphapp.dll`), are in `contributing/design/repl.md`.
- **R installs in agent containers** (`apt-get install -y --no-install-recommends r-base-core`, or the CRAN apt repo for current R). `fs` needs `libuv1-dev`; the tidyverse needs `libcurl4-openssl-dev libharfbuzz-dev libfribidi-dev libfreetype-dev libpng-dev libjpeg-dev`. Free disk first (`target/debug/incremental`).
- **Export manifests** (`types/*.exports`) come from a live session (`Rscript scripts/export-manifests.R`) on an R at least as new as the recorded one; a hand-written or older-R manifest turns real exports into false `unresolved`. Meta-packages expand via `stubs::META_PACKAGE_MEMBERS`.
- **Stubs** are declaration-only `.Rtypes` files in `types/` (the `#:` grammar, loader in `semantics/src/stubs.rs`), a set-once salsa input; project `stubs/*.Rtypes` override. An overload set ends with an `Any` fallback.
- **Landing-page and README samples are captured from the running tools**, never hand-edited; a changed message or blame range means re-capturing them (`backlog.md` lists the pages; `scripts/render-diagnostic-svg.rs` makes the README image).

### Conventions

- **Verify with CI's toolchain:** `rustup update stable` first, and check for a formatting diff rather than an exit code piped through `head`. The nightly-only `rustfmt.toml` keys warn on stable.
- **Exclude exactly `zed_ry`** (`--workspace --exclude zed_ry`); cargo only warns on a misspelled `--exclude` and builds the crate anyway.
- **`[check] typing` defaults off**: CLI probes need `[check]\ntyping = true` in `ry.toml`. `[check] unused` defaults on.
- **`reference/formatting-rules.md` is generated** from `crates/format/tests/formatter.template.md` (`RY_BLESS=1 cargo test -p format --test test_format_docs`).
- **Scratch output goes to absolute paths**: `cd dir && job &` backgrounds the whole compound, so later commands run in the repo, and `git add -A` then commits their output. Read `git status` before `git add -A`.
- **The `fuzz/` lockfile is gitignored**; a fresh resolution can pick a `salsa-macro-rules` that does not match the pinned `salsa`. Seed it from the workspace lock.
- **Docs writing:** plain declarative English, no jargon or sales lines; motivate a design with its technical reason; explain a thing once where it is introduced, tabulate it once in Reference, link elsewhere; never use a capability before the page introduces it. Fixed terms: finding, diagnostic code, annotation, check (command), code analysis (capability), the language server, project. Verify every docs claim against the binary.
- **Docs site structure:** five sidebar groups by purpose (Introduction, Type checking, Guides, Reference, Contributing); breadcrumbs come from the sidebar (`docs/src/components/PageTitle.astro`).
- **UI:** no `border-l` accent rail on rounded elements.
