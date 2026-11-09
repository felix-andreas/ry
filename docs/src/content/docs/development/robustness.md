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
[Testing](/development/testing/).

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
the simulator pins that property with a test. A seeded battery loops through
`syntax::testing::for_each_seed`, which runs seeds `0..N` and on a failure prints the exact command, such as
`FUZZ_SEED=14 cargo test -p loader --test test_reload_fuzz a_reload_matches_a_fresh_load -- --exact`.
Three variables move the window. `FUZZ_SEED` alone replays that one seed. `FUZZ_ITERS` sets the
count, starting at `FUZZ_SEED` or 0. `FUZZ_DEPTH` multiplies each loop's own default count instead,
so a test with several loops keeps their proportions when a campaign deepens it.

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
- push and pull diagnostics clients, and sessions whose first actions arrive before the startup
  warm-up has run.

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
`ry` binary in the scenario's workspace. The in-process server trusts the check records that binary
writes, as a real server trusts its own, so a journey can run `ry check` and then open the project.
When a journey ends, its editor session is checked against a
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
| `requires gnu-r` / `requires ry-runtime` / `requires r-package:NAME` | Skip the scenario unless GNU R is installed, `libR.so` is built beside `ry` and ry's base R is built (`just base-r`), or GNU R has the package installed. |
| `hide gnu-r` | Run every later `ry` with no GNU R: `PATH` loses the directories that hold `R` or `Rscript`, and `R_HOME` is unset. |
| `client pull` / `client utf8` / `client cold` | Start the editor as a pull-diagnostics or UTF-8 client, or one whose first steps arrive before the startup warm-up has run. Must come before the first editor step. |
| `file PATH` + body | Write a file; a running editor sees a watcher event. `$ROOT` in the body is the scenario's workspace. |
| `env NAME VALUE` | Set a variable for every later `run`; `$ROOT` in the value is the scenario's workspace. |
| `remove PATH` | Delete a file, with a watcher event. |
| `open`, `close`, `save PATH` | Buffer lifecycle. |
| `type PATH after "A" "T"` / `type PATH at end "T"` | Type text one character per change, without settling in between. |
| `replace PATH "old" "new"` / `erase PATH "text"` | One ranged change at the first occurrence. |
| `rename PATH "anchor" NEW` | Rename the symbol at the anchor and apply the edit, opening files it touches. |
| `format PATH` | Format the buffer and apply the edits. |
| `expect diagnostics PATH none` / `expect diagnostics PATH "needle"` | No diagnostics, or one whose code or message contains the needle. |
| `expect no diagnostic PATH "needle"` | No diagnostic contains the needle. |
| `expect early diagnostics PATH "needle"` / `expect no early diagnostic PATH "needle"` | The first diagnostics the server pushed for the file do, or do not, contain the needle. |
| `expect hover PATH "anchor" "needle"` | The hover at the anchor contains the needle. |
| `expect definition PATH "anchor" TARGET "text"` | Goto-definition lands at the text in the target file. |
| `expect completion PATH after "anchor" LABEL` | Completion after the anchor offers the label. |
| `expect buffer PATH [lacks] "needle"` / `expect file PATH [lacks] "needle"` | Buffer or disk content. |
| `expect signature PATH after "anchor" "needle"` | Signature help after the anchor contains the needle, with the active parameter in brackets. |
| `expect symbol PATH "a/b/c"` | The outline has that path of nested symbols. |
| `expect workspace-symbol QUERY PATH NAME` | The workspace symbol query finds the name in the file. |
| `expect fold PATH "from" "to"` | A fold spans the lines of the first occurrences of both needles. |
| `expect selection PATH "anchor" "text"` | Expanding the selection from the anchor reaches exactly the text. |
| `expect lens PATH "anchor" "title"` | A code lens on the anchor's line has that title, after resolving it. |
| `expect token PATH "anchor" CLASS` | The semantic token at the anchor has that type, with modifiers as `+name` (`function+defaultLibrary`). |
| `fix PATH "anchor" TITLE` | Apply the quick fix with that title at the anchor. |
| `refactor PATH "selection" TITLE` | Select the first occurrence of the text and apply the code action with that title. |
| `expect [no] action PATH "selection" KIND TITLE` | Asking for the code actions of that kind (`refactor.extract`, or `all`) over the selection does, or does not, offer the title. |
| `debug start PATH [entry] [prelude "R"] [test "name"] [break N \| break FILE:N]...` | Launch `ry debug-adapter` on the script, with breakpoints in it or in another file. |
| `debug continue` / `next` / `step-in` / `step-out` / `disconnect` / `evaluate "expr"` | Drive the debug session. |
| `expect stopped REASON at N` / `expect stack "f@2 g@7"` / `expect variable NAME "value"` | The next stop, its frames, and a variable of the top frame. |
| `expect evaluated "value"` / `expect debug-output "needle"` / `expect exited N` | The last evaluation, the program's output so far, and its exit status. |
| `run ry ARGS...` | Run the `ry` binary in the workspace. |
| `install DIR` | Install a source package through ry's `R CMD INSTALL` into `library/`. |
| `spawn ry ARGS...` | Start the `ry` binary in the workspace and leave it running, as a watcher runs. |
| `await stdout\|stderr "needle" [before "other"]` | Read the spawned process's output until a line contains the needle, failing if none does within two minutes, or if a line with `other` comes first. |
| `stop` | Kill the spawned process and everything it started; its output becomes the last run's, for `expect stdout\|stderr`. |
| `expect exit N` / `expect stdout\|stderr [lacks] "needle"` | Check the last `run`, `install` or `stop`. |

