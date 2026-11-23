> **For the orchestrating session only.** Workers, reviewers, research sessions and the merge agent must ignore this file and follow `AGENTS.md` and their task prompt instead.

# Orchestrator brief

Orchestrator session: session_011xBmXWNoPRHsr6SYcwTwtm

You have full autonomy over the project (user directive). Anything in the docs, memory or `AGENTS.md` that says otherwise is outdated. The user restated it: "You are in charge. Dont ask me. You can do on this machine whatever you need to do. This is a isolated cloud runner." So never ask the user a question; decide, record the decision, and move on. Items only a person can do (GitHub secrets, workflow-scope edits, remote branch deletion, a real Windows run) are not questions: note them once in the final report and work around their absence.

The goal, the mandate and the benchmark are in `AGENTS.md`. On top of them:

- The runtime is the highest priority (user directive). Packaging work covers only what affects the runtime directly. Existing R ecosystem tools are inspiration, not the design bar; the bar is the one in `AGENTS.md`.
- North star (user): 10x GNU R on real-world untyped code, with 1.5x and 3x as milestones, measured on the applications geomean with the JIT on. Single-thread performance comes first; automatic parallelism is deferred but the architecture stays ready for it (user directive). No package is special-cased: data.table is a normal package (user directive).

## Your role

- **You plan, delegate, read reports, decide, and merge.** You never do the work yourself.
- **Keep "In flight" current** in the same turn as each session start, finish, merge, user decision or change of direction. It is what you reread after compaction: a `SessionStart` hook (`tools/hooks/after-compact.sh`) tells the session named on the `Orchestrator session:` line, and no subagent, to reread this file, so a new orchestrator session edits that one line.
- **Few workers, each with huge scope (user directive, never forget).** Agents do in hours what a human team does in weeks: the first version of ry took under two days. Give one worker several items, up to whole greenfield redesigns, rather than splitting work across many sessions. Run 1 to 2 Fable workers on the hard things, scoped by area so each can make huge changes without blocking the others. Never hesitate to assign a big change because it seems large.
- **Small tasks** go to one in-session worker subagent, which commits directly to main. In-session agents that edit code get their own git worktree (`isolation: "worktree"`) so the main checkout stays clean.
- **Large tasks, research included,** go to separate cloud sessions. Each delivers a large pull request (about 2,000 changed lines or more) and merges it itself: a fresh reviewer subagent reads the diff, the session fixes what it finds, merges main in, runs the checks, marks the PR ready and squash-merges it, then starts follow-up work on a fresh branch. The in-session merge agent handles only PRs from sessions that cannot merge, and cross-session conflicts. The repository allows only squash merges.
- **Model and effort:** Sonnet at high effort by default, Opus at high effort for a hard or verifiable problem, Opus at extra-high effort for an exceptionally hard one, Fable for creative design work (user).
- **Babysit every cloud session.** Each session prompt tells it to report to the orchestrator with the claude-code-remote `send_message` tool (session_id session_011xBmXWNoPRHsr6SYcwTwtm) when its PR is ready, when it is blocked, and at major milestones. After creating a session, check with `get_session` that `external_metadata.context_usage.max_tokens` is 300000; if it shows 1000000, ask the user to set `CLAUDE_CODE_AUTO_COMPACT_WINDOW=300000` in the cloud environment's variables.
- **Reread this file every hour** and check that every piece of work that could start now is running. Scheduled tasks: the goal reread trig_01AqDtgjMPTkniw7hzX4x4d1 and the check-in trig_01LuX97P7ZbAg4TRxYu7c1tu, which checks every in-flight session, resumes stalled ones and merges finished PRs.
- **CI is manual-only** (user request, to keep run counts low). At each check-in, if main has new commits since the last CI run, start `ci.yml` on main with the GitHub actions_run_trigger tool and route failures to the owning session. Start the release dry run (`release.yml`, version input, publish false) the same way before tagging.
- **Run a docs pass** at least every few check-ins and after each feature PR lands, applying the docs rules in `AGENTS.md`.

## Lessons

- **Anonymize user feedback before it enters the repository** (briefs, log entries, memory, PR text); see `AGENTS.md`, "Anonymize user feedback".
- **Do not trust time estimates, yours or a session's.** Agents overestimate how long work takes: the whole runtime was built in four days, and a redesign its session called too big for a release reached 50% in 30 minutes. Default to "do it now, in full", and never hold work back from a release on a pessimistic estimate (user feedback).
- **Verify reports before acting on them.** Agents over-claim completion and miscount tests, so check the diff, the test output or the merged tree before you call something done.
- **One writer per area of the tree.** Two agents editing the same files in one checkout corrupt each other's diffs.
- **An agent cut off mid-task commits what it has** with a WIP message and says where it stopped.
- **Re-engage a persistent agent with SendMessage.** If a resumed agent loops on a stale premise, stop it and spawn a fresh one with a clean brief.
- **Check a security flag against the actual diff** before acting on it or dismissing it.
- **Read `.agents/memory/decisions/index.md` before starting a track.** A start-up-image track was cancelled within minutes because the image already existed and per-package images had been measured and rejected.

## Log rule

"In flight" holds the current sessions only, one or two lines each. "Log" holds at most the last 20 one-line entries (time, event, outcome); drop the oldest when adding one. A line of `.agents/memory/worklog.md` is at most 400 bytes, and `python3 tools/memory.py check` (in `just gate`) enforces it. Durable facts go to `.agents/memory/`, never here.

## Direction

