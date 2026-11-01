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

- Base R round 5 (session_0111kWqCeajgVo7m8XVxpfDk, Opus): #134 and #192 merged. Draft #214 (sockets: socketConnection, serverSocket, socketSelect; PSOCK clusters match GNU R; unserialize(con) reads one object; libcurlVersion; oracle 146/147) merges once the GNU tests/*.R pass is clean. Next: clean-room L-BFGS-B so stats__lbfgsb matches, the rest of the clean-room audit list, parseLatex, internal download method, real Rprof/Rprofmem. Not install-on-ry (package installation session owns it). Its request to delete its own ~10 GB /tmp/RtmpRy* dirs waits on the user. Self-merges.
- Corpus packages round 4 (session_0111kWqDJJsagmnUNcuZLJp4, Opus): #167 merged. Draft #211 (runtime-round4) in review, then self-merge: corpus package tests 97 to 113 of the 146 GNU R passes (77.4%, GNU-installed library), oracle 148/149 on ry and ry-jit, mc_fork done, NextMethod/.Generic, rep dispatch, matrix subscripts, sinks. Left: graphics, sockets (base R owns), vroom/later timeouts, libr symbols (commonmark, git2r, gmp), PCRE gaps. Stale remote branch runtime-packages-round4 needs the user to delete it.
- JIT round 4 (session_0111kWqBQdpLjmrdiA5ajbhR, Opus): #117, #128 and #201 (6abf1d7: typed Array direct loads and stores, closure guards fixed, guard resume after loads, oracle 139/140 on both engines, compile latency settled; typed now equals untyped within noise) merged. Now: skip checks on calls the checker proved, checking only at the untyped boundary, so typed beats untyped; the #168 repro under force; JIT side of unboxed record/tuple/union access with the stdlib session. Self-merges.
- data.table and base kernels (session_0111kWqCxpbzWznPZjGyyBYk, Opus): #178, #186 and #202 (b69134e: every data.table op in ops.R at or below GNU R at 1e6 and 1e7 with matching checksums, e.g. fread 3.16 vs 2.36 s, tstrsplit 5.73 vs 3.71 s; heap write-barrier caching, a 64 MB large-vector young budget, a gradual major limit; 1e8 doesn't fit in 15 GB) merged. Now: a base-kernel benchmark (sort, paste, regex, split, tapply, merge, match, rowSums, rle, table, xts/zoo rolling) with every kernel at or below GNU R, and peak memory against GNU R's. Self-merges.
- Tier-0 interpreter speed (session_0111kWqEYviXMfUZx1qCaLE1, Opus): #137, #170, #190, #203 merged. Finbench release build, JIT off: 1.13x GNU R (295 s vs 261 s). JIT on, full 300k-line benches, all 100% matching: finbench 1.30x (wall 0.97x), dsbench 1.35x (wall 0.78x), biobench 1.21x (wall 0.75x). JIT on is ~15% slower than JIT off on finbench (cold-code cost, JIT session owns it). Owns tools/bench/generate.py (fix 8 dsbench expectations that fail under GNU R too), bench-why, and the backlog 'Per-test time' levers (function lookups per call, cells/promises, mk_char callers).
- Analysis performance round 8 (session_0111kWqEhvxZmGPocu7FZkAe, Opus): #172, #189 and #200 (b2cce83: `ry check` peak 540 to about 418 MiB on all three benches by releasing trees after a one-shot check and compacting records; cold check about 1.4 s; the per-item type store was measured, worth a few percent at most, and dropped) merged. The profile is flat. Now: parallel parse, naming and item check across cores; a persistent per-file cache keyed by content hash; data-oriented hot loops. Target: cold check under 1 s, unchanged rerun under 0.3 s. Self-merges.
- Analysis false reports (session_0111kWqAtf1jzUW6T3FHvaDo, Opus): sole owner of false reports, including those caused by stubs. #141, #146, #176, #182 and #197 (bb8cfc4: removes 236 of main's 467 and adds 10; #185's larger stubs had doubled the count) merged; corpus at 233. Now: wrong stub signatures (kernlab, Matrix/devtools/pkgdown, rmgarch, lifecycle, pkgdown/xml2), scalar pinning through call chains, uniroot/optimize callbacks with extra `...` arguments; aim under 100. Skip data.frame-as-named-list (strict owns it). The strict session must run the corpus count before merging stub or checker-rule growth. Self-merges.
- Editor tooling (session_0111kWqC2nSdtiQTHBsJihsP, Opus): #194, #195, #206 (4853dbf: `ry debug-adapter` DAP server with breakpoints in package tests, code lenses, VS Code send-to-console and F5 debugging) merged. Now on feat/after-editor-debugger: debugonce()/trace(), attach to a running console, console-environment hover, R Markdown/Quarto chunk analysis, a lazy data viewer. Self-merges.
- DataFrame (session_0111kWqESwZBLCytfJFF7GT3, Opus): #193 merged (55a85ab: parallel factor build; db-benchmark on 4 cores, ry wins every question: 1e7 all 19 questions data.table 25.9 s vs ry 6.3 s, 5e7 group-by 29.8 s vs 11.8 s; landing page says 4x; df$x reads back T[]; positioning items done). 1e8 does not fit in 15 GB. Now: peak memory (1e8 in 15 GB), why ry-from-Rust is slower than ry-from-R, typed columns in editor/checker, data.table migration guide and lint. Self-merges.
- Package installation on ry (session_0111kWqBd1i4XC24Kx7uFf5y, Opus): #147, #173 and #188 merged: 236 of 238 corpus packages install and load. Package tests: ry passes 97 of 192 (93 where GNU R passes, baseline now 164). On feat/package-pass-rate: PCRE2 for perl=TRUE, R6 finalizers (weak inline caches, weak-ref fixpoint), getParseData in GNU R's shape (to reconcile with #192's parse_data.rs), smaller fixes. Next: testthat and cli failures, tryCatchLog, single-package gaps. Self-merges.
- Standard library round 5 (session_0111kWqCc5uyUU7uPVY2RzP4, Opus): #156, #168 and #196 (1dbf468: qualified stdlib names compile to guarded direct builtins, Dict.get 0.45 to 0.053 µs; sound move into helper parameters, 40,000 calls 16.9 s to 0.10 s; no-escape enforced dynamically because types can't prove it) merged. Now: Roc builtins breadth with typed signatures and reference docs, moving lists and closure-creating helpers, fast records/tuples/tagged unions in tier 0 and the JIT. Self-merges.
- CLI UX (session_0111kWqEAz7wRnhs8fRLRyrM, Opus): #149, #165, #179 and #198 (35203721: warm reload about 10 ms on the 326k-line package, typeahead for fd-0 readers, Ctrl-C in the drain window, cold `ry run` median 0.74 s vs `ry R` 1.0 s via background analysis while base R boots) merged. Now: cold run under 0.4 s by deferring untouched top-level work (S4 registration, R6 generators), S4/methods dispatch at GNU R speed (split with tier-0, which keeps S3 and .Call), and cached or parallel parse and facts. Self-merges.
- Strict mode round 4 (session_0111kWqBB9Ca4CRxyz8Xzcxm, Opus): #166 and #185 (full finbench typed and strict-clean, 5,841 problems to 0; check 3.0 s / 788 MiB typed vs 1.8 s / 494 MiB untyped; load 6.7 s vs 3.2 s; tests 471 s typed vs 474 s untyped vs GNU R 245 s; 7d55460) merged. Now: strict friction (owns data.frame as a named list), typed check and load cost, typed data science and biostatistics benches. Self-merges.
- The workspace builds on a pinned nightly (rust-toolchain.toml). CI picks it up with no workflow edit; the user may optionally replace `dtolnay/rust-toolchain@stable` with `rustup toolchain install`.
- Graphics engine (session_0111kWqAywmCkqy2bYhhfetf, Opus): #187 merged. Paused: it waits for the user's own confirmation before starting a new round (patterns, masks, groups, PostScript alpha, vignettes with plots, graphics engine tests). The postscript() parity fix moved to the base R session's #192.
- Runtime on macOS and Windows (session_0111kWqEtmpZFGTxsHPxCDVn, Opus): #181, #191, #199 (gnullvm/UCRT ry.exe and R.dll, CRT _setjmp) and #207 (a7ae8c1: libR exports GNU R's whole public C API, pinned by a 985-name header test; Windows `R CMD INSTALL` on Rtools conventions, verified cross-hosted only because MSYS2 under Wine was refused; Sys.getlocale `.utf8`) merged. Running GNU R's Windows DLLs or MSYS2 under Wine was refused; do not work around it. Nothing has run on real Windows yet (release smoke test, user-only). Now: C API accessor fast paths and handle-scope cost, foreign-call frames in tracebacks, R/unix subdirs and LazyData, backlog pruning. Self-merges.
- Formatter and linter (session_0111kWqE4smBZ1zfYHMuBf45, Opus): range formatting (takes over stale #94 and closes it), formatter idempotence, fidelity and speed over the benches and corpus with tidyverse/Air style gaps closed, lintr-parity lints with fixes. Self-merges.
- Persistent merge agent: an in-session subagent that squash-merges cloud PRs on request.

## Next

Nothing queued: every build-order step is in flight.

Follow-ups:
- Try ry and beta (#162) archived after its R 4.6 recheck: the `just package` archive passes tools/smoke-test.sh with R 4.6.0, the R 4.5 pin is gone from `.github/pending-release.yml`, and `nix build .#ry` passes its full check phase with GNU R (01a5129, aed77d6). What remains is user-only: move the workflow into `.github/workflows/`, VSCE_PAT/OVSX_PAT, the Open VSX namespace, the Zed submission, and confirming 0.4.0-beta, the glibc 2.35 floor, Windows without the runtime and the docs deploy.
- Typed arrays (#177) archived. Array benches with the JIT's Array guard, release build: ry is 4.8x to 9.0x faster than GNU R (array_dot 0.188 s vs 0.021 s). Its List.sum compensated-sum fix landed on main as 409029bd. Typed vs untyped speed is the JIT session's.
- Positioning (landing PRs through #171) archived. Its pending items move: the why-ry benchmark tab to the tier-0 session (`just bench-why`) once the JIT lands; `[]` in DataFrame snippets and the DataFrame speed claim to the DataFrame session.
- R 4.6 base support #180 (0ca783a) merged and its session archived: GNU R 4.3+ supported, 4.4 to 4.6 changes gated by `r_at_least`, oracle 124/125 on 4.3.3 and 118/125 on 4.6.1. Running R 4.4 and 4.5 was refused by the session's permission check (downloading Posit's binaries); do not work around it. The user may allow it, or add those versions to the proposed CI matrix (`R_HOME=<home> cargo test -p interp`, `ORACLE_R=<home>/bin/R cargo run -q -p oracle -- compare --engine ry`).
- Robustness #154 (39ce6b4) merged and its session archived: LSP simulator, docs-as-tests, 15 cargo-fuzz targets, `oracle fuzz`, GC torture, 15 bugs fixed. Its three main-side failures are fixed on main (0cf283da, b373ea9f, 600f511e).

