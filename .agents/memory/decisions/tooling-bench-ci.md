# Decisions: Build, benchmarks and CI

Build profiles, releases, platforms, the dev shell, CI, fuzzing harnesses and benchmark suites.

## Every pipeline stage is fuzzed from its first commit

**Status:** built, except the "CI does not fuzz" paragraph, which "CI runs `just gate` against GNU R 4.6.2, and nothing ignored" supersedes.

This is a user directive. The testing doctrine made fuzzing mandatory for the `syntax` crate from
day one. It applies to every other pipeline stage too. Fuzzing is never bolted on later, for any
layer.

Every stage gets fuzz and property coverage the day it exists, alongside its fixtures. That covers
lowering, naming, inference, diagnostics, the incremental layer, the formatter and the IDE
features. The formatter is fuzzed for idempotence and losslessness. An IDE feature is fuzzed for
never panicking at any cursor position.

The semantics harness at `crates/semantics/tests/test_fuzz.rs` is the template. It asserts four
things over a generator biased toward semantically live shapes, plus a token-soup robustness arm.

- Nothing panics across the full pipeline, which includes fixpoints converging.
- Results are deterministic across fresh databases.
- Diagnostic ranges have valid geometry.
- Incremental equivalence holds, so editing through the setter gives the same result as a fresh
  build. That is the red-green invariant.

`FUZZ_ITERS` scales the budgets. A bounded pass runs in each crate's own test suite, and the
`fuzz_deep` variants carry the long runs.

**CI does not fuzz today, and it is worth stating plainly rather than assuming otherwise.** The root
`Cargo.toml` sets `default-members = ["crates/ry"]` and the workflow omits `--workspace`, so
`cargo test --all-targets --all-features` lists 171 tests where the workspace has 712. Every fuzz
arm lives in another crate. The second job runs `-- --ignored`, which lists no tests at all for the
same reason, so it is a release build followed by an empty test run. `backlog.md` holds the blockers
that a workspace-wide CI run has to clear first.

The first semantics fuzz runs found two real crashes within seconds. One was a non-converging
cycle, where a growing self-referential type rode the iteration cap into a panic. The other was
inference variables leaking through exported schemes into foreign tables. Both are of the class
that only surfaces in the large, which is exactly what per-stage fuzzing exists to catch early.

## The shipping binary links no Apple frameworks

**Status:** built, except the `justfile` preflight for framework-linking crates, which does not exist.

The user set the criteria: fewer dependencies, and no licensing exposure.

A release macOS binary is cross-linked on Linux by zig, through cargo-zigbuild inside the nix
build. Zig ships stubs for libSystem, libc and libm only. The binary linked nothing else before the
REPL existed, so the SDK-less link worked by accident. The REPL added the first Apple-framework
edge. reedline depends on chrono with the clock feature, which depends on iana-time-zone, which
depends on core-foundation-sys, which emits `-framework CoreFoundation`. Zig cannot resolve that
without a macOS SDK.

`[patch.crates-io]` replaces iana-time-zone with `patches/iana-time-zone`, a version-matched stub
whose `get_timezone()` always errors. That is safe because chrono consults it only as a fallback,
after its primary timezone sources of `TZ` and `/etc/localtime` and before its final UTC default.
The only local-time user in reedline is its default prompt's clock display, and the REPL does not
use it, because it renders R's own prompt.

The `release` recipe in the justfile preflights the aarch64-apple-darwin graph for known
framework-linking crates, so a regression fails in seconds with a named culprit instead of deep
inside the nix zig link. The stub must stay version-compatible and feature-compatible with what
chrono requests, or cargo silently prefers the real crate, and the preflight catches exactly that.

One nix-specific trap applies. crane's dependency-only builds compile dependencies against a
dummified workspace copy, where local `.rs` files are emptied so the dependency cache survives a
source edit. That would empty the patch crate too and break chrono's compile, so `flake.nix`
restores `patches/` verbatim into the dummy source through crane's `extraDummyScript`. It
interpolates only that directory, so the cache stays source-independent.

One alternative was implemented first and then reverted. It fetched a macOS SDK from the widely
used third-party mirror of Apple's SDKs and exported `SDKROOT` for the darwin cross-build. It was
mechanically verified, because with the SDK the failing build links a valid arm64 Mach-O, and it is
the most general option with no behavior change. The user directed reverting it. It adds a large
third-party artifact to the release closure, and Apple's license on a redistributed SDK is gray.
Vendoring the tarball is legally worse, because you become the redistributor.

Framework-free is sustainable for this product. libR is dlopen'd at run time, so it has no
link-time dependency, and a terminal REPL's plotting story is file output, a terminal image
protocol, or a browser. Nothing on the roadmap needs `-framework` at link time.

Two things would trigger a revisit: a dependency that genuinely needs an Apple framework, such as
native windows or clipboard integration, or pressure to sign and notarize. At that point build the
mac artifact on a real macOS runner, which is the only fully license-clean way to use Apple's SDK.
That is also the natural home for running the REPL end-to-end suite against a real R in CI.

## The Linux release binary links glibc 2.17 through zig

**Status:** superseded by "Releases are built natively per platform, with the runtime beside the binary". Kept for the reasoning about loaders, musl and allocators.

**Problem.** The Linux release package was crane's native build against nixpkgs' glibc, so the
shipped binary's ELF interpreter and RUNPATH were `/nix/store/...` paths. It started only on a
machine whose store held that exact glibc.

**Shape.** Two Linux derivations, one job each:

- `ry-linux-x86_64`, symmetric with `ry-macos-aarch64` and `ry-windows-x86_64`, is cross-linked by
  zig through cargo-zigbuild for `x86_64-unknown-linux-gnu.2.17`. It uses the standard loader
  `/lib64/ld-linux-x86-64.so.2`, needs only `libc`, `libm`, `libpthread`, and `libdl`, and uses no
  symbol newer than `GLIBC_2.17`, so it runs on every glibc distribution since 2.17 and can `dlopen`
  R for the console.
- `default` is the native crane build against nixpkgs, for NixOS and flake users. Its console loads
  nixpkgs' R.

Every release derivation sets `allowedReferences = [ ]`, so an interpreter, a RUNPATH, or any other
store path in a release binary fails the build. The allocator is glibc's own, and the release
binary measured within noise of the native build (0.95 to 1.08 times its time on `ry check` over
rlang, ggplot2, dplyr, and testthat, and on `fmt --check`).

**Trap: release derivations cannot run their tests.** A release binary uses the FHS loader, which
the Nix build sandbox lacks, so a test binary linked the same way cannot start there. The native
package carries the test suite, and the release configuration is exercised outside Nix with
`cargo-zigbuild test --target x86_64-unknown-linux-gnu.2.17`.

**Rejected: a static musl binary.** It runs everywhere, NixOS and Alpine included, but a static
binary cannot `dlopen`, so it would lose `ry repl` and `ry run`; keeping the console in the release
binary decided it. Measurements for whoever revisits this: musl's own `malloc` made `ry check` 2 to
5 times slower; zig's libc, whose `malloc` is zig's `SmpAllocator`, restored glibc's speed with 13 to
23% more peak memory; jemalloc was 10 to 32% faster than glibc but compiles C through `configure` and
`make`; and mimalloc was 24 to 25% slower than glibc on the largest packages.

**Also rejected.** Static glibc, because its `dlopen` runs libR against a second, uninitialized libc
("Error parsing /proc/self/maps", "R home directory is not defined"). And a glibc download beside a
musl download, the shape uv and ruff use, because the directive is one release build per platform.

**Accepted cost.** The release binary does not run on musl distributions such as Alpine, or on
NixOS without nix-ld. NixOS users install the flake's `default`, and Alpine users build from source.

