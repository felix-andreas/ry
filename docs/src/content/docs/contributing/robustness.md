---
title: Robustness
description: How every part of ry is built to be fuzzed, simulated and scripted, and what each layer's harness checks
---

ry's long-run priority is robustness. A compiler, a language server and a runtime that most of the
time give the right answer are not good enough, because the rare wrong answer is the one a user
cannot diagnose. So every component is designed so that a machine can drive it with generated input,
check it against an independent answer, and reproduce any failure from one number. This page is the
contract for that design: the rules every component follows, and what each layer's harness checks
today. The fixture contract and the individual fuzz batteries are documented in
[Testing](/contributing/testing/).

The approach borrows from deterministic simulation testing as FoundationDB, TigerBeetle and
Antithesis practice it, and from coverage-guided and differential fuzzing. Three kinds of harness
work together:

- **Simulation** runs a whole component (today, the language server) in one thread under a seeded
  scheduler, with every source of nondeterminism behind a seam the seed drives, and checks it against
  a component started from scratch.
- **Fuzzing** feeds a stage generated or mutated input and checks properties that must hold for every
  input. Seeded batteries run inside `cargo test`; the same batteries back the coverage-guided
  `cargo fuzz` targets. Differential fuzzing compares two implementations of the same semantics,
  such as ry's runtime against GNU R.
- **Scenarios** script a realistic user journey (open a package, type, rename, run `ry check`, install
  a package) in a small readable format, so a new journey costs a few lines.

## The rules

**Every source of nondeterminism sits behind a seam.** Time, randomness, IO, the file system,
threads, scheduling and allocation must be injectable, so a harness can drive them from a seed. A
component that reads the clock, spawns a thread or iterates a hash map in a way the output can
observe is a bug, even if it never misbehaves in production, because it makes failures
unreproducible. Ordered collections (`BTreeMap`, `BTreeSet`) are the default wherever iteration order
reaches the output.

**Every failure reproduces from its seed.** Harnesses draw all randomness from
`syntax::testing::SplitMix64` (or `ByteStream`, which reads a fuzzer's bytes through the same
`Entropy` trait), print the seed and a replay command when they fail, and never consult the
wall clock or the OS's randomness. Running the same seed twice must produce the same transcript, and
the simulator pins that property with a test.

**Invariants are always on in simulation.** Cross-checks that are too expensive for production run
whenever a harness asks for them, and panic at the step that broke them with the inputs that
reproduce it. `syntax::testing::enable_invariants()` switches them on for the process. Today it makes
every splice reparse also parse from scratch and compare the two trees. A new incremental shortcut
gets such a cross-check the day it lands.

**The oracle is a from-scratch run.** Incremental results are checked against the same computation
done from nothing: a fresh salsa database, a fresh language server on the same disk and buffers, a
full parse. Runtime results are checked against GNU R and against ry's own tier-0 interpreter.

**A bounded pass of every harness runs in the default test suite.** `FUZZ_ITERS` scales every
budget, so `cargo test` stays fast while a long run is one environment variable away. Long runs have
`just` recipes.

**A failure becomes a regression case.** A bug a harness finds is fixed together with a pinned
input: a `fuzz_regressions_hold_invariants` entry, a splice regression, a fixture, or a scenario.
A bug that is not small and local is filed in `.agents/memory/backlog.md` with its reproducing seed.

## The language-server simulator

`crates/ry/src/server/simulation.rs` runs the real server, the same `ServerState` and `Worker` code
that serves an editor, without its worker thread or socket. Two seams in `server.rs` make that
possible:

- **The outbox.** Everything the worker sends the client (diagnostics, messages, capability
  registrations, refresh requests) goes through an `Outbox`. In production it is the socket; in a
  simulation it records the messages in order.
- **The job step.** `run_job` is one step of the worker loop: initialize, refresh cancellation, then
  run one queued job or one unit of idle work. The production loop calls it on its thread; the
  simulator calls it whenever the seed says the worker gets to run, so the seed decides how
  notifications, requests and idle work interleave.

Cancellation is injected the way a real edit causes it. `cancel_at_checkpoint` makes the worker's
cancellation token flip at the Nth cancellable checkpoint of the current job, and the simulator
always follows the flip with a real notification, because the server relies on a newer edit being
queued whenever a query is cancelled.

`run_session(entropy, root, actions)` plays a random editor session against a package workspace:

- edits, including several ranged changes in one notification, typing bursts, full-text
  replacements, emoji and CJK text, and UTF-8 or UTF-16 position encodings;
- opening, closing and saving files, untitled buffers, and an R Markdown document whose positions
  count on the document's own text, not on the R script its chunks make;
