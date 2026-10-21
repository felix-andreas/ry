# Overview

ry is a language tool for R: a language server plus CLI, aiming at rust-analyzer-level code analysis with a static type checker at its core, plus formatting and linting. ry defines R's typing semantics itself (contract: `docs/src/content/docs/reference/type-system.md`); annotations live in `#:` comments, so annotated code stays plain R.

Crates (`crates/`): `syntax` (lexer/parser, lossless rowan trees), `semantics` (salsa analysis core and type checker), `format` (formatter, syntax-only), `ide` (editor features as pure reads), `ry` (LSP server + CLI), `repl` (the R console behind `ry repl`/`ry run`; R is loaded at runtime, so nothing else needs R).

AI agents drive development with light human steering. Two written homes must stay current in the same change as the work: the docs site (`docs/`, the contracts) and `.agents/memory/` (agent knowledge).

# Goals

- Diagnostics in the style of Rust and Elm: clear, precise, actionable wording, no internal jargon, the tightest accurate source range.
- Full editor tooling (hover, completion, goto-definition, references, rename, inlay hints), keeping the semantic facts those features need.
- First-class formatting and linting.
- Scale to more than 300,000 LoC; performance matters.

# Ownership mandate

The user delegated full technical ownership: empty the backlog and reach rust-analyzer quality, covering structure, crate boundaries, naming, performance, architecture, correctness and deduplication. Bring code to its intended shape, including large refactors; do not optimize for minimal diffs. Make design decisions yourself and record them in `.agents/memory/decisions.md` (or the docs page they belong to) in the same change. Work lands through pull requests (user directive).

# Do not think like a human (user directive)

De-risking, staging and small reviewable diffs exist because human time is scarce and restarting is expensive. Neither holds for an agent:

- **Go directly to the intended end shape in one change**, however invasive; break the build mid-change and fix everything in one sweep.
- **Never choose a watered-down design because the full version is big.** Restarting is cheap; shipping the wrong shape is not.
- **File size is not a problem.** Split only for a genuine new logical component.
- The gates are unchanged: fixtures, witnesses, clippy and fmt are green before a change lands. **Run the gate once, after the whole change**, not after each edit: for an agent the gate costs more than the change. Brief subagents the same way.

# Incremental analysis

Analysis is salsa queries at item granularity behind per-name firewalls, with cooperative cancellation and idle-time diagnostics in the server. `docs/src/content/docs/contributing/architecture.md` is the contract: read it before touching the query graph or the server's scheduling, and keep it accurate. A check never reads a project-wide map directly: go through a per-name or per-item tracked projection and pin it in `crates/semantics/tests/test_incremental.rs`.

# Working autonomously

- Never use the interactive question tool (`AskUserQuestion`); ask in plain text (user directive).
- On a multi-step goal, commit and push after each logical step. One invasive redesign is one step: commit it when green.
- Run one subagent at a time (user directive).
- Commit messages and PR titles follow Conventional Commits (`refactor: …`, `docs: …`).

# Knowledge base and docs

- **`.agents/memory/` lives in the repository**, so every agent and session reads the same truth; never keep project knowledge in a private agent store. `MEMORY.md` is the index with exactly three sections: **Short-term** (current focus; delete once resolved), **Mid-term** (priorities, open bugs, debt), **Long-term** (durable, non-obvious traps and rationale). Larger material lives in `backlog.md` (open work) and `decisions.md` (settled decisions); do not create files for small things. `worklog.md` is a chronological routine log; leave it alone. Add what is durable and prune what is resolved in the same change.
- **Every entry and every comment is context-free and timeless**: no milestone/phase names, commit hashes, "this session", or process history. State the fact and point at code or docs.
- **The docs site is the contract.** Typing: `type-checking/` (tutorial) and `reference/type-system.md`. Contributing: `contributing/{architecture,structure,testing,authoring-stubs}.md`. Unsettled designs go to `contributing/design/` (out of the sidebar, listed on its index page), the only docs allowed to describe behavior that does not exist. Update the affected page with every behavior change; note implementation gaps instead of rewriting the spec around them.
- **Run the tool before claiming what it does.** Every behavior claim in docs, memory or a commit message is confirmed by executing it (a throwaway `ry.toml` plus one `.R` file), or marked unverified. Prose feels like it needs no test, which is exactly how false claims ship. Have an adversarial reviewer subagent check a design document against the implementation before calling it done.

