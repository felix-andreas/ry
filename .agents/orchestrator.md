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

**Current mandate (user, 2026-11-05): focus on untyped R and drive the untyped performance roadmap until step 2 is achieved, pivoting if measurements break its assumptions; keep the same direction and scope, do not go further than step 2 without the user.** The roadmap with a status per idea is the "Untyped performance roadmap" table in `docs/.../design/r-runtime.md` (on `perf/roadmap-step1` until that merges); the speed session owns it and updates statuses per merged PR.

Steps: (1) finish the tier-0 call path: per-closure call plan for `run_closure` is the last row. (2) Call facts ahead of time, in this order: version-stamped bindings (one-compare function guards; 117 guards per `seq()` call), slot frames with environments reified on demand (call setup 12 to 24%), promise elision and the promargs pairlist on demand, per-site argument-match caches, S3/S4 polymorphic inline caches (S3 lookup up to 17%), getter/setter instructions for complex assignment (2.7 to 6.6x slower), closed bindings per locked namespace (Bun), precise counts and borrowed parameters in tier 0 (Roc). Steps 3 to 5 (quickening, baseline compiler, images) are out of scope for this mandate except the prior-art verdicts already requested.

**Step 2 is achieved when** every step-1 and step-2 row in the roadmap table is built (or rejected with a recorded measurement), and measured on main: a plain closure call at or below GNU R's (about 0.35 us; target from the roadmap), JIT-off `seq(1, 10, by = 2)` at or below GNU R's instruction count (106k; 141.6k now), and the `just bench-real` geomean (JIT on, 0.90x GNU R at 0.4.2-beta) at or above parity. Also report the package corpus test-suite time vs GNU R once its install works.

