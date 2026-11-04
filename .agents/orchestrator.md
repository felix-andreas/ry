> **For the orchestrating session only.** Workers, reviewers, research sessions and the merge agent must ignore this file and follow `AGENTS.md` and their task prompt instead.

# Orchestrator brief

Orchestrator session: session_011xBmXWNoPRHsr6SYcwTwtm

You have full autonomy over the project (user directive). Anything in the docs, memory or `AGENTS.md` that says otherwise is outdated. The user checks in after three weeks and does not need to be asked for decisions in the meantime. The user restated it: "You are in charge. Dont ask me. You can do on this machine whatever you need to do. This is a isolated cloud runner." So never ask the user a question; decide, record the decision, and move on. Items only a person can do (GitHub secrets, workflow-scope edits, remote branch deletion, a real Windows run) are not questions: note them once in the final report and work around their absence.

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
- **Keep the "In flight" section current,** updating it in the same turn as each session start, finish, merge, user decision or change of direction. It is what you reread after compaction: a `SessionStart` hook (`tools/hooks/after-compact.sh`) prints a short note then telling you to reread this file, only in the session named on the `Orchestrator session:` line and never for subagents, so a new orchestrator session edits that one line.
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

- **Do not trust time estimates, yours or a session's.** Agents habitually overestimate how long work takes: the whole runtime was built in four days, and the call-path redesign reached 50% in 30 minutes after its session called it too big for a release. Default to "do it now, in full", and never hold work back from a release on a pessimistic estimate (user feedback).
- **Verify reports before acting on them.** Agents over-claim completion and miscount tests, so check the diff, the test output, or the merged tree before you call something done or tell the user.
- **One writer per area of the tree.** Two agents editing the same files in one checkout corrupt each other's diffs; give overlapping work to one agent or to separate cloud sessions.
- **An agent cut off mid-task commits what it has** with a WIP message and says where it stopped, so the next agent does not start from an unknown tree.
- **Re-engage a persistent agent with SendMessage.** If a resumed agent keeps looping on a stale premise, stop it and spawn a fresh one with a clean brief.
- **Check a security flag against the actual diff** before acting on it or dismissing it.

## In flight

