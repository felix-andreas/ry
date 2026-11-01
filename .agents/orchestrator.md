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

- Base R round 4 (session_0111kWqCeajgVo7m8XVxpfDk, Opus): #134 merged. #192 (GNU tests/*.R 63/66 exit status and 26 exact; the rest are pf last-bit, env.profile hash counts, and reg-large's memory needs; url()/curlDownload/curlGetHeaders on reqwest; oracle 134/136) in review, self-merging. Next: .Internal(curlVersion) and serverSocket, install-on-ry semantics with the corpus session.
- Corpus packages round 4 (session_0111kWqDJJsagmnUNcuZLJp4, Opus): #167 (97/146 corpus packages, 0ef27e93) merged. Now on runtime-packages-round4: the .bincode integer bug (cut(1:5, breaks) all NA, which empties parallel::splitIndices and lgr's future_lapply), mc_fork, sink(type="message"), flushed file connections, then the ALTREP gaps from #188's package-test failures. Self-merges.
- JIT round 2 (session_0111kWqBQdpLjmrdiA5ajbhR, Opus): direct calls with promise elision, builtin inlining, the closures regression, the why-ry benchmark. PR #128 (why-ry Speed table: drawdown 13x, loop_sum 6.3x, fib 0.9x) merged as 6bfb590. Now round 3: typed code faster than untyped (static types as profiles, stricter arrays that convert to R structures), shown as the third benchmark column. The JIT (PR #117) merged as 3401282.
- data.table speed (session_0111kWqCxpbzWznPZjGyyBYk, Opus): #178 and #186 (306b311) merged. At 1e6 rows ry vs GNU R: by two keys 0.007 vs 0.011 s, dcast(sum) 0.018 vs 0.027 s, keyby at parity; checksums match at 1e6 and 1e7. Now: CJ (0.013 vs 0.007 s), dcast warm-up, whole-column picks (0.010 vs 0.006 s), then the full ops.R matrix at 1e7 and 1e8. Self-merges.
- Tier-0 interpreter speed (session_0111kWqEYviXMfUZx1qCaLE1, Opus): #137, #170, #190 (bitmap-page heap) merged. Finbench on main, release build, `oracle bench`: JIT on 390 s vs GNU R 289 s (1.35x, the canonical number); JIT off 352 s vs 274 s (1.29x), and ry's process wall is faster. The strict session's 1.93x was a stale branch. why-ry Speed table refreshed (699a7b1). It also owns the benchmark tab. Now: profile and close the interpreter gap toward per-test time at or below GNU R. The closures regression (CallRebinding not inlined) and slow typed Array `[[` on main went to the JIT session.
- Analysis performance round 7 (session_0111kWqEhvxZmGPocu7FZkAe, Opus): #172 and #189 (compact HIR, NonZeroU32 ids, index-hashed Symbol; cold check 1.52-1.58 s, peak about 535 MiB; 167f17d) merged. Now: #166's top-level classifier and cheaper tree eviction toward under 450 MB, then the per-item type store toward a cold check under 1 s. Self-merges.
- Analysis false reports (session_0111kWqAtf1jzUW6T3FHvaDo, Opus): sole owner of false reports. #141, #146, #176 and #182 (rlang type predicates count as type probes; 68aecc1) merged; corpus at 233. The jump to 468 came from #174 exposing a hidden inference bug, not from #169. Now: the remaining 233 by class, aiming under 100. Self-merges.
- Editor tooling (session_0111kWqC2nSdtiQTHBsJihsP, Opus): #175, #184 and #194 (80ce3d4) merged. #194: real keystroke latency is 1.5 to 2.8 ms for a body edit and 4 to 6 ms for a new top-level name (the old 60 to 74 ms came from a broken probe); quick fixes as `Diagnostic.fix`, outline sections, folding, selection ranges, richer semantic tokens; Symbol hash back to a stored FxHash of the spelling for determinism. Now: the "Open: editor parity" backlog (`ry check --fix`, more fixes, defaultLibrary, sections inside function bodies, cached code-action diagnostics), then rust-analyzer feature parity. Self-merges.
- DataFrame round 4 (session_0111kWqESwZBLCytfJFF7GT3, Opus): #158 and #183 (bitset group flags, one-pass strings_as_factors, CSV read 9 s vs fread 31 s, mimalloc default kept, `[...]` columns in DataFrame docs; b6af533) merged. Now: factor representation (remove FrozenCategories::new's 6 s), join queries and 1e8 groupby against data.table, typed column reads. Self-merges.
- Package installation on ry (session_0111kWqBd1i4XC24Kx7uFf5y, Opus): #147, #173 and #188 merged: 236 of 238 corpus packages install and load. Package tests: ry passes 97 of 192 (93 where GNU R passes, baseline now 164). On feat/package-pass-rate: PCRE2 for perl=TRUE, R6 finalizers (weak inline caches, weak-ref fixpoint), getParseData in GNU R's shape (to reconcile with #192's parse_data.rs), smaller fixes. Next: testthat and cli failures, tryCatchLog, single-package gaps. Self-merges.
- Standard library round 4 (session_0111kWqCc5uyUU7uPVY2RzP4, Opus): #156 and #168 (nested in-place updates 3.75 s to 0.028 s, compiled-frame register check fixing an aliasing bug in plain R, wrong-type key errors, GNU R message) merged, #168 as 596a8c4. Now: stdlib call overhead under 0.1 µs via direct builtin entries (with the tier-0 and JIT sessions), then sound move-into-parameter via escape analysis. Self-merges.
- CLI UX (session_0111kWqEAz7wRnhs8fRLRyrM, Opus): #149, #165 and #179 (typeahead kept; cold `ry run` on a 326k-line package 2.9 s to 1.0 s vs `ry R` 1.2 s, by raising JIT thresholds to 1000 entries and lazy mimalloc commit; d146c11) merged. Now: warm-reload latency, typeahead for fd-0 readers and Ctrl-C in the drain window, cold start below 1.0 s. Self-merges.
- Strict mode round 4 (session_0111kWqBB9Ca4CRxyz8Xzcxm, Opus): #166 and #185 (full finbench typed and strict-clean, 5,841 problems to 0; check 3.0 s / 788 MiB typed vs 1.8 s / 494 MiB untyped; load 6.7 s vs 3.2 s; tests 471 s typed vs 474 s untyped vs GNU R 245 s; 7d55460) merged. Now: strict friction (owns data.frame as a named list), typed check and load cost, typed data science and biostatistics benches. Self-merges.
- The workspace builds on a pinned nightly (rust-toolchain.toml). CI picks it up with no workflow edit; the user may optionally replace `dtolnay/rust-toolchain@stable` with `rustup toolchain install`. The flake.lock rust-overlay bump is unverified under `nix build` (the try/beta session checks it).
- Graphics engine (session_0111kWqAywmCkqy2bYhhfetf, Opus): #187 merged. Paused: it waits for the user's own confirmation before starting a new round (patterns, masks, groups, PostScript alpha, vignettes with plots, graphics engine tests). The postscript() parity fix moved to the base R session's #192.
- Try ry and beta release (session_0111kWqBeVFPbHpyqC8bDyWu, Opus): `just try` end-to-end demo, working nix builds on the pinned nightly, beta release readiness. #162 (9e0b198) merged: version 0.4.0-beta (untagged), nix build/develop/flake check pass, release workflow staged at `.github/pending-release.yml`. User-only actions relayed to the user (move the workflow, VSCE_PAT/OVSX_PAT, Open VSX namespace, Zed submission, glibc 2.35 floor, Windows without runtime, docs deploy). Blocked on the R 4.6 runtime fix for the smoke test; reruns it once that lands. Self-merges.
- Runtime on macOS and Windows (session_0111kWqEtmpZFGTxsHPxCDVn, Opus): #181 (macOS) and #191 (59eb037, Windows: R.dll and ry.exe for windows-gnu through a platform seam, linked with lld because GNU ld breaks SECREL32 TLS; `ry run` correct under Wine; @loader_path gap fixed) merged. Now: gnullvm/UCRT toolchain for both binaries, Rblas and Rtools package DLLs under Wine, the Windows build in `.github/pending-release.yml`, two minor review findings. Self-merges.
- Persistent merge agent: an in-session subagent that squash-merges cloud PRs on request.

