# Memory

The knowledge base of the agents building ry, in three horizons. **Short-term** holds the current focus and loose ends, and an item is deleted once it is resolved. **Mid-term** holds active priorities, open bugs and technical debt. **Long-term** holds durable, non-obvious decisions and traps with their reasons. Every entry is context-free and timeless, and points at code or docs instead of restating them. Measurements and long explanations belong in the companion files, never here.

- `backlog.md`: the prioritized punch-list of open work.
- `decisions/index.md`: the index of settled decisions, one file per area (`decisions/<area>.md`). Read the index, then only the entries you need. Every entry carries a `**Status:**` line (built, planned or superseded).
- `test-user-reports.md`: closed findings from simulated-user testing.
- `worklog.md`: one line per cycle, appended by a routine.
- `crates/<crate>/AGENTS.md`: the traps of one crate. Read it before you edit that crate.
- `.agents/orchestrator.md`: the brief for the orchestrating session only. Every other agent ignores it.

`python3 tools/memory.py check` (in `just gate`) enforces a byte budget per file.

The specs live in the docs site under `docs/src/content/docs/`. `reference/type-system.mdx` is the typing contract, `guide.mdx` introduces it, and `development/` covers the internals (`index`, `architecture`, `structure`, `testing`, `robustness`, `corpus-and-oracle`, `releasing`, `authoring-stubs`). Unsettled design work goes to `development/design/`, listed on its index page and kept out of the sidebar, never here.

## Short-term

- **`tools/unsafe.py check` fails** on `bytecode.rs`, `eval.rs`, `interp.rs` and `profile_summary.rs` in `crates/interp/src`. Move the profile summary's call and return code and the call path's newer handlers (`fast_call_rebinding`, `call_declined`, `op_super_sub_assign`, `assign_enclosed_element` and their helpers) onto handles, never raise the baseline (`crates/interp/AGENTS.md`). Delete this line after.
- **Reinstall `.corpus/ry-library`** (`just corpus-ry-install`) once: ry refuses its compiled packages, which carry `RyBuilt:` without `RyAbi:` (`decisions/libr-rcmd.md`, "A package ry compiled records its headers' binary interface"). Delete this line after.
- **Vector kernels.** Toward 10x on element-wise code, the next steps are fusing `sum`/`mean` consumers into the fused pass and an AVX-512 `exp` (`crates/interp/src/kernels.rs`, `vector_math.rs`; `backlog.md`, "Open: untyped R against GNU R").
- **Ahead-of-time compilation.** `ry run --aot` compiles a script's top-level typed functions, and those of files it sources into the global environment, before they run. `ry install` and `ry build --aot` install a typed package's elaborated sources and write its typed code file, whose code a session maps and attaches per function at its first call (`development/typed-code-file.md`); `--train` adds the versions the workload compiled. Native code that gives up after R calls replays the call in the interpreter (`interp::replay`). `ry build --aot --exe main.R` appends the elaborated script and its code file to a copy of `ry` (`crates/ry/src/executable.rs`), and the executable attaches from itself. Steps 1 to 4 of `backlog.md`, "Open: ahead-of-time compilation", are built; its loose ends (macOS runs, embedded sourced files) wait for a machine or a real program.
- **Closure calls.** 72% of the hot suite's closure calls from compiled code take tier 0's fast call path, which costs 750 to 1,200 instructions. Next are the calls that leave it (sites without a plan, unprepared plans, callees with a native entry, `...`) and the path's own cost against the 200-instruction end shape (`backlog.md`, "Open: closure calls that leave tier 0's fast call path").
- **Typed core.** Next are non-constant defaults and box conversion at the boundary (`backlog.md`, "Open: the typed core", with its parts "Gaps the typed public ports found" and "What the `native` check pins and over-reports"). Every program of the public suite has a typed port that runs natively; `typed_benchmarks_run_natively` covers only the kernels and applications, because the public ports run full-size inputs, so check a public port with `RY_TYPED_LOG=1` on a release build. `RY_TYPED_LOG=1` names every typed closure whose call fell back to the interpreter, and `RY_JIT_LOG=1` names each compiled body, each native function, and the instructions left to tier 0.

## Mid-term

