# Overview

ry is a language tool for R, built as a language server plus CLI. It aims to be world class at three things: code analysis on the level of rust-analyzer — with a static type checker at its core — plus code formatting and linting.

The type checker is central: no static type checker exists for R, so ry defines its own typing semantics (the contract lives in the typing reference at `docs/src/content/docs/reference/type-system.md`). Because R itself has no type-annotation syntax, annotations are written in `#:` comments using a JSDoc-like notation, which keeps annotated code fully compatible with ordinary R tooling.

Crates:

- `crates/` — the shipping product: `syntax` (lexer/parser, lossless rowan trees), `semantics` (the salsa-based analysis core and type checker), `format` (the formatter, syntax-only), `ide` (editor features as pure reads), `ry` (LSP server + CLI), and `repl` (the R console behind `ry repl` and `ry run` — runtime-loaded R, so the rest of the workspace stays R-less)

The project is built by AI agents driving development, with light human steering. Agents keep two written homes current: the docs site (`docs/`) holds the authoritative, user- and contributor-facing specs (they are contracts — mandatory to keep accurate), and `.agents/memory/MEMORY.md` is the agent knowledge base (engineering state, priorities, debt, and non-obvious design rationale, so they are not rediscovered). Update both in the same session as the work that changes them.

# Goals

- Deliver high-quality diagnostics for R in the style of Rust and Elm: clear, precise, actionable wording; avoid overly internal or theory-heavy language when user-facing wording would be clearer; prefer precise source ranges over coarse fallback ranges.
- Provide full editor tooling — hover, completion, goto-definition, references, rename, inlay hints — and preserve the semantic information those features need whenever practical.
- Provide first-class formatting and linting alongside analysis.
- Scale to very large code bases, including more than 300,000 LoC; performance matters.

# Ownership mandate

The user has delegated full technical ownership to the agents: empty the backlog and bring the project to the best possible state — rust-analyzer quality. That explicitly covers code structure, crate boundaries, naming, performance, pipeline architecture, semantic correctness, and judged deduplication. Do not optimize for "safe, risk-free" minimal diffs; bring code to its intended shape, including large refactors, and take responsibility for the outcome. Design decisions that previously required a user check-in are now the agent's to make: decide, implement, and record the decision and rationale in `.agents/memory/decisions.md` (or the docs page it belongs to) in the same session. Work lands through pull requests (user directive).

# Do not think like a human (user directive)

Human engineering instincts — de-risking, staging, keeping diffs small and reviewable, avoiding "scary" rewrites — exist because a human's time is scarce and starting over is expensive for them. Neither is true for an agent, so those instincts pick the wrong strategy here:

- **Go directly to the intended end shape in one change**, however large and invasive. Break the whole codebase mid-change if the target design calls for it, then fix everything — compiler errors, warnings, tests — afterwards in one sweep. Do not sequence a redesign into small incremental steps to "manage risk"; that trades the right design for ceremony.
- **Never propose or choose a watered-down variant of a design because the full version is a big change.** If the full version is right, implement the full version. Starting over after a failed attempt is cheap; shipping the wrong shape is not.
- **File size is not a problem.** Do not split, reorganize, or flag a file merely for being large (the LSP server module is fine as one file). Split only when a genuine new logical component exists.
- Correctness gates are unchanged: the fixture suites, witnesses, clippy, and fmt must be green before a change lands — the point is to reach green in one big pass, not to shrink the change.

# Incremental analysis

Analysis is salsa queries at item granularity behind per-name firewalls, with cooperative cancellation and idle-time diagnostics scheduling in the server. The architecture page (`docs/src/content/docs/contributing/architecture.md`) is the contract — read it before touching the query graph or the server's scheduling, and keep it accurate. A check must never read a project-wide map directly: go through a per-name or per-item tracked projection, and pin the property in `crates/semantics/tests/test_incremental.rs`.

# Working autonomously