# Skills

If the user says:

- `get started`: read `.agents/memory/MEMORY.md` and the relevant docs, then continue with the next mid-term priority.
- `cleanup memory`: aggressively prune the short-term section of `MEMORY.md`; keep mid- and long-term continuity.
- `code check`: review the relevant code against the coding guidelines; report findings first, and explicitly verify top-down module ordering and the `use` style (types imported directly; functions with at least one module-level import).
- `authoritative check`: compare the docs specs with the fixture suites; report contradictions, stale wording, or undocumented coverage.
- `implementation check`: compare the implementation with the docs specs; report contract or architecture mismatches.
- `session check`: verify decisions, open questions and newly found work are captured in memory or docs, and that both match the implementation; report anything still hanging.

# Rust coding guidelines

* Comments only explain a non-obvious "why" (an invariant, an R semantic quirk, a trap, why the simpler alternative is wrong), in one to three lines. No organizational or summarizing comments, no doc comments that restate a name, no history.
* Prefer existing files unless the code is a new logical component; no eager single-file directories; never `mod.rs`.
* New crates set `[lib] path = "<name>.rs"` instead of `lib.rs`.
* No panicking shortcuts (`unwrap()`, unchecked indexing); propagate errors with `?`. Never discard a fallible result with `let _ =`.
* No creative additions unless requested. Full-word variable names.
* Import types directly; give functions at least one module-level import instead of repeated full paths, unless ambiguous.
* Prefer free functions; use `impl` blocks for genuinely stateful types or clarifying constructors, not to namespace procedural code.
* Organize modules top-down: core types and public functions first, containers before their contents, private helpers after, caller before callee.
* When you touch an area, bring it to its intended shape: remove dead paths and temporary seams, and pay down nearby debt.
* Inline one-off helpers unless they materially help readability or testing.

# Design bar

- World-class implementation quality, not merely passing behavior.
- The simplest correct data model; no abstraction that does not remove real complexity.
- Make illegal states unrepresentable; one source of truth per semantic fact; do not store what is cheaply derivable without a clear performance reason.
- Minimize cloning, copying and whole-structure rebuilding; optimize for fast incremental analysis and low memory churn.
- Surface structural design problems early instead of working around them.

# Design review trigger

On multiple sources of truth, duplicated metadata, persisted derived state without justification, snapshot-local ids where stable indirection would do, cloning only to maintain convenience state, or a design more complicated than the semantics: stop, design the fix and implement it. A decision record states the previous source of truth, what was weak, the target shape, and the impact on correctness, simplicity, performance and incremental analysis.

# Error handling

- Never swallow analysis, synchronization or document-loading errors.
- A failure that leaves analysis state incoherent panics immediately. Document-sync failures in the LSP path (`did_open`, `did_change`, `did_save`) are unrecoverable: `panic!`, never stale state or best-effort logging.

# Testing strategy

- Fixtures first: they are the semantics contract, readable in diffs, and cheap to add. Add or tighten a fixture before a crate-local unit test, and give a new module a fixture suite before ad-hoc tests. Renderers expose semantic facts, not implementation detail. Use the lightest fixture that captures the failing shape.
- Fuzzing is pipeline-wide from day one (user directive): every stage gets fuzz and property coverage when it is created, with a bounded pass in the default suite (see `decisions.md`).
- Read `docs/src/content/docs/contributing/testing.md` before changing the fixture harness or adding a suite.
- Focused case: `just fixture group__case [test-binary] [package]` (defaults `test_typing_fixtures`, `semantics`). Iterate with `cargo test -p <crate>`; `just gate` is the pre-landing check.
- `group__case` names are the stable identity; duplicates are rejected.
- Change an expectation only when behavior or wording intentionally improves; never bless a known-wrong outcome. Clean up a case that is no longer worth keeping.

# Rules hygiene

This file is read by every session; keep it high-signal. A new rule must be non-obvious, repeatedly encountered, and concrete enough to act on. Crate-specific rules go in that crate's own `AGENTS.md`. Write traps to avoid, not maps of the code.