Most important first. Each line names the open item, and the detail is at its pointer into `backlog.md`.

**Runtime correctness and compatibility**

- Failures main already has: "Open: broken on main".
- Memory safety: interpreter code moves onto handle types that make holding an unrooted value across an allocation a compile error (`development/design/memory-safety.md`). Tier 0's stores, the attribute setters, name matching and the subset forms are on them, and the order of the rest is in "Open: the handle migration".
- Open bug: with `RY_JIT=force`, compiled code misses a write that an argument promise makes to its caller's variable, so `crates/oracle/cases/environments__code_argument_references.R` fails in force mode ("Open: the JIT").
- Runtime gaps, among them annotations at run time, graphics ("Graphics gaps"), the corpus packages' own tests ("Corpus package tests: what still fails"), the top CRAN packages' tests ("Top CRAN packages: what still fails") and GNU R's regression tests: "Open: the runtime".
- The three benchmark code bases pass every generated test, and their per-test time trails GNU R's. Close that gap, then run `ry check` over the generated code for false reports ("Open: the benchmark packages under ry's runtime").
- Windows has no crash report handler ("Open: Windows", "Crash reports").
- The methods package's core moves to Rust, and the next step is a native class table with dense ids ("Open: native methods core").
- Package loader gaps: "Open: the package loader". `RY_FAST_PATHS=0` covers three prologues only ("Open: one switch for every fast path").

**Untyped performance**

- The target is 10x GNU R on one thread, and 1.5x is the floor for every workload. The work is the "Untyped performance roadmap" table in `development/design/r-runtime.md`, one row per idea in build order, so update a row's status with the change that builds it. Raw measurements: "Open: untyped R against GNU R".
- The hot suite (`just bench hot`) orders this work: take the program with the largest weighted cost, read its `--breakdown`, fix it, and compare instruction counts ("Open: the hot suite").
- The ranked runtime targets, with the instruction price of each R-level mechanism (`just bench-mechanisms`) and the unfinished branch for S3 dispatch: "Open: performance targets the hot suite ranks".
- Builtin fast paths (roadmap row (a) of `decisions/jit-ssa.md`, "The road to 3x") are in progress: `x[[i]]`, `x[[i]] <- v`, `x[i] <- v` and `.Call` cost more instructions than GNU R's, and closure frame setup dominates `structure()` and `format.default` ("Open: untyped R against GNU R", "Open: performance and memory").
- The code bases run at 1.5x because package closures cost about what GNU R's do: a closure call, an S3 dispatch and a `tryCatch` are near 1x, and those closures hold 60% of the time. The ranked fixes are in `backlog.md`, "Why the code bases run at 1.5x, ranked", and the measurements in `development/design/codebase-gap.md`.
- Closure calls and promises: widen promise elision past effect-free callees only after measuring how often the eager path fires ("Open: untyped R against GNU R").
- Call path allocation: attribute cells (most pairlist cells, an object-model question), the getter levels of nested replacement targets (`names(x$a)[2] <- v`), environments that only stand for a frame and detached argument references are the sources left, with measured counts in `backlog.md`, "Open: data-oriented layouts and explicit allocators" ("The call path after slot frames bind on demand").
- JIT: tier 1 compiles a body only when its native work covers what compiled code costs (an entry is about 380 machine instructions, a step back to tier 0 about 180; `decisions/jit-ssa.md`, "The cost model counts frames, and free steps count nothing"), so most application and code-base bodies stay in tier 0 and JIT on gains little over JIT off except where a compiled loop inlines a closure (`orders`, `bond_book`). Closure calls are inlined only into the bodies tier 1 compiles (a callee without type feedback is planned from its instructions, and a calling handler does not stop the call; `development/design/generic-tier.md`), `x <- f(x, ...)` is inlined only when `x` is proven plain, and invalidated machine code is never freed ("Open: JIT on against JIT off", "Open: the JIT"). The next three tier-1 steps have their designs there: inlined frames in shared code (planner half on the branch `perf/tier1-shared-frames`, which does not compile), the inline back-edge count, and wider frames.
- Collector: some builder peaks sit above GNU R's, and the collector's share of the three benchmark suites is unmeasured ("Open: collector throughput on the benchmark suites").
- Exported functions slower than GNU R: `tools/bench/builtins/slower.txt`, a ratchet that may only shrink ("Open: builtins slower than GNU R").
- Kernels slower than GNU R: "Open: base R kernels against GNU R" (`just bench-base`) and "Open: standard library rows slower than GNU R" (`just bench-stdlib`).
- A user's package-test profiles as fourteen kernels: "Open: the kernels of a package test suite's profile" (`just bench-ci`).