Never use the interactive question tool (`AskUserQuestion`); ask questions as plain text in your reply (user directive).

When working autonomously on a larger goal — a workflow, a multi-step change, or any task that spans several logical units — commit and push after each logical step, instead of saving everything for one final commit. A single large invasive redesign is ONE logical step: commit it when it is green, not in fragments along the way.

# Knowledge base and documentation (we can reduce repeition/duplication with MEMORY.md)

There are two written homes. Keep both current; spend the minimum that keeps them useful, and prefer bullet points.

- **`.agents/memory/` — the agent knowledge base, kept IN THE REPOSITORY.** `MEMORY.md` is the **index**, and it MUST be organized into three horizons — keep these exact sections:
  - **Short-term** — current focus and loose ends. Prune aggressively; delete each item once it is resolved or obvious from the tree.
  - **Mid-term** — active priorities, open bugs, and technical debt. Lives across sessions until done.
  - **Long-term** — durable, non-obvious design decisions and their rationale. Only things a future agent would otherwise rediscover. Keep terse and point at code or the docs.

  `MEMORY.md` also names every other knowledge document. Keep a *separate* document in this folder only for genuinely larger-scope material — currently `backlog.md` (the prioritized work punch-list) and `decisions.md` (the settled architecture decision log) — and reference it from `MEMORY.md`; never spawn a new knowledge file for something small (inline it into the right horizon instead). **A design document is not a memory file**: unsettled design work — proposals, open questions, sketchpads — belongs in the docs site under `docs/src/content/docs/contributing/design/`, listed on that folder's index page and deliberately kept out of the sidebar. Write new ones there. One deliberate exception to both the timeless rule and the no-new-files rule: `worklog.md` is a chronological one-line-per-cycle record that a scheduled routine appends to. Do not prune it as a rules violation; durable facts still go in the files above. Keeping this current — **adding** what is durable **and pruning** what is resolved, stale, or duplicated, and **promoting/demoting** items across horizons as their status changes — is part of repository hygiene, not optional; do it in the same session as the work.
  - **In-repo on purpose.** Memory lives in git so it is portable and shared: it travels with a `git clone` to any machine or cloud session, and every agent — including a fresh restart — reads the same source of truth, so nothing is lost when an agent or session is replaced. **Never** keep project knowledge in a private/local agent memory store (e.g. a per-tool `~/.claude/…` folder): other agents cannot read it and it does not travel. Because a reader may have zero project history, every entry must be **context-free and timeless** — no internal milestone/phase/gate names, no commit hashes, no "this session" (the same rule as code comments); state durable facts and point at the code or docs.
- **`docs/` (the docs site)** — the authoritative, user- and contributor-facing specs. They are contracts; keeping them accurate is mandatory.
  - Type checking: `type-checking/` (the tutorial) and `reference/type-system.md` (the semantics contract).
  - Contributing: `contributing/{architecture,structure,testing,authoring-stubs}.md`, plus `contributing/design/` — the unsettled drafts, which are explicitly NOT contracts and are the one place in the docs allowed to describe behavior that does not exist. Keep them out of the sidebar; the index page lists them.
  - Treat docs as a first-class deliverable: when behavior, design, or the fixture contract changes, update the relevant page in the same session and keep it in genuinely good shape — clear, accurate, no stale status. Do not rewrite a spec to paper over a temporary implementation gap; note the gap instead.
  - **Run the tool before claiming what it does.** Any statement about actual behavior — in a docs page, a memory document, or a commit message — must be confirmed by executing it, not recalled or inferred from the code. Build a throwaway project (a `ry.toml` plus one `.R` file) and read the real output. This is cheap, and skipping it is the single most reliable way this project ships a false statement: writing prose does not feel like a task that needs a test, so plausible-sounding claims go in unchecked, and the ones that are wrong are indistinguishable from the ones that are right until a user hits them. If a claim cannot be verified cheaply, mark it as unverified instead of asserting it. Before calling a design document done, have an adversarial reviewer subagent check it against the implementation and the settled decisions.

