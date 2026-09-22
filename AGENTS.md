# Overview

ry is a language tool for R, shipped as a language server and a command-line tool. It aims to be world class at three things: code analysis on the level of rust-analyzer, code formatting, and linting. A static type checker sits at the core of the analysis.

No static type checker exists for R, so ry defines its own typing semantics, and the typing reference at `docs/src/content/docs/reference/type-system.md` is their contract. R has no syntax for type annotations either, so annotations live in `#:` comments written in a JSDoc-like notation. Annotated code therefore stays ordinary R that every other tool can read.

The workspace has two crate directories:

- `crates/` is the shipping product.
  - `syntax`: the lexer and parser, which build lossless rowan trees.
  - `semantics`: the salsa-based analysis core and the type checker.
  - `format`: the formatter, which reads syntax only.
  - `ide`: editor features, implemented as pure reads.
  - `ry`: the LSP server and the CLI.
  - `repl`: the R console behind `ry repl` and `ry run`. It loads R at runtime, which is why the rest of the workspace can build without R.
- `legacy/` is the frozen previous implementation: `analysis-legacy`, `engine-legacy`, `roughly-legacy`, and their `fixtures` harness. It also holds `differential`, which is now only the cross-stack benchmark. The identity-parity program that once compared the two stacks is complete and retired by user decision, so the new stack's fixtures are the contract and no change needs the old stack to agree. Everything legacy lives in one directory because every dependency edge points into it: deleting the old stack will be a single directory removal, once the performance witnesses that only measure the new stack have moved out.

AI agents drive development here, with light human steering. They keep two written homes current. The docs site in `docs/` holds the specs for users and contributors, and because those specs are contracts, keeping them accurate is mandatory. The knowledge base in `.agents/memory/MEMORY.md` holds engineering state, priorities, debt, and the non-obvious reasons behind designs, so that no agent has to rediscover them. Update both in the same session as the work that changes them.

# Goals

- Deliver diagnostics for R in the style of Rust and Elm: clear, precise, and actionable. Prefer plain user-facing wording to internal or theory-heavy language, and precise source ranges to coarse fallback ones.
- Provide full editor tooling (hover, completion, goto-definition, references, rename, and inlay hints), and preserve the semantic information those features need whenever practical.
- Provide first-class formatting and linting alongside analysis.
- Scale to very large code bases, beyond 300,000 lines of code. Performance matters.

# How to write

Agents write this repository's documentation, code comments, commit messages, and pull request descriptions, and people read all of them. Write for those people. The aim is prose that is clear first and pleasant to read second, and the two rarely pull apart: text that is easy to follow is usually a pleasure to read.

Clarity is also a correctness tool. A reviewer cannot check a claim they cannot parse, so dense prose hides wrong statements, and a pull request description nobody can follow gets approved without being understood.

These rules apply to everything an agent writes.

- **Start where the reader is.** Decide who is reading (a new user, someone looking up a rule, a contributor about to change the code) and open with what they need most: the answer, the rule, or what changed. Background comes after.
- **Teach in order.** Introduce a concept before you use it, and finish one idea before starting the next. When a small example is easier to grasp than the general rule, show the example first.
- **Write connected prose.** A sentence should carry one step of reasoning, not five, so split a sentence that chains several steps together. But do not overcorrect into a row of short, same-shaped sentences: that reads like a telegram and hides how the ideas relate. The small words that carry logic (because, so, but, unless, which means) are what make an explanation an explanation. Vary sentence length, and read the paragraph aloud. If three sentences in a row start with "It", rewrite them.
- **Be concrete.** A real snippet and its real output beat a description of them. Get the output by running the tool (see "Run the tool before you claim what it does" below).
- **Pick a natural subject, in the active voice.** In a specification, the construct is the subject: write "the pipe types as that call", not "ry types the pipe as that call". Name ry where the statement really is about the tool, such as a diagnostic, a default, or a deliberate limit. In guides, talk to the reader as "you".
- **Say it literally first.** A metaphor must never stand in for the mechanism. Once the mechanism is stated plainly, a comparison may help it stick.
- **Use one name for one thing.** Do not coin private terms: a name like "the literal courtesy" means nothing to a reader who has not seen the code that named it. Define a term of art before you rely on it, and use the same word for the same thing every time.
- **Cut filler.** Drop throat-clearing openers, hedges that carry no information, and closing flourishes that state no fact. A sentence of motivation that makes the reader care is not filler.
- **Punctuate for the reader.** Do not use em-dashes; a comma, a colon, a short parenthetical, or a new sentence does the job better. Use semicolons rarely. If an aside grows past a few words, give it its own sentence or cut it.
- **Match the form to the content.** Use a list for items that are genuinely parallel, such as options, cases, or steps, and use prose for reasoning, because there the connectives are the point. Use a table when the reader compares items along the same columns. Do not introduce a list with a sentence that only counts it ("There are four cases.").
- **Keep normative words exact in a specification.** "Must", "may", "is an error", and "is not supported" carry meaning, so do not soften them.
- **In a commit message or pull request description, describe the change, not the text.** Say what changed and why it matters, then give the details a reviewer needs. Do not narrate a document ("The section states the goal. It then lists...").