**Typed ry**

- Typed ry grows by steps that `just climb` scores on speed against Rust, friendliness cases and soundness fuzzing (`development/design/language-climb.md`, with open targets under "Open questions"). Taste questions go to the orchestrator in one batch.
- Language gaps: literals, record and tag payloads and empty lists outside elaborated code (`ry load`, `source()`), f-strings, `..rest` patterns, and a literal accumulator through a generic call ("Open: the typed core").
- Typed code that builds vectors runs at untyped speed ("Typed natives, the rest of the end shape", under "Open: the JIT").
- `DataFrame` beats data.table on every db-benchmark question at 1e7 rows; what is open is the 1e8 memory margin, the reference counts some calls leave on a frame, and API gaps ("Open: DataFrame").
- Records, tuples, tags and the standard library: "Open: findings from the records, library and frame round" and "Open: ry's standard library".
- Strict mode: friction from typing the benchmark code bases, the cost of a first strict `ry load`, and the top-level rule's follow-ups ("Open: strict-mode friction from typing finbench", "A first strict `ry load` costs the strict check's wall time", "Open: strict mode's top-level rule"). The typed benchmark code bases check clean in strict mode, so a template change that makes one report is a regression or a new friction item.

**Analysis and editor** (ranks behind the reimplementation, and matters as the semantics contract)

- False reports on correct R: "Open: what `ry check` still reports on the GNU R corpus" and "Open: false errors on real CRAN code". Measure on packages the corpus has not mined, because the corpus is fitted, and delete the matching line under Limitations in `reference/type-system.mdx` with each fix.
- A cold check of a 300,000-line code base sits at the edge of its 1 s target with a flat profile ("Check cache follow-ups", "Benchmark code bases (measured)").
- Data-oriented layouts and explicit allocators: the ranked audit is "Open: data-oriented layouts and explicit allocators".
- Adoption findings, each with a minimal repro: "Open: what the adoption reviews left".
- Editor: "Open: editor parity" and "Open: editor latency at 300,000 lines". `ry debug analysis-stats <path>` measures hover, definition, references and rename coverage.
- Robustness: harness gaps in "Open: robustness harnesses", and test debt in "Open: test and fuzz architecture", whose "Judged fine" list is deliberate.

**Releases and operations**

- Releases stay on 0.4.x and focus on untyped R (speed, memory, compatibility) until the typed language settles and ry is significantly faster, and the user says when a release is cut (user decision). Procedure: `development/releasing.md`.
- After each release, append rows to the progress dashboard (`releasing.md`, "The progress dashboard"). Its `cran_popular` dataset is empty ("Open: the CRAN package corpus").
- The Zed extension is not in Zed's registry, and the `VSCE_PAT` fallback for the VS Code Marketplace stops working on December 1, 2026 (`releasing.md`).
- CI runs only on manual dispatch (user decision), and the orchestrator starts it on main when main has new commits (`decisions/tooling-bench-ci.md`, "CI runs in the flake's dev shell").
- `.github/pending/test.yml` (the tests that need no GNU R and a 60-second pass of every fuzz target that builds without Polars, started by hand or by a release, never on a push) and the release workflow's dependency on it wait for the user's `git pull && just accept` (`decisions/tooling-bench-ci.md`, "The tests that need no GNU R run by hand and gate a release, never on push").

## Long-term

Cross-cutting traps and conventions. A trap that belongs to one crate lives in that crate's `crates/<crate>/AGENTS.md`, under "Traps".

### Runtime and numerics