`cargo test -p ry-lang --test test_scenarios` runs them all, and `just scenarios <name>` runs one.

## Each layer

The table lists what drives each layer and what checks its answers. A layer marked as a gap has no
harness of that kind yet; the gap is filed in the backlog.

| Layer | Driven by | Checked against |
| --- | --- | --- |
| Parser | generated programs, token soup, byte mutations, edit streams (`syntax` `test_fuzz`); `cargo fuzz` `parse` | lossless round trip, tree invariants, splice reparse equals full parse |
| Lowering, naming, inference, diagnostics | generated programs and projects (`semantics` `test_fuzz`); `cargo fuzz` `semantics` | no panic, determinism, well-formed ranges, incremental equals fresh |
| Incrementality and cancellation | the simulator's edits and injected cancellations; `test_incremental` | a fresh server; per-item firewalls |
| Formatter | seeds, mutations, fixture sources (`format` `test_fuzz`, `test_format_range`); `cargo fuzz` `format` | preservation, idempotence, determinism; for range formatting also convergence, restriction to the whole-file edits, additivity and composition |
| IDE features | generated documents and positions (`ide` `test_fuzz`); `cargo fuzz` `ide` | no panic, ranges inside the document |
| Language server | the simulator; scenarios; `cargo fuzz` `lsp` | a fresh server, the push contract, range geometry |
| NAMESPACE conditionals | generated `if`/`else` directive trees over R version and platform tests (`semantics` `test_fuzz` `namespace_conditionals_resolve_to_the_taken_branches`) | the directives of the taken branches, evaluated by a model |
| Loader | random reload sequences that edit R files, `NAMESPACE` (with `if` blocks and S3 registrations), `DESCRIPTION` and data (`loader` `test_reload_fuzz`); `cargo fuzz` `reload`, `load_index` | a fresh load; a model of the namespace's bindings and S3 table; every name whose call graph reaches changed code, directly or through S3 dispatch, is among the callers `ry test` selects; the `test-watch` scenario |
| Deferred S4 | first-use probes (dispatch, `is()`, `initialize` with `callNextMethod`, primitives, `show`, objects read back with `readRDS`) in fresh processes (`ry` `test_load`); `tools/loader-bench/s4_check.R` on the seeded `just bench-s4-load` package | the same probes with `options(ry.load.lazy_s4 = FALSE)`, and GNU R |
| Package builds (`R CMD` under `ry load`) | generated makefiles with recursive, simple, appended, conditional and self-referencing variables, continuations, escapes and `include` lines (`rcmd` `make_expansion_agrees_with_gnu_make`); GNU R's own `Makeconf` | GNU make's expansion of the same variables |
| Help server | generated requests written back to back on one connection, and their byte mutations (`interp` `help_server` tests) | the parsed request round trips; percent-decoding and query splitting match tables recorded from GNU R |
| Debugger | scripted sessions and a seeded client that sends random requests, including ry's own `ry*` requests, at every stop (`interp` `test_debugger`); valid and mutated message framing (`ry` `test_debug_framing`); simulated editors attaching to the console, reinitializing and racing each other (`interp` `test_console_server`); scenarios | transcripts; every request answered once, the session ends, the interpreter left clean; frames read back exactly, a malformed or oversized frame is refused without a panic or an unbounded allocation |
| Interpreter | generated programs (`interp` `test_fuzz`); differential fuzzing; code images: generated code round-tripped, mutated image files and mutated-but-rehashed payloads loaded (`fuzz_code_image_round_trip`, `fuzz_code_image_file`, `fuzz_code_image_load`); `cargo fuzz` `interp_lower`, `bytecode`, `interp_code_image`, `interp_code_image_file` | GNU R, tier 0; for code images the original code object, the evaluator's value, and an error or a warning, never a panic |
| Graphics renderer | random drawings with nested, dangling and wrong-kind definitions and non-finite geometry (`interp` `test_fuzz`); the `graphics__` and `grid__` oracle cases; the plot gallery against GNU R (`oracle` `test_gallery`) | no panic, identical bytes on a second render, one file per page, and files a reader parses (PDF cross-reference offsets and page count, balanced SVG markup, a decodable PNG, BMP, TIFF, JPEG and PostScript headers); GNU R's warnings and capabilities |
| Garbage collector | seeded allocation and mutation sequences (`heap` `test_gc`); `RY_GC_STRESS` over the oracle cases and generated programs; `RY_GC_VERIFY` | a model of the live graph; the unstressed run, the write-barrier invariant, a full collection over every reconcile's frees |
| C API | native fuzzing (`libr` `test_fuzz_native`); mutated serialized streams (`rdb` `mutated_streams_never_panic`); `cargo fuzz` `unserialize` | GNU R's behavior; an R error, never a panic; the PROTECT balance check on every `.Call` |
| JIT | `test_tiers`, including generated typed closures over vectors, records, tuples, tags and `match` that the first call compiles (`fuzz_typed_closures_agree_with_tier_zero`); random lists through every library traversal the JIT loops (`fuzz_stdlib_traversals_agree_with_model`); differential fuzzing with `RY_JIT=force` | tier 0's value, output and error message; GNU R; a Rust model of the library traversals |
| Table engine | `table` property tests: random and mutated delimited text with quotes, embedded line breaks and random read options, read whole, through the slicer and through a memory-mapped file; grouped aggregates; sorting (`fuzz_csv_reading`, `fuzz_grouped_aggregates_against_a_reference`, `fuzz_sort_unique_and_runs_against_a_reference`); `cargo fuzz` `csv`, whose byte decoding `fuzz_csv_target_bytes` runs in the default suite | a whole read of the same text; a reference implementation; the eager implementation; the sniffer's promised columns |
| Console (`ry repl`) | typed keys, typeahead split at random, input chunks with hook code (`repl` `test_console_input`); seeded sessions against GNU R's console (`test_console_session`) | a model of the key decoder and of R's grammar; GNU R's prompts and output |
| Package install | the `install-package` scenario; `test_install` | the package's own results |
| CLI | scenarios run the real binary | expected output and exit status |
| Check cache | seeded edits to a generated package (`ry` `cli::tests::cached_checks_match_fresh_checks_across_seeded_edits`, `CACHE_SEED`, `FUZZ_ITERS`) | a fresh check of the same files |
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
| `just fuzz-deep [MINUTES] [BASE] [DEPTH]` | Every seeded harness at a deep budget until the time limit, described below. |
| `just fuzz-run TARGET` | One `cargo fuzz` target under libFuzzer. |
| `just fuzz-all SECONDS` | Every `cargo fuzz` target for `SECONDS` each, after seeding the corpora. |
| `just docs-test [PAGE]` / `just docs-bless [PAGE]` | The docs examples, or a rewrite of their exact output from the real tool. |

