# Overview

ry is a language tool for R. It ships as a language server and a command-line tool. It aims to be world class at three things: code analysis on the level of rust-analyzer, code formatting, and linting. A static type checker sits at the core of the analysis.

No static type checker exists for R, so ry defines its own typing semantics. The contract for those semantics is the typing reference at `docs/src/content/docs/reference/type-system.md`. R has no syntax for type annotations. ry therefore writes annotations in `#:` comments, in a notation similar to JSDoc. Annotated code stays fully compatible with ordinary R tooling.

The workspace has two crate directories.

- `crates/` holds the shipping product.
  - `syntax` is the lexer and parser. It builds lossless rowan trees.
  - `semantics` is the salsa-based analysis core and the type checker.
  - `format` is the formatter. It reads syntax only.
  - `ide` provides editor features as pure reads.
  - `ry` is the LSP server and the CLI.
  - `repl` is the R console behind `ry repl` and `ry run`. It loads R at runtime, so the rest of the workspace needs no R.
- `legacy/` holds the frozen previous implementation. It contains `analysis-legacy`, `engine-legacy`, `ry-legacy`, and the `fixtures` harness. It also contains `differential`, which is now only the cross-stack benchmark harness. The identity-parity program is complete, and the user retired it. The new stack's fixtures are the contract, so no change needs the oracle to agree. Everything sits in one directory because every dependency edge points at the oracle. Deleting the legacy stack is then a single directory removal. The performance witnesses that apply to the new stack alone move out first.

AI agents drive development on this project, and humans steer it lightly. Agents keep two written homes current. The docs site in `docs/` holds the authoritative specs for users and contributors. Those specs are contracts, so keeping them accurate is mandatory. The agent knowledge base in `.agents/memory/MEMORY.md` holds engineering state, priorities, debt, and non-obvious design rationale, so that no agent has to rediscover them. Update both in the same session as the work that changes them.

# Goals

- Deliver high-quality diagnostics for R in the style of Rust and Elm. Their wording is clear, precise, and actionable. Avoid internal or theory-heavy language when user-facing wording is clearer. Prefer precise source ranges over coarse fallback ranges.
- Provide full editor tooling: hover, completion, goto-definition, references, rename, and inlay hints. Preserve the semantic information those features need whenever practical.
- Provide first-class formatting and linting alongside analysis.
- Scale to very large code bases, including more than 300,000 lines of code. Performance matters.

# How to write

Agents write this repository's documentation, code comments, commit messages, and pull request descriptions. Humans read them. The agent does not need the prose, so writing that sounds clever only costs the reader time. There is a second reason to write plainly. Dense prose hides wrong claims, because a reader cannot check a statement they cannot parse. Unreadable text is unreviewable text, and a reviewer who cannot parse a pull request description approves the change without understanding it.

Write plain English. The plain-language standard ISO 24495-1 states the goal: the reader finds what they need, understands it, and can use it. The rules below are how to reach that goal here. They apply to everything an agent writes: answers to the user, documentation, code comments, commit messages, and pull request descriptions.

## Rules

- Use plain English. Keep the language simple, precise, and concise. Optimize for reading comprehension, not for sounding clever, academic, or lawyer-like.
- Put one idea in each sentence. Split a sentence that contains several logical steps into several sentences.
- State the rule first, then state the reason in its own sentences.
- Explain things in the order the reader should learn them. Introduce a concept before you use it, and finish one idea before you start the next.
- Write complete sentences with a subject and a verb. Do not open with a verbless phrase such as "two edits, one addition".
- Write in the active voice, and make the thing you are describing the subject. In a specification the subject is the construct, not the tool. Write "the pipe types as that call", not "ry types the pipe as that call". Name ry only where the statement is genuinely about the tool: a diagnostic, a default, or a deliberate limit. The passive is better than a pointless actor, so prefer "a flexible operand is constrained to numeric" over naming ry.
- Write literally. Do not use metaphors, and do not reason by analogy.
- Do not invent a term. A coined name such as "the literal courtesy" means nothing to a reader who has not read the code that named it. Use the same word for the same thing every time.
- Cut filler. Do not close a paragraph or a section with a flourish that states no fact.
- Avoid em-dashes, semicolons, and parentheses. A sentence that needs one usually wants to be two sentences, and a side note usually wants to be cut.
- Keep normative words exact in a specification. The words "must", "may", "is an error", and "is not supported" carry meaning, so do not soften them.
- Prefer bullet points in reference pages.