- **Code translated from GNU R lives only in a GPL crate, under a provenance header that `just notices` reads.** A UPL crate never depends on `interp`, and `crates/libr/include` (LGPL) must not copy R's GPL-only `Rinterface.h` or `Rembedded.h` (`reference/licensing.md`).
- **A bare `cargo test` or `cargo build` at the root builds only `crates/ry` (`default-members`), not the `libr` cdylib**, so `target/debug/libR.so` goes stale and every test that runs ry's runtime fails with "ry's runtime ... is out of date". Pass `--workspace` or build `-p libr` too.
- **Last-digit output follows the C math library and FMA contraction.** libr test packages build with `NO_CONTRACTION`, and oracle cases that print such digits carry `# oracle: library-dependent` (`decisions/r-compat.md`, "The oracle suite names the GNU R build it was recorded with").

### Measuring

- **Compare instruction counts only between binaries built the same way.** A release archive is built with PGO and BOLT (`just package`), and on a testthat workload (400 `expect_equal` calls under callgrind) the 0.4.4 archive ran 32.3G instructions where a fat-LTO build of the same commit ran 37.7G. Setting an archive against a plain `cargo build --release` of a later commit once showed a false +5% regression; built alike, the later commit was 10% cheaper.
- **Alternate the two binaries run by run**, measure analysis with `--output json` (the human reporter is quadratic in findings), and compare corpus outputs as unique sorted records. An in-container timing is a two-core number.
- **An A/B build of another commit needs its own target directory**, or it links the other commit's crates. Where the disk has no room for one, touch every file under `crates/` (never `patches/`, which rebuilds Polars) in a tree before building it. Code images belong to the build that installed the library (`refuse_foreign_images`, `bench.py`).
- **Compare peak `RssFile` only from the same page-cache state**: evict both builds with `tools/bench/evict.py` first (`decisions/heap-gc.md`, "Peak RSS is judged as anonymous, image and code size, not as one number").
- **Measure loaders with the same `keep.source`** (`tools/loader-bench`): `ry run` keeps source references and `Rscript` does not.
- **testthat calls `gc()` after every test file with a teardown**, so suite time and process wall time give different ratios against GNU R.
- **Measure a runtime change on the hot suite first, in instruction counts** (`just bench hot --rounds 0 --instructions --save-instructions FILE`, about 80 s, then `--baseline-instructions FILE` on the changed build): it replays the package calls that hold 77 to 80% of the three code bases' time, the weighted total per code base repeats to 0.01%, and its changes track the code bases at r = 0.98. Wall time (`--rounds 3 --extra-rounds 0`, about two minutes) shows only a change above about 10% on a shared VM, and only between builds timed in the same rounds (`--baseline PATH`): read the predicted speed-ups, never one program's ratio, and never a prediction against a code base's own time. `testing.md`, "The hot suite"; `decisions/tooling-bench-ci.md`, "The hot suite replays the package entries the code bases spend their time in"; `backlog.md`, "Open: the hot suite".
- **The application workloads are ours, so never call them real-world.** The bar for the typed suite's geometric mean is 2x Rust's time (user direction); check a local build with `just bench typed --ry PATH --any-build`.
- **Never quote the numeric kernels' geometric mean as general speed.** It comes from tight loops and vector code. The call-heavy application workloads (`tools/bench/applications/results.json`) are the measure of ordinary R code.
- **A package with `Config/testthat/parallel` runs its tests in worker processes** (`rlang`, `purrr`), so the main process only waits in `processx` and its profile is empty. Profile with `ry test --profile-summary`, which runs the suite in one process, or set `RY_PROFILE_SUMMARY_DIR` and read the workers' summaries.
- **A fat-LTO link of `ry` beside callgrind runs can exhaust a 16 GB machine.** The tier tests take minutes under `--release` for the same reason, so run them with `cargo test-release` (the `iterate` profile).
- **A full `just bench-migrate` run over the three domains takes over 50 minutes on four cores.** Pass `--domain` while iterating, and rebuild the release `ry` first, because a stale binary misses stub types such as `condition`.

### Testing