## Answers to the user

- Make the answer self-contained. Assume the reader has not followed the conversation: give the context, then make your point.
- Keep answers short. The user asks for details when they want them.
- Cut everything from a recommendation that does not change the choice.
- When two options seem to conflict, look for the third option that resolves the conflict, and name it instead of presenting a false choice.
- Do not use the interactive multiple-choice picker. Ask with a numbered list instead.

## Example

Here is one idea written three ways. The first packs five facts into one sentence behind a colon:

> `x |> f(y)` is syntax R's own parser rewrites to `f(x, y)` before evaluation, and it types as exactly that call: the piped value becomes the first positional argument, every call rule above applies (arity, argument compatibility, overload selection), chains compose left to right, and a type error on the piped value blames the left-hand expression.

The second overcorrects. Every sentence is short, and none of them says how it relates to the one before:

> R's parser rewrites `x |> f(y)` into `f(x, y)` before it evaluates the code. ry types the pipe as that call and nothing else. The piped value becomes the first positional argument. All call rules above apply to it: arity, argument compatibility, and overload selection. Chains compose from left to right. If the piped value has a type error, ry reports it on the left-hand expression.

The third is the one to write:

> R's parser rewrites `x |> f(y)` into `f(x, y)` before anything runs, so the pipe types exactly like that call. The piped value becomes the first positional argument, and every call rule applies to it: arity, argument compatibility, and overload selection. A chain composes from left to right, and a type error in the piped value is reported on the left-hand expression.

# Ownership mandate

The user has delegated full technical ownership to the agents: empty the backlog and bring the project to the best possible state, which means rust-analyzer quality. That covers code structure, crate boundaries, naming, performance, pipeline architecture, semantic correctness, and judged deduplication. Do not optimize for a safe, minimal diff. Bring code to its intended shape, large refactors included, and take responsibility for the outcome.

Design decisions that once needed a user check-in are now yours to make. Decide, implement, and record the decision and its rationale in `.agents/memory/decisions.md` (or in the docs page it belongs to) in the same session.

Two constraints stand, both user directives: work directly on `main`, and do not open new pull requests.

# Do not think like a human (user directive)

Human engineers de-risk work: they stage it, keep diffs small and reviewable, and shy away from rewrites that look scary. Those instincts exist because a human's time is scarce and starting over is expensive for them. Neither is true for an agent, so here those instincts pick the wrong strategy.

- **Go directly to the intended end shape in one change**, however large and invasive. If the target design calls for it, break the whole codebase mid-change and then fix everything (compiler errors, warnings, tests) in one sweep. Splitting a redesign into small steps to manage risk trades the right design for ceremony.
- **Never choose a watered-down design because the full version is a big change.** If the full version is right, build the full version. Starting over after a failed attempt is cheap; shipping the wrong shape is not.
- **File size is not a problem.** Do not split, reorganize, or flag a file just because it is large (the LSP server module is fine as one file). Split only when a genuinely new logical component appears.
- **The correctness gates do not move.** The fixture suites, witnesses, clippy, and fmt must be green before a change lands. The point is to reach green in one big pass, not to shrink the change.