## Next

Nothing queued: every build-order step is in flight.

Follow-ups:
- Typed arrays (#177) archived. Array benches with the JIT's Array guard, release build: ry is 4.8x to 9.0x faster than GNU R (array_dot 0.188 s vs 0.021 s). List.sum now uses the compensated sum (fix/list-sum-compensated, merging into jit-typed). The JIT session owns that merge and the 5 to 15% gap by which typed Array code trails untyped.
- Positioning (landing PRs through #171) archived. Its pending items move: the why-ry benchmark tab to the tier-0 session (`just bench-why`) once the JIT lands; `[]` in DataFrame snippets and the DataFrame speed claim to the DataFrame session.
- R 4.6 base support #180 (0ca783a) merged and its session archived: GNU R 4.3+ supported, 4.4 to 4.6 changes gated by `r_at_least`, oracle 124/125 on 4.3.3 and 118/125 on 4.6.1. Running R 4.4 and 4.5 was refused by the session's permission check (downloading Posit's binaries); do not work around it. The user may allow it, or add those versions to the proposed CI matrix (`R_HOME=<home> cargo test -p interp`, `ORACLE_R=<home>/bin/R cargo run -q -p oracle -- compare --engine ry`).
- Robustness #154 (39ce6b4) merged and its session archived: LSP simulator, docs-as-tests, 15 cargo-fuzz targets, `oracle fuzz`, GC torture, 15 bugs fixed. Its three main-side failures are fixed on main (0cf283da, b373ea9f, 600f511e).