- **Typing is off unless `ry.toml` has `[check]` `typing = true` or the file starts with `# typing: on`**, so a throwaway project passes every type rule silently. To read an inferred type, annotate the binding with a type it cannot have and read the `found` half.
- **Check a change to a path that keeps a value across a call with each case run twice in one process under seeded collection stress**: `oracle compare --engine ry-repeat-gctorture-seeded` on the affected case groups, or `just gc-torture repeat` for every case. A second call reaches the reused frames, plans and caches that one run never does.
- **An oracle is trusted only once an injected bug turns it red.** Determinism, idempotence and round-trip checks pass on a uniformly wrong answer.
- **Give a differential oracle inputs that always exist**: a test pointed only at a gitignored corpus never runs, and the runner hides its skip note.
- **A foreign renderer's expectations are never bulk-blessed**; adjudicate each case against the typing reference.
- **A gate must fail on its own exit status**: `cmd | grep …; echo OK` always passes, and a background job's output file can be read mid-write.
- **A background job starts with SIGINT ignored, and GNU R keeps an ignored SIGINT**, so a harness running interrupt cases resets it in each child, as the oracle's `run_command` does.
- **Re-run a filed finding before ranking work off it**, and add a control in the same file (an obviously undefined name), so that "no finding" cannot mean "the check is off". Treat a "landed" claim in an old note the same way: the verified state is `backlog.md` plus the fixtures.
- **Every generated benchmark test must pass under GNU R at every grid point.** The clustering template runs k-means with `nstart = 25L`, since fewer starts land in local optima at some grid points, and the logistic AUC bound is 0.7 because the grid's minimum is 0.78 (`tools/bench/tooling/templates/`).
- **`just docs-test` needs base R for the `runtime=ry` blocks.** A plain `cargo build` embeds no base R, so those blocks fail with "this ry has no base R" unless `R_HOME` names a GNU R installation (`R_HOME=/usr/lib/R just docs-test`) or `just base-r` has run.

### Build, CI and platforms

- **The toolchain is a dated nightly in `rust-toolchain.toml` (user directive), which `flake.nix` reads too.** Its rustfmt applies `rustfmt.toml`'s unstable import options; a stable rustfmt ignores them and rewrites every `use` tree.
- **The Zed extension is the one stable-Rust crate** (`editors/zed/rust-toolchain.toml`): the nightly's `wasm32-wasip2` std imports `exit-with-code`, which Zed's host lacks. In the nix shell, build it through the rustup proxy.
- **Exclude exactly `zed_ry` from workspace builds**: cargo only warns on a misspelled `--exclude` and builds the crate anyway.
- **The interpreter is linked into `libR.so`, which `cargo build -p ry-lang` does not rebuild**, so a runtime fix seems to have no effect. Use `just ry`; `just doctor` names stale binaries. `cargo test` puts its fresh `libR.so` under `target/<profile>/build/libr/<hash>/out/` (`library_path`, `harness.rs`).
- **A new dev-dependency feature goes into `crates/features`**, or building and testing compile two dependency trees (`decisions/tooling-bench-ci.md`, "Workspace-wide feature unification").
- **A full disk shows up as a test-build failure** (ENOSPC, or a Bus error at link). Run `just prune` first; a cloud container's quota is about 40 GB whatever `df` says.
- **CI is `nix develop .#ci -c just ci`.** Reproduce a failure in that shell, and change CI in the `ci` recipe or `flake.nix` (`decisions/tooling-bench-ci.md`, "CI runs in the flake's dev shell").
- **A crash is never a known failure.** The oracle counts a case whose engine was killed by a signal as new under every engine, even when `known_failures.toml` lists it, and the file has no intermittent mark (`decisions/r-compat.md`, "A crash is never a known failure").
- **Agents work in the nix dev shell: start commands with `source tools/env.sh &&`.** `RY_ENV=system` plus `tools/dev-setup.sh` is for package-corpus work only, and `flake.nix` inputs stay `git+https` (`decisions/tooling-bench-ci.md`, "Agents work in the nix dev shell, loaded by tools/env.sh", "Flake inputs are git URLs").
- **Nightly releases are GitHub pre-releases with `--latest=false`, and the Zed extension reads full releases only** (`pre_release: false`, `editors/zed/src/lib.rs`). Zed matches the flag exactly, so the two move together.
- **BOLT's `-use-gnu-stack` overwrites libR's `PT_GNU_STACK` header, and glibc 2.41+ refuses a library without it.** `tools/layout/optimize.sh` restores it (`gnu_stack.py`), and `just package` checks.
- **Type-check macOS and Windows from Linux before touching platform code** (`cargo clippy --target …` with a clang wrapper as `CC_<target>`; nothing links), because the release job is otherwise the first build there.
- **Cross-build the macOS runtime with clang and ld64.lld, not zig**, whose Mach-O linker ignores `-exported_symbols_list` and so hides export bugs.
- **`R.dll` and `ry.exe` are built for gnullvm, linked with lld, from the same tree** (`decisions/libr-rcmd.md`, "Windows runtime: `R.dll` and `ry.exe` for windows-gnullvm, linked with lld"). A windows-gnu cross-link needs a `synchronization` import library, made by the flake's `preBuild`; a crane deps-only build cannot catch its breakage.