# Skills

If the user says:

- `get started`: read `.agents/memory/MEMORY.md` and the relevant docs pages, then continue with the next item in the mid-term priorities (assume fresh context).
- `cleanup memory`: aggressively prune the short-term section of `.agents/memory/MEMORY.md`; keep the mid- and long-term continuity intact.
- `code check`: review the relevant code for compliance with the coding guidelines. Report findings first and explicitly verify top-down module ordering plus the preferred `use` qualification style; types should usually be imported directly, and functions should usually have at least one module-level import instead of repeated fully qualified calls unless ambiguity requires qualification.
- `authoritative check`: compare the docs specs against the fixture suites and report contradictions, stale wording, or missing documented coverage.
- `implementation check`: compare the implementation against the docs specs and report contract or architecture mismatches.
- `session check`: end-of-session closure pass. Verify that decisions, open questions, and newly discovered work are captured in `.agents/memory/MEMORY.md` or the docs (watch for side investigations that created uncaptured follow-up work), and that memory and the docs are consistent with the implementation; report anything still hanging.

# Rust coding guidelines

* Do not write organizational comments or comments that summarize the code. Comments should only be written in order to explain "why" the code is written in some way in the case there is a reason that is tricky / non-obvious.
* Comments must be **context-free**. Never reference internal milestones, phases, process history, ticket/PR names, or commit hashes (e.g. "R0", "M3", "Phase 4", "gate (c)", "3f", "the spike", "added in the cutover"). A reader with zero project history must understand every comment — explain the "why" in domain terms, not in terms of when or how the code came to be.
* Prefer implementing functionality in existing files unless it is a new logical component. Avoid creating many small files.
* Do not create a sub-directory eagerly for a single file. A directory should hold more than one file before it exists; until then keep the file alongside its siblings (`foo.md`, not `foo/foo.md`). Promote a file to a directory only when a second file genuinely belongs with it.
* Avoid using functions that panic like `unwrap()`, instead use mechanisms like `?` to propagate errors.
* Be careful with operations like indexing which may panic if the indexes are out of bounds.
* Never silently discard errors with `let _ =` on fallible operations.
* Never create files with `mod.rs` paths - prefer `src/some_module.rs` instead of `src/some_module/mod.rs`.
* When creating new crates, prefer specifying the library root path in `Cargo.toml` using `[lib] path = "...rs"` instead of the default `lib.rs`, to maintain consistent and descriptive naming (e.g., `gpui.rs` or `main.rs`).
* Avoid creative additions unless explicitly requested.
* Use full words for variable names (no abbreviations like "q" for "queue").
* Prefer importing types directly. For functions, prefer at least one module-level import instead of fully qualifying every call; fully qualified paths are still fine when needed to avoid ambiguity.
* Prefer procedural or functional code over OOP-style method organization when there is no clear stateful abstraction. Use free functions by default. Use `impl` blocks when a type genuinely owns stateful behavior or when constructor-style helpers materially improve clarity, but do not use methods just to namespace procedural code.
* Organize modules top-down. Put core types and public functions first, order container types before the types they contain, and keep private types and helper functions after public items in the same caller-before-callee order.
* Do not optimize for the smallest safe fix. When you touch an area, bring it to the intended shape for that change, remove dead paths or temporary seams, and pay down nearby technical debt needed to keep the code coherent. You are responsible for code quality, not just feature delivery.
* Avoid helper-function indirection when logic is only used once and does not materially improve testability or readability. Prefer inlining small one-off solutions unless doing so would create large duplication.

# Design bar