# Incremental analysis

The analysis core is incremental. `semantics` is a salsa database, so queries are memoized and an edit cooperatively cancels analysis that is still in flight. Per-item interface firewalls keep an edit inside one item from invalidating the rest of the project, and the LSP server publishes a fast diagnostics wave first and schedules the slower semantic wave for idle time.

The architecture page at `docs/src/content/docs/contributing/architecture.md` is the contract. Read it before you touch the analysis core or the server's scheduling, and keep it accurate. Deferred performance work lives in `.agents/memory/backlog.md`.

# Working autonomously

When you work autonomously toward a larger goal (a workflow, a multi-step change, anything that spans several logical units), commit and push after each logical step rather than saving everything for one final commit. A single large, invasive redesign counts as one step: commit it once it is green, not in fragments along the way.

# Knowledge base and documentation

There are two written homes. Keep both current, spend the minimum effort that keeps them useful, and prefer bullet points.

## `.agents/memory/`

The agent knowledge base lives in the repository. Its index is `MEMORY.md`, which must keep these three sections under exactly these names:

- **Short-term**: the current focus and loose ends. Prune it aggressively, deleting each item once it is resolved or obvious from the source tree.
- **Mid-term**: active priorities, open bugs, and technical debt. Items live here across sessions until they are done.
- **Long-term**: durable, non-obvious design decisions and why they were made. Record only what a future agent would otherwise have to rediscover, keep each entry terse, and point at the code or the docs.

`MEMORY.md` also names every other knowledge document. Keep a separate document only for material of genuinely larger scope, and link it from `MEMORY.md`. There are three today: `backlog.md` is the prioritized punch-list of open work, `decisions.md` is the log of settled architecture decisions, and `test-user-reports.md` collects the closed findings from simulated-user testing (a finding moves there from the backlog once it is fixed). Never start a new knowledge file for something small; fold it into the right horizon instead.

A design document is not a memory file. Unsettled design work (proposals, open questions, sketchpads) belongs in the docs site under `docs/src/content/docs/contributing/design/`. List each one on that folder's index page, and keep it out of the sidebar.

`worklog.md` is a deliberate exception to both the timeless rule and the no-new-files rule. It is a chronological record, one line per cycle, that a scheduled routine appends to, so do not prune it as a rules violation. Durable facts still belong in the files above.

Keeping memory current is part of repository hygiene, not an optional extra, and it happens in the same session as the work: add what is durable, prune what is resolved, stale, or duplicated, and move items between horizons as their status changes.

Memory lives in git on purpose. It travels with every `git clone` to any machine or cloud session, so every agent, including one restarting from nothing, reads the same source of truth, and nothing is lost when an agent or session is replaced. Never keep project knowledge in a private agent memory store such as a per-tool `~/.claude/` folder: other agents cannot read it, and it does not travel. Since a reader may have no project history at all, every entry must be context-free and timeless. Do not name internal milestones, phases, or gates, do not cite commit hashes, and do not write "this session". State durable facts and point at the code or the docs, exactly as for code comments.

## `docs/`

The docs site holds the specs for users and contributors. They are contracts, so keeping them accurate is mandatory.

- Type checking has two homes: `type-checking/` is the tutorial, and `reference/type-system.md` is the semantics contract.
- The contributing pages are `contributing/architecture.md`, `contributing/structure.md`, `contributing/testing.md`, and `contributing/authoring-stubs.md`.
- `contributing/design/` holds unsettled drafts. They are explicitly not contracts, which makes them the one place in the docs allowed to describe behavior that does not exist. Keep them out of the sidebar; the folder's index page lists them.
- Treat the docs as a first-class deliverable. When behavior, design, or the fixture contract changes, update the relevant page in the same session, and leave it clear, accurate, and free of stale status. Never rewrite a spec to paper over a temporary gap in the implementation; note the gap instead.

**Run the tool before you claim what it does.** Every statement about actual behavior, whether in a docs page, a memory document, or a commit message, must be confirmed by running the tool, not recalled or inferred from the code. Build a throwaway project (a `ry.toml` and one `.R` file) and read the real output.