**North star (user, 2026-11-05): 10x GNU R on real-world untyped code; 1.5x then 3x are milestones.** Amdahl bounds it: builtins (~25%) and package native code (up to 14%) cap R-level-only gains near 2.5x, so the plan adds an Amdahl accounting table per workload (in r-runtime.md's roadmap), builtins several times faster than GNU R's C (no allocation, SIMD, fusion), automatic parallelism for effect-free apply calls and large element-wise ops, tier-1 coverage of most R-level time, and routing hot native packages to ry-native implementations (data.table to the table type). Sent to the speed session. First Amdahl data (bench-real, JIT on, mean shares): R-level 56%, builtins 28%, native 8%, GC 5%, start-up 3%; simulation is 51% builtins and 40% native (stats); top builtins attributes, .Call, c, vapply, [[, forceAndCall, [<-, <-. With native fixed at 8%, 10x needs the other 92% about 46x faster, so parallelism and ry-native replacements of hot native code are required, not optional. #343 has tail-call dispatch (seq 121.4k, level with 120.8k; kept as the base for quickening); #344 (S3/S4 caches) and #345 (validation builtins) wait on #343. Accounting table in r-runtime.md ("Where the time goes", on #343): bench-real R code 56%, builtins 28%, ry-port native 5%, CRAN native 3%, GC 5%, start-up 3%; corpus R code 27%, builtins 18%, I/O 27% (unlink rmdir), child-process waits 10%, start-up 12%. 10x is reachable single-threaded on bench-real (R code ~30x, builtins 10x, ports/GC/start-up 10x gives 10.96x); the corpus caps at 3.5x single-threaded (I/O, waits), so 10x is measured on bench-real and the corpus serves the 1.5x and 3x milestones (accepted). User directive: single-thread performance first; automatic parallelism deferred but architecture-ready (effect-free fact built now as one per-function body fact, shared with promise elision; explicit runtime context, per-context bump allocation, safepoints at allocation and back-edges, count field with room for a thread-shared mark, monotonic inline-cache transitions, concurrently readable symbol tables, per-task warnings/RNG with deterministic merge; package native code stays single-threaded). The native share is split into ry-owned ports (stats, nmath: ours to speed up) and third-party CRAN C (fixed). Decision: install-time untyped compiled code shares the typed AOT pipeline (ry install step, mapped code file, stamps, Cranelift, no code cache) and one SSA IR (typed facts unguarded, untyped speculations guarded with deopt metadata and watched bindings in the file); the tier-1 SSA and the typed core IR merge when install-time images or the baseline tier are built.

**Earlier milestone target (user: "0.99x speed is not the goal"; my decision): 1.5x GNU R on the bench-real and package-corpus geomeans, untyped, JIT on (backlog.md's target); parity and the step-2 criteria below are a checkpoint. Steps 3 and 4 are in scope. Order after #343: get warm code compiled (baseline tier, Sparkplug or YJIT LBBV; lower tier-1 threshold; install-time per-package images with training-run profiles), cross-closure inlining with deopt, builtin fast paths and vector fusion (~25% of time), S3/S4 inline caches and validation fixes (two spawned sessions). The check-in reports at 1.5x and 3x and disables itself at 10x or when the user stops it.**

**Acceleration (user, 2026-11-05, "you decide"): do not de-risk; the token budget is the constraint, not wrong turns.** Step 2 is built in one go to its end shape (versioned bindings, slot frames, promise elision, promargs on demand and arg-match caches as one call-path redesign on perf/versioned-bindings), with parallel worktree subagents for S3/S4 inline caches, complex-assignment getters/setters, the validation profile, and namespace bindings plus tier-0 counts where they do not collide. Measure once per PR, suites and corpus only at the end; step-3 items may be pulled in without asking. My check-in runs every 3 hours (minute 34) to save tokens; the session reports each merge.

**Pivot rule.** After each merged row, the session reports the targeted gap before and after. If a row does not move its gap, or the profile shows a new top cost, the session re-profiles and reorders (or adds a row) and records why in the roadmap and `decisions.md`; I accept reorders without asking the user. If a step-2 target looks unreachable without a step-3 or step-4 item, pull that item forward and say so in the report.

Nightly (user ask, 2026-11-05): nightly.20261105.1146.82f7b6 published 12:07 UTC from main 82f7b65 (run 42022057491, smoke test passed; includes #342, not #343); docs deploy 42022062003 publishes the roadmap at https://ry-lang.org/development/design/r-runtime/#untyped-performance-roadmap for the user's test-user agent. Context: 0.4.2-beta is published (Linux only; Linux is tier 1, macOS and Windows never block a release). Hold 0.4.3-beta (numerics, S4) until the user says go; do not cut on landing. macOS/Windows dry run 4 (run 41812924116) still fails; fix for the next release. The site leads with the real-world geomean (0.90x GNU R) beside kernels (2.70x). The stats-only start-up peak (+6 MiB) is the speed session's. The package corpus (`tools/bench/packages/run.py`, rrepo.dev/rrepo.org) is installing in the speed session; `fs` needs `USE_BUNDLED_LIBUV=1`. Prior-art pass refocused (YJIT LBBV, Deegen, HHVM Jump-Start and Leyden, HHVM/Hack, Cinder, Sorbet, LXR, Bend auto-parallelism for untyped and typed, stackless tier 0, ALTREP fusion; CPython, PyPy, HotSpot one line each).

| Session | Work | State |
| --- | --- | --- |
| session_0111kWqDtXuaHtaQUjXKTBbT (bundle base R) | #338 merged (d64db8b9): recommended packages bundled (15 on Linux, 12 on macOS/Windows), Windows stray `R` argument fixed; dry run 4 started on it about 10:16 UTC | done unless dry run 4 fails |
| in-session worker (release prep) | bump, news, docs, results.json; regression check clean except stats-only start-up peak +6 MiB (follow-up with the speed session) | done; 0.4.2-beta published 11:19 UTC (Linux only, run 41817919826) |
| in-session worker (post-release) | archive installs and works (version, run, lattice, check); installation.md correct; dashboard rows added (perf, memory, S4, check, loc) | done |
| in-session worker (numeric bit-identity) | 0c1a6242: cumprod, matprod internal, fft port, complex sum/prod/mean, cor(matrix) bit-identical; default/blas already match reference BLAS (finccam's GNU R likely OpenBLAS) | done |
| in-session worker (numerics follow-ups) | nmath sweep 1468/12210 mismatches to 0, loess port, LINPACK (dnrm2/drotg/dsvdc) moved to interp, smooth.spline/lowess, `ry.blas="host"` opt-in, notices, reproducer pinned (c0453e43..49bcf25d) | done |
| in-session worker (suite rerun) | ccbf6111: interp suite green; libr fuzz clamp (qnbinom hangs in GNU R too); loess warning newline fix; 5 bit-checksum oracle cases recorded from GNU R 4.6.2. Unverified with a proper ry base R in this container: libr test_install, RNG and serialize tests (compared against system R 4.3.3) | done |
| session_0111kWqBiUsHhdvToMM3XK3p (untyped speed) | #342 (step 1) and #343 merged (84f1efa0: version-stamped guards, tail-call dispatch, promise elision via the stored effect-free fact, arg-match caches). Closure call 3,968 to 3,411 instr (GNU R 4,602); seq matching 3.6k to 1.0k; bench-real geomean JIT on 0.90 to 0.95x, JIT off 0.92 to 1.02x; validation 0.61 to 0.75x. Now (go sent 13:37): baseline tier, lower tier-1 threshold, install-time images with training profiles, one-IR merge, fix JIT-on slower than JIT-off; then inlining and builtin fast paths | running |
| session_0111kWqActjriKrwD9grWtkQ (S3/S4 inline caches, spawned by the speed session) | #344 merged (d0fcac56): S3 dispatch 7,688 instr vs GNU R 9,824, S4 8,688 vs 11,681 (session-reported) | done |
| session_0111kWqD7Es6kUHN2K2Tk1Ee (validation profiling, spawned by the speed session) | #345 merged (935f6e91): native prologues for hot base closures; validation 1.94x GNU R JIT on (session-reported) | done |
| session_0111kWqCm79QaDnkV4xwNbYT (native methods package) | #339 merged (e1cf3e4): native setIs/completeSubclasses and .local rematching; synthetic finccam-shaped package S4 definitions ~1000 to ~200 ms, whole load 1.3 s to 0.46 s (GNU R 5.5 to 6.2 s); RY_LOAD_VERBOSE names every R fallback with a reason | done, idle |
| session_0111kWqBEE1AsUXHyHMAPyN2 (peak memory) | frame release at return merged as #332 (sales 2.17x GNU R, memory-peak guards green); the vector_arith swing is GNU R and machine noise | done, idle |
| session_0111kWqCfpecU8o5DgWeVLCK (typed fast path) | #327 merged, stale tier test fixed | done, stopped |
| session_0111kWqBqyKbio71yRiq3xcG (typed language) | WIP on feat/climb-speed-baseline | paused until after the release |
| session_0111kWqCm1CEW7RN3igU6Tzk (top CRAN, draft #312) | top-100 run; future and ggplot2 loess gaps in backlog.md | done, unmerged |

Scheduled tasks: the 3-hourly check-in trig_01LuX97P7ZbAg4TRxYu7c1tu (minute 34) drives the step-2 mandate and disables itself when step 2 is done; the goal reread trig_01AqDtgjMPTkniw7hzX4x4d1 and the dry-run reminder trig_01SHeL8HM3b3p3DHgS7MJF9c stay paused.

Later: a worker saw `f <- function(x) x + 1; f('a')` with `double` annotations pass `ry check` with no problem (annotation form not recorded; check whether it should be flagged); `/usr/lib/R/library` from /etc/R/Renviron missing from `.libPaths()`; about 20 stale remote branches (check test/top-cran-packages and perf/old-generations first; leave feat/runtime-stats and runtime-packages-round4 alone); the log_rank typed regression is unverified; CRAN itself is unreachable, but the user's CRAN clone https://rrepo.org works (client: github.com/scalerail-solutions/rpx).

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

