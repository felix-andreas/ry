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
- **Each cloud session merges its own pull request**: a fresh reviewer subagent reads the diff, the session fixes what it finds, merges main in, runs the checks, marks the PR ready and squash-merges it, then starts follow-up work on a fresh branch. The persistent in-session merge agent handles only PRs from sessions that cannot merge, and cross-session conflicts.
- **In-session agents that edit code get their own git worktree** (`isolation: "worktree"`) so the main checkout stays clean.
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
- Corpus packages round 4 (session_0111kWqDJJsagmnUNcuZLJp4, Opus): PR #167 (97/146) self-merging. Next: parallel's mc_fork (blocks lgr, doParallel, foreach), cli's sink(type="message"), flushing file connections before close.
- JIT round 2 (session_0111kWqBQdpLjmrdiA5ajbhR, Opus): direct calls with promise elision, builtin inlining, the closures regression, the why-ry benchmark. PR #128 (why-ry Speed table: drawdown 13x, loop_sum 6.3x, fib 0.9x) merged as 6bfb590. Now round 3: typed code faster than untyped (static types as profiles, stricter arrays that convert to R structures), shown as the third benchmark column. The JIT (PR #117) merged as 3401282.
- data.table code on ry's engine (session_0111kWqCxpbzWznPZjGyyBYk, Opus): #145, #164 and #178 (lazy mimalloc arena commit in libR, no zeroing of filled vectors; rbindlist first calls 2 to 6 s down to 1.1 to 1.3 s, f4a550c) merged. unique/uniqueN, between and chin at parity with GNU R. Now: dcast (0.257 vs 0.196 s). Self-merges.
- Tier-0 interpreter speed (session_0111kWqEYviXMfUZx1qCaLE1, Opus): #137 merged; #170 (suites 1.25x/1.39x/1.18x GNU R, process wall 0.97x/0.84x/0.79x) self-merging. Next: bulk frame-cell allocation, bump allocation and the young push, toward per-test time at or below GNU R.
- Analysis performance round 6 (session_0111kWqEhvxZmGPocu7FZkAe, Opus): #172 (struct-of-arrays HIR from the green tree, 590749072d) merged; findings identical, wall 2.15 to 2.23 s, peak about 515 MB. Main had regressed from 1.8 s / 438 MB to 2.2 s / 554 MB via #166's top-level classifier and #174's all-item SCC graph. Now: win those back, then cold check under 1 s (parse cost, Symbol re-interning, batch barriers, per-item type store). Self-merges.
- Analysis false reports (session_0111kWqAtf1jzUW6T3FHvaDo, Opus): sole owner of false reports. #141, #146 and #176 (NULL placeholders, negation, lazy arguments; f934d0c) merged. Main had regressed to 468 corpus reports through #169's callee-directed argument typing (rlang `as_function` pins `.f` to character); main plus #176 is 318. Now: fix that regression at the root, back to at most 235. Self-merges.
- Editor tooling (session_0111kWqC2nSdtiQTHBsJihsP, Opus): #150, #163 and #175 (cd84139) merged; goto/references/rename p99 1.4 to 4.2 ms (was 13 to 25), settled startup 430 to 540 ms, completeness pass (ranking, argument names, parameter and loop-variable hover, annotation completion). Now: settled toward 200 ms (conditional_slot_items or lazy per-file naming), SyncTable contention, data.frame()/matrix() hover gaps. Self-merges.
- Positioning (session_0111kWqC4b6STPYdoGhJV3zZ, Opus): landing PRs #139, #142, #151, #153, #157 and #160 merged (#160: balanced frames, Venn step bar and captions, `ry run`). Pending: `[]` in DataFrame snippets once #159 lands, the benchmark tab once the JIT session pings, the DataFrame claim checked against #158's final numbers.
- DataFrame round 3 (session_0111kWqESwZBLCytfJFF7GT3, Opus): #158 (19 db-benchmark queries at 1e7 rows, 5.22 s vs data.table 26.16 s) merged as ed579ff. Now: data-oriented layouts and typed `[...]` literals in the DataFrame. Self-merges.
- Package installation on ry (session_0111kWqBd1i4XC24Kx7uFf5y, Opus): the R-level half of R CMD INSTALL (lazy-load DB, Meta/, help, lazy data) on ry's interpreter, bin/R and bin/Rscript on ry, binary installs; exit criterion is step 4's: the 232 closure packages install, load and pass their tests with no GNU R at install time. Coordinates with corpus packages round 3. PR #147 (235 of 236 corpus packages install and load with no GNU R; Rblpapi excluded) merged as 39cc252; #173 (fcaa521) merged: callr::r_session works, R version derived from the linked base DESCRIPTION, 93/192 package test suites pass on ry before the version fix (GNU R 165). Now: reinstall against derived headers, full rerun, PR with numbers and install-side fixes (S7, rcmdcheck, pkgbuild).
- Standard library round 4 (session_0111kWqCc5uyUU7uPVY2RzP4, Opus): #156 and #168 (nested in-place updates 3.75 s to 0.028 s, compiled-frame register check fixing an aliasing bug in plain R, wrong-type key errors, GNU R message) merged, #168 as 596a8c4. Now: stdlib call overhead under 0.1 µs via direct builtin entries (with the tier-0 and JIT sessions), then sound move-into-parameter via escape analysis. Self-merges.
- Typed arrays round 3 (session_0111kWqCzWoF4Pi1fKpGsAiB, Opus): #159 (905ac5a) and #169 (contextual lambda typing, f8d07fe) merged. #177 (runtime audit of typed docs examples under strict `ry run`, edecc195) merged. Now: Array benchmarks once the JIT's Array guard lands (check-in scheduled). Self-merges.
- CLI UX (session_0111kWqEAz7wRnhs8fRLRyrM, Opus): #149, #165 and #179 (typeahead kept; cold `ry run` on a 326k-line package 2.9 s to 1.0 s vs `ry R` 1.2 s, by raising JIT thresholds to 1000 entries and lazy mimalloc commit; d146c11) merged. Now: warm-reload latency, typeahead for fd-0 readers and Ctrl-C in the drain window, cold start below 1.0 s. Self-merges.
- Strict mode round 3 (session_0111kWqBB9Ca4CRxyz8Xzcxm, Opus): #166 (strict top level, `@pure`, lazy pure constants; strict load about 150 ms faster on finbench) merged as 018b7a5. Now: stub-purity scoping, local() replacement, .onLoad writes, then a fully typed strict port of the finance code base to dogfood strict mode and measure typed vs untyped. Self-merges.
- The workspace builds on a pinned nightly (rust-toolchain.toml). CI picks it up with no workflow edit; the user may optionally replace `dtolnay/rust-toolchain@stable` with `rustup toolchain install`. The flake.lock rust-overlay bump is unverified under `nix build` (the try/beta session checks it).
- Graphics engine (session_0111kWqAywmCkqy2bYhhfetf, Opus, branch feat/graphics-engine): the GE device API in libr, grid and ragg, one engine under base graphics and grid, jpeg and postscript output; owns graphics, taken over from the base R session. Self-merges.
- R 4.6 base support (session_0111kWqDR2SRxg7Gei82wZJy, Opus, branch fix/r-4-6-base): beta blocker. ry's runtime fails to load R 4.6's default packages (version check, then `registerS3method` substring error); R 4.3.3 works. Support R 4.3 and later, oracle and tests against both. Self-merges; tells the beta session when done.
- Try ry and beta release (session_0111kWqBeVFPbHpyqC8bDyWu, Opus): `just try` end-to-end demo, working nix builds on the pinned nightly, beta release readiness. #162 (9e0b198) merged: version 0.4.0-beta (untagged), nix build/develop/flake check pass, release workflow staged at `.github/pending-release.yml`. User-only actions relayed to the user (move the workflow, VSCE_PAT/OVSX_PAT, Open VSX namespace, Zed submission, glibc 2.35 floor, Windows without runtime, docs deploy). Blocked on the R 4.6 runtime fix for the smoke test; reruns it once that lands. Self-merges.
- Runtime on macOS and Windows (session_0111kWqEtmpZFGTxsHPxCDVn, Opus, branch feat/runtime-portable): libR.dylib exports and install_name, package linking, R_HOME discovery for R.framework and Homebrew, rcmd flags; then R.dll and Rtools import libraries. Verified by cross-compilation in Linux containers until the release workflow runs on macOS runners. Self-merges.
- Persistent merge agent: an in-session subagent that squash-merges cloud PRs on request.

## Next

Nothing queued: every build-order step is in flight.

Follow-ups:
- An in-session worker is running a docs pass over the recent merges (#154, #162, #166 to #177).
- Robustness #154 (39ce6b4) merged and its session archived: LSP simulator, docs-as-tests, 15 cargo-fuzz targets, `oracle fuzz`, GC torture, 15 bugs fixed. An in-session worker is fixing the three main-side failures it found (oracle bytecode__hot_loops expected, libr numerics.expected, REPL e2e stderr flake).