The check is cheap, and skipping it is the most reliable way this project ships a false statement. Writing prose does not feel like a task that needs a test, so plausible claims go in unchecked, and a wrong claim looks exactly like a right one until a user trips over it. If you cannot verify a claim cheaply, mark it as unverified instead of asserting it. Before calling a design document done, have an adversarial reviewer subagent check it against the implementation and the settled decisions.

# Skills

If the user says:

- `get started`: read `.agents/memory/MEMORY.md` and the relevant docs pages, then continue with the next mid-term priority. Assume you are starting with fresh context.
- `cleanup memory`: prune the short-term section of `.agents/memory/MEMORY.md` aggressively, and keep the mid-term and long-term sections intact.
- `code check`: review the relevant code against the coding guidelines and report the findings first. Explicitly verify top-down module ordering and the preferred `use` style: types are usually imported directly, and functions usually get at least one module-level import instead of repeated fully qualified calls, unless ambiguity forces qualification.
- `authoritative check`: compare the docs specs against the fixture suites, and report contradictions, stale wording, and documented behavior that no fixture covers.
- `implementation check`: compare the implementation against the docs specs, and report mismatches in the contract or the architecture.
- `session check`: run an end-of-session closure pass. Verify that `.agents/memory/MEMORY.md` or the docs capture every decision, open question, and newly discovered piece of work (watch for side investigations that left follow-up work uncaptured), and that memory and the docs agree with the implementation. Report anything still hanging.

# Rust coding guidelines

- Do not write comments that organize or summarize the code. A comment explains *why* the code is written the way it is, and only when that reason is tricky or non-obvious.
- Keep comments context-free. Never refer to internal milestones, phases, process history, ticket or pull request names, or commit hashes ("R0", "M3", "Phase 4", "gate (c)", "the spike", "added in the cutover"). A reader with no project history must understand every comment, so explain the reason in domain terms, not in terms of when or how the code came to be.
- Prefer adding functionality to an existing file unless it is a new logical component. Avoid creating many small files.
- Do not create a directory for a single file. Keep the file beside its siblings (`foo.md`, not `foo/foo.md`) until a second file genuinely belongs with it.
- Avoid functions that panic, such as `unwrap()`; propagate errors with `?` instead.
- Be careful with operations that may panic, such as indexing that may go out of bounds.
- Never silently discard an error with `let _ =` on a fallible operation.
- Never create a `mod.rs` file: write `src/some_module.rs`, not `src/some_module/mod.rs`.
- In a new crate, name the library root in `Cargo.toml` with `[lib] path = "...rs"` instead of using the default `lib.rs`, so that roots have descriptive names (such as `gpui.rs` or `main.rs`).
- Avoid creative additions the user did not ask for.
- Use full words for variable names: `queue`, not `q`.
- Import types directly. For functions, prefer at least one module-level import to fully qualifying every call; a fully qualified path is still fine where it avoids ambiguity.
- Prefer procedural or functional code to OOP-style method organization when there is no clear stateful abstraction. Use free functions by default, and an `impl` block when a type genuinely owns stateful behavior or when a constructor-style helper makes things materially clearer. Do not use methods merely to namespace procedural code.
- Organize modules top-down. Core types and public functions come first, a container type comes before the types it contains, and private types and helpers follow the public items in the same caller-before-callee order.
- Do not optimize for the smallest safe fix. Bring the area you touch to its intended shape: remove dead paths and temporary seams, and pay down the nearby debt the code needs to stay coherent. You are responsible for code quality, not only for delivering the feature.
- Avoid helper-function indirection for logic that is used once and gains no testability or readability from the extraction. Inline a small one-off solution unless that would duplicate a lot of code.

# Design bar

- Aim for world-class implementation quality, not merely passing behavior.
- Use the simplest correct data model and implementation that can express the required semantics.
- Do not introduce a complicated abstraction unless it removes real complexity.
- Make illegal states unrepresentable whenever practical.
- Keep a single source of truth for each semantic fact whenever practical.
- If a fact can be derived cheaply and reliably from an existing source of truth, do not store it separately without a clear performance reason.
- Do not introduce duplicated state, mirrored tables, or cached derived data that can drift out of sync, unless there is a clear justification.
- Prefer designs that minimize cloning, copying, and rebuilding whole structures.
- Optimize for very fast incremental analysis and low memory churn.
- Surface a structural design problem early and explicitly instead of working around it.