**Field practice.** rust-analyzer's main Linux binary targets glibc 2.28. uv and ruff default to
glibc builds (2.17 and 2.31) and fall back to static musl only where glibc is missing or too old.
rustc's Linux toolchain targets glibc 2.17.

## The benchmark harness compares test by test through `ry load`

**Status:** built.

- **Shape.** `oracle bench` (`crates/oracle/src/bench.rs`) runs a generated package's testthat suite as `ry load <package> --file bench.R`, once with `--runtime gnu-r` and once with `--runtime ry`. The loader plan is the same on both sides, so a difference is an evaluation difference, not a loading one. The driver (`bench.R`) writes one JSON line per `test_that()` block with its status, problems, a deparse of its value and its time.
- **Why per test, not per file.** A whole-file pass rate hides progress: one missing native fails a file of 30 tests. Per-test results also give the failure messages that the tally groups into fix classes.
- **GNU R runs twice.** A test whose value or problems GNU R does not reproduce (a UUID, a timestamp, a temp path) is compared by status alone, so only what GNU R itself reproduces counts against ry, and no hand-maintained ignore list is needed.

## Everything is fuzzable, simulatable and scriptable

**Status:** built.

This is a user directive: robustness is the long-run priority, so the whole toolchain is designed to
be fuzzed, simulated and driven by scenario tests. It widens the per-stage fuzzing record above from
the analysis pipeline to every component. `docs/src/content/docs/development/robustness.md` is the
contract.

- **Previous shape.** Each analysis stage had a seeded fuzz battery, but the language server could be
  tested only over a real socket with a real worker thread, so its interleavings (edits racing
  queries, cancellations, delayed watcher events) were untestable and unreproducible. The worker
  iterated a `HashSet` of open documents when refreshing, so the order of its publishes was
  nondeterministic. No harness compared a long editor session with a fresh start, and no test
  scripted a user journey across the editor, the CLI and the runtime.
- **Target shape.** Nondeterminism lives behind seams a seed drives. The server's worker sends
  through an `Outbox` (the socket, or a recorder) and its loop body is `run_job`, so
  `server::simulation` runs the real server single-threaded with a seeded schedule and injects
  cancellation at the Nth cancellable checkpoint (`cancel_at_checkpoint`), always followed by a real
  notification because the server assumes a newer edit is queued whenever it is cancelled. The
  oracle is a fresh server on the same disk and buffers, compared by input state, diagnostics, random
  requests, range geometry and the push contract. Expensive incremental cross-checks run behind
  `syntax::testing::enable_invariants()`. Scenario files script journeys through the simulator and
  the real binary. The file-system seam is a directory only the simulator writes, not an in-memory
  file system; injected IO errors are a gap.
- **Why a recorder and a step function, not a mock server.** The simulator must exercise the code an
  editor talks to. Two narrow seams keep production and simulation on one code path, and the
  simulated client sends real `lsp_types` notifications through the real `ServerState` handlers.
- **Harness modules ship in the library, not behind a cargo feature.** `server::simulation` and the
  crates' `testing` modules compile into release builds. A package's own integration tests see only
  its default features, so a feature gate would make the bounded passes opt-in, which breaks the
  rule that every harness runs in plain `cargo test`. They panic on a broken contract by design,
  since that is how a harness reports.
- **Impact.** Correctness: the simulator found divergences that unit tests and the existing fuzzers
  missed (a closed test file staying in the project, a lost `library()` rescan after cancellation,
  a formatting edit measured in bytes instead of UTF-16 units, and two splice reparses that
  disagreed with a full parse: an edit that opened a string the suffix closed, and one that left a
  `function(...)` whose body a full parse found in the suffix). Differential fuzzing against GNU R
  found three runtime bugs (NA against NaN propagation in `mean`, `sum`, `prod` and the cumulative
  functions, a zero-length replacement into a zero-length vector, and a top-level `nargs()`). Simplicity: two seams, no
  parallel implementation. Performance: none in production; the cross-checks are off unless a
  harness enables them. Incrementality: the from-scratch oracle now covers the server's own
  bookkeeping (open documents, attach sets, config, metadata), not only salsa queries.

## `just ry` builds ry's runtime; ry-lang does not depend on libr

**Status:** built.

- `ry run` loads ry's runtime (`libR.so`) beside the executable at run time, and libr is a separate
  cdylib, so `cargo run -p ry-lang` never builds it. `just ry` builds `-p ry-lang -p libr` first,
  and `just build` builds the whole workspace; the not-found error names both.
- A nightly artifact dependency (`artifact = "cdylib"`) was rejected: every `ry-lang` build,
  including checker-only and LSP work, would compile libr and Polars, and `cargo install` would
  still lose the library, because the artifact path points into the build directory. Embedding
  the library in the binary would double its size for users who never run code.

## Releases are built natively per platform, with the runtime beside the binary

**Status:** built, except the runtime beside the binary, which "A release is `ry` alone, with the runtime embedded and the R front end folded in" supersedes.

- **Before.** `just release` cross-built `ry` alone for three targets with nix and zig on one
  machine, and the archives held a single `ry`. That was fine while ry was only a checker, but
  ry's runtime is a second artifact (`libR.so`/`libR.dylib`) that links zlib, bzip2 and xz on
  Linux, which zig's glibc 2.17 sysroot does not carry, and macOS cross-linking has only
  libSystem stubs.
- **Shape.** `.github/workflows/release.yml` (staged in `.github/pending/workflows/` until a
  maintainer moves it) builds each platform on its own runner with `just package VERSION`,
  smoke-tests the archive with `tools/smoke-test.sh`, and publishes. `just package` is the one
  recipe CI and `just release-dry-run` share, so a local dry run exercises the same packaging. An
  archive is flat (`ry`, `LICENSE`, `THIRD_PARTY_NOTICES`), because the Zed extension expects `ry`
  at the archive root; the runtime is embedded in `ry` (see "A release is `ry` alone").
- **Targets.** Linux x86_64, macOS arm64 and Windows x86_64, the three the VS Code extension has
  always bundled. Linux arm64 and Intel macOS were dropped for the first runtime release, because
  their runtimes had never run anywhere; `ry.vsix` without a binary still serves them, and adding
  a matrix row brings them back.
- **Windows is `x86_64-pc-windows-gnullvm`, not MSVC.** The archive carries `ry.exe`, `R.dll` and
  `ry-rcmd.exe`, built with MSYS2's clang64 toolchain, so all three link the UCRT as Rtools-built
  package DLLs do and can share one C runtime with them. `just package` refuses a Windows binary
  that imports a toolchain runtime the archive does not carry.
- **The VS Code extension bundles the archive's `ry`.** `just build-platform-vsix` copies it into
  `editors/code/bin`, and `just build-extension` adds the combined `LICENSE` and the notices.
- **Publishing.** The GitHub release uses the workflow's `GITHUB_TOKEN`. The extension goes to the
  VS Code Marketplace only, by user decision; Positron and VSCodium users install the `.vsix` files
  attached to the GitHub release, and there is no Open VSX step or token. The Marketplace step uses
  trusted publishing (`vsce publish --oidc`, which needs vsce 4.1 and a policy on the publisher; packaging stays on the locked vsce 3.6, because 4.1 rejects bun's `node_modules` layout),
  because the only token the Marketplace accepts is an Azure DevOps global PAT, and those are
  retired on December 1, 2026. A `VSCE_PAT` secret, when set, is used instead, as a fallback.
- **One version per release.** `just bump-version` sets the CLI to `X.Y.Z-beta` and both
  extensions to `X.Y.Z`; the Zed extension's version used to be bumped by hand, which let it drift.
  The news page is the release record, and the stale `CHANGELOG.md` was removed so there is one.
- **Linux glibc floor.** Native builds on `ubuntu-22.04` need glibc 2.35, up from 2.17. Accepted
  for the beta; a manylinux-style container is the fix if users on older distributions ask.
- **Nix.** The flake's toolchain comes from `rust-toolchain.toml` (rust-overlay's
  `fromRustupToolchain`). `nix build` builds `ry` and libR, installs the library at
  `lib/ry/libR.so`, and wraps `ry` with `R_HOME` defaulting to nixpkgs' R, because a Nix-built
  binary cannot load another distribution's libR; a user-set `R_HOME` still wins. `ry-unwrapped` skips the wrapper. The zig cross outputs
  remain for the checker-only binaries.

