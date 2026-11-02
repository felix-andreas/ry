> **For the orchestrating session only.** Workers, reviewers, research sessions and the merge agent must ignore this file and follow `AGENTS.md` and their task prompt instead.

# Orchestrator brief

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

- Base R round 5 (session_0111kWqCeajgVo7m8XVxpfDk, Opus): #134 and #192 merged. #214 merged (sockets, PSOCK clusters, download.file fixes; GNU tests/*.R 64 match; oracle 152/153, only stats__lbfgsb off by one printed value). Now: one clean-room L-BFGS-B shared by interp optim and libr's C API (replacing the two copies), then the rest of the clean-room audit list, parseLatex, internal download method, real Rprof/Rprofmem. Not install-on-ry (package installation session owns it). Its request to delete its own ~15 GB /tmp/RtmpRy* dirs (five dirs, ~1.9 GB disk free) waits on the user. Self-merges.
- Corpus packages round 5 (session_0111kWqDJJsagmnUNcuZLJp4, Opus): #167 and #211 (f866e28e: package tests on the GNU-installed library 115 of 146 (78.8%); the ry-installed suite was last measured at 90 of 165 by #188; oracle 149/150 on ry, 148/150 on ry-jit, the extra one a stale JIT global binding sent to the JIT session) merged. Now: target 130+/146 via libr symbols (commonmark, git2r, gmp), PCRE gaps, vroom/later hangs, socket-based tests on base R's #214, parallel.rs cfg guards, break-from-closure, re-recorded datatable__* cases. Stale remote branch runtime-packages-round4 needs the user to delete it. Self-merges.
- JIT round 5 (session_0111kWqBQdpLjmrdiA5ajbhR, Opus): #117, #128, #201, #205 merged (#205: frameless native scalar closures, fib(30) 0.007 s vs GNU R 1.12 s; hidden-function-binding fix; ry-jit oracle 143/144). Typed equals untyped on hot loops because the profile proves what types prove. Steered: typed wins before a profile exists, so compile strict typed functions at first call or at load from signatures (no warm-up, no guards) and measure cold-code cost on finbench/dsbench plus first-call latency on why-ry rows. Coordinates with the strict session. Self-merges.
- data.table and base kernels (session_0111kWqCxpbzWznPZjGyyBYk, Opus): #178, #186 and #202 (b69134e: every data.table op in ops.R at or below GNU R at 1e6 and 1e7 with matching checksums, e.g. fread 3.16 vs 2.36 s, tstrsplit 5.73 vs 3.71 s; heap write-barrier caching, a 64 MB large-vector young budget, a gradual major limit; 1e8 doesn't fit in 15 GB) merged. Now: a base-kernel benchmark (sort, paste, regex, split, tapply, merge, match, rowSums, rle, table, xts/zoo rolling) with every kernel at or below GNU R, and peak memory against GNU R's. Self-merges.
- Tier-0 interpreter speed (session_0111kWqEYviXMfUZx1qCaLE1, Opus): #137, #170, #190, #203 merged. Finbench release build, JIT off: 1.13x GNU R (295 s vs 261 s). JIT on, full 300k-line benches, all 100% matching: finbench 1.30x (wall 0.97x), dsbench 1.35x (wall 0.78x), biobench 1.21x (wall 0.75x). JIT on is ~15% slower than JIT off on finbench (cold-code cost, JIT session owns it). Owns tools/bench/generate.py (fix 8 dsbench expectations that fail under GNU R too), bench-why, and the backlog 'Per-test time' levers (function lookups per call, cells/promises, mk_char callers). #212 (bench-why rerun; x87 inline-asm long-double sums fix #192's 3.5x vector_arith regression; vasicek 0.9x is the one row slower than GNU R) and #216 (cov/var/sd on x87; sd 1.7x GNU R instructions) merged. #217 merged (per-frame symbol filter, per-call lookup-cache pointers; sd(x) 115k vs GNU R 72k instructions). Compiled promise code on branch compiled-promises (bytecode in the promise's attribute slot, expression kept in the code slot), being validated, then PR. why-ry rerun waits on #205.
- Analysis performance round 9 (session_0111kWqEhvxZmGPocu7FZkAe, Opus): #172, #189, #200 and #209 (559f94e8: `ry check` findings cached in `.ry_cache/check.json` with per-file surface hashes and project-fold shares; unchanged rerun 0.10 s / 57 MB on finance, body edit ~0.45 s; cold 1.45 s, already ~3.4 of 4 cores busy) merged. #224 merged (callgrind -11%, but cold wall still ~1.5 s, under-1 s target not met; LSP replays .ry_cache/check.json as the first diagnostics wave, ~0.2 s). Now: remove the checker's per-call allocations (per-check bump arena, shared schemes) and cheapen parse/rowan/HIR, target cold check under 1 s. Self-merges.
- Analysis false reports (session_0111kWqAtf1jzUW6T3FHvaDo, Opus): sole owner of false reports, including those caused by stubs. #141, #146, #176, #182, #197 and #221 (ba27962: corpus 118 to 98, under-100 target met; ~20 of the 98 are real package bugs listed in backlog.md; four gaps stated in type-system.mdx and pinned) merged. Now: zero false reports on finbench/dsbench/biobench with typing on, corpus false reports under 40, and the cheap documented gaps (top-level `<<-` read-back first). Self-merges.
- Editor tooling (session_0111kWqC2nSdtiQTHBsJihsP, Opus): #194, #195, #206 (4853dbf: `ry debug-adapter` DAP server with breakpoints in package tests, code lenses, VS Code send-to-console and F5 debugging) merged. #219 merged (console attach, paged data viewer, live-value hover/completion, R Markdown/Quarto; socket start-up ~1.4 ms). Now on feat/after-console-attach: CRLF chunk finding, browser() into the attachable debugger, viewer sort/filter pushed to the table engine, bounded event queue, then the largest rust-analyzer-bar gap. Self-merges.
- DataFrame (session_0111kWqESwZBLCytfJFF7GT3, Opus): #193 merged (55a85ab: parallel factor build; db-benchmark on 4 cores, ry wins every question: 1e7 all 19 questions data.table 25.9 s vs ry 6.3 s, 5e7 group-by 29.8 s vs 11.8 s; landing page says 4x; df$x reads back T[]; positioning items done). 1e8 does not fit in 15 GB. Now: peak memory (1e8 in 15 GB), why ry-from-Rust is slower than ry-from-R, typed columns in editor/checker, data.table migration guide and lint. Self-merges.
- Package installation on ry (session_0111kWqBd1i4XC24Kx7uFf5y, Opus): #147, #173, #188 and #226 (3c7c0b3: perl = TRUE on static PCRE2, finalizer timing, one implementation per fact) merged. 236 of 238 corpus packages install and load. Package tests (`oracle packages --installed --runtime ry`): 128 pass, 126 of GNU R's 164 (76.8%). Ten packages regressed in the merge with main (fs, purrr, tibble, pillar, plyr, cachem, sessioninfo, randtoolbox, PerformanceAnalytics, Matrix; in backlog.md). Now: bisect those, then rlang dots, pkgload getNativeSymbolInfo, testthat snapshots, downlit gzcon. Self-merges.
- Standard library round 6 (session_0111kWqCc5uyUU7uPVY2RzP4, Opus): #156, #168, #196 and #210 (fe91757d: Bool, unchecked Num, bit ops, Str UTF-16/32; lists and closure helpers move into parameters, 6.9 s to 0.12 s; record `$` compiles to Op::Field and JIT Intrinsic::Field, 5M field loop 0.074 s vs 1.83 s over a list) merged. #223 merged (bc6d0843: tuple reads 5M 2.27 s to 0.03 s, tag match 1.29 s to 0.58 s, tag construction 0.69 s to 0.18 s; CallTailUpdate in closure-creating helpers 4.9 s to 0.055 s; Num widening, NaN as `:Undefined`; more Roc builtins). Now: plain-list element speculation, Construct as a JIT op, guarded match, bounded List.range/repeat, then records/tuples/tags adopted in the typed benchmark conversion. Self-merges.
- CLI UX and loader (session_0111kWqEAz7wRnhs8fRLRyrM, Opus): #149, #165, #179, #198 and #213 (7e68a75: cold `ry run` on the 326k-line package 352 ms median with a warm load-index cache, 570 ms cold, vs `ry R` 843 ms; top-level S4 definitions held back as one lazy unit, `options(ry.load.lazy_s4 = FALSE)`; PackageIndex cached under $XDG_CACHE_HOME/ry/load-index/) merged. #220 merged (held-back S4 metadata cached across processes: force + first dispatch ~100 ms on a hit vs GNU R ~150 ms; a miss is still ~350 ms, all tier-0 cost, forwarded to tier-0). #225 merged (S4 methods on other packages' generics held back too; finance start-up ~2 s to ~0.6 s vs GNU R 2.1-2.8 s). Next: lazy R6 generators (~60 ms run phase, needs a purity rule for top-level calls), and measure the editor socket start-up cost once it lands. It owns standardGeneric/S4 paths in dispatch.rs and eval.rs, tier-0 owns S3, .Call, GetFun and lookups. Self-merges.
- Strict mode round 5 (session_0111kWqBB9Ca4CRxyz8Xzcxm, Opus): #166, #185 and #204 (df8a6f6: all three typed benches strict-clean; typed check fin 1.86 s / 644 MiB, ds 1.85 s, bio 1.69 s; typed load 5.4 to 5.9 s first run, 2.9 s repeat via a cached strict verdict, vs 2.7 s untyped; data.frame end to end, `Any`-only ambiguity is `Any`; corpus false reports 385 to 373) merged. Now: matrix `[`, operator unions, lambdas to `...: Any`, polars `$`, Unknown-absorbing annotations, typed first-run load within 10% of untyped, insert-inferred-annotation fix for strict adoption. Must run the corpus false-report count before merging stub changes. Self-merges.
- The workspace builds on a pinned nightly (rust-toolchain.toml). CI picks it up with no workflow edit; the user may optionally replace `dtolnay/rust-toolchain@stable` with `rustup toolchain install`.
- Graphics engine (session_0111kWqAywmCkqy2bYhhfetf, Opus): #187 merged. Told "go" by the orchestrator, citing the user's verbatim delegation: one self-merged PR with postscript() parity (onefile = FALSE, EPS, horizontal), patterns, masks, groups, PostScript alpha, vignettes with plots, graphics engine tests. If it still refuses without the user's own reply, archive it and start a fresh graphics session with the same round.
- Runtime on macOS and Windows, then heap and GC (session_0111kWqEtmpZFGTxsHPxCDVn, Opus): #181, #191, #199, #207 (libR exports GNU R's whole public C API; Windows `R CMD INSTALL` verified cross-hosted only) and #218 (18da816: inline C API accessors, CAR/CDR walk 14x faster and STRING_ELT 12x faster than GNU R; sliding 1024-handout handle window, 5e6 temporaries 0.44 s / 150 MB vs GNU R 2.5 s / 114 MB; foreign-call contexts give GNU R's `Calls:` traceback; ry load sources R/unix and lazy data/) merged. Running GNU R's Windows DLLs or MSYS2 under Wine was refused; do not work around it. Nothing has run on real Windows yet (release smoke test, user-only). Now: owns crates/heap; GC and allocation throughput (per-type pools, generational nursery, cheap roots so RY_GC_STRESS=startup works, full gc-torture), coordinating allocation call sites with tier-0. Self-merges.
- Formatter and linter, then help system (session_0111kWqE4smBZ1zfYHMuBf45, Opus): #215, #222, #228 merged (range and on-type formatting; 30 lintr ports). User directive: lintr is not a priority; add a lint only if it fits the architecture cheaply, otherwise cut it. lintr parity stopped. Now: R's help system on the runtime (help/?, Rd to text and HTML, dynamic help server). Self-merges.
- Persistent merge agent: an in-session subagent that squash-merges cloud PRs on request.