## Rules for answers to the user

- Make the answer self-contained. Assume the reader has not read the conversation. Introduce the context, then make your point.
- Give short answers. The user asks for details when they want them.
- Cut everything from a recommendation or a decision that does not change the choice.
- Name the third option when two options appear to conflict. Do not present a false choice when a third option resolves the conflict.
- Do not use the interactive multiple-choice picker. Ask with a numbered list instead.

## Example

Avoid writing like this:

> `x |> f(y)` is syntax R's own parser rewrites to `f(x, y)` before evaluation, and it types as exactly that call: the piped value becomes the first positional argument, every call rule above applies (arity, argument compatibility, overload selection), chains compose left to right, and a type error on the piped value blames the left-hand expression.

Write like this instead:

> R's parser rewrites `x |> f(y)` into `f(x, y)` before it evaluates the code. ry types the pipe as that call and nothing else. The piped value becomes the first positional argument. All call rules above apply to it: arity, argument compatibility, and overload selection. Chains compose from left to right. If the piped value has a type error, ry reports it on the left-hand expression.

# Ownership mandate

The user has delegated full technical ownership to the agents. Empty the backlog and bring the project to the best possible state, which means rust-analyzer quality. This covers code structure, crate boundaries, naming, performance, pipeline architecture, semantic correctness, and judged deduplication. Do not optimize for a safe, risk-free, minimal diff. Bring code to its intended shape, including large refactors, and take responsibility for the outcome.

Design decisions that once needed a user check-in are now the agent's to make. Decide, implement, and record both the decision and its rationale in `.agents/memory/decisions.md`, or in the docs page it belongs to, in the same session.

Two constraints stand. Work directly on `main`, which is a user directive. Do not open new pull requests.

# Do not think like a human (user directive)

Human engineers de-risk work, stage it, keep diffs small and reviewable, and avoid rewrites that look scary. These instincts exist because a human's time is scarce, and because starting over is expensive for a human. Neither is true for an agent, so these instincts pick the wrong strategy here.

- Go directly to the intended end shape in one change, however large and invasive that change is. Break the whole codebase in the middle of the change if the target design needs it. Then fix everything in one sweep: compiler errors, warnings, and tests. Do not split a redesign into small steps to manage risk. That trades the right design for ceremony.
- Never propose or choose a watered-down variant of a design because the full version is a big change. Implement the full version if the full version is right. Starting over after a failed attempt is cheap. Shipping the wrong shape is not.
- File size is not a problem. Do not split, reorganize, or flag a file only because it is large. The LSP server module is fine as one file. Split a file only when it holds a genuine new logical component.
- The correctness gates do not change. The fixture suites, the witnesses, clippy, and fmt must be green before a change lands. Reach green in one big pass instead of shrinking the change.

# Incremental analysis

The analysis core is incremental. `semantics` is a salsa database, so queries are memoized and an edit cancels in-flight analysis cooperatively. Per-item interface firewalls stop an edit inside one item from invalidating the rest of the project. The LSP server publishes a fast diagnostics wave first and schedules the semantic wave at idle time.

The architecture page at `docs/src/content/docs/contributing/architecture.md` is the contract. Read it before you touch the analysis core or the server's scheduling, and keep it accurate. Deferred performance work lives in `.agents/memory/backlog.md`.

# Working autonomously