## The bench templates are inline-typed

**Status:** built.

- **Previous shape.** The templates carried `#:` comment annotations, and `generate.py` stripped them with a regex for untyped runs. Comment annotations never reach ry's runtime, so the typed benches could not show what types buy at run time, and records and tags (which have no R spelling) could not appear at all.
- **Target shape.** The templates use inline syntax. An untyped run erases each generated file with `ry debug erase` (`syntax::erase_inline_annotations`, the same erasure `ry R` runs) and then drops the `#: @type` lines the erasure leaves, so there is one source of truth for the program and the erasure is exercised on 180,000 lines per run. Where typed code models data better with a record or a tag union than with S4, R6 or a named list, the template keeps both versions in `#@ typed` / `#@ untyped` / `#@ end` blocks; the untyped version stays the plain R the dependency set expects, and `generate.py` counts lines on it so both runs instantiate the same templates.
- **Impact.** The typed and untyped programs differ only where a record or tag replaces an R object system; everything else is the same program. `return()` had to accept native values (it passes its argument through, like `invisible()`).

## Seeded batteries replay through one loop

**Status:** built.

- **Previous shape.** Each battery looped over its own seed range and printed its own replay hint, if any, so replaying a failure meant reading the test to find how its seed was formed.
- **Shape.** `syntax::testing::for_each_seed(default, |seed| ...)` runs seeds `0..default` scaled by `FUZZ_ITERS`, runs only `FUZZ_SEED` when it is set, and on a panic prints `FUZZ_SEED=<seed> cargo test -p <package> <target> <test> -- --exact`. A battery whose seeds are offset (`1000 + seed`) keeps the offset inside the closure, so the printed seed is still the one to pass.

## Every seeded harness rotates through `FUZZ_SEED`

**Status:** built.

- **Shape.** Each case of a harness is one seed of `for_each_seed`, and a harness that shares one generator across cases builds it as `SplitMix64(CONST ^ seed)`. `tools/fuzz-deep.py` finds harnesses by reading the sources, resolves each against its binary's `--list`, and runs it with `FUZZ_SEED` at a fresh start and `FUZZ_DEPTH`, which scales each loop's own default count. A scaled depth, rather than one `FUZZ_ITERS` count, keeps the proportions of a test with several loops of different sizes and needs no budget read out of the source.
- **Why.** A single stream drawn across iterations neither replays one case nor moves to new inputs, so a deep campaign over it repeats the default run. Discovery from the sources means a new harness joins the campaign without being registered.

## CI runs `just gate` against GNU R 4.6.2, and nothing ignored

**Status:** built, except that the job sets up GNU R itself and waits on a pending workflow, which "CI runs in the flake's dev shell" supersedes: the job runs `just ci` in that shell.

- **Previous shape.** The live `ci.yml` used stable Rust, which cannot build the workspace (the toolchain is a pinned nightly for `allocator_api`), and tested only `crates/ry` through `default-members`. Its second job ran `-- --ignored` in a release build; once widened to the workspace that pulls in `ide::fuzz_deep` (about 55 minutes), the corpus benchmarks (which need a fetched corpus) and `format_directory` (which needs `RY_FORMAT_DIR`).
- **Shape.** One job runs `just fmt-check`, `just clippy` and `just battery`, the recipes `just gate` runs, with the toolchain read from `rust-toolchain.toml`, so CI and the local gate cannot diverge. GNU R is installed because the runtime's suites skip without an installation, which would leave the runtime untested. It is 4.6.2, the release the oracle's expectations are recorded with and ry bundles as its base R (the live `ci.yml` still installs 4.3.3 until `.github/pending/ci.yml` is accepted). The zlib, bzip2 and xz development packages are installed because libr links them. The ignored suites stay manual (`just fuzz-deep`, `just perf`), because none of them is a pass/fail gate that fits a push.
- **Triggers (user decision).** CI runs only on `workflow_dispatch`, never on push or pull request. The orchestrator starts it on main at its hourly check when main has new commits.
- **The tests that need no GNU R follow the same rule.** The staged `test.yml` starts by hand or from a release, never on a push ("The tests that need no GNU R run by hand and gate a release, never on push").
- **Impact.** CI covers every crate except `zed_ry` (wasm), at most once an hour on main, or on any ref when started by hand.

## The tests that need no GNU R run by hand and gate a release, never on push

**Status:** planned. Both workflow files are staged in `.github/pending/` and run nothing until the user runs `just accept`.

- **Triggers (user decision).** CI stays manual-only, to avoid flooding GitHub's infrastructure with runs: agents commit to main many times an hour. `test.yml` triggers on `workflow_dispatch` and `workflow_call`, and a `push` trigger is ruled out. Between releases the tests run locally (`just gate`, or `just test-fast` and `just fuzz-all 60 runtime` for the part that needs no GNU R) or when someone starts the workflow.
- **Previous source of truth.** `ci.yml` is the only workflow that runs a test, and it starts by hand. `release.yml` builds and publishes with no test, and the libFuzzer targets in `fuzz/` run only when someone types `just fuzz-all`.
- **What was weak.** Nothing stood between a push and a release. A regression in the fixture suites reached a tag unless someone had started the gate on that commit, and a fuzz target could stop building without anyone noticing.
- **Target shape.** `test.yml` has two jobs, each one `just` recipe, so each also runs locally. `tests` runs `just test-fast`: every test of the crates that build without the interpreter, libR and Polars (`syntax`, `format`, `semantics`, `ide`, `sniff`, `loader`, `heap`, `numeric`, `ssa`, `rdb`, `crash`, `oracle`, `rcmd`, `repl`, `ry-lang`). The job first checks that the runner has no `R`, so a test that compares with GNU R or runs ry's runtime skips instead of running against whatever R the image ships. `fuzz` runs `just fuzz-all 60 runtime`, every libFuzzer target that builds without Polars (15 of 17) for 60 seconds of wall clock each, and uploads a failing input. The bound is `timeout`, and a target interrupted at the bound has passed: libFuzzer's `-max_total_time` counts only after the whole corpus is replayed, which takes `ide` about 15 minutes for its 5,300 seeds, so it cannot bound a run. Seeds replay in random order, so a slow target reads a different sample on each run. Every target runs before the job fails. The fuzz crate's features are split for this: `runtime` is the interpreter's targets, and `polars` is `csv` (through `table`) and `unserialize` (through `libr`). Built with coverage instrumentation, Polars does not fit a 15 GB container with four build jobs (the compiler is killed on `polars-core` past 7 GB), so those two targets run only where someone builds them by hand. The file triggers by hand and as a called workflow: the staged `release.yml` calls it and `publish` needs it, so a tag whose tests fail publishes nothing. `nightly.yml` is not gated, because a nightly is a hand-started build of any ref and main may be broken.
- **Why a second workflow and not a job in `ci.yml`.** The full gate builds GNU R in the flake's shell and runs the oracle. `test.yml` needs neither Nix nor R, so it is the part a release can afford to wait for on a hosted runner, and the part a maintainer can start for a quick answer.
- **Impact.** A release cannot publish while a fixture suite, a test of those crates or a fuzz target fails. Nothing in CI catches a regression between releases unless someone starts a workflow, so the local gate is the check that runs on every change. `test.yml` does not replace the gate, because the runtime, the oracle and everything else that compares ry with GNU R are tested only there.