- disk changes behind the editor's back to R files, `DESCRIPTION`, `NAMESPACE`, `ry.toml`, data files
  and test files, with watcher events delayed, duplicated, and delivered in shuffled batches;
- requests of every kind at valid and invalid positions, abandoned requests, and requests that race
  edits;
- protocol violations, such as changing, saving or closing a document that was never opened;
- push and pull diagnostics clients.

At random checkpoints the session lets everything settle and checks the server against a fresh one
started on the same disk with the same open buffers:

- every outstanding request got a response;
- the server's text for each open document is the client's;
- every analysis input (files, kinds, project order, configuration, package metadata, data-file
  layouts) equals the fresh server's;
- diagnostics and random requests at random positions answer exactly as the fresh server does;
- every range in every response lies inside the document, in the negotiated encoding;
- the push contract holds: the last push for an open document is the settled wave, it contains the
  first wave when nothing else changed in between, and it equals a pull.

The bounded pass is `cargo test -p ry-lang --test test_simulation`. `just simulate 1000 120` runs a
thousand seeds of 120 actions, and `SIMULATION_SEED=<seed>` replays one, printing the session's last
actions on failure.

The workspace is a real directory that only the simulator writes. An in-memory file system with
injected IO errors is not built yet, so read failures and partial writes are not simulated.

## Scenario tests

Each `crates/ry/tests/scenarios/*.scenario` file is one journey. Editor steps drive the language
server in-process through the simulator and settle after every step; command-line steps run the real
`ry` binary in the scenario's workspace. When a journey ends, its editor session is checked against a
fresh server, exactly as a simulation checkpoint is. A scenario reads like this:

```text
# Renaming from a call site rewrites the definition in a file that is not open.
file R/area.R
| area <- function(width, height) width * height
file R/report.R
| report <- function() area(2, 4)

open R/report.R
rename R/report.R "area" surface
expect buffer R/area.R "surface <- function"

run ry check
expect exit 0
```

One command sits on each line, `#` starts a comment, and `|` lines form the body of the command
above them. Quoted words may hold spaces and the escapes `\n`, `\t`, `\"` and `\\`.

| Command | Meaning |
| --- | --- |
| `requires gnu-r` / `requires ry-runtime` / `requires r-package:NAME` | Skip the scenario unless GNU R is installed, `libR.so` is built beside `ry` too, or GNU R has the package installed. |
| `client pull` / `client utf8` | Start the editor as a pull-diagnostics or UTF-8 client. Must come before the first editor step. |
| `file PATH` + body | Write a file; a running editor sees a watcher event. |
| `remove PATH` | Delete a file, with a watcher event. |
| `open`, `close`, `save PATH` | Buffer lifecycle. |
| `type PATH after "A" "T"` / `type PATH at end "T"` | Type text one character per change, without settling in between. |
| `replace PATH "old" "new"` / `erase PATH "text"` | One ranged change at the first occurrence. |
| `rename PATH "anchor" NEW` | Rename the symbol at the anchor and apply the edit, opening files it touches. |
| `format PATH` | Format the buffer and apply the edits. |
| `expect diagnostics PATH none` / `expect diagnostics PATH "needle"` | No diagnostics, or one whose code or message contains the needle. |
| `expect no diagnostic PATH "needle"` | No diagnostic contains the needle. |
| `expect hover PATH "anchor" "needle"` | The hover at the anchor contains the needle. |
| `expect definition PATH "anchor" TARGET "text"` | Goto-definition lands at the text in the target file. |
| `expect completion PATH after "anchor" LABEL` | Completion after the anchor offers the label. |
| `expect buffer PATH [lacks] "needle"` / `expect file PATH "needle"` | Buffer or disk content. |
| `expect signature PATH after "anchor" "needle"` | Signature help after the anchor contains the needle, with the active parameter in brackets. |
| `expect symbol PATH "a/b/c"` | The outline has that path of nested symbols. |
| `expect workspace-symbol QUERY PATH NAME` | The workspace symbol query finds the name in the file. |
| `expect fold PATH "from" "to"` | A fold spans the lines of the first occurrences of both needles. |
| `expect selection PATH "anchor" "text"` | Expanding the selection from the anchor reaches exactly the text. |
| `expect lens PATH "anchor" "title"` | A code lens on the anchor's line has that title, after resolving it. |
| `expect token PATH "anchor" CLASS` | The semantic token at the anchor has that type, with modifiers as `+name` (`function+defaultLibrary`). |
| `fix PATH "anchor" TITLE` | Apply the quick fix with that title at the anchor. |
| `debug start PATH [entry] [prelude "R"] [test "name"] [break N \| break FILE:N]...` | Launch `ry debug-adapter` on the script, with breakpoints in it or in another file. |
| `debug continue` / `next` / `step-in` / `step-out` / `disconnect` / `evaluate "expr"` | Drive the debug session. |
| `expect stopped REASON at N` / `expect stack "f@2 g@7"` / `expect variable NAME "value"` | The next stop, its frames, and a variable of the top frame. |
| `expect evaluated "value"` / `expect debug-output "needle"` / `expect exited N` | The last evaluation, the program's output so far, and its exit status. |
| `run ry ARGS...` | Run the `ry` binary in the workspace. |
| `install DIR` | Install a source package through ry's `R CMD INSTALL` into `library/`. |
| `expect exit N` / `expect stdout\|stderr [lacks] "needle"` | Check the last `run` or `install`. |