Commit and push after each logical step when you work autonomously on a larger goal. This applies to a workflow, a multi-step change, and any task that spans several logical units. Do not save everything for one final commit. One large invasive redesign is a single logical step. Commit it when it is green, not in fragments along the way.

# Knowledge base and documentation

There are two written homes. Keep both current. Spend the minimum effort that keeps them useful, and prefer bullet points.

## `.agents/memory/`

This folder is the agent knowledge base, and it lives in the repository. `MEMORY.md` is its index. It must keep these three sections, under these exact names.

- **Short-term** holds the current focus and loose ends. Prune it aggressively. Delete an item once it is resolved, or once the source tree makes it obvious.
- **Mid-term** holds active priorities, open bugs, and technical debt. These items live across sessions until they are done.
- **Long-term** holds durable, non-obvious design decisions and their rationale. Record only what a future agent would otherwise rediscover. Keep each entry terse and point at the code or the docs.

`MEMORY.md` also names every other knowledge document. Keep a separate document in this folder only for material of genuinely larger scope, and reference it from `MEMORY.md`. Two such documents exist today. `backlog.md` is the prioritized work punch-list. `decisions.md` is the settled architecture decision log. Never create a new knowledge file for something small. Inline it into the right horizon instead.

A design document is not a memory file. Unsettled design work belongs in the docs site under `docs/src/content/docs/contributing/design/`. This covers proposals, open questions, and sketchpads. List a new design document on that folder's index page, and keep it out of the sidebar.

`worklog.md` is a deliberate exception to both the timeless rule and the no-new-files rule. It is a chronological record with one line per cycle, and a scheduled routine appends to it. Do not prune it as a rules violation. Durable facts still go in the files above.

Keeping memory current is repository hygiene, not an optional extra. Do it in the same session as the work. That means three things: add what is durable, prune what is resolved or stale or duplicated, and promote or demote items across horizons as their status changes.

Memory lives in git on purpose, because git makes it portable and shared. It travels with a `git clone` to any machine or cloud session. Every agent then reads the same source of truth, including an agent that restarts from nothing, so no knowledge is lost when an agent or a session is replaced. Never keep project knowledge in a private or local agent memory store, such as a per-tool `~/.claude/` folder. Other agents cannot read that store, and it does not travel. A reader may have zero project history, so every entry must be context-free and timeless. Do not name internal milestones, phases, or gates. Do not cite commit hashes. Do not write "this session". This is the same rule that applies to code comments. State durable facts and point at the code or the docs.

## `docs/`

The docs site holds the authoritative specs for users and contributors. They are contracts, so keeping them accurate is mandatory.

- Type checking has two homes. `type-checking/` is the tutorial. `reference/type-system.md` is the semantics contract.
- Contributing has four pages: `contributing/architecture.md`, `contributing/structure.md`, `contributing/testing.md`, and `contributing/authoring-stubs.md`.
- `contributing/design/` holds unsettled drafts. They are explicitly not contracts. They are the one place in the docs allowed to describe behavior that does not exist. Keep them out of the sidebar. The index page lists them.
- Treat docs as a first-class deliverable. Update the relevant page in the same session when behavior, design, or the fixture contract changes. Keep every page in genuinely good shape: clear, accurate, and free of stale status. Never rewrite a spec to paper over a temporary implementation gap. Note the gap instead.

Run the tool before you claim what it does. Confirm any statement about actual behavior by executing it. Do not recall it, and do not infer it from the code. This applies to a docs page, a memory document, and a commit message alike. Build a throwaway project with a `ry.toml` and one `.R` file, then read the real output.

This check is cheap, and skipping it is the most reliable way this project ships a false statement. Writing prose does not feel like a task that needs a test, so a plausible claim goes in unchecked. A wrong claim then looks exactly like a right one until a user hits it. Mark a claim as unverified instead of asserting it when you cannot verify it cheaply. Have an adversarial reviewer subagent check a design document against the implementation and the settled decisions before you call it done.

# Skills

If the user says:

- `get started`: read `.agents/memory/MEMORY.md` and the relevant docs pages, then continue with the next item in the mid-term priorities. Assume you have fresh context.
- `cleanup memory`: prune the short-term section of `.agents/memory/MEMORY.md` aggressively. Keep the mid-term and long-term sections intact.
- `code check`: review the relevant code against the coding guidelines. Report the findings first. Verify two things explicitly: top-down module ordering, and the preferred `use` qualification style. Types should usually be imported directly. Functions should usually have at least one module-level import instead of repeated fully qualified calls, unless ambiguity forces qualification.
- `authoritative check`: compare the docs specs against the fixture suites. Report contradictions, stale wording, and documented coverage that is missing.
- `implementation check`: compare the implementation against the docs specs. Report mismatches in the contract or the architecture.
- `session check`: run an end-of-session closure pass. Verify that `.agents/memory/MEMORY.md` or the docs capture the decisions, the open questions, and the newly discovered work. Watch for side investigations that created follow-up work nobody captured. Verify that memory and the docs are consistent with the implementation. Report anything still hanging.

# Rust coding guidelines

- Do not write organizational comments, and do not write comments that summarize the code. Write a comment only to explain why the code is written in some way, and only when that reason is tricky or non-obvious.
- Keep comments context-free. Never reference internal milestones, phases, process history, ticket names, pull request names, or commit hashes. Examples of forbidden references are "R0", "M3", "Phase 4", "gate (c)", "3f", "the spike", and "added in the cutover". A reader with zero project history must understand every comment. Explain the reason in domain terms, not in terms of when or how the code came to be.
- Prefer implementing functionality in an existing file, unless the functionality is a new logical component. Avoid creating many small files.
- Do not create a sub-directory for a single file. A directory should hold more than one file before it exists. Until then, keep the file alongside its siblings. Write `foo.md`, not `foo/foo.md`. Promote a file to a directory only when a second file genuinely belongs with it.
- Avoid functions that panic, such as `unwrap()`. Propagate errors with `?` instead.
- Be careful with operations that may panic, such as indexing with an index that may be out of bounds.
- Never discard an error silently with `let _ =` on a fallible operation.
- Never create a file at a `mod.rs` path. Write `src/some_module.rs` instead of `src/some_module/mod.rs`.
- Specify the library root path of a new crate in `Cargo.toml` with `[lib] path = "...rs"` instead of the default `lib.rs`. A descriptive name such as `gpui.rs` or `main.rs` keeps the naming consistent.
- Avoid creative additions unless the user asks for them.
- Use full words for variable names. Do not abbreviate, for example "q" for "queue".
- Import types directly. For functions, prefer at least one module-level import over fully qualifying every call. A fully qualified path is still fine where it avoids ambiguity.
- Prefer procedural or functional code over OOP-style method organization when there is no clear stateful abstraction. Use free functions by default. Use an `impl` block when a type genuinely owns stateful behavior, or when a constructor-style helper materially improves clarity. Do not use methods only to namespace procedural code.
- Organize modules top-down. Put core types and public functions first. Order a container type before the types it contains. Keep private types and helper functions after the public items, in the same caller-before-callee order.
- Do not optimize for the smallest safe fix. Bring the area you touch to its intended shape, remove dead paths and temporary seams, and pay down the nearby technical debt that the code needs to stay coherent. You are responsible for code quality, not only for feature delivery.
- Avoid helper-function indirection when the logic is used once and does not materially improve testability or readability. Inline a small one-off solution, unless inlining would create large duplication.

# Design bar

- Deliver world-class implementation quality, not merely passing behavior.
- Use the simplest correct data model and implementation that can express the required semantics.
- Do not introduce a complicated abstraction unless it removes real complexity.
- Make illegal states unrepresentable whenever practical.
- Keep a single source of truth for each semantic fact whenever practical.
- Do not store a fact separately when it is cheaply and reliably derivable from an existing source of truth. A clear performance reason is the only exception.
- Do not introduce duplicated state, mirrored tables, or cached derived data that can drift out of sync. A clear justification is the only exception.
- Use designs that minimize cloning, copying, and whole-structure rebuilding.
- Optimize for very fast incremental analysis and low memory churn.
- Surface a structural design problem early and explicitly. Do not work around it.