## Optimized tests build with the bench profile, not fat LTO

**Status:** superseded by "Build profiles: release for numbers, iterate for loops". Kept for why optimized tests avoid fat LTO and for the dev profile's `line-tables-only` debug info.

- **Previous shape.** Optimized tests ran as `cargo test --release`, which builds with the release profile and so with fat LTO (`lto = true`, one codegen unit). Its link of libr's `test_linpack` peaked at 11.1 GiB of RSS and took about 30 minutes; on a 16 GB machine with anything else running it was OOM-killed.
- **Why not change release.** Shipped binaries and every benchmark number in the docs come from `cargo build --release`, and fat LTO is part of what those numbers measure. Cargo always builds `cargo test --release` with the release profile, so tests cannot get a different setting under that command.
- **Measured** (`cargo test ... -p libr --test test_linpack --no-run -j4` from a clean profile directory, peak RSS of the largest child process): fat LTO 11.08 GiB in 1,797 s, the bench profile 3.60 GiB in 685 s.
- **Target shape.** `[profile.bench]` inherits release with `lto = false` (thin local LTO) and 16 codegen units. `cargo test-release` (`.cargo/config.toml` alias for `cargo test --profile bench`), `just perf`, `just simulate` and `cargo bench` use it. Plain `cargo test --release` still works on a machine with the memory.
- **Dev debug info.** `[profile.dev] debug = "line-tables-only"`: libr's `test_linpack` went from 0.28 to 0.11 GB and the debug `libR.so` from 1.03 to 0.85 GB. An interp test binary barely shrinks (0.88 to 0.78 GB), because its `.text` (185 MB) and relocations dominate, not debug info.

## How bench-real measures

**Status:** superseded by "Four published benchmark suites, one runner". Kept for the measured cause of the noise and the rejected estimators, which explain why the runner samples in paired rounds.

- **Previous shape.** `tools/bench/bench.py` ran each engine three times in a block (all GNU R runs, then all ry runs) and divided median by median. Two pull requests measuring the same commit each timed GNU R on their own and reported the awfy geomean as 1.90x and 1.58x; awfy/bounce went from 1.53x to 1.12x because GNU R's bounce moved 31% between the runs while ry's moved 4%.
- **Cause.** The cloud VMs' effective CPU speed drifts with other tenants, and the drift is invisible as steal time or load average (steal at most 1.3%, its correlation with a run's deviation -0.16). A fixed C integer loop took 0.30 to 0.39 s across processes on a quiet VM; `taskset` and `setarch -R` do not narrow it. Ordering, the cold run and frequency scaling (no cpufreq in the guest) are not the cause, and no awfy program is short (all take over 0.4 s).
- **Chosen shape.** Rounds: each round runs every engine once in a fresh process, in an order shuffled from `--seed`, and ratios are formed within a round, so a drift longer than one round cancels. The table prints the median per-round ratio with the lowest and highest round, and each geomean carries a 95% bootstrap interval over rounds. `--repeat` is at least 5. `--baseline PATH` puts a second ry build in the same rounds, which is the only valid way to compare two builds; never compare suite numbers from separate sittings. Rows under 0.1 s on any engine are flagged and left out of the geomean (`SHORT` in `run.py`).
- **Rejected.** Pinning cores and disabling ASLR (no effect on this noise). The fastest of N runs as the estimator: within a sitting it varied as much as the median of paired ratios (about ±3% across sittings on awfy), and it cannot be paired.
- **Measured noise after the change** (5 rounds, one sitting, 95% interval of an old build against main, JIT on): representative mix 0.97 to 1.04, awfy 0.92 to 1.07, rcp 0.95 to 1.06, shootout 0.98 to 1.03. Short rows: rcp empty-while and scalar-while, shootout spectralnorm and spectralnorm_naive.
- **Docs.** `benchmarks.mdx`, "Timing the real-world workloads", carries the measured noise per suite.

## Published benchmark numbers come only from the results files

**Status:** built, except the renderer-side too-short floor and workload rule, which "Benchmark suites are directories the renderers only display" supersedes.

- **Previous shape.** The benchmarks page and READMEs mixed rendered tables with hand-typed figures in prose. Each `run.py` rewrote its own README block. Workloads counted as too short when any engine took under 0.1 s, and kernels when GNU R did. Our in-house programs were labelled "real-world".
- **Target shape.** `docs/src/lib/benchmarks.ts` (site) and `tools/bench/readme.py` (READMEs) render every number, with the same rules: one too-short floor (GNU R under 0.1 s, left out of every geometric mean), only `kind == "workload"` rows count as application workloads, and every chart, table, summary and README block carries the metadata line (date, ry commit, GNU R version, machine, rounds). `just bench-readme --check` in `just gate` catches drift. The suites are named "Numeric kernels" and "Application workloads", both marked "(ours)", and typed is a column, not a suite.
- **Why GNU R's time.** It measures how much work a program does. A short ry time is the speed-up being measured, so flagging it would drop `fib` and every typed loop for being fast.
- **Impact.** Prose names categories rather than figures, so a fresh `--write` cannot leave the text contradicting the charts.

## Benchmark suites are directories the renderers only display

**Status:** built, except the list of four suites, which "The typed suite scores typed ry against Rust" supersedes.

- **Previous shape.** The renderers knew two suites by path (`why/`, `real/`) and their own row shapes, judged too-short programs by GNU R's time and computed every geometric mean. Program names lived in the page and the renderer code.
- **Target shape.** A suite is `tools/bench/<suite>/` with `suite.toml` (id, name, summary, optional groups and harness, and per program a display name, summary, `source` and optional `typed_source`) and a schema-1 `results.json` (meta, engines, per-program ratios with intervals, flags and geometric means). The page and the README list suites by globbing those directories, in the fixed order public, packages, kernels, applications; a suite without results gets no row and no page section, so the page never announces a suite it cannot show. `suite.toml` is the only source of labels.
- **The runner judges, the renderers display.** Too-short, refused-typed and failure flags and the geometric means come from the runner, so the site and the README cannot disagree about which programs count. This supersedes the renderer-side GNU R floor of "Published benchmark numbers come only from the results files". The runner flags a too-short time per engine, but only a GNU R time under `meta.too_short_seconds` removes a program from the geometric means, so the renderers fade a bar and print "too short" beside a ratio only for that case (`isTooShort`); a fast ry time is the speed-up being measured.
- **One schema, no adapters.** Both renderers read schema 1 only and fail on anything else; the adapters that mapped the older `why/` and `real/` files went with those directories. A package `failures` entry renders as "fails on <engine>: <reason>" in its chart row, table cell and program list, with no bar. `results.json` stores `engines` as a key-to-label map, and the labels shown are the runner's. The method table shows `just bench <suite> --write` rather than the recorded `meta.command`, which holds the measuring machine's paths.

## Four published benchmark suites, one runner

**Status:** built, except typed as a column only, which "The typed suite scores typed ry against Rust" supersedes.