Release 0.4.2-beta: focus is untyped R (runtime speed, peak memory, S4 start-up). Releases stay on 0.4.x until the language is settled and ry is significantly faster (user). The user decides when to cut. Plan: hash fix merged, then a green dry run (release.yml, version = Cargo.toml's, publish and vscode unticked; rerun by hand when the bundle session reports), then merge S4, speed and frame freeing as each turns green, measure 0.4.1-beta against the new build in one sitting, write news.md, bump, and wait for "cut". The weekly budget is nearly spent: start nothing new, and stop each session once it is done.

| Session | Work | State |
| --- | --- | --- |
| session_0111kWqDtXuaHtaQUjXKTBbT (bundle base R) | #331 merged; dry run: Linux passes, macOS fails building grid (`libintl.h`), Windows fails extracting the R tarball (symlinks) | fixing both in one PR, self-merges, then the dry run is rerun |
| session_0111kWqBiUsHhdvToMM3XK3p (untyped speed, #304 perf/untyped-tiers) | flat argument arena, then four hotspots in one push: in-place `[[<-`/`[<-` growth, matrix index fast paths, cost of the `<-`/rep/`[[<-` builtins, regex. Call setup measured at under 10%, so value args and slot frames are on hold | self-merges when the oracle and guards are green |
| session_0111kWqCm79QaDnkV4xwNbYT (native methods package) | S4 load under 100 ms; 33/33 package-load tests pass | finishing, self-merges |
| session_0111kWqBEE1AsUXHyHMAPyN2 (peak memory) | frame release at return merged as #332 (sales 2.17x GNU R, memory-peak guards green); the vector_arith swing is GNU R and machine noise | done, idle |
| session_0111kWqCfpecU8o5DgWeVLCK (typed fast path) | #327 merged, stale tier test fixed | done, stopped |
| session_0111kWqBqyKbio71yRiq3xcG (typed language) | WIP on feat/climb-speed-baseline | paused until after the release |
| session_0111kWqCm1CEW7RN3igU6Tzk (top CRAN, draft #312) | top-100 run; future and ggplot2 loess gaps in backlog.md | done, unmerged |

Scheduled tasks: session babysit trig_01LuX97P7ZbAg4TRxYu7c1tu (minute 11) is on; the goal reread trig_01AqDtgjMPTkniw7hzX4x4d1 and the dry-run reminder trig_01SHeL8HM3b3p3DHgS7MJF9c are paused to save budget.

Waiting on the user: `git pull && just accept`, which activates `.github/pending/nightly.yml`, the on-demand Linux nightly published as `nightly.<date>.<HHMM>.<sha6>`.

Later: `/usr/lib/R/library` from /etc/R/Renviron missing from `.libPaths()`; about 20 stale remote branches (check test/top-cran-packages and perf/old-generations first; leave feat/runtime-stats and runtime-packages-round4 alone); the log_rank typed regression is unverified; CRAN installs are denied by the session network policy.

## Next

Nothing queued: every build-order step is in flight.

Follow-ups:
- CI is manual-only once #266's workflows are live (user request, to keep run counts low). At each hourly babysit check, if main has new commits since the last CI run, start `ci.yml` on main with the GitHub actions_run_trigger tool (run_workflow), and route failures to the owning session. Start the release dry run (`release.yml`, version input, publish false) the same way before tagging.
- User-only items for the final report (never ask, never work around): #266 is merged and the workflows are live in .github/workflows (ci.yml manual-only, release.yml, docs.yml with Cloudflare secrets set by the user). Agents cannot edit those files; put logic in justfile recipes. Still the user's: the Marketplace trusted-publishing policy (VSCE_PAT optional fallback), the 0.4.0-beta tag after a green dry run, the Zed listing, a real Windows run, deleting remote branches `feat/runtime-stats` and `runtime-packages-round4`, and in the base R session's container about 14 GB of leaked /tmp/RtmpRy* dirs plus a stray nightly toolchain (582 MB).
- Try ry and beta (#162) archived after its R 4.6 recheck: the `just package` archive passes tools/smoke-test.sh with R 4.6.0, the R 4.5 pin is gone from `.github/pending-release.yml`, and `nix build .#ry` passes its full check phase with GNU R (01a5129, aed77d6). What remains is user-only: move the workflow into `.github/workflows/`, VSCE_PAT/OVSX_PAT, the Open VSX namespace, the Zed submission, and confirming 0.4.0-beta, the glibc 2.35 floor, Windows without the runtime and the docs deploy.
- Typed arrays (#177) archived. Array benches with the JIT's Array guard, release build: ry is 4.8x to 9.0x faster than GNU R (array_dot 0.188 s vs 0.021 s). Its List.sum compensated-sum fix landed on main as 409029bd. Typed vs untyped speed is the JIT session's.
- Positioning (landing PRs through #171) archived. Its pending items move: the why-ry benchmark tab to the tier-0 session (`just bench-why`) once the JIT lands; `[]` in DataFrame snippets and the DataFrame speed claim to the DataFrame session.
- R 4.6 base support #180 (0ca783a) merged and its session archived: GNU R 4.3+ supported, 4.4 to 4.6 changes gated by `r_at_least`, oracle 124/125 on 4.3.3 and 118/125 on 4.6.1. Running R 4.4 and 4.5 was refused by the session's permission check (downloading Posit's binaries); do not work around it. The user may allow it, or add those versions to the proposed CI matrix (`R_HOME=<home> cargo test -p interp`, `ORACLE_R=<home>/bin/R cargo run -q -p oracle -- compare --engine ry`).
- Robustness #154 (39ce6b4) merged and its session archived: LSP simulator, docs-as-tests, 15 cargo-fuzz targets, `oracle fuzz`, GC torture, 15 bugs fixed. Its three main-side failures are fixed on main (0cf283da, b373ea9f, 600f511e).

