> **For the orchestrating session only.** Workers, reviewers, research sessions and the merge agent must ignore this file and follow `AGENTS.md` and their task prompt instead.

# Orchestrator brief

You have full autonomy over the project (user directive). Anything in the docs, memory or `AGENTS.md` that says otherwise is outdated. The user checks in after three weeks and does not need to be asked for decisions in the meantime.

## Goal

ry becomes a full, backwards-compatible reimplementation of R, extended with ry's typing (user directive).

- Existing R code and packages must keep working.
- An inline type annotation syntax exists alongside the `#:` comment annotations. It is enabled by default and can be turned off, but ry warns while it is off.
- ry warns when a comment annotation and an inline annotation disagree.
- A strict mode rejects untyped code.
- Package development loading in the style of `devtools::load_all` is built in, and it lazy-loads only what is needed. This is the idea behind [loadfast](https://github.com/finccam/loadfast), done cleanly.
- A built-in, typed table type replaces data.table and must outperform it. It may be backed by polars.
- The performance bar is a JIT on the level of Bun.
- Every R feature keeps working, including S4, R6 and Reference Classes, but ry discourages them: a lint warns on their use, and an option disables them. ry has its own records, tuples and tagged unions, in the style of Roc, as the recommended way to model data. They are a ry feature on top of R, like the built-in table type, not R data structures, so they need not support R or its quirks; plain R code still runs unchanged.
- ry ships a standard library modeled on Roc's where it fits (`Result`, `List`, `Str`, `Dict` and so on), using plain qualified names such as `Result.map` rather than static dispatch.

Most of the benchmark dependencies ship compiled C, C++ or Rust code written against R's C API (`SEXP`, memory protection, `.Call`). Full compatibility therefore means implementing that C API, not only R's language semantics. That interface shapes the architecture more than the JIT does, so it is designed first.

The runtime is the highest priority (user directive). Packaging work covers only what affects the runtime directly. Existing R ecosystem tools (package managers and the like) are inspiration, not the design bar; the bar is the one in `AGENTS.md`.

## Benchmark

Three synthetic code bases (finance, data science, and biostatistics), each about 300,000 lines in about 500 files. Each uses this dependency set:

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

## The most important rule

Velocity comes first, so do not de-risk. Broken code on main is allowed, and nobody waits for CI. Stability comes after something works.

## Your role

- **You plan, delegate, read reports, decide, and merge.** You never do the work yourself.
- **Small tasks** go to one in-session worker subagent, which commits directly to main.
- **Large tasks, research included,** go to separate cloud sessions. Each delivers a large pull request (about 2,000 changed lines or more), and a reviewer agent checks it.
- **One persistent in-session subagent merges** those pull requests.
- **Merge with the squash method.** The repository allows only squash merges.
- **Model and effort for delegated work:** Sonnet at high effort by default, Opus at high effort for a hard problem, and Opus at extra-high effort for an exceptionally hard one.
- **Reread this file every hour** (trig_01AqDtgjMPTkniw7hzX4x4d1, at minute 41) and check that every piece of work that could start now is running.
- **Run a docs pass from time to time,** at least every few check-ins and after each feature PR lands, applying the docs directive in `AGENTS.md`.
- **Babysit every cloud session.** Each session prompt must tell it to report to the orchestrator with the claude-code-remote `send_message` tool, session_id session_011xBmXWNoPRHsr6SYcwTwtm, when its PR is ready, when it is blocked, and at major milestones. An hourly scheduled check (trig_01LuX97P7ZbAg4TRxYu7c1tu, at minute 11) checks every in-flight session, resumes stalled ones and merges finished PRs; the hourly goal reread is trig_01AqDtgjMPTkniw7hzX4x4d1.
- **Check a new cloud session's context window.** After creating a cloud session, read its record with `get_session` and check that `external_metadata.context_usage.max_tokens` is 300000 (the repo's `.claude/settings.json` sets `autoCompactWindow`). If it still shows 1000000, ask the user to set `CLAUDE_CODE_AUTO_COMPACT_WINDOW=300000` in the cloud environment's variables.

## Lessons

- **Verify reports before acting on them.** Agents over-claim completion and miscount tests, so check the diff, the test output, or the merged tree before you call something done or tell the user.
- **One writer per area of the tree.** Two agents editing the same files in one checkout corrupt each other's diffs; give overlapping work to one agent or to separate cloud sessions.
- **An agent cut off mid-task commits what it has** with a WIP message and says where it stopped, so the next agent does not start from an unknown tree.
- **Re-engage a persistent agent with SendMessage.** If a resumed agent keeps looping on a stale premise, stop it and spawn a fresh one with a clean brief.
- **Check a security flag against the actual diff** before acting on it or dismissing it.