# Design review trigger

If you see any of the following, do not work around it. Stop, design the fix, implement it, and record the decision in `decisions.md` when it settles an architectural question.

- multiple sources of truth
- duplicated metadata
- derived state that is persisted without clear justification
- snapshot-local ids where stable indirection would do
- repeated cloning or copying that exists only to maintain convenience state
- a design that feels more complicated than the semantics require

The recorded decision must state the previous source of truth, what was duplicated or structurally weak, the chosen target shape, and the expected impact on correctness, simplicity, performance, and incremental analysis.

# Error handling

- Never swallow an analysis, synchronization, or document-loading error anywhere in the project.
- If an operation is required to keep analysis state coherent and it fails, `panic!` immediately rather than logging and carrying on with corrupted or stale state.
- In particular, a document-sync or analysis-sync failure in the LSP path is unrecoverable: panic instead of keeping the server alive in a bad state. If syncing an open document into analysis state fails during `did_open`, `did_change`, or `did_save`, do not fall back to stale state or settle for best-effort logging. Call `panic!`.

# Testing strategy

- Prefer fixtures. They are the primary way to validate analysis behavior, they read well in a diff, and they make it cheap to write many tests quickly.
- Fuzz the whole pipeline from day one (user directive). Every stage (parsing, lowering, naming, inference, diagnostics, incrementality, formatting, the IDE layer) gets fuzz and property coverage the day it exists, never as a later add-on, and a bounded pass belongs in the default test suite. The fuzzing decision record in `decisions.md` has the details.
- Add or tighten a fixture before writing a parser-local or engine-local unit test, unless the behavior is genuinely awkward to express as a fixture.
- Favor fixture renderers that expose semantic facts over ones that expose implementation detail.
- When adding a new phase or module, add or extend a fixture suite for it before relying on ad hoc unit tests.
- Use the lightest fixture change that captures the failing shape.
- Read the testing page (`docs/src/content/docs/contributing/testing.md`) before you change the fixture harness or add a fixture suite.
- Run a single fixture case with `FIXTURE_FILTER=group__case cargo test -p semantics --test test_typing_fixtures`. The other fixture targets are `test_naming_fixtures`, `test_lowering_fixtures`, and `test_lint_fixtures` in `semantics`; `test_syntax_fixtures` and `test_error_messages` in `syntax`; `test_ide_fixtures` in `ide`; and `test_format_fixtures` in `format`.
- While iterating, prefer focused crate tests; `cargo test -p semantics` is the default crate test command.
- A fixture's `group__case` name is its test identity, so keep it stable, and reject a duplicate name across the suite instead of letting one case silently shadow another.
- Treat fixtures as the desired semantics contract, not as a regression suite that preserves known-wrong behavior. Review every expectation change deliberately, update an expectation only when wording or behavior improves on purpose, and never commit an intentionally wrong outcome just to keep the suite green.
- Some fixture cases are unreasonable or no longer worth keeping. Clean such a case up rather than treating it as authoritative by default.
- Do not reintroduce end-to-end named-argument mismatch fixtures until function-parameter lowering can represent the semantics they need.

# Rules hygiene

Every agent session reads this file, so keep it extremely high-signal.

Editing or clarifying an existing rule is always welcome. A new rule must meet all three of these criteria:

1. **Non-obvious**: someone familiar with the codebase would still get it wrong without the rule.
2. **Repeatedly encountered**: it came up more than once (several hits in one session count).
3. **Specific enough to act on**: a concrete instruction, not a vague principle.

A rule that applies to a single crate belongs in that crate's own `AGENTS.md`, not here.

Avoid architectural descriptions of a crate, such as its module layout, data flow, or key types. They go stale fast, and an agent can gather them by reading the code. A rule should mark a trap to avoid, not draw a map to follow.