- **Previous shape.** Benchmarks lived in `tools/bench/why` (typed kernels with untyped twins) and `tools/bench/real` (workloads plus the public programs and the generated packages' tests), each with its own runner, results format and sampling (best time over repeats in one, medians of three rounds in the other). The package corpus had a third runner (`packages/run.py`) and its own README table. Typed versions lived in a parallel folder, so a program's two forms could drift apart in size.
- **Target shape.** Four published suites, named the same everywhere: Public R JIT benchmarks (`public`, the 54 Ř/rcp programs), Package test suites (`packages`, the pinned CRAN packages that have tests), Numeric kernels (`kernels`, ours) and Application workloads (`applications`, ours). Each is a folder `tools/bench/<suite>/` with a `suite.toml`, the single source for the suite's name and summary and each program's display name and summary, and a `results.json` in one shared schema. Typed is a way of running, not a suite: a typed program is `<name>.typed.R` beside `<name>.R`, and the engines are GNU R, ry and ry typed (strict). One runner, `tools/bench/bench.py` (`just bench <suite>`), times every suite. `internal/` keeps unpublished benchmarks (base, stdlib, capi, table) and `tooling/` the code-base generator, `check.py`, LSP latency and the corpus installer (`tooling/packages.py`).
- **Method.** Paired rounds: every round runs each program on every engine, in a shuffled engine order and a shuffled program order, so slow machine drift hits all engines alike. A program's ratio is the median of its per-round ratios (GNU R time over engine time), with a 95% bootstrap interval over rounds; a suite's geometric mean resamples each program's rounds. `--write` needs at least 10 rounds and every program, and a program whose interval reaches more than 10% from its median gets 10 more rounds first.
- **Gates and flags.** A crash or a checksum that differs from GNU R's fails the run and writes nothing; numeric checksums may differ by a relative 1e-10, because a typed program may sum in another order. Package suites gate on neither, since a package's own test suite is the program and its result on ry is the measured gap: differing outcomes are a `tests_differ` flag, and a suite that crashes or does not finish on an engine is a failure recorded in the package's row, which leaves it out of the geometric mean. A test script (a package without testthat or tinytest) that exits with status 1 ended in an R error, which is how such a script reports failing tests (`test.data.table()` raises its summary after running every test), so it counts as one failed test and the remaining scripts still run; any other non-zero status, a signal or the suite timeout is a failure. The cost is that a script stopped early by a ry bug also counts as finished, with a shorter time, but its `tests_differ` flag shows it. A package that also fails on GNU R shows a `gnu_r` failure, so a broken pin is told apart from a ry gap. `too_short` flags an engine's median under 0.1 s, but only a GNU R time under 0.1 s leaves a program out of the geometric means (GNU R's time measures the work). `typed_refused` records what `RY_TYPED_LOG` reports for the typed program beyond the harness alone.
- **Kernels against applications.** A program is a kernel when its time goes into arithmetic on numbers and numeric vectors, and an application workload when it goes into lists, attributes, dispatch, strings and tables. `simulation` moved to kernels and the untyped `lapply` duplicate of `closures` was dropped.
- **Impact.** One schema means one renderer (`docs/src/lib/benchmarks.ts`, `tools/bench/readme.py`) for every chart and README block, and `just readme --check` keeps them in sync with the results. The numbers become reproducible within their intervals, which lets a PR claim a speed-up only when the intervals separate.

## Flake inputs are git URLs

**Status:** built, except "No workflow uses the flake", which "CI runs in the flake's dev shell" supersedes.

- **Previous shape.** `flake.nix` took its inputs as `github:owner/repo`, which nix fetches as archive tarballs from github.com, codeload and api.github.com. A cloud agent session reaches GitHub through a proxy that serves git clones of public repositories but answers those tarball URLs with 403, so `nix develop` could not start in any cloud session.
- **Target shape.** Every input is `git+https://github.com/owner/repo?shallow=1` (nixpkgs with `ref=nixos-unstable`), and `flake.lock` keeps the same revs and narHashes, so every pinned tree is byte-identical; devshell's nixpkgs now follows the root nixpkgs, which drops its separate pin. nixpkgs fetched as a shallow git clone took 16 s against 29 s for the channels.nixos.org tarball, and only the git form pins a commit. No workflow uses the flake, and git works on GitHub's runners too. Do not switch back to `github:` inputs: the cloud sessions break again.

## Cloud sessions are provisioned by tools/dev-setup.sh, not the nix shell

**Status:** built.

- **Previous shape.** Each session installed what it found missing by hand: an apt `just` too old for the justfile, a distribution GNU R without memory profiling or cairo (so the Rprofmem and graphics oracle cases differed), missing oracle packages, missing apt libraries for the corpus.
- **Target shape.** `tools/dev-setup.sh` (idempotent, so an environment's setup script can run it) installs the apt libraries, `just` 1.59.0 from PyPI, and, through `tools/reference-r.sh`, the reference GNU R 4.6.2 in `/opt/R/4.6.2` with the documented configure flags and the oracle's packages with rlang 1.3.0. `just doctor` (`tools/doctor.py`) checks each, plus binary freshness by recomputing the runtime identity, and names the fixing command. Building R from source takes about 3.5 minutes on four cores; the binary cache cuts that to seconds ("Prebuilt trees come from the ry.cachix.org binary cache").
- **Two details that look optional and are not.** `--enable-R-shlib` changes no output, but without `libR.so` ry's GNU R console cannot load that R and Posit's binary packages cannot link. The packages come from Posit's Ubuntu binaries, which the mirror serves only to a user agent starting `R/<version>` (R's default lacks it): about 10 s for the set instead of 5 minutes of compiling.
- **Why not nix for cloud sessions.** The script restores the prebuilt reference R with all the oracle's packages from the binary cache in seconds, needs no nix store in the container's 40 GB quota, and shares the rustup toolchain with every other tool. CI and anyone reproducing it use the flake's `ci` shell instead ("CI runs in the flake's dev shell"), whose GNU R is built the same way.

## CI runs in the flake's dev shell

**Status:** built.

- **Previous source of truth.** `.github/workflows/ci.yml` set up the test job itself: `r-lib/actions/setup-r` installed Posit's ubuntu-2604 build of GNU R 4.6.2 (OpenBLAS, GCC 15, libcurl 8.18), wrote a `~/.Rprofile` and exported `R_LIBS_USER`, and further steps installed rlang, apt libraries and locales. That R was not the one the oracle recorded, so CI failed on differences between the two builds rather than on ry bugs, and every change to the job needed the user's `just accept` because the agent token cannot write workflows.
- **Target shape.** `flake.nix` builds `lib.gnuR` the way `tools/reference-r.sh` builds the reference R on Ubuntu 24.04: the same configure line; GCC and gfortran 13.3 (from a pinned nixos-24.11, wrapped against the current nixpkgs' glibc); Ubuntu's default hardening flags instead of Nix's, and a position-independent `R` executable; Ubuntu's own `liblapack.so.3.12.0` and `libblas.so.3` debs; glibc's `*pi` functions and `pown` hidden from configure; Debian's `which`, `less`, `zip` and `unzip` recorded in Renviron; rlang 1.3.0 in R's own library. The `ci` dev shell adds the pinned Rust toolchain, puts that GCC first on PATH (the toolchain propagates nixpkgs' GCC 15), links zlib, bzip2, xz, LAPACK, BLAS and libgfortran (libR and `just base-r`'s Matrix), points `LD_LIBRARY_PATH` at those LAPACK and BLAS (ry dlopens them by soname, and Nix's loader never searches `/usr/lib`), and sets `TZDIR` and a locale archive with the oracle's locales. `just ci` runs the job's steps (`R_HOME`, locales, base R, rustfmt, clippy, tests), and `.github/workflows/ci.yml` only installs nix, reads ry.cachix.org (pushing the R and the shell) and runs `nix develop .#ci -c just ci`.
- **Each pin answers a measured difference.** Against `crates/oracle/cases`, nixpkgs' GCC 13.4 stops vectorizing complex addition, which swaps NA and NaN (`kernels__complex_na_nan_sums`); a non-PIE `R` puts the heap at low addresses, so a case that reads a heap pointer as a vector length gets a short vector instead of a long-vector error (`base__replacement_promises_and_errors`); GNU `which` prints absolute paths (`system__sys_which`); no pager breaks `file.show` (`tools__help_text`); without a `libblas.so` link configure falls back to R's internal LAPACK, which changes `La_library()`.
- **Impact.** CI's GNU R matches the recordings: `ORACLE_R=<the shell's R> just oracle verify` passes all 440 cases (17 skip for packages the shell does not install, as on the old CI). Agents change CI by editing the `ci` recipe or the flake. The `ci` shell's closure is 2.3 GiB; GNU R builds in about 3.5 minutes on four cores when the cache misses. In a four-core container `nix develop .#ci -c just ci` takes about 43 minutes cold (summed over a clean-clone run that a full disk interrupted) and about 31 warm: running the tests takes 23 of those, and the steps before them take 8 even when warm, because `just base-r` rebuilds base R on every run. The battery's `target/` reaches 22 GB, so the workflow's free-disk step matters.

## Workspace-wide feature unification

**Status:** built.

- **Previous shape.** Cargo resolved features per command, so `cargo build -p ry-lang` and `cargo test -p interp` compiled libc, tokio, anyhow and futures-io with different features, and every crate above them, polars included, into a second tree: 61 crates and 203 s after a ry build, and gigabytes of disk per extra tree.
- **Target shape.** `.cargo/config.toml` sets the nightly `resolver.feature-unification = "workspace"`, which unifies the normal dependencies of every member, and `crates/features` is a member nothing links whose normal dependencies carry the features only dev-dependencies turn on (libc `extra_traits`, tokio `process`, tokio-util `compat`, anyhow and futures-io defaults). `cargo --unit-graph` shows the same dependency units for the ry build and for `cargo test -p interp`, `-p libr` and `-p semantics`; a full workspace test adds only separate build-script units (tree-sitter's host serde_json), which no workspace crate links.
- **Impact.** Switching between building and testing in one profile rebuilds only workspace crates. A new dev-dependency that brings a feature reopens the split, and the comment in `crates/features/Cargo.toml` says how to spot it.

## Prebuilt trees come from the ry.cachix.org binary cache

**Status:** built.

- **Previous shape.** Every fresh container built the reference GNU R from source (about 3.5 minutes on four cores) and installed the package corpus, and a container restart threw both away: a session that lost `/opt/R/4.6.2` could not run the oracle until someone rebuilt it.
- **Target shape.** `tools/cache.sh` packs a finished tree (the reference R with its packages, or the corpus library) as a zstd tarball, turns it into a store path with `nix store add-file`, and pushes it to the public cache `ry.cachix.org`. `tools/cache.lock` maps a hash of everything the tree was built from (the build script, the pinned versions and checksums, the Ubuntu release and architecture) to that store path. `tools/reference-r.sh` and `just bench-packages install` restore a tree whose hash is in the lock before building anything: the reference R restores in 5.5 seconds against 3.5 minutes to build, and the corpus (119 packages) in 8.8 seconds against 8 minutes to install. Reading needs no token; a cloud session's proxy injects the write token for `app.cachix.org` only, so pushes pass `--host https://app.cachix.org`.
- **Why tarballs and not nix derivations.** The trees must be the Ubuntu builds the oracle was recorded with ("Cloud sessions are provisioned by tools/dev-setup.sh"), so nix only stores and verifies them. A store path is the hash of the tarball's bytes, which a rebuild never reproduces, so the lock has to record it.
- **Why no CI workflow pushes the trees.** A workflow that rebuilt and pushed on `main` would then have to commit the new lock back to `main`. Instead, whoever changes a tree's inputs has built it already, runs `just cache-push <tree>` and commits the lock in the same change; `just doctor` warns while the lock has no build of the current inputs.
- **Release builds of the flake share the cache (user directive).** `.github/workflows/cachix.yml` builds `.#ry` and the dev shell for x86_64-linux at each published release and nightly and pushes them, so flake users who name a release tag substitute instead of compiling; `flake.nix`'s `nixConfig` names the cache. Nix derivations need no lock, because a store path follows from its inputs. Main builds are not pushed: the cache holds 5 GB, and a build per `main` push would crowd out the trees above. The workflow also filters out crane's `ry-deps-*` build, which no install reads.
- **Why `workflow_run` and not only `release`.** Release and Nightly publish with `GITHUB_TOKEN`, and GitHub starts no workflow for an event that token causes, so their releases never fire `release: published`. The workflow follows their completed runs and builds only when the run's `publish` job succeeded (a hand-started Release run with `publish` unticked publishes nothing); `release: published` covers a release a person makes.

## Build profiles: release for numbers, iterate for loops

**Status:** built.

- **Previous shape.** `[profile.bench]` (release without fat LTO) served both `cargo test-release` and `cargo bench`, and `--profile bench` writes into `target/release`, so a bench-profile binary could overwrite a release one and its numbers were quoted as release numbers. `profiling` kept full debug info for workspace crates under fat LTO, and its link ran out of memory on the 16 GB cloud machines. Nothing in a binary said which profile built it.
- **Target shape.** `release` (fat LTO) is the only source of published or quoted numbers. `iterate` is release with `lto = false` and 16 codegen units, for `cargo test-release`, `just perf` and `just simulate`: after an edit to interp it rebuilds `ry` and `libR.so` in 2m06s (2.1 GB peak) against release's 5m51s (6.3 GB peak), both measured while `interp` was one crate ("The interpreter is an engine crate between a numeric crate and four leaf crates" has the loop since), but single kernels run from 24% faster to 28% slower than release (geometric mean 4.5% slower), so it is not a stand-in for release. `crates/ry/build.rs` appends the profile to the version of any non-release build (`ry 0.4.3-beta (iterate build)`), and `tools/bench/bench.py` refuses a binary whose version names a build. `profiling` is release with `debug = "line-tables-only"` and the same fat LTO: its link peaks at 10.8 GB, and its `.text` is within 0.2% of release's (`size -A`), so profiles show release's inlining. `[profile.bench]` is gone, so `cargo bench` inherits release.
- **Why not thin LTO for profiling.** Thin or no LTO changes which calls inline across crates, which is exactly what a profile of the interpreter must show; the measured iterate spread (up to 28% per kernel) is that difference. Line tables alone bring the fat-LTO link within the machine.

## Worktrees keep their own target directories and share sccache

**Status:** built.

- **Previous shape.** Each worktree or fresh clone compiled every dependency into its own `target/` (a cold debug build of `ry` takes 4m28s on four cores), and sharing one `CARGO_TARGET_DIR` between worktrees made concurrent builds block on cargo's lock and rebuild each other's workspace crates.
- **Target shape.** `tools/dev-setup.sh` installs sccache and sets `build.rustc-wrapper = "sccache"` in the user's `~/.cargo/config.toml`, so dependency compilations land in one shared cache (219 MB for the debug dependency set) while target directories stay per worktree and `just prune` trims them. A cold debug build in a fresh target directory with a warm cache takes 1m13s (439 of 441 compilations hit). sccache does not cache incremental compilations, so an edit to a workspace crate rebuilds exactly as before (12.0 s with the wrapper, 13.7 s without, for a debug rebuild after touching interp), and turning the wrapper on rebuilds nothing.

## Published benchmarks measure the release archive

**Status:** built.

- **Previous shape.** `just bench <suite> --write` recorded a plain `cargo build --release` of the working tree. The Linux release is profile-guided and laid out by BOLT (`tools/layout/optimize.sh`), so the published numbers measured a build nobody downloads. At one commit, the shipped archive beat a plain build of it by 1.12x on `lapply` (process wall) and 1.30x on the validation workload (median warm run), medians of 15 interleaved runs each.
- **Target shape.** `bench.py --release ARCHIVE` unpacks the official Linux x86_64 archive into a private directory, points `XDG_CACHE_HOME` there and clears `RY_LIBR`, `RY_BASE_R` and `R_HOME`, so the archive's `ry` extracts and loads only its own runtime and base R. `--write` requires `--release`, refuses a nightly, and stores `meta.release` (tag, archive name, sha256) plus the commit the tag names. Recording happens once per release, right after the tag.
- **Why not reproduce the release locally.** The full `optimize.sh` does not fit a 15 GB machine without swap: the instrumented fat-LTO link of `ry` alone reaches about 14 GB. CI builds it on its runner, and the archive it uploads is the artifact to measure, so there is no stand-in to keep in sync.
- **Images.** `refuse_foreign_images` reads an embedded runtime's identity through the R home it extracted (`.libr` names the library), so the archive's shipped images pass and any `.ryc` another build wrote into a package library stops the run.
- **Change gating is separate.** A change is still judged on plain release builds by `Ir`, with the no-op control for layout effects.
- **Pinned by** `tools/bench/test_bench.py` (a fake archive), run from `test_release_metadata::the_benchmark_runner_records_the_release_archive_and_refuses_foreign_images`.

## A fatal signal prints one report from the executable, which asks the runtime for the R stack

**Status:** built.

- **Previous shape.** A crash of ry's runtime left no trace: no handler existed for SIGSEGV, SIGBUS or SIGABRT, so a CI log ended at the last line the program printed. GNU R's runtime installs its own handler, which prints its traceback.
- **Chosen shape.** `crates/crash` installs the handlers once, first thing in `ry`'s `main`, so every command, front end and `ry test` worker has them. The R call stack lives in `libR.so`, which the executable loads later, so the library exports `ry_crash_traceback` (`interp::crash::write_traceback`) and `repl` registers it with `crash::set_traceback` after loading. One handler and one copy of the signal code serve both sides; the runtime only reads its frame stack. The handler passes the runtime a writer and a memory probe (a `write` into a pipe, which fails with EFAULT where a load would fault), so a corrupted frame stack ends a line of the report and not the report.
- **Details a change must keep.** The handler runs on a 256 KiB `sigaltstack` of its own (a stack overflow reports), allocates nothing, and uses `SA_NODEFER` so a fault inside the report re-enters and prints the native stack instead of being killed by the kernel. After the report it calls the handler installed before it (Rust's stack overflow message) and then restores `SIG_DFL`; a signal a process sent (`kill`, `abort`) is raised again, a hardware fault repeats on return, so the exit status and the core dump are unchanged. The native stack is glibc's or macOS's `backtrace_symbols_fd` (no allocation), warmed once at install because the first call loads libgcc. Release binaries are stripped, so frames read `module(+offset)`.
- **Left out on purpose.** SIGILL and SIGFPE (not asked for, and GNU R's handler covers SIGILL only), a Windows filter (backlog), and the GNU R runtime, which replaces the handler when it starts and prints its own report.
- **Pinned by** `crates/crash/tests/test_crash.rs` (each signal, a stack overflow, a probe, a faulting printer) and `crates/ry/tests/scenarios/crash-report.scenario` (R function names and a native frame from a real `ry run`).

## Platform code is type-checked from Linux before the release job builds it

**Status:** built.

- The release job is the first native build of macOS and Windows code, and a round there costs most of an hour, so platform code is type-checked from Linux first with `cargo clippy --target aarch64-apple-darwin` and `cargo clippy --target x86_64-pc-windows-gnullvm`.
- Nothing links in a clippy run, so no SDK is needed. The C build scripts do run, so `CC_<target>` and `CXX_<target>` point at a clang wrapper that passes `--target`, `AR_<target>` is `llvm-ar-18`, and `WINDRES` is `llvm-windres-18`. Empty objects are enough.
- The check finds the usual platform traps: `pipe2`, `sched_setaffinity` and `sched_getaffinity`, `cpu_set_t`, and Apple's `iconv` signatures.
- A real macOS cross-build uses `clang --target=arm64-apple-macos11 -isysroot $SDKROOT -fuse-ld=lld`. zig's Mach-O linker ignores `-exported_symbols_list` and exports every symbol, so a build linked with zig cannot show a missing export. The Windows CLI alone type-checks with `zig cc -target x86_64-windows-gnu` as the C compiler.

## The dev shells' GNU R is cached on every change to its inputs

- **Previous shape.** Only releases pushed the flake to ry.cachix.org, and the CI job pushed the R only when it happened to build it, so a fresh `nix develop` could compile `lib.gnuR` from source.
- **Target shape.** `nix develop` (`default` and `ci`) downloads everything from `cache.nixos.org` or `ry.cachix.org`. `.github/workflows/cachix.yml` also runs on a push to main that touches `flake.nix`, `flake.lock` or `tools/base-r/**` (the only inputs of `lib.gnuR`), builds just the two shells, and pushes with the filter that leaves out the Rust toolchain and `ry-deps-*`. The `ry` package stays release-only. Because the path filter keeps it rare, it does not spam CI, and the 5 GB cache holds only paths nixpkgs lacks.

## The hot suite replays the package entries the code bases spend their time in

- **Previous state.** Runtime performance work was measured on the application workloads, the package test suites, the public JIT suite and the numeric kernels, whose geometric means (about 3.0x, 2.4x, 3.5x and 5.5x GNU R) disagree with the three benchmark code bases (about 1.5x). The code bases are the goal, and one run of them takes 60 to 135 s per engine, so no suite was both fast and representative.
- **Target shape.** `tools/bench/hot/` (`just bench hot`): 100 programs, each one package entry with up to three captured calls of it, replayed by `harness.R` as `eval(call, env)` in batches of about 20 ms of GNU R time, one process per engine. `tools/bench/tooling/hot.py` generates it (`capture`, `validate`, `select`) and checks it (`track`); `testing.md`, "The hot suite", is the user-facing description.
- **The unit is a package entry: a closure call at the boundary from user code into package code**, keyed by the callee's namespace and the name it was called by. That is the granularity at which a code base's time can be both attributed (inclusive time of package code) and replayed (arguments are values the user code built). Rejected: hot closure bodies anywhere in the call tree, because their arguments are promises into package frames that a replay cannot rebuild, and their times nest.
- **User code is code that evaluates in an environment whose top-level environment is the user's, however it was entered** (`capture::evaluating`, called from promise forcing, `eval`, and libr's `Rf_eval`, `R_tryEval` and `R_ParseEvalString`). Counting only closures of the user's namespace charged every `test_that()` block and every argument promise to the package entry that forced it, so `testthat::test_that` and `withr::with_tempdir` held most of the run as entries no replay could rebuild: the captured share of the three runs was 68%, 50% and 41%, and is 80%, 71% and 72% with the rule.
- **A weight is a ratio estimate.** The capture measures ry's time with the recorder on and the JIT off, and a weight needs GNU R's time. A key's GNU R time is its package seconds in the capture (captured and uncapturable calls) times GNU R's seconds over the capture's seconds for the same sampled calls. The recorder draws a key's samples uniformly (reservoir), so the configuration's overhead cancels. A code base's predicted speed-up is the weighted harmonic mean of its programs' speed-ups.
- **The recorder keeps the draw uniform and its own cost out** (`crates/interp/src/capture.rs`). The reservoir draws from every capturable call of a key and serializes only the selected ones; a selected call too large to store empties its slot (counted in `oversize`) instead of leaving the earlier sample, which would bias the samples toward small calls. The recorder's own work runs suspended (no recording, no interrupts) and its duration is subtracted from every open record and from the total, so it neither inflates the entries nor the ratio estimate. Times are integer nanoseconds with the unit in the column name (`nanoseconds`, `uncapturable_nanoseconds`, `total_nanoseconds`, ...): decimal seconds with six places keep one significant digit of a 2 µs call. A failure inside the recorder (collecting a call's names, inspecting its bindings) makes the call uncapturable with reason `uninspectable` and never becomes an R error, because a measurement tool must not change the program it measures. A forked child (`parallel::mclapply`) stops recording in a `pthread_atfork` child handler, the one point every fork passes, and writes nothing, so it cannot overwrite the parent's files.
- **Every replay runs in the code bases' session**: all imported packages loaded first (`session` in `suite.toml`), testthat's third edition, handlers that continue expectations and muffle messages and warnings. Loading only each program's own namespaces made results depend on program order: `zoo::rollapply` on an xts series pads differently once xts is loaded, and ompr finds no solver before `ROI.plugin.highs` loads.
- **A checksum hashes everything exact and sums the numbers** (`replay.R`, `checksum`; `bench.same_checksum_part`). The hash covers types, lengths, attributes at every depth (but data.table's `.internal.selfref`, an address), text, logical values and each numeric vector's NA, NaN, Inf and -Inf counts. Every numeric vector at any depth adds its log weighted size and its weighted sum over that size, both weighted by its position among the vectors with the golden-ratio sequence `1 + frac(k * phi)` (no short period, unlike a weight modulo a small number), to two fields compared to an absolute 1e-10 or a relative 1e-13. One tolerant comparison at every depth replaced hashing nested numbers rounded to twelve significant digits, where a last-bit difference that crosses a rounding boundary fails the program. The tolerance exists for `power.t.test`, which finds its root through `uniroot` on `pt(ncp =)`, where ry differs from GNU R by a few ulp, one part in 1e13 of the root. A sample whose value differs between two GNU R calls is marked `varies` by the validator and not compared.
- **Captured temporary paths are relocated, and a sample may write only under its temporary directory** (`replay.R`, `relocate`, called by both `validate.R` and `harness.R`). A capture names files under its own session's `/tmp/RtmpXXXXXX`, which is gone or belongs to another process by replay time: writers failed, and the ones that ran wrote into that foreign directory. Relocation rewrites every string under an `Rtmp` directory to lie under the replay's `tempdir()` and creates the parent. The validator runs GNU R with `TMPDIR` set to a directory of its own, watches it (`include.dirs = TRUE`), and drops a sample that touches any other path or whose second call leaves another set of paths than its first, so files cannot pile up across thousands of timed calls. A sample that opens a graphics device is dropped too.
- **A sample whose call changes its own arguments gets a fresh copy for every call** (`fresh` in `suite.toml`). The validator serializes the variables, replays once with the JIT off (so the comparison does not see bytecode attached to a closure), and compares. Without the copy, a `:=` or `set*` call in data.table replays on the result of the previous call, so the batch times other work than the capture did and its checksum depends on the number of iterations. The harness unserializes one environment per call before each batch, outside the clock and the callgrind region; the validator's calibration prepares and times the same way, as the median of five batches like the runner.
- **The deterministic metric is callgrind instruction counts per program region** (`--instructions`), because the measuring VMs expose no hardware counters (`perf_event_open` fails). `.ry_bench_region(name)` toggles instrumentation and dumps per region; a dump written after instrumentation stops carries its count on the `totals:` line, and `summary:` reads zero. The same builtin scopes the sampling profiler for `--breakdown`. The pass deals the programs over four harness processes by position (`bench.INSTRUCTION_SHARDS`, never the core count, so a program has the same process neighbours on every machine) and runs each on one OpenMP thread, because callgrind counts the spinning of idle worker threads: a grouped data.table `:=` counted 80 to 96 million instructions on four threads and 45.8 million on one. Two passes over one build agree to 0.01% on a code base's weighted total, 0.013% on the median program and 0.41% on the worst (`brio::write_lines`). The whole suite takes about 80 s.
- **The suite tracks the code bases at r = 0.976 over 33 changes** (`hot.py track`: eleven configurations against a twelfth on three code bases, from 13% faster to 124% slower by the suite's prediction; per code base r is 0.986 to 0.987). A predicted change shows up 0.85 times in the code bases (0.78 finbench, 0.85 dsbench, 0.90 biobench), because the programs hold 77 to 80% of a run and the rest changes less.
- **An instruction change in the suite shows in a code base's count with the same sign.** dsbench's whole test run under callgrind (about 65 minutes; `valgrind --tool=callgrind` around the ry process `oracle bench --ry-only` starts) cost 318.11 billion instructions, 317.45 billion with the JIT off (-0.21%; the suite's dsbench-weighted total said -0.27%) and 322.91 billion with the builtin fast paths off (+1.51%; the suite said +2.95%). Each configuration ran once and the count covers the whole process, so the repeatability of a code base's count is unmeasured.
- **The predicted level is pessimistic and is not the number to read**: 1.34x, 1.39x and 1.47x predicted where the code bases measured 1.62x, 1.68x and 1.62x in the same session. The cause is open (`backlog.md`, "Open: the hot suite"). Read changes between predictions, never a prediction against a code base's time.
- **Wall time on a shared VM cannot show a change under about 10%.** The same build timed twice, 100 minutes apart, was 14 to 16% slower on the code bases and 21 to 27% slower in the suite the second time, and within one run a program's median batch time varies by about 20% between identical runs on either engine. Wall-time comparisons therefore time both builds in the same rounds (`--baseline`), `track` resolves only large changes because it runs each code base once per configuration, and a change under about 10% is decided on instruction counts.

## The code bases are a benchmark suite of their own, outside the headline

- **Previous shape.** The three generated code bases were measured by `oracle bench` for the backlog's scoreboard only. The published benchmarks held no suite built from them.
- **Choice.** `tools/bench/codebases` is a suite like the others: `bench.py codebases --release ARCHIVE --write` records a code base's time as the sum of its test blocks' own times, with GNU R, ry and ry with the JIT off in the same rounds, plus each code base's per-block geometric mean and each engine's slowest code base. The benchmarks page shows it in its own section after the package suites.
- **Why it has no headline row.** The code bases are generated, so they say less about real code than the package suites do. One round takes about 22 minutes with three engines, so the release run skips the extra rounds for wide intervals (`--extra-rounds 0`) and takes about 4 hours. The corpus library lacks polars, Rblpapi and esgtoolkit, so the code bases run without them. The suite is kept out of the summary table, the landing page and the README (`outsideSummary` in `docs/src/lib/benchmarks.ts`, `outside_summary` in `tools/bench/readme.py`).
- **Revisit.** Reconsider a headline row after two release runs, once the intervals are known.
- **Dependency note.** The Posit snapshot's xts binary was built for an older R and truncates `ATTRIB`'s return value, so xts subsetting aborts on R 4.6 under either engine. `tools/corpus/install.R` reinstalls xts from source for that reason.

## The profile summary hashes private package names with a secret key per project

- **Previous shape.** `package_display` (`crates/interp/src/profile_summary.rs`) printed a private package as `private-` and the FNV-1a hash of its name, cut to 32 bits. Anyone holding a summary who guessed a name could hash the guess and confirm it.
- **Choice.** The label is SipHash-2-4 of the name under a 128-bit secret key, cut to 32 bits. The key comes from `RY_PROFILE_KEY` (32 hexadecimal digits) when set, else from a key file in the user's cache directory, `ry/profile-keys/`: one file per project, named by an unkeyed SipHash of the project root's canonical path, or `outside-projects` for a session that starts in no project. The project is the nearest directory at or above the session's starting directory with `ry.toml`, `roughly.toml` or `DESCRIPTION`. A file is created once from OS randomness (`getrandom`), mode 0600, by writing a staging file and hard-linking it into place, so racing sessions agree on one key. When no file can be kept (no cache directory, an unwritable one), the run uses a key of its own and its summary says why; an invalid `RY_PROFILE_KEY` is an error.
- **Why the cache directory, not the project's `.ry/`.** A file inside the project can be committed by accident, and `ry run` does not otherwise write into a project. The price is that two machines key the same project differently, so summaries from a laptop and a CI runner do not share labels; `RY_PROFILE_KEY` from a CI secret gives a fleet one key.
- **Randomness seam.** OS randomness is drawn only when a key file is created or a run key is needed; tests call `package_display` with a fixed key, and the scenario sets `RY_PROFILE_KEY`.
- **Impact.** One SipHash per private package per summary, so no measurable cost. Labels change once for every existing user, because the key is new.