# Design review trigger

Stop when you see any of the items below. Do not work around it. Design the fix and implement it. Record the decision in `decisions.md` when it settles an architectural question.

- multiple sources of truth
- duplicated metadata
- derived state that is persisted without clear justification
- snapshot-local ids where stable indirection would suffice
- repeated cloning or copying that exists only to maintain convenience state
- a design that feels more complicated than the semantics require

The recorded decision must state four things: the previous source of truth, what was duplicated or structurally weak, the chosen target shape, and the expected impact on correctness, simplicity, performance, and incremental analysis.

# Error handling

- Never swallow an analysis error, a synchronization error, or a document-loading error anywhere in the project.
- Surface a failure immediately with `panic!` when the failed operation is required to keep analysis state coherent. Do not log the failure and continue with corrupted or stale state.
- A document-sync failure or an analysis-sync failure in the LSP path is unrecoverable. Panic immediately instead of keeping the server alive in a bad state.
- For example, syncing an open document into analysis state can fail during `did_open`, `did_change`, or `did_save`. Do not fall back to stale state, and do not settle for best-effort logging. Call `panic!`.

# Testing strategy

- Prefer fixtures. They are the primary way to validate analysis behavior, they are easy for humans to read in diffs, and they make it easy to create many tests quickly.
- Fuzz the whole pipeline from day one. This is a user directive. Every stage gets fuzz and property coverage on the day it exists, never as a later add-on. The stages are parsing, lowering, naming, inference, diagnostics, incrementality, formatting, and the IDE layer. A bounded pass belongs in the default test suite. See the fuzzing decision record in `decisions.md`.
- Add or tighten a fixture before you write a parser-local or engine-local unit test, unless the behavior is genuinely awkward to express as a fixture.
- Favor fixture renderers that expose semantic facts rather than implementation detail.
- Add or extend a fixture suite for a new phase or module before you rely on ad hoc unit tests.
- Use the lightest fixture change that captures the failing shape.
- Read the testing page at `docs/src/content/docs/contributing/testing.md` before you change the fixture harness or add a new fixture suite.
- Run a focused fixture case with `FIXTURE_FILTER=group__case cargo test -p analysis --test test_fixtures <suite> -- --nocapture`.
- Prefer running focused crate tests while you iterate. `cargo test -p analysis` is the default crate test command.
- Keep a fixture's `group__case` name stable, because that name is the test identity. Reject a duplicate name across the suite instead of letting one case silently shadow another.
- Treat fixtures as the desired semantics contract, not as a regression suite that preserves known-wrong behavior. Review an expectation change deliberately. Update an expectation only when the wording or the behavior improves on purpose. Never commit an intentionally wrong outcome only to keep the suite green.
- Some fixture cases are unreasonable, or are no longer worth preserving. Clean up such a case instead of treating it as authoritative by default.
- Do not reintroduce end-to-end named-argument mismatch fixtures until function-parameter lowering can represent the needed semantics.

# Rules hygiene

Every agent session reads this `AGENTS.md` file, so keep it extremely high-signal.

Editing or clarifying an existing rule is always welcome. A new rule must meet all three criteria below.

1. **Non-obvious.** Someone familiar with the codebase would still get it wrong without the rule.
2. **Repeatedly encountered.** It came up more than once. Several hits in one session count.
3. **Specific enough to act on.** It is a concrete instruction, not a vague principle.

A rule that applies to a single crate belongs in that crate's own `AGENTS.md` file, not in this one.

Avoid architectural descriptions of a crate, such as its module layout, its data flow, or its key types. Such descriptions go stale fast, and an agent can gather them by reading the code. A rule should be a trap to avoid, not a map to follow.