`cargo test -p ry-lang --test test_scenarios` runs them all, and `just scenarios <name>` runs one.

## Each layer

The table lists what drives each layer and what checks its answers. A layer marked as a gap has no
harness of that kind yet; the gap is filed in the backlog.

| Layer | Driven by | Checked against |
| --- | --- | --- |
| Parser | generated programs, token soup, byte mutations, edit streams (`syntax` `test_fuzz`); `cargo fuzz` `parse` | lossless round trip, tree invariants, splice reparse equals full parse |
| Lowering, naming, inference, diagnostics | generated programs and projects (`semantics` `test_fuzz`); `cargo fuzz` `semantics` | no panic, determinism, well-formed ranges, incremental equals fresh |
| Incrementality and cancellation | the simulator's edits and injected cancellations; `test_incremental` | a fresh server; per-item firewalls |
| Formatter | seeds, mutations, fixture sources (`format` `test_fuzz`); `cargo fuzz` `format` | preservation, idempotence, determinism |
| IDE features | generated documents and positions (`ide` `test_fuzz`); `cargo fuzz` `ide` | no panic, ranges inside the document |
| Language server | the simulator; scenarios; `cargo fuzz` `lsp` | a fresh server, the push contract, range geometry |
| Loader | random reload sequences (`loader` `test_reload_fuzz`); `cargo fuzz` `reload`, `load_index` | a fresh load |
| Debugger | scripted sessions and a seeded client that sends random requests at every stop (`interp` `test_debugger`); scenarios | transcripts; every request answered once, the session ends, the interpreter left clean |
| Interpreter | generated programs (`interp` `test_fuzz`); differential fuzzing; `cargo fuzz` `interp_lower`, `bytecode` | GNU R, tier 0 |
| Garbage collector | `RY_GC_STRESS` over the oracle cases and generated programs; `RY_GC_VERIFY` | the unstressed run, the write-barrier invariant |
| C API | native fuzzing (`libr` `test_fuzz_native`); `cargo fuzz` `unserialize` | GNU R's behavior; the PROTECT balance check on every `.Call` |
| JIT | `test_tiers`; differential fuzzing with `RY_JIT=force` | tier 0, GNU R |
| Table engine | `table` property tests; `cargo fuzz` `csv` | the eager implementation; the sniffer's promised columns |
| Package install | the `install-package` scenario; `test_install` | the package's own results |
| CLI | scenarios run the real binary | expected output and exit status |
| Docs examples | every marked output block in the docs, and the landing page samples (`test_docs`) | the output the page shows |

The PROTECT balance check runs on every `.Call` and `.C` in every build: when a routine returns with a
different protect-stack depth than it entered with, ry warns `stack imbalance in '<routine>'`, as GNU R
does. A stricter checker that catches an unprotected object at the moment it is collected is
`RY_GC_STRESS`, which collects before every allocation.

## Running the long harnesses

| Recipe | What it runs |
| --- | --- |
| `just simulate COUNT ACTIONS BASE` | The language-server simulator over `COUNT` seeds from `BASE`. |
| `just scenarios [NAME]` | Every scenario, or one. |
| `just fuzz-differential SEED COUNT ENGINES` | Generated programs under GNU R and ry's engines. |
| `just gc-torture` | Every oracle case under collection stress. |
| `just fuzz-deep` | The semantic pipeline's long seeded run. |
| `just fuzz-run TARGET` | One `cargo fuzz` target under libFuzzer. |
| `just fuzz-all SECONDS` | Every `cargo fuzz` target for `SECONDS` each, after seeding the corpora. |
| `just docs-test [PAGE]` / `just docs-bless [PAGE]` | The docs examples, or a rewrite of their exact output from the real tool. |

## Gaps

- The file system seam for the language server is a real directory, not an in-memory file system, so
  IO errors are not injected.
- Allocation failure is not injectable; the heap aborts on an out-of-memory condition.
- The loader, the REPL and the runtime are not driven by a seeded scheduler; their nondeterminism
  (GNU R's own, threads in packages) is controlled only by running them single-threaded.
- The simulator interrupts a step only with a notification, so a request preempting idle work is not
  simulated.