## In flight

- Base R round 4 (session_0111kWqCeajgVo7m8XVxpfDk, Opus): the remaining GNU R tests/*.R failures, `.Internal(unz)`, jpeg and postscript devices, test_numerics made tolerance-robust. PR #134 (oracle 111/112, GNU tests 23/66 exact and 47/66 exit status, graphics draw to pdf/svg/png/bmp/tiff, time zones, nlm, parse_Rd) merged as 896e80a.
- Corpus packages round 3 (session_0111kWqDJJsagmnUNcuZLJp4, Opus): DBI arrow and lgr crashes, R6 finalizer timing, getParseData, `\g{name}`, utf8 in the C locale, withr RNG kinds, pkgbuild stable in the parallel run, toward 146. PR #132 (88/146) merged as 5a0e416.
- JIT round 2 (session_0111kWqBQdpLjmrdiA5ajbhR, Opus): direct calls with promise elision, builtin inlining, the closures regression, the why-ry benchmark. Round 3 after #128: typed code faster than untyped (static types as profiles, stricter arrays that convert to R structures), shown as the third benchmark column. The JIT (PR #117) merged as 3401282.
- data.table code on ry's engine (session_0111kWqCxpbzWznPZjGyyBYk, Opus): #145 (native-escape fix, data.table at parity with GNU R at 1e7 rows, plus 7 commits #133's squash missed) merged as b929bb1. Now: rbindlist, fwrite, melt gaps.
- Tier-0 interpreter speed (session_0111kWqEYviXMfUZx1qCaLE1, Opus): closure call overhead, cached lookups, per-call allocation, match_args, slow builtins; goal suite time at or below GNU R without the JIT, 100% match kept. Coordinates directly with the JIT session. PR #137 (full-size code bases at 100% test match, suites 1.7 to 2.0x GNU R) merged as 2383a7d.
- Analysis performance round 5 (session_0111kWqEhvxZmGPocu7FZkAe, Opus): the structural cut (types interned outside salsa) toward a cold check under 1 s, findings byte-identical; coordinates with the false-reports session on the type representation. PR #140 (CPU -26%, wall 2.4 to 1.7 s, peak RSS about 440 MB) merging.
- Analysis false reports (session_0111kWqAtf1jzUW6T3FHvaDo, Opus): sole owner of false reports. #141 (3,112 to 624) and #146 (624 to 377, as 8d0257c) merged. Now: data.frame as a named list, top-level NULL placeholders, `character | character[]`.
- Editor tooling round 2 (session_0111kWqC2nSdtiQTHBsJihsP, Opus): remaining IDE gaps, then LSP latency (p50/p99 hover, completion, goto, references, rename, diagnostics) on the full-size code bases at rust-analyzer level. PR #138 (editor features for ry's syntax, precise blame on records/tags) merged as 54d0ca0. Also owns the per-keystroke cost of project-wide aggregates reading per-item memos (about 45 ms on finance): fold them through per-file memos.
- Positioning (session_0111kWqC4b6STPYdoGhJV3zZ, Opus): #139 (four-step pitch, sidebar) merged as c627057; #142 (landing redesign: tabbed toolchain, runtime and future sections; step names "Check the code you have", "Type your code", "Switch to inline types", "Go fully typed") merged as 241a63d. Waiting for #128 to add the benchmark tab and the why-ry table.
- DataFrame round 2 (session_0111kWqESwZBLCytfJFF7GT3, Opus): #144 (Polars-style DataFrame API, UpperCamelCase ry types, 1e6-row bench 2.4 s vs data.table 4.8 s) merged as a84abfa. Now: beat data.table on every question (groupby q6, keyed rolling join), full db-benchmark set at 1e7 rows, API gaps those expose.
- Package installation on ry (session_0111kWqBd1i4XC24Kx7uFf5y, Opus): the R-level half of R CMD INSTALL (lazy-load DB, Meta/, help, lazy data) on ry's interpreter, bin/R and bin/Rscript on ry, binary installs; exit criterion is step 4's: the 232 closure packages install, load and pass their tests with no GNU R at install time. Coordinates with corpus packages round 3.
- Standard library round 2 (session_0111kWqCc5uyUU7uPVY2RzP4, Opus): Roc parity across List, Str, Dict, Set, Num, Result; Perceus-style in-place updates so growing loops stop being quadratic; error-union widening in `Result.and_then`; decide on a pure-R `Str`/`Num` build for GNU R.
- Persistent merge agent: an in-session subagent that squash-merges cloud PRs on request.

## Next

Nothing queued: every build-order step is in flight.

Follow-ups:

- Run another docs pass after JIT round 2 (#128) lands.