### The deep campaign

`just fuzz-deep` runs `tools/fuzz-deep.py`, which reads the sources for every `#[test]` (and every
`harness = false` `main`) that loops through `for_each_seed`, `fuzz_seeds` or `fuzz_iterations`,
directly or through a function in its file, so a new harness joins without an entry anywhere. Each
test is then looked up in its binary's `--list`, which gives a test inside a module its full path; a
test the binary does not list, or a run that runs no test, is a failure. It then works in three steps:

1. The libr suites that run vendored packages' native code run once under `RY_GC_STRESS=startup`
   with `RY_GC_VERIFY=1`.
2. Each round runs every harness with `FUZZ_SEED` at the round's start and `FUZZ_DEPTH=DEPTH`, and
   four times that depth for the language-server simulation (at 150 actions), the C API sequences
   and the collector. Rounds start ten million seeds apart. The oracle's GC torture runs on a fresh
   sample of cases each round.
3. Rounds repeat until `MINUTES` have passed.

A failure does not stop the campaign: it is logged to `target/fuzz-deep.log` with its replay command,
and the run exits 1 at the end. A run the campaign's deadline cuts short is logged as `cut`, not as a
failure, and a timed-out run is killed with every process it started. Successive rounds and a different `BASE` cover new seeds, so a
scheduled run passes a fresh base, for example `just fuzz-deep 480 $(date +%s)`. The wall clock bounds
the campaign only; every harness still draws its choices from its seed.

## Gaps

- The file system seam for the language server is a real directory, not an in-memory file system, so
  IO errors are not injected.
- Allocation failure is not injectable; the heap aborts on an out-of-memory condition.
- The loader, the REPL and the runtime are not driven by a seeded scheduler; their nondeterminism
  (GNU R's own, threads in packages) is controlled only by running them single-threaded.
- The simulator interrupts a step only with a notification, so a request preempting idle work is not
  simulated.
- The parser's completeness check (`syntax::input_is_unfinished`) treats a string left open across
  lines as finished; the console's `repl::input_is_complete` holds such a string back itself. Both
  treat `pkg::`, a trailing `\`, a leading `;`, `f(1\n2` and a bare `[` as
  unfinished where GNU R reports an error. The differential against GNU R skips the `pkg::` and `\`
  endings.