- We require world-class implementation quality, not merely passing behavior.
- Use the simplest correct data model and implementation that can express the required semantics.
- Do not introduce complicated abstractions unless they remove real complexity.
- Make illegal states unrepresentable whenever practical.
- Maintain a single source of truth for each semantic fact whenever practical.
- If a fact is cheaply and reliably derivable from an existing source of truth, do not store it separately unless there is a clear performance reason.
- Do not introduce duplicated state, mirrored tables, or cached derived data that can drift out of sync without clear justification.
- Use designs that minimize cloning, copying, and whole-structure rebuilding.
- Optimize for very fast incremental analysis and low memory churn.
- If you notice a structural design problem, you must surface it early and explicitly instead of working around it.

# Design review trigger

If you see any of the following, do not work around it — stop, design the fix, and implement it (recording the decision in `decisions.md` when it settles an architectural question):

- multiple sources of truth
- duplicated metadata
- derived state being persisted without clear justification
- snapshot-local ids where stable indirection would suffice
- repeated cloning or copying introduced only to maintain convenience state
- a design that feels more complicated than the semantics require

The recorded decision must state: the previous source of truth, what was duplicated or structurally weak, the chosen target shape, and the expected impact on correctness, simplicity, performance, and incremental analysis.

# Error handling

- Do not swallow analysis, synchronization, or document-loading errors anywhere in the project.
- If an operation is required to keep analysis state coherent, surface the failure immediately with `panic!` rather than logging and continuing with corrupted or stale state.
- In particular, document-sync or analysis-sync failures in the LSP path are unrecoverable and should panic immediately rather than trying to keep the server alive in a bad state.
- Example: if syncing an open document into analysis state fails during `did_open`, `did_change`, or `did_save`, do not fall back to stale state or best-effort logging; `panic!`.

# Testing strategy

- Prefer fixtures: they are the primary way to validate analysis behavior, they are easy for humans to read in diffs, and they make it easy to create many tests quickly.
- Fuzzing is pipeline-wide and from day one (user directive): every stage — parsing, lowering, naming, inference, diagnostics, incrementality, formatting, IDE — gets fuzz + property coverage the day it exists, never as a later add-on. A bounded pass belongs in the default test suite; see the fuzzing decision record in `decisions.md`.
- Prefer adding or tightening fixtures before writing parser-local or crate-local unit tests unless the behavior is genuinely awkward to express as a fixture.
- Favor fixture renderers that expose semantic facts rather than implementation detail.
- When adding a new phase or module, add or extend a fixture suite for that phase before relying on ad hoc unit tests.
- Use the lightest fixture change that captures the failing shape.
- Read the testing page in the docs (`docs/src/content/docs/contributing/testing.md`) before changing the fixture harness or adding a new fixture suite.
- Run focused fixture cases with `just fixture group__case [test-binary] [package]` (defaults: `test_typing_fixtures`, `semantics`).
- Prefer running focused crate tests while iterating (`cargo test -p semantics`); `just gate` is the full pre-landing check.
- Keep fixture `group__case` names stable as the test identity, and reject duplicate names across the suite instead of silently shadowing one case with another.
- Treat fixtures as the desired semantics contract, not as a regression suite for preserving known-wrong behavior. Review expectation changes deliberately and update expectations only when wording or behavior intentionally improves; never commit an intentionally wrong outcome just to keep the suite green.
- Some fixture cases may be unreasonable or no longer worth preserving. If you encounter one, clean it up instead of treating it as authoritative by default.

# Rules hygiene

This `AGENTS.md` file is read by every agent session. Keep it extremely high-signal.

Editing or clarifying existing rules is always welcome. New rules must meet **all three** criteria:

1. **Non-obvious** — someone familiar with the codebase would still get it wrong without the rule.
2. **Repeatedly encountered** — it came up more than once (multiple hits in one session counts).
3. **Specific enough to act on** — a concrete instruction, not a vague principle.

Rules that apply to a single crate belong in that crate's own `AGENTS.md` file, not the repo root.

Avoid architectural descriptions of a crate (module layout, data flow, key types) — these go stale fast and the agent can gather them by reading the code. Rules should be **traps to avoid**, not **maps to follow**.
