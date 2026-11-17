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
so a test with several loops keeps their proportions when a campaign deepens it. Each case builds
its generator from its seed alone, as `SplitMix64(CONSTANT ^ seed)`, because a generator shared
across cases makes a replayed seed draw different inputs and a deeper round rerun the same ones.
`for_each_seed` takes the plain default count: wrapping it in `fuzz_iterations` applies
`FUZZ_DEPTH` twice.

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

**A harness and its pinned inputs are never cut for being slow or quiet.** The rule for which tests
earn their place is in [What earns a test its place](/development/testing#what-earns-a-test-its-place):
unit tests that duplicate a harness go, and the harness stays.

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
| `requires gnu-r` / `requires ry-runtime` / `requires r-package:NAME[>=VERSION]` | Skip the scenario unless GNU R is installed with its shared library (`--enable-R-shlib`), `libR.so` is built beside `ry` and ry's base R is built (`just base-r`), or the package loads, at `VERSION` or later (`packageVersion`), in every runtime the scenario requires: ry's runtime for `ry-runtime`, and for `gnu-r` the GNU R that `--runtime gnu-r` picks. The skip names the runtime and, for an old package, the installed version. A misspelled requirement fails the scenario. |
| `hide gnu-r` | Run every later `ry` with no GNU R: `R` and `Rscript` vanish from `PATH` while everything beside them (such as `sh` in `/usr/bin`) stays, and `R_HOME` is unset. |
| `ignore SIGINT` | Start every later `run` with SIGINT ignored, as a background job of a non-interactive shell starts. |
| `client pull` / `client utf8` / `client cold` | Start the editor as a pull-diagnostics or UTF-8 client, or one whose first steps arrive before the startup warm-up has run. Must come before the first editor step. |
| `file PATH` + body | Write a file; a running editor sees a watcher event. `$ROOT` in the body is the scenario's workspace. |
| `stdin PATH` | Hand the next `run` that file as its standard input, which is how a scenario types into a console. |
| `env NAME VALUE` | Set a variable for every later `run`; `$ROOT` in the value is the scenario's workspace. |
| `unset NAME` | Remove a variable, including one the test process inherited, for every later `run` until an `env` sets it again. |
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
| `run ./PATH ARGS...` | Run an executable the scenario wrote, such as one `ry build --aot --exe` built, with the environment `run ry` gets. |
| `install DIR` | Install a source package through ry's `R CMD INSTALL` into `library/`. |
| `spawn ry ARGS...` | Start the `ry` binary in the workspace and leave it running, as a watcher runs. |
| `await stdout\|stderr "needle" [before "other"]` | Read the spawned process's output until a line contains the needle, failing if none does within two minutes, or if a line with `other` comes first. |
| `stop` | Kill the spawned process and everything it started; its output becomes the last run's, for `expect stdout\|stderr`. |
| `expect exit N` / `expect stdout\|stderr [lacks] "needle"` | Check the last `run`, `install` or `stop`. |

`cargo test -p ry-lang --test test_scenarios` runs them all, and `just scenarios <name>` runs one.

A scenario that requires an R package loads it from the corpus library (`just corpus`, in
`$RY_CORPUS_DIR/library`, else `.corpus/library`) when every package in it was built by the R
release ry's runtime presents and, for a `gnu-r` scenario, by the release of the GNU R it runs. The
runner then sets `R_LIBS` and `R_LIBS_USER` to that library for every `ry` and R the scenario
starts. Other scenarios see the caller's variables, so the ones about library defaults test the
defaults. A library built by another release is not used, because `library()` would warn. The
runner prints the library it uses, or why it uses none, before the first scenario. To use another
library, set `R_LIBS` yourself; `R_LIBS=` (empty) turns the default off.

## Each layer

The table lists what drives each layer and what checks its answers. A layer marked as a gap has no
harness of that kind yet; the gap is filed in the backlog.

| Layer | Driven by | Checked against |
| --- | --- | --- |
| Parser | generated programs, token soup, byte mutations, edit streams (`syntax` `test_fuzz`); `cargo fuzz` `parse`; fragments of every construct parsed by GNU R and by the runtime (`oracle` `fuzz_parser_matches_gnu_r`) | lossless round trip, tree invariants, splice reparse equals full parse; GNU R's tree, warnings and rejection for the same text |
| Lowering, naming, inference, diagnostics | sessions of project states a small edit apart, from generated typed programs, every fixture case and the program corpus (`semantics` `test_fuzz`); `cargo fuzz` `semantics` | a command's database with no history, on every state and after the return to the first one; no panic, well-formed ranges |
| Incrementality and cancellation | the simulator's edits and injected cancellations; `test_incremental` | a fresh server; per-item firewalls |
| Formatter | seeds, mutations, generated programs and fixture cases with one small edit, fixture sources, the program corpus (`format` `test_fuzz`, `test_format_range`); `cargo fuzz` `format` | the same tokens, the same parse tree and the same comments; idempotence, determinism; a refusal only on a syntax error or an annotation that interrupts its statement; for range formatting also convergence, restriction to the whole-file edits, additivity and composition |
| IDE features | generated projects of mixed file kinds with small edits, fixture cases and the program corpus on one warm database, every feature at sampled positions (`ide` `test_fuzz`); `cargo fuzz` `ide` | no panic, ranges inside the document; each answer against the others (definition and references round trip, one occurrence set from every occurrence, ranges that spell the cursor's name and cover the cursor); a rename that leaves every file parsing with no new finding |
| Language server | the simulator; scenarios; `cargo fuzz` `lsp` | a fresh server, the push contract, range geometry |
| NAMESPACE conditionals | generated `if`/`else` directive trees over R version and platform tests (`semantics` `test_fuzz` `namespace_conditionals_resolve_to_the_taken_branches`) | the directives of the taken branches, evaluated by a model |
| Loader | random reload sequences that edit R files, `NAMESPACE` (with `if` blocks and S3 registrations), `DESCRIPTION` and data (`loader` `test_reload_fuzz`); `cargo fuzz` `reload`, `load_index` | a fresh load; a model of the namespace's bindings and S3 table; every name whose call graph reaches changed code, directly or through S3 dispatch, is among the callers `ry test` selects; the `test-watch` scenario |
| Deferred S4 | first-use probes (dispatch, `is()`, `initialize` with `callNextMethod`, primitives, `show`, objects read back with `readRDS`) in fresh processes (`ry` `test_load`); `tools/loader-bench/s4_check.R` on the seeded `just bench-s4-load` package | the same probes with `options(ry.load.lazy_s4 = FALSE)`, and GNU R |
| Package builds (`R CMD` under `ry load`) | generated makefiles with recursive, simple, appended, conditional and self-referencing variables, continuations, escapes and `include` lines (`rcmd` `make_expansion_agrees_with_gnu_make`); GNU R's own `Makeconf` | GNU make's expansion of the same variables |
| Help server | generated requests written back to back on one connection, and their byte mutations (`interp` `help_server` tests) | the parsed request round trips; percent-decoding and query splitting match tables recorded from GNU R |
| Debugger | scripted sessions and a seeded client that sends random requests, including ry's own `ry*` requests, at every stop (`frames` `test_debugger`); valid and mutated message framing (`ry` `test_debug_framing`); simulated editors attaching to the console, reinitializing and racing each other (`frames` `test_console_server`); scenarios | transcripts; every request answered once, the session ends, the interpreter left clean; frames read back exactly, a malformed or oversized frame is refused without a panic or an unbounded allocation |
| Interpreter | generated programs, including reads and writes of attributes, names and elements and `c()` calls that compiled code sends to the builtins' direct entries (`interp` `test_fuzz`); closures that write elements, rows and columns of an argument whose length sits at a storage threshold, called three times (`fuzz_replacements_agree_across_repeated_calls`); differential fuzzing (`oracle fuzz`) of nine kinds of program: generated grammar programs, including element stores into argument copies of 64 elements and more, interpreted and compiled; metamorphic trials, where one body on one value from a zoo of types, sizes, names and encodings must give an identical result however often and wherever it runs (`fuzz_metamorphic_forms_agree_across_tiers`); builtins on the same zoo (`fuzz_builtins_agree_across_tiers`); calls of functions drawn from every export of base, stats and utils with arguments of any type (`fuzz_api_calls_agree_across_tiers`); round trips of zoo values through files and connections, one writer and several readers each (`fuzz_io_round_trips_agree_across_tiers`); closures built from promises, frame inspection, `...`, conditions and restarts, replacement functions, S3, S4 and Reference Class dispatch, `eval` and quoted code, called one, a few and sixty times with the first and the last call recorded (`fuzz_language_constructs_agree_across_tiers`); grammar, metamorphic and language programs with statements inserted into their function bodies that never run or have no effect on the result (`fuzz_perturbed_programs_agree_across_tiers`); source fragments of every construct, whole and damaged, handed to `parse(text =)` (`fuzz_parser_matches_gnu_r`); and mutated snippets of real R code (base help examples, R's regression tests, the oracle cases), compared only where the unmutated snippet agrees (`fuzz_corpus_mutants_never_crash` is the bounded pass), each compared through a canonical dump of every top-level value and condition; code images: generated code round-tripped, mutated image files and mutated-but-rehashed payloads loaded (`fuzz_code_image_round_trip`, `fuzz_code_image_file`, `fuzz_code_image_load`); `cargo fuzz` `interp_lower`, `bytecode`, `interp_code_image`, `interp_code_image_file` | GNU R, tier 0; the same statements run once at top level, and the caller's value unchanged; for code images the original code object, the evaluator's value, and an error or a warning, never a panic |
| Native package functions | seeded random frame stacks of closures, operators, `::`, `do.call`, `lapply`, `vapply`, `Reduce`, handlers, restarts, `eval`, `local`, `Recall`, S3 dispatch and trimming (`fuzz_native_trace_back_matches_its_r_body` in `oracle` `test_differential`, at tier 0 and under the JIT, `FUZZ_SEED` replays; the `conditions__rlang_trace_back_generated` oracle case) | the function's own R body on the same frame, by `identical()`; GNU R, through the oracle case |
| Environment variables | seeded random `Sys.setenv`, `Sys.unsetenv` and Rust-side writes over prefix-sharing, empty, `NA`, `=`-holding and non-ASCII names and values (`interp` `test_fuzz` `fuzz_environment_variables_agree_with_the_c_library`); the `system__sys_env_round_trips` oracle case | the C library's environment, on every `Sys.getenv`, `Sys.setenv` and `Sys.unsetenv` result; GNU R, through the oracle case |
| Native helpers (withr, base) | seeded random calls of withr's environment and option helpers, `Sys.getenv`, `Sys.setenv`, `Sys.unsetenv`, `intersect` and `structure(names =)` with `NA`, empty, unset and non-ASCII values (`fuzz_native_helpers_match_their_r_bodies` in `oracle` `test_differential`, at tier 0 and under the JIT, `FUZZ_SEED` replays) | each function's own R body, by `identical()` on the value, visibility, condition and environment and option state; GNU R, through the oracle cases |
| Graphics renderer | random drawings with nested, dangling and wrong-kind definitions and non-finite geometry (`interp` `test_fuzz`); the `graphics__` and `grid__` oracle cases; the plot gallery against GNU R (`oracle` `test_gallery`) | no panic, identical bytes on a second render, one file per page, and files a reader parses (PDF cross-reference offsets and page count, balanced SVG markup, a decodable PNG, BMP, TIFF, JPEG and PostScript headers); GNU R's warnings and capabilities |
| Garbage collector | seeded allocation and mutation sequences (`heap` `test_gc`), including string and symbol interning in every encoding; `RY_GC_STRESS` and `RY_GC_STRESS=young` over the oracle cases and generated programs; `RY_GC_STRESS=seed:N` with `RY_GC_VERIFY` over the cases that run native code, and over cases evaluated twice in one process; native code permuting a list or character vector through every element pointer accessor (`libr` `element_pointers_keep_permuted_young_elements_alive`) | a model of the live graph; reference `HashMap`s of the interned strings and symbols; the unstressed run, the write-barrier invariant, a full collection over every reconcile's frees |
| C API | native fuzzing (`libr` `test_fuzz_native`); mutated serialized streams (`rdb` `mutated_streams_never_panic`); `cargo fuzz` `unserialize` | GNU R's behavior; an R error, never a panic; the PROTECT balance check on every `.Call` |
| JIT | `test_tiers`, including generated typed closures over vectors, records, tuples, tags and `match` that the first call compiles (`fuzz_typed_closures_agree_with_tier_zero`); random lists through every library traversal the JIT loops (`fuzz_stdlib_traversals_agree_with_model`); random comparison, `&`, `|` and `!` programs over lists and scalars with `NA`, `NaN` and length mismatches through the fused mask count (`typed` `test_typed` `values::fused_counts_follow_three_valued_logic`); differential fuzzing with `RY_JIT=force` | tier 0's value, output and error message; GNU R; a Rust model of the library traversals and of R's three-valued logic |
| Compiling ahead of time (`ry run --aot`) | seeded scripts of typed functions built from the shapes that make compiling ahead differ from compiling at a first call: a call before the callee's definition, a name defined twice or rebound between calls, a definition inside a block, alias-typed parameters, conversions of what R returns, a conversion whose vector is narrower than the checker's type or empty, a conversion whose type changes between calls after an R call that printed, a literal that warns when it is lowered, a changed `keep.source`, a script that stops partway (`ry` `test_scenarios` `typed_scripts_agree_however_they_compile`, `FUZZ_SEED` replays); every typed benchmark program with `--aot` (`typed_benchmarks_run_natively`); a script with more definitions than the session's limit on compiled closures; the `run-aot` scenario | the same script interpreted (`RY_JIT=off`), compiled at first call, compiled ahead, compiled ahead with every sibling guess failing (`RY_AHEAD_MISS=1`), compiled ahead with tier 1 forced, and with its annotations erased: one exit status, standard output and standard error; no compile after the script starts on the benchmark programs |
| Typed code files (`ry install`, `ry build --aot`) | random valid metadata written and read back, then truncated, bit-flipped and rewritten with a fixed hash (`typed` `test_typed` `file::fuzz_typed_code_parse`, `cargo fuzz` `typed_code_parse`); an installed file damaged in its header, metadata or code, and a second write against the same install (`ry` `test_scenarios` `fuzz_typed_code_file`); every generated typed script installed as a package and run with its file and with every attach failing (`typed_scripts_agree_however_they_compile`); `RY_GC_STRESS` and the copied-code path over attached code, shapes interned in another order, a run-time copy and a reinstall in a running session (`install-typed-package` scenario) | the metadata as written; the same bytes from both writes; what the script prints with an intact file or none |
| Executables (`ry build --aot --exe`) | random bytes read as an executable, and payloads appended to random bytes, read back, and then read again with bytes of the trailer and metadata changed (`ry` `test_fuzz` `fuzz_executable_payloads`, `cargo fuzz` `executable_payload`); every generated typed script built into an executable and run (`typed_scripts_agree_however_they_compile`); arguments, an error partway, every attach failing and the default output name (`build-exe` scenario) | the payload as written, or none or a damaged one, never a panic; what `ry run` prints |
| Table engine | `table` property tests: random and mutated delimited text with quotes, embedded line breaks and random read options, read whole, through the slicer and through a memory-mapped file; grouped aggregates; sorting (`fuzz_csv_reading`, `fuzz_grouped_aggregates_against_a_reference`, `fuzz_sort_unique_and_runs_against_a_reference`); `cargo fuzz` `csv`, whose byte decoding `fuzz_csv_target_bytes` runs in the default suite | a whole read of the same text; a reference implementation; the eager implementation; the sniffer's promised columns |
| File removal (`unlink`) | seeded trees of files, nested and read-only directories, links into and out of the tree, dangling links, FIFOs and Unix sockets, removed with random `recursive`, `force` and working directory (`interp` `test_file_removal`, `FUZZ_SEED` replays); the `io__unlink` oracle case | a port of GNU R's `R_unlink` on a copy of the same tree: the failure result, and every entry left, with its mode and link target; GNU R, through the oracle case |
| Console (`ry repl`) | typed keys, typeahead split at random, input chunks with hook code (`repl` `test_console_input`); seeded sessions against GNU R's console (`test_console_session`) | a model of the key decoder and of R's grammar; GNU R's prompts and output |
| Package install | the `install-package` scenario; `test_install` | the package's own results |
| CLI | scenarios run the real binary | expected output and exit status |
| Check cache | seeded edits to a generated package (`ry` `cli::tests::cached_checks_match_fresh_checks_across_seeded_edits`, `CACHE_SEED`, `FUZZ_ITERS`) | a fresh check of the same files |
| Docs examples | every marked output block in the docs, and the landing page samples (`test_docs`) | the output the page shows |

The PROTECT balance check runs on every `.Call` and `.C` in every build: when a routine returns with a
different protect-stack depth than it entered with, ry warns `stack imbalance in '<routine>'`, as GNU R
does. A stricter checker that catches an unprotected object at the moment it is collected is
`RY_GC_STRESS`, which collects before every allocation.

Plain stress runs a minor collection each time, which ages every survivor into the old generation
at once, and a minor collection never frees an old object. An object that survived one collection
and is then held only by a Rust local is therefore safe under plain stress, although outside
stress it is still young and the next minor collection frees it. `RY_GC_STRESS=young` catches that
case. Every allocation runs a collection that traces again every object it has not tenured, so
survivors stay collectable until every 64th collection tenures them, and major collections keep
the plain mode's spacing. A small script runs about five times slower than under plain stress.

| `RY_GC_STRESS` | Stress starts | Each allocation runs |
| --- | --- | --- |
| `1` | when the script starts | a minor collection |
| `startup` | at the first allocation | a minor collection |
| `young` | when the script starts | a collection that keeps survivors untenured |
| `startup-young` | at the first allocation | a collection that keeps survivors untenured |
| `seed:N` | when the script starts | a collection at random gaps drawn from seed `N` |
| `startup-seed:N` | at the first allocation | a collection at random gaps drawn from seed `N` |

Both modes above collect so often that an object is promoted before the operation that made it
is done with it, so neither catches a store that skips the write barrier: by the time native code
moves a young object, the object is old. `RY_GC_STRESS=seed:N` collects at random gaps instead,
from one allocation to a few thousand with a heavy tail, so objects routinely stay young across a
whole native call. Seven collections in eight are minor, the rest intermediate, and a major one
runs once per as many allocations as there are live objects. With `RY_GC_VERIFY=1` each
collection short of a major one first checks the barrier's invariant: every old object that
points at a young one is remembered, has the element on a dirty card, or is escaped (see the
barrier contract in [R runtime](/development/design/r-runtime/)). A missed barrier then panics
at the next collection instead of freeing a live object only when the timing is unlucky.

The same verification checks every root before the collector traces it, and every reference a
reachable object holds as it is traced. A root or reference that is not nil and not the start of
an allocated object panics with `heap corrupted`. A value kept without a root, and a write that
runs past an object's storage into its neighbor, then fail at the next collection instead of when
the damaged object is used. The report says what holds the reference, what the reference points
at, and where it is stored:

```
heap corrupted: protect stack entry 0 of 1 is 0x2000000200c0, which is an object that collection 1 freed. Found by collection 2.

heap corrupted: a reachable List of length 1 at 0x2000000200f0 holds 0x2000000200c0 as element 0,
which is an object that collection 1 freed. The reference is the word at 0x200000020110. Found by
collection 2.

heap corrupted: a reachable List of length 2 at 0x2000000200c0 holds 0x3ff0000000000000 as
element 1, which is no address of the heap (the bits of the double 1). The reference is the word
at 0x2000000200e8. Found by collection 1.
```

| The report says | What happened |
| --- | --- |
| `an object that collection N freed` | Something kept the reference without a root across collection `N`. The holder names where: a protect stack entry, a handle, a handed-out value, a value the interpreter's root source names, or a field of another object. |
| `no address of the heap (the integer N)` or `(the bits of the double X)` | A write ran past the end of a neighboring object and stored data over the reference. |
| `an address N bytes into the S-byte slot of a live object` | The reference was computed, or a write overlapped it by part of a word. |
| `a free S-byte slot` | The slot holds no object: none was allocated there yet, or one was freed before poisoning was on. |

Small objects live at fixed addresses, so a run with the same seed reports the same address and
the same collection number for them. A large object's address changes from run to run.

A build with debug assertions also checks the mutator between collections: a store into an object,
a stored value and a value being rooted as a handle must each be an allocated object. A builtin
that allocates its result without rooting it, then fills it while allocating each element, fails
at its first store after the collection that freed the result, and one that roots a value too late
fails where it roots it:

```
heap corrupted: the object a store writes into is 0x200000b15000, which is an object that collection 2 freed.

heap corrupted: a value being rooted is 0x2000000200c0, which is an object that collection 1 freed.
```

The panic's backtrace names the builtin. A release build leaves this case to the poison, because
the check would cost every store and every root a branch.

The default suite runs a seeded sample of the oracle cases (`cases_survive_gc_torture`,
`cases_survive_young_gc_torture`) and of the generated programs (`fuzz_programs_survive_gc_torture`)
under the first two modes, compared with ry unstressed. A third test
(`native_cases_keep_the_write_barrier_under_seeded_gc_torture`) runs three cases from the groups
that run the most native code (`datatable`, `packages`, `altrep`, `serialize`) and one more from
anywhere under `seed:N` with verification, where `N` is `FUZZ_SEED`. A fourth
(`cases_survive_a_second_pass_under_seeded_gc_torture`) runs twelve cases under the same stress
with each case evaluated twice in one process (the `ry-repeat-gctorture-seeded` engine in
[Corpus and oracle](/development/corpus-and-oracle)). A function's second call runs through what
its first call left: a compiled body, a call site's plan, a reused frame. A value kept there
without a root, or kept in a representation the second call does not expect, shows up only then.
Each sample always includes the cases in `REGRESSION_CASES` and the seeds in `REGRESSION_SEEDS`,
which once read freed memory or lost a live object.

Collecting often is not enough on its own, because each sweep hands a freed slot out again at the
next allocation, and a stale reference then reads a valid object. So while `RY_GC_STRESS` or
`RY_GC_VERIFY` is on, in any build, the collector poisons every object it frees (fills it with
`0xDB`) and quarantines its memory: the allocator skips the freed small slots until 16 MiB of newer
ones have joined them, and holds freed large objects until 64 MiB have. In a debug build, reading
the kind of a freed object then panics with `read the kind of a freed object at <address>`. A
release build skips that check, because it would cost every call of the hottest accessor in the
runtime, so there a freed object reads as `FREESXP` and its other fields as poison. Without stress
or verification, debug builds poison freed slots but reuse them at once, and release builds do
neither.

`just asan` runs the `heap` and `interp` tests in a build with AddressSanitizer. The heap maps its
own memory, which the sanitizer knows nothing about, so in such a build the collector tells it:
a freed object's memory past its first word, which holds the kind, is marked unaddressable, and a
slot is marked addressable again when the allocator hands it out. A read or write of a freed object's fields or elements then
stops at the access, in Rust or in C, instead of reading poison:

```
ERROR: AddressSanitizer: use-after-poison on address 0x2000000200d0 ...
READ of size 8 ...
    #1 heap::object::Sexp::length crates/heap/src/object.rs:995
    #2 heap::object::Sexp::slice crates/heap/src/object.rs:1039
```

The sanitizer sees no boundary between two live objects, because slots sit side by side with no
gap, so a write past one object into the next is left to the verifier's reference check above.
Code the JIT generates and a package's compiled code are not instrumented, which is why the recipe
covers the two crates' tests and not the oracle cases.

Before the argument-reference rooting fix, `datetime__year_overflow` crashed unstressed under the
oracle harness. In a debug build of that tree, `RY_GC_STRESS=young` with quarantine panicked
with the message above in 3 of 3 runs, when a collection traced a root that still held a freed
object, while plain `RY_GC_STRESS=1` passed 3 of 3, since it tenures the object before it can die.

## Running the long harnesses

| Recipe | What it runs |
| --- | --- |
| `just simulate COUNT ACTIONS BASE` | The language-server simulator over `COUNT` seeds from `BASE`. |
| `just scenarios [NAME]` | Every scenario, or one. |
| `just fuzz-differential SEED COUNT ENGINES` | Generated grammar programs under GNU R and ry's engines. |
| `just fuzz-kind KIND SEED COUNT ENGINES` | One kind of program (`grammar`, `metamorphic`, `builtins`, `api`, `io`, `language`, `perturbed`, `parser`, `corpus`) or `all`, with each diverging program and its report written to `target/fuzz-findings`. |
| `just fuzz-coverage COUNT` | No engine: how many of the functions base, stats and utils export the programs of `COUNT` seeds of each kind name in a call. |
| `just fuzz-long HOURS BASE ENGINES` | Every kind for `HOURS` of wall clock from seed `BASE`, under tier 0, the forced JIT and seeded collection stress unless `ENGINES` says otherwise. The staged Fuzz workflow runs it. |
| `just fuzz-reduce FILE ENGINE` | One diverging program shrunk to a minimal one that diverges the same way (`oracle reduce`), the form a finding takes as an oracle case. |
| `just fuzz-corpus SEED COUNT ENGINES` | Mutated snippets of real R code (base help examples, R's regression tests, the oracle cases) under GNU R and ry's engines, observing. |
| `just gc-torture [young\|seeded\|repeat]` | Every oracle case under collection stress, under `RY_GC_STRESS=young`, under `RY_GC_STRESS=seed:N` with the heap checked, or under the last with each case evaluated twice in one process. |
| `just asan [ARGS]` | The `heap` and `interp` tests under AddressSanitizer, which stops at a read or write of a freed object. |
| `just ci-stress SEED` | One of the release gate's stress jobs: `just gc-torture repeat` with `FUZZ_SEED=SEED`, then 300 generated programs under GNU R, tier 0, the JIT and the repeated seeded stress. CI runs seeds 0, 1 and 2. |
| `just fuzz-deep [MINUTES] [BASE] [DEPTH]` | Every seeded harness at a deep budget until the time limit, described below. |
| `just fuzz-run TARGET` | One `cargo fuzz` target under libFuzzer. |
| `just fuzz-all SECONDS [FEATURES]` | Every `cargo fuzz` target for `SECONDS` of wall clock each, after seeding the corpora and building the targets. `FEATURES` limits the targets: `runtime` leaves out the two that link Polars. |
| `just docs-test [PAGE]` / `just docs-bless [PAGE]` | The docs examples, or a rewrite of their exact output from the real tool. |

### The deep campaign

`just fuzz-deep` runs `tools/fuzz-deep.py`, which reads the sources for every `#[test]` (and every
`harness = false` `main`) that loops through `for_each_seed`, `fuzz_seeds` or `fuzz_iterations`,
directly or through a function in its file, so a new harness joins without an entry anywhere. Each
test is then looked up in its binary's `--list`, which gives a test inside a module its full path; a
test the binary does not list, or a run that runs no test, is a failure. It then works in three steps:

1. The libr suites that run vendored packages' native code run once under `RY_GC_STRESS=startup`
   and once under `RY_GC_STRESS=startup-young`, both with `RY_GC_VERIFY=1`.
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