## Next

Nothing queued: every build-order step is in flight.

Follow-ups:
- Try ry and beta (#162) archived after its R 4.6 recheck: the `just package` archive passes tools/smoke-test.sh with R 4.6.0, the R 4.5 pin is gone from `.github/pending-release.yml`, and `nix build .#ry` passes its full check phase with GNU R (01a5129, aed77d6). What remains is user-only: move the workflow into `.github/workflows/`, VSCE_PAT/OVSX_PAT, the Open VSX namespace, the Zed submission, and confirming 0.4.0-beta, the glibc 2.35 floor, Windows without the runtime and the docs deploy.
- Typed arrays (#177) archived. Array benches with the JIT's Array guard, release build: ry is 4.8x to 9.0x faster than GNU R (array_dot 0.188 s vs 0.021 s). Its List.sum compensated-sum fix landed on main as 409029bd. Typed vs untyped speed is the JIT session's.
- Positioning (landing PRs through #171) archived. Its pending items move: the why-ry benchmark tab to the tier-0 session (`just bench-why`) once the JIT lands; `[]` in DataFrame snippets and the DataFrame speed claim to the DataFrame session.
- R 4.6 base support #180 (0ca783a) merged and its session archived: GNU R 4.3+ supported, 4.4 to 4.6 changes gated by `r_at_least`, oracle 124/125 on 4.3.3 and 118/125 on 4.6.1. Running R 4.4 and 4.5 was refused by the session's permission check (downloading Posit's binaries); do not work around it. The user may allow it, or add those versions to the proposed CI matrix (`R_HOME=<home> cargo test -p interp`, `ORACLE_R=<home>/bin/R cargo run -q -p oracle -- compare --engine ry`).
- Robustness #154 (39ce6b4) merged and its session archived: LSP simulator, docs-as-tests, 15 cargo-fuzz targets, `oracle fuzz`, GC torture, 15 bugs fixed. Its three main-side failures are fixed on main (0cf283da, b373ea9f, 600f511e).