### Docs and conventions

- **The landing page and `guide.mdx` share the four migration step names**, and the tagline outside the user-owned hero is "A type checker and a fast runtime for R, written in Rust." (`decisions/docs-process.md`, "Positioning").
- **Docs content:** explain a thing once where it is introduced, and never use a capability before the page introduces it. A new user topic joins a guide step as an H3. Fixed terms: finding, diagnostic code, annotation, check (the command), code analysis (the capability), the language server, project.
- **A contradiction between two sections of one spec is invisible to every gate**, so read a page end to end when you touch it.
- **Docs output blocks are tests** (`development/testing.md`, "Docs examples"): a `text ry check typing` fence pairs with the code before it. Keep marker words free of `/`, quotes and braces, which Expressive Code reads as highlight markers.
- **Docs are served as Markdown too** (`/<page>.md`, `/llms.txt`), built from raw MDX by `docs/src/lib/markdown.ts`. Build with `bun --bun run astro build`, because the container's Node is older than Astro requires.
- **`reference/formatting-rules.md` is generated** from `crates/format/tests/formatter.template.md`, so a site-wide link rewrite must include the template.
- **Landing-page and README samples are captured from the running tools, never hand-edited** (`test_docs`, `just readme --check`). Nothing checks the VS Code README's two plain-text diagnostics, so re-capture them when a message changes.
- **Scratch output goes to absolute paths.** `cd dir && job &` backgrounds the whole compound, so later commands run in the repository; read `git status` before `git add -A`.
- **Seed the gitignored `fuzz/` lockfile from the workspace lock** (`just fuzz-all` does), or a fresh resolution picks a `salsa-macro-rules` that does not match the pinned `salsa`.
- **UI:** no `border-l` accent rail on rounded elements.
- **A new em-dash in a diagnostic or a comment is a regression.** Three test strings hold one on purpose as multi-byte test data (`crates/syntax/src/literate.rs`, `crates/ry/tests/test_cli.rs`, `crates/ry/src/server/simulation.rs`), and `worklog.md` keeps its original wording.
- **Validate a docs link against the ids in the built HTML** (`docs/dist/**/index.html` after `cd docs && bun run build`), never against a hand-rolled slug. Two headings that differ only in punctuation slug to the same id, and the second silently becomes `-1`.
- **Before rewriting a docs page, run an adversarial reader subagent with the owner's objections as the rubric:** too abstract, information the reader does not need, a term used before it is introduced, and copy about the tool instead of what the reader gets. A page that reads like a telegram is as much a regression as dense prose.
- **Docs vocabulary for data shapes:** an R list is "fixed-shape" or "any length", so "record" and "tuple" name only ry's own types. A tagged union in an example goes one variant per line with a leading `|` (user preference), which `ry fmt` keeps.
- **`reference/std/` is generated from `types/std.Rtypes` by `just docs-std` and never edited by hand.** The examples in `std.Rtypes` run in the docs test, so regenerate an example's output by running it.
- **The former spellings of the tool fall back** (`roughly.toml`, `# roughly: allow`, `ROUGHLY_*`, the editor extension's `roughly.*`), and `roughly.toml` must stay.
- **Format only the hunks you wrote, never the whole workspace.** `cargo fmt --all` rewrites files that other tracks are editing, because work lands without waiting for the format check, and the stray hunks turn into merge conflicts for them.