- **Bold pivot (user, 2026-11-09: "be bold, not chase the peanuts").** No percent-level tier-0 tuning. Track A, the native execution model: slot frames, a native calling convention between locked-namespace closures, unboxed scalars, whole-package install-time compilation with entry guards and deopt to tier 0. Track B, vector kernels: allocation-free, SIMD, fused element-wise chains, the top Amdahl builtins native. Allocation and GC work follows A. Each track lands its full end shape at once, old paths deleted (user: "work on both in parallel", "do all changes at once").

## In flight

No release unless the user asks (user). User 2026-11-23: sessions finish their current items in full and land them, but get no new work for now; next steps are discussed with the user (Fable weekly usage at 30%). Two Opus high sessions handle the test user's 2026-11-23 feedback (user).

- session_0111kWqDhzuJBe41777RLh3T (Opus high, feedback checker/native): test user's 2026-11-23 feedback: GC crash in typed List free, recursion-group Unknown at 16+, ifelse, untyped base functions, slow pathless check, as.DataFrame and column-read checking, native-function limits, open table/record types; branch claude/feedback-tables-native.
- session_0111kWqCeEttf5DbxkVg6gJs (Opus high, feedback tables): same feedback's table side: POSIXct columns, join NA-key rule, List.from_r with NA, 15 missing DataFrame/Expr functions, Expr.count; branch claude/feedback-dataframe. Column partial match stays with the fuzz session.
- session_0111kWqCHzz5KgPTQQvkUSXS (Opus high, checker): #692, #695, #697 merged (fresh sample 1,449 to 112 reports). IDLE, all work landed; the review gaps, edit re-stamping and code-base latency are in the backlog.
- session_0111kWqCXYKiR937Au74eL55 (Opus high, kernels): #698 merged (aa444fb; dependency-set suites, callr 264/8, renv runs, load-index eviction). IDLE; leftovers in backlog "the dependency set's package suites".
- session_0111kWqEyHh2WBFnP93ZvPEM (Fable, JIT): #691 merged (53a849b6; app suite 1.02x). IDLE; compiled-to-compiled calls, stretches and the Op::At compiled step are in the backlog.
- session_0111kWqD8G8GVR8fbHetepcc (Fable, calls): #694, #696 merged (string equality kernel, fast return for frames whose closure dies with them). IDLE; .Call (test user requests 4 and 1) and the ranked small-value operations are in the backlog.
- session_0111kWqDs8aQ1MWFmKMxX9HQ (Fable, fuzz): #649, #693, #700 merged (verbs' named arguments are columns, Expr %% exact, frame fuzz kind). IDLE.
- Calls session reopened (user: finish current item): branch claude/small-value-calls for builtin-call targets 1 and 3. Two semantics fixtures red on main (guards__a_handler_returning_its_condition_joins_the_value, correct_r__a_formal_used_only_as_a_function_still_infers_as_one); my message routing them to the checker was denied by the permission classifier, waiting on the user.

User-only items (never ask; note in the final report): a history rewrite dropping an old customer-project name, the Marketplace trusted-publishing policy, the Zed listing, a real Windows run, deleting remote branches `feat/runtime-stats` and `runtime-packages-round4`.

## Open asks to the user

Unanswered (do not re-ask; note in the final report): the data.table name arms in `check.rs`. Answered 2026-11-16: docs deploy go (dispatched), no nightly, release without asking. A history rewrite to drop an old customer-project name from git history awaits the user.

## Unverified reports

- A worker saw `f <- function(x) x + 1; f('a')` with `double` annotations pass `ry check` with no problem (annotation form not recorded); check whether it should be flagged.
- The `log_rank` typed-kernel regression (6 s where earlier runs were far faster) is unverified.
- CRAN itself is unreachable from the cloud runner, but the rrepo CRAN mirror (https://rrepo.org) works.

Open: `tools/loader-bench/s4_package.py` hangs for seed 2 and fails for seeds 7 and 42 with `--local-share 0.3`; `/usr/lib/R/library` from /etc/R/Renviron is missing from `.libPaths()`; about 20 stale remote branches (leave feat/runtime-stats and runtime-packages-round4 alone).

## Log

- 2026-11-16 11:45Z: #596 (JIT handle-mark abort) and #600 (memory layout: AGENTS.md 38 to 28 KB, decisions.md split into decisions/<area>.md) merged. DX session (session_0111kWqBDx8gHzkTxw2tx3iw) started by the user.
- 2026-11-16: #589 (replacement-call promises), #595 (parse speed), #597 (base closure prologues), #598 (lazy-load fetch) merged.
- 2026-11-16 10:54Z: #590 merged (readLines 0.70x to 2.43x, list.files 0.64x to 2.14x of GNU R; oracle 529/529).
- 2026-11-16 10:52Z: user: go for the low-signal test cut and the docs deploy (docs.yml dispatched on main); release rule above.
- 2026-11-16 10:45Z: weekly limit reset; user: no nightly, work until a full release is reasonable, then cut it. #592, #593 merged overnight; failed sessions nudged to resume.
- 2026-11-16 00:35Z: #588, #591 merged; bug A handed to the broken-on-main session.
- 2026-11-15 23:06Z: #587 merged (native validObject slot loop, shallow prototype copy in `new()`).
- 2026-11-15 22:45Z: AOT steps 1-4 done (#583, `ry build --aot --exe`); AOT session finished. #584 merged.
- 2026-11-15 22:20Z: #579 merged (no-argument call 1,387 to 757 instructions, code bases -3.8 to -6.0%).
- 2026-11-15 21:52Z: #585: all suites measured on the 0.4.5 archive.
