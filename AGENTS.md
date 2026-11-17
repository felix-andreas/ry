# Overview

ry is a backwards-compatible reimplementation of R, extended with ry's static typing (user directive). It runs R on its own runtime with a bundled base R on Linux, macOS and Windows, and its ABI-compatible `libR` implements R's C API, so `R CMD` installs compiled packages against it. It runs the three benchmark code bases below and pinned CRAN packages' test suites faster than GNU R on every benchmark suite (`docs/src/content/docs/benchmarks.mdx`). Typed ry compiles to native code at a typed closure's first call or ahead of time (`ry run --aot`, a package's typed code file), and a JIT compiles hot untyped code. The runtime is incomplete: code that needs something it lacks stops with an error naming it.

The same binary is the tooling: a language server and a command-line tool that aim to be world class at code analysis on the level of rust-analyzer, code formatting, and linting, with a static type checker at the core of the analysis.

R has no static type system, so ry defines its own typing semantics, and the typing reference at `reference/type-system.mdx` is their contract. Annotations are written inline (`function(price: double) -> double`) or in `#:` comments in a JSDoc-like notation, which keep annotated code ordinary R that every other tool can read.

The crates in `crates/`:

- `syntax`: lexer and parser (lossless flat syntax trees).
- `semantics`: the salsa analysis core and the type checker.
- `format`: the formatter (reads syntax only).
- `ide`: editor features, as pure reads.
- `rdb`: installed packages' lazy-load databases and help pages, read without R.
- `ry`: the LSP server and the CLI.
- `loader`: the package-development loader behind `ry load`.
- `repl`: the R console behind `ry repl` and `ry run`; it loads R at runtime, so the rest of the workspace builds without R.
- `heap`: the runtime's object model and garbage collector.
- `interp`: the R interpreter, above `numeric` (numeric kernels) and below the leaf crates `stats`, `graphics`, `frames` (`DataFrame` over `table`, the Polars engine) and `services` (HTTP), so it links no Polars, rasterizer or HTTP stack.
- `sniff`: delimited-text dialect and column-type inference, shared by the table reader and the checker.
- `jit`: the optimizing tier (hot bytecode to machine code with Cranelift).
- `typed`: the typed core (strict typed closures on native values, at run time or ahead of time).
- `ssa`: the one mid-level SSA IR that `jit` and `typed` lower to machine code.
- `libr`: R's C API as an ABI-compatible `libR`.
- `rcmd`: ry's R home and the `R CMD` commands.
- `crash`: the report a fatal signal prints.
- `oracle`: the differential test harness against GNU R.
- `features`: links nothing; unifies dev-dependency features into one dependency tree.

# Goals

## The mandate (user directive)

- Existing R code and packages must keep working unchanged.
- An inline type annotation syntax exists alongside the `#:` comment annotations. It is enabled by default and can be turned off, but ry warns while it is off.
- ry warns when a comment annotation and an inline annotation disagree.
- A strict mode rejects untyped code.
- Package development loading in the style of `devtools::load_all` is built in, and it lazy-loads only what a session actually needs. [loadfast](https://github.com/finccam/loadfast) prototypes the idea, and ry does it cleanly.
- A built-in, typed table type replaces data.table and must outperform it. It may be backed by polars.
- (user directive) data.table is a normal package. ry adds no special cases for it and never re-implements or re-routes it; DataFrame replaces it only as the table users choose for new code.
- The performance bar is a JIT on the level of Bun.
- (user directive) Performance comes first, so the analysis core, the runtime heap and the JIT use data-oriented design: struct-of-arrays and cache-friendly layouts, dense indices into arenas instead of pointer graphs, and batch processing in hot loops. Allocation is explicit, with an arena or bump allocator per analysis or evaluation phase and pools per object type (`decisions/docs-process.md`, "Data-oriented design and explicit allocators", says where each applies).
- Every R feature keeps working, including S4, R6 and Reference Classes, but ry discourages them: a lint warns on their use, and an option disables them. ry has its own records, tuples and tagged unions, in the style of Roc, as the recommended way to model data. They are a ry feature on top of R, like the built-in table type, not R data structures, so they need not support R or its quirks; plain R code still runs unchanged.
- ry ships a standard library modeled on Roc's where it fits (`Result`, `List`, `Str`, `Dict` and so on), using plain qualified names such as `Result.map` rather than static dispatch.
- (user directive) Typed ry is a small, friendly language in the spirit of Roc, independent of R's semantics and designed for close to Rust's performance: sound types, functions and a reasonable standard library, and nothing more until it earns its place (no static dispatch or other advanced features by default). A typed program can be written entirely in ry's own types (numbers, `Bool`, `Str`, `List`, `Dict`, `Set`, records, tuples, tags and functions) without touching an R data type, and the typed benchmark programs are written that way. R values meet ry's types only at an explicit boundary.

The tooling has goals of its own. Diagnostics follow the style of Rust and Elm: clear, precise, and actionable, in plain user-facing wording rather than internal or theory-heavy language, and with precise source ranges rather than coarse fallback ones. Editor tooling stays complete (hover, completion, goto-definition, references, rename, and inlay hints), so preserve the semantic information those features need whenever practical. Formatting and linting stay first class.

## The C API comes first

Most packages that real R code depends on ship C, C++ or Rust code written against R's C API (`SEXP` values, the memory-protection discipline, `.Call`), so full compatibility means implementing that C API, not only R's language semantics. The interface constrains the object model, the memory manager and the calling convention, so it shapes the architecture more than the JIT does: it is designed first, and the JIT fits around it.

## Benchmark

Three synthetic code bases, one each for finance, data science, and biostatistics, measure progress. Each has about 300,000 lines in about 500 files, and each uses exactly this dependency set:

```
Imports:
    abind, assertthat, azidentity (>= 1.1.0), AzureAuth, AzureKeyVault, AzureStor, AzureTableStor,
    brio, cachem, callr, cli, data.table, DBI, devtools, digest, doParallel, esgtoolkit (>= 1.0.0),
    FLSSS, foreach, fs, git2r, httr, httr2, jsonlite, lgr, loadfast, lubridate, memoise, methods,
    NlcOptim, odbc, ompr, ompr.roi, openxlsx, parallel, partitions, pdftools, PerformanceAnalytics,
    plumber, polars, pool, progress, purrr, quadprog, R6, rappdirs, Rblpapi, RcppDE, RcppSimdJson,
    readr, readxl, renv, Rglpk, rlang, rmgarch, ROI.plugin.highs, rugarch, rvest, snakecase, stats,
    stringr, testthat, tools, tryCatchLog, utils, uuid, withr, xml2, xts, YieldCurve, zeallot, zoo
Remotes:
    github::Techtonique/esgtoolkit@v1.0.0,
    github::finccam/loadfast
Additional_repositories:
    https://ak-finccam.github.io/r-packages
```

ry must run these code bases, and its analysis must check them fast enough for incremental editing.

# How to write

These rules cover everything an agent writes: docs, code comments, commit messages and pull request descriptions. Aim for prose that is clear first and pleasant second. Clarity is also a correctness tool, because a reviewer cannot check a claim they cannot parse. `development/index.md`, "Writing: one idea three ways", shows the target style.

- **Start where the reader is.** Decide who is reading and open with what they need most: the answer, the rule, or what changed. Background comes after.
- **Teach in order.** Introduce a concept before you use it, and finish one idea before starting the next. When a small example is easier to grasp than the general rule, show the example first.
- **Write connected prose.** One step of reasoning per sentence, but not a row of short, same-shaped sentences: the words that carry logic (because, so, but, unless) make the explanation. Vary sentence length.
- **Be concrete, and dense.** A snippet with its real output beats a paragraph about it, and a table beats a list of similar sentences.
- **Pick a natural subject, in the active voice.** In a specification the construct is the subject ("the pipe types as that call", not "ry types the pipe as that call"); name ry where the statement is about the tool, such as a diagnostic or a default. In pages for users, address the reader as "you".
- **Say it literally first.** A metaphor never stands in for the mechanism; once the mechanism is plain, a comparison may help it stick.
- **Use one name for one thing.** Coin no private terms, and define a term of art before you rely on it.
- **Cut filler**: throat-clearing openers, empty hedges, closing flourishes. Motivation that makes the reader care is not filler.
- **Write timelessly.** The README, the docs, memory entries and code comments say what is true, never how it changed, so avoid "stays", "now", "no longer", "still" and "previously", and never name milestones, phases, gates, process history, tickets, pull requests, commit hashes or "this session". A reader with no project history must understand every line, so explain reasons in domain terms. A change belongs in a commit message or the news page.
- **Punctuate for the reader.** No em-dashes (use a comma, a colon, a parenthetical or a new sentence), few semicolons, and no aside longer than a few words.
- **Match the form to the content.** A list for genuinely parallel items, prose for reasoning, a table when the reader compares items along the same columns. Do not introduce a list with a sentence that only counts it.
- **Keep normative words exact in a specification.** "Must", "may", "is an error", and "is not supported" carry meaning, so do not soften them.
- **In a commit message or pull request description, describe the change, not the text**: what changed, why it matters, and the details a reviewer needs.

**Run the tool before you claim what it does.** Every statement about actual behavior, whether in a docs page, a memory document, or a commit message, must be confirmed by running the tool, not recalled or inferred from the code. Build a throwaway project (a `ry.toml` and one `.R` file) and read the real output, because a wrong claim looks exactly like a right one until a user trips over it. If you cannot verify a claim cheaply, mark it as unverified instead of asserting it. Before calling a design document done, have an adversarial reviewer subagent check it against the implementation and the settled decisions.

## Answers to the user

- Make the answer self-contained: assume the reader has not followed the conversation, give the context, then make your point.
- Keep answers short. The user asks for details when they want them.
- Cut everything from a recommendation that does not change the choice.
- When two options seem to conflict, look for the third option that resolves the conflict, and name it instead of presenting a false choice.
- Never use the interactive question tool (`AskUserQuestion`); ask in plain text in your reply, with a numbered list for options (user directive).

# Process (user directives)

## Velocity first

Velocity is the top priority, so do not de-risk. Committing broken code to main is allowed, and so is not waiting for CI. Main may stay broken for the next two to three weeks, because stability comes after something works. Fixtures and tests are still the semantics contract and still get written and kept honest, but they do not block landing during this phase.

Human engineers stage work, keep diffs small, and avoid rewrites that look scary, because their time is scarce and starting over is expensive for them. Neither is true for an agent, so here those instincts pick the wrong strategy.

- **Go directly to the intended end shape in one change**, however large and invasive. Splitting a redesign into small steps to manage risk trades the right design for ceremony.
- **Refactor strategically as soon as the direction looks wrong.** A large architectural refactor is encouraged, not something to postpone.
- **Never choose a watered-down design because the full version is a big change.** Starting over after a failed attempt is cheap; shipping the wrong shape is not.
- **File size is not a problem.** Do not split, reorganize, or flag a file just because it is large (the LSP server module is fine as one file). Split only when a genuinely new logical component appears. Otherwise add to an existing file, never create a directory for a single file, and never start a knowledge file for something small.

## How work lands

An in-session worker commits directly to main. A separate cloud session delivers one large pull request (at least about 2,000 changed lines, always one whole logical step) that a reviewer agent checks, and agents may merge pull requests. Commit and push after each logical step. Commit messages and pull request titles follow Conventional Commits (`refactor: drop the legacy resolver`).

A release is cut only from a commit whose CI is green, including the stress job (`development/releasing.md`).

`.agents/orchestrator.md` is for the orchestrating session only; every other agent ignores it.

## Ownership

The user has delegated full technical ownership to the agents: code structure, crate boundaries, naming, performance, pipeline architecture, semantic correctness, and judged deduplication. Do not optimize for a safe, minimal diff: bring the area you touch to its intended shape, remove dead paths and temporary seams, and pay down the nearby debt the code needs to stay coherent. Every design decision is yours to make, with no user check-in: decide, implement, and record the decision and its rationale in `.agents/memory/decisions/<area>.md` (or in the docs page it belongs to) in the same session.

## Standing constraints

- Keep `roughly.toml` support and the REPL.
- Never edit `NOTES.md`, which is human-maintained, or the landing-page hero animation, which is user-owned.
- The agent token cannot write `.github/workflows/*`. Stage a workflow change in `.github/pending/` (same file name), push it, and ask the user to run `git pull && just accept`. Whenever you need the user to do something your token cannot, extend `just accept` the same way, so the ask stays that one short command.
- Never put model identifiers in commits or pull requests.

# Compact instructions

When compacting, the summary must open with the agent's role and its standing rules verbatim. The orchestrating session states that it plans, delegates, decides and merges and never does the work itself, and points to `.agents/orchestrator.md`. A worker, subagent or reviewer states that it is not the orchestrator, then repeats its task brief and constraints verbatim: branch, commit rules, what not to touch, and who to report to.

# Knowledge base and documentation

AI agents drive development, with light human steering, and keep two written homes current in the same session as the work: the docs in `docs/`, whose specs are contracts, and the knowledge base in `.agents/memory/`. Spend the minimum effort that keeps them useful, and prefer bullet points.

## Anonymize user feedback

Feedback from users and test users (bug reports, profiles, reproductions, kernels) often contains real names of customers, companies, internal projects, packages, files, functions, columns or data, and an agent-written reproduction may be only lightly simplified real code. Before anything from such feedback enters the repository (code, fixtures, oracle cases, scenarios, benchmarks, docs, memory, commit messages, PR titles and bodies, or `.agents/orchestrator.md`), anonymize it:

- Replace identifying names with neutral ones (`f`, `col1`, `Key`, `pkg`).
- Reduce the example to the minimal shape that shows the behavior.
- Drop file names, paths, IDs and data values that come from the user's code.
- Refer to the source only as "a user" or "a test user".

A reproduction that names the user's own functions or files is real code, not an abstraction. Public package names (data.table, xts, CRAN packages) and the public repositories in the dependency list above are fine.

## `.agents/memory/`

Memory lives in git so every agent reads the same source of truth; never keep project knowledge in a private store such as `~/.claude/`.

- **`MEMORY.md`** is the index, naming every other knowledge document, and keeps three sections under exactly these names. **Short-term**: current focus and loose ends, pruned once resolved or obvious from the source tree. **Mid-term**: active priorities, open bugs and debt, kept until done. **Long-term**: durable, non-obvious, cross-cutting traps and conventions, terse, pointing at the code or the docs.
- **`decisions/<area>.md`** holds settled decisions, one H2 entry each with a `**Status:**` line. Append a new one to its area file, then run `python3 tools/memory.py index`. To look one up, search `decisions/index.md` and read only that entry.
- **`backlog.md`** lists open work, prioritized. Delete an item once it ships: rationale to `decisions/<area>.md`, contract to the docs, a closed user-testing finding to `test-user-reports.md`.
- **`worklog.md`** is the one chronological file, one line of at most 400 bytes per cycle, appended by a scheduled routine; do not prune it as a rules violation.
- **`crates/<crate>/AGENTS.md`** holds the traps of one crate.
- Unsettled design work belongs in `development/design/`, not in memory.

Add what is durable, prune what is resolved, stale or duplicated, and move items between horizons as their status changes. `python3 tools/memory.py check` (in `just gate`) enforces byte budgets and line limits; prune a file over budget (split a decisions area file) instead of raising it. `just` may be absent in a cloud container: every recipe is a plain command in the `justfile`.

## `docs/`

- (user directive) The docs are written for humans, so they get straight to the point in plain, concise language and cut everything irrelevant. Each page puts the most important information first and gives details after. Agents own the docs and radically cut outdated or irrelevant material instead of preserving it.
- When behavior, design, or the fixture contract changes, update the page in the same session and leave it free of stale status. Never rewrite a spec to paper over a gap in the implementation; note the gap.
- `development/index.md`, "The docs site", is the contract for the sidebar, annotation tabs, `benchmarks.mdx` (never type a benchmark number into prose), redirects, and the drafts in `development/design/`, the one place that may describe behavior that does not exist.

# Skills

If the user says:

- `get started`: assume fresh context, read `.agents/memory/MEMORY.md` and the relevant docs pages, then continue with the next mid-term priority.
- `cleanup memory`: prune the short-term section of `.agents/memory/MEMORY.md` aggressively, and keep the mid-term and long-term sections intact.
- `code check`: review the relevant code against the coding guidelines and report the findings first, explicitly verifying top-down module ordering and the `use` style (types imported directly, functions through at least one module-level import unless ambiguity forces qualification).
- `authoritative check`: compare the docs specs against the fixture suites, and report contradictions, stale wording, and documented behavior that no fixture covers.
- `implementation check`: compare the implementation against the docs specs, and report mismatches in the contract or the architecture.
- `session check`: run an end-of-session closure pass. Verify that memory or the docs capture every decision, open question and newly found piece of work (including follow-ups from side investigations), and that memory and the docs agree with the implementation. Report anything still hanging.

# Rust coding guidelines

- A comment explains *why* the code is written the way it is, and only when that reason is tricky or non-obvious; never write comments that organize or summarize the code. Keep a comment to a few lines at most: longer rationale goes to the decisions area file, with a one-line pointer in the comment. Comments follow "Write timelessly" above.
- Avoid functions that panic, such as `unwrap()` (propagate errors with `?`), and be careful with operations that may panic, such as out-of-bounds indexing. Never silently discard an error with `let _ =`.
- Never create a `mod.rs` file: write `src/some_module.rs`, not `src/some_module/mod.rs`.
- In a new crate, give the library root a descriptive name with `[lib] path = "...rs"` in `Cargo.toml` instead of the default `lib.rs`.
- Avoid creative additions the user did not ask for.
- Use full words for variable names: `queue`, not `q`.
- Import types directly; for functions, prefer at least one module-level import to fully qualifying every call, unless qualification avoids ambiguity.
- Prefer free functions to OOP-style method organization; use an `impl` block only when a type genuinely owns stateful behavior or a constructor-style helper is materially clearer, never to namespace procedural code.
- Organize modules top-down: core types and public functions first, a container type before the types it contains, then private types and helpers in caller-before-callee order.
- Do not extract a helper for logic used once that gains no testability or readability; inline it unless that duplicates a lot of code.

# Performance principles (user directive)

Apply these when writing or optimizing code in the analysis core, the runtime, the GC and the JIT.

- **Redesign before you tune.** Ask how to rewrite the code to use data-oriented design, not how to make the current shape a little faster.
- **Use flat arrays, not hash tables, wherever the keys can be numbered densely.** Make references indices into those arrays, not pointers, and pack the most often read data (a kind, a tag, a small length) into the spare upper bits of the index.
- **Caching is a sign of a slow design.** Do not add a cache: redesign the code so the fast path needs none, and delete an existing cache once a redesign makes it unnecessary. Two mechanisms are not caches in this sense, because they are the design: salsa memoization, which is how incremental analysis works, and the JIT's inline caches and type feedback, which drive speculation.
- **Prefer a struct of arrays to an array of structs** when hot loops read only a few fields of each item.
- **Keep small lists on the stack.** A list that is usually short gets inline capacity on the stack (a `SmallVec`-style type) instead of a heap allocation.
- **Use a bitset, not an array of bools.**

# Design bar

- Aim for world-class implementation quality, not merely passing behavior.
- (user directive) For compiler, runtime and JIT design, the quality bar is the state-of-the-art implementations Bend, vx-lang, goose and Roc. Measure design choices against them and adopt their best ideas where they fit R's C API constraints; the comparison lives in `development/design/prior-art.md`.
- Use the simplest correct data model that expresses the semantics, and no complicated abstraction unless it removes real complexity.
- Make illegal states unrepresentable whenever practical.
- Keep a single source of truth for each semantic fact; store a fact that derives cheaply from it only for a clear performance reason, and never as state that can drift out of sync.
- Minimize cloning, copying and rebuilding whole structures; optimize for fast incremental analysis and low memory churn.
- A check must never read a project-wide map directly, because every edit would invalidate it: go through a per-name or per-item tracked projection (details in `crates/semantics/AGENTS.md` and `development/architecture.md`).
- **Do not work around a structural problem** (multiple sources of truth, duplicated metadata, unjustified persisted derived state, snapshot-local ids where stable indirection would do, cloning only to maintain convenience state, a design more complicated than the semantics): design the fix, implement it, and when it settles an architectural question record in `decisions/<area>.md` the previous source of truth, what was duplicated or weak, the target shape, and the expected impact on correctness, simplicity, performance, and incremental analysis.

# Error handling

- Never swallow an analysis, synchronization, or document-loading error anywhere in the project.
- If an operation is required to keep analysis state coherent and it fails, `panic!` immediately rather than logging and carrying on with corrupted or stale state.
- A document-sync or analysis-sync failure in the LSP path is unrecoverable: `panic!`, never fall back to stale state (details in `crates/ry/AGENTS.md`).

# Testing strategy

- Robustness is the long-run priority, so everything must be fuzzable and simulatable (user directive). Every component (each analysis stage, the language server, the loader, the runtime, the GC, the C API, the JIT, the table engine, package install, the CLI) gets fuzz, simulation or scenario coverage the day it exists, never as a later add-on, with a bounded pass in the default test suite. `development/robustness.md` is the contract.
- Never let nondeterminism escape a seam: no wall clock, OS randomness, unordered iteration that reaches output, or unsteppable threads in a component's logic. Harnesses draw all randomness from one seed and print a replay command on failure. A new incremental shortcut gets a from-scratch cross-check behind `syntax::testing::enable_invariants()`.
- A bug a harness finds lands with a pinned regression input (a regression battery entry, fixture, or scenario). A user journey a feature enables gets a `crates/ry/tests/scenarios/*.scenario` file.
- Prefer fixtures. A new phase or module gets a fixture suite, and a behavior gets the lightest fixture change that captures it, before any crate-local unit test unless it is awkward as a fixture. Favor renderers that expose semantic facts over implementation detail. Read `development/testing.md` before you change the harness or add a suite.
- Fixtures are the desired semantics contract, not a record of known-wrong behavior. Change an expectation only when wording or behavior improves on purpose, never commit a wrong outcome to keep the suite green, and delete a case no longer worth keeping. A `group__case` name is the test identity: keep it stable and unique across the suite.
- Start every shell command with `source tools/env.sh &&` (the nix dev shell; `testing.md`).
- Run one fixture with `just fixture group__case [test-binary] [package]` (default `test_typing_fixtures`), and prefer focused crate tests (`cargo test -p semantics`) while iterating. `just gate` checks all.

# Rules hygiene

Every agent session reads this file, so keep it extremely high-signal: state each rule once.

Editing or clarifying an existing rule is always welcome. A new rule must be:

1. **Non-obvious**: someone familiar with the codebase would still get it wrong without the rule.
2. **Repeatedly encountered**: it came up more than once (several hits in one session count).
3. **Specific enough to act on**: a concrete instruction, not a vague principle.

A rule for a single crate belongs in that crate's `AGENTS.md`. Its `CLAUDE.md` (`@AGENTS.md`) loads it; other agents read it.

Do not describe a crate's architecture (module layout, data flow, key types): it goes stale fast, and the code shows it. A rule marks a trap to avoid, not a map to follow.
