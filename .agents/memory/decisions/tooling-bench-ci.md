# Decisions: Build, benchmarks and CI

Build profiles, releases, platforms, the dev shell, CI, fuzzing harnesses and benchmark suites.

## Every pipeline stage is fuzzed from its first commit

**Status:** built.

- **User directive:** every stage (lowering, naming, inference, diagnostics, the incremental layer, the formatter, IDE features) gets fuzz and property coverage the day it exists, never later. The formatter is fuzzed for idempotence and losslessness, an IDE feature for never panicking at any cursor.
- **Template: `crates/semantics/tests/test_fuzz.rs`.** Over a generator biased toward live shapes plus a token-soup arm it asserts no panic (fixpoints converge), determinism across fresh databases, valid diagnostic ranges, and incremental equivalence with a fresh build. `FUZZ_ITERS` scales budgets; a bounded pass runs in each crate's tests and `fuzz_deep` variants carry long runs.
- The first runs found a non-converging cycle and inference variables leaking through exported schemes, the class of bug that only surfaces in the large.

## The shipping binary links no Apple frameworks

**Status:** built, except the `justfile` preflight for framework-linking crates, which does not exist.

- **Why (user criteria):** fewer dependencies and no licensing exposure. A redistributed macOS SDK (tried, then reverted) adds a large third-party artifact under Apple's gray license; vendoring it makes ry the redistributor.
- **`[patch.crates-io]` replaces iana-time-zone with `patches/iana-time-zone`,** a stub whose `get_timezone()` errors, cutting reedline → chrono → iana-time-zone → core-foundation-sys (`-framework CoreFoundation`). Safe because chrono consults it only after `TZ` and `/etc/localtime`, and the REPL does not use reedline's clock prompt. The stub must stay version- and feature-compatible with what chrono requests, or cargo silently prefers the real crate.
- **Nix trap:** crane's dependency-only build empties local `.rs` files, which would empty the patch crate; `flake.nix` restores `patches/` through `extraDummyScript`, interpolating only that directory so the cache stays source-independent.
- **Revisit** when a dependency truly needs a framework (native windows, clipboard) or for signing and notarizing: build on a real macOS runner, the only license-clean use of Apple's SDK.

## The Linux release binary links glibc 2.17 through zig

**Status:** superseded by "Releases are built natively per platform, with the runtime beside the binary". Kept for the reasoning about loaders, musl and allocators.

- Release derivations set `allowedReferences = [ ]`, so a `/nix/store` interpreter or RUNPATH fails the build. A binary using the FHS loader cannot run its tests in the Nix sandbox.
- **Rejected: static musl,** because a static binary cannot `dlopen` libR, losing `ry repl` and `ry run`. For whoever revisits: musl's `malloc` made `ry check` 2 to 5 times slower; zig's libc matched glibc with 13 to 23% more peak memory; jemalloc was 10 to 32% faster but builds C through `configure`; mimalloc was about 25% slower on the largest packages.
- **Rejected: static glibc,** whose `dlopen` runs libR against a second, uninitialized libc; and glibc plus musl downloads, because the directive is one release build per platform.
- **Accepted cost:** no Alpine, and NixOS needs nix-ld or the flake's `default`.

## The benchmark harness compares test by test through `ry load`

**Status:** built.

- `oracle bench` (`crates/oracle/src/bench.rs`) runs a package's testthat suite as `ry load <package> --file bench.R` under `--runtime gnu-r` and `--runtime ry`, so loading is identical and a difference is an evaluation difference. One JSON line per `test_that()` block.
- **Per test, not per file,** because one missing native fails a whole file of tests.
- **GNU R runs twice;** a test whose value GNU R does not reproduce (UUID, timestamp, temp path) is compared by status alone, so no hand-kept ignore list is needed.

## Everything is fuzzable, simulatable and scriptable

**Status:** built.

- **User directive;** `docs/src/content/docs/development/robustness.md` is the contract.
- **Nondeterminism lives behind seeded seams.** The server's worker sends through an `Outbox` (socket or recorder) and its loop body is `run_job`, so `server::simulation` runs the real server single-threaded on a seeded schedule and cancels at the Nth checkpoint (`cancel_at_checkpoint`), always followed by a real notification because the server assumes a newer edit is queued when cancelled. The oracle is a fresh server on the same disk and buffers.
- **A recorder and a step function, not a mock server,** so production and simulation share one code path through the real `ServerState` handlers.
- **Harness modules ship in the library, not behind a cargo feature,** because a package's integration tests see only default features and a gate would make bounded passes opt-in. They panic on a broken contract by design.
- **Gap:** the file-system seam is a directory only the simulator writes; injected IO errors are not covered.

## `just ry` builds ry's runtime; ry-lang does not depend on libr

**Status:** built.

- `ry run` loads `libR.so` beside the executable, and libr is a separate cdylib, so `cargo run -p ry-lang` never builds it; `just ry` builds both.
- **Rejected:** a nightly artifact dependency (every checker-only build would compile libr and Polars, and `cargo install` would still lose the library), and embedding the library (doubles the binary for users who never run code).

## Releases are built natively per platform, with the runtime beside the binary

**Status:** built, except the runtime beside the binary, which "A release is `ry` alone, with the runtime embedded and the R front end folded in" supersedes.

- **Native runners, not cross-links,** because the runtime links zlib, bzip2 and xz, which zig's glibc 2.17 sysroot lacks, and macOS cross-linking has only libSystem stubs. `just package VERSION` is the one recipe CI and `just release-dry-run` share; `tools/smoke-test.sh` tests the archive. An archive is flat because the Zed extension expects `ry` at its root.
- **Targets:** Linux x86_64, macOS arm64, Windows x86_64. Linux arm64 and Intel macOS were dropped because their runtimes had never run; a matrix row brings them back.
- **Windows is `x86_64-pc-windows-gnullvm` with MSYS2's clang64,** so `ry.exe`, `R.dll` and `ry-rcmd.exe` link the UCRT as Rtools-built package DLLs do. `just package` refuses a binary importing a toolchain runtime the archive lacks.
- **Marketplace only, by user decision** (no Open VSX; others use the release's `.vsix`). Publishing uses trusted publishing (`vsce publish --oidc`, vsce 4.1), because the Marketplace's only token is an Azure DevOps global PAT, retired December 1, 2026; packaging stays on vsce 3.6, which accepts bun's `node_modules`. `VSCE_PAT`, when set, is a fallback.
- **One version per release** (`just bump-version`); the news page is the release record.
- **glibc floor 2.35** (`ubuntu-22.04`), accepted for the beta; a manylinux-style container is the fix if users ask.
- **Nix** wraps `ry` with `R_HOME` defaulting to nixpkgs' R, because a Nix-built binary cannot load another distribution's libR; `ry-unwrapped` skips the wrapper.

## The bench templates are inline-typed

**Status:** built.

- The templates use inline syntax, because comment annotations never reach the runtime and records and tags have no R spelling. An untyped run erases each file with `ry debug erase` (`syntax::erase_inline_annotations`) and drops the leftover `#: @type` lines, so there is one program.
- Where a record or tag union replaces S4, R6 or a named list, `#@ typed` / `#@ untyped` / `#@ end` blocks keep both; `generate.py` counts lines on the untyped version so both runs instantiate the same templates.

## Seeded batteries replay through one loop

**Status:** built.

- `syntax::testing::for_each_seed(default, |seed| ...)` runs seeds scaled by `FUZZ_ITERS`, only `FUZZ_SEED` when set, and on a panic prints the exact replay command. An offset seed (`1000 + seed`) stays inside the closure so the printed seed is the one to pass.

## Every seeded harness rotates through `FUZZ_SEED`

**Status:** built.

- Each case is one seed of `for_each_seed`; a shared generator is `SplitMix64(CONST ^ seed)`, because one stream drawn across iterations neither replays a case nor reaches new inputs in a deep run.
- `tools/fuzz-deep.py` discovers harnesses from the sources (so a new one joins without registration) and runs each with a fresh `FUZZ_SEED` and `FUZZ_DEPTH`, which scales each loop's own default and so keeps the proportions of a test with several loops.

## CI runs `just gate` against GNU R 4.6.2, and nothing ignored

**Status:** built, except that the job sets up GNU R itself, which "CI runs in the flake's dev shell" supersedes: the job runs `just ci` in that shell.

- CI runs the recipes `just gate` runs, with the toolchain from `rust-toolchain.toml` (a pinned nightly for `allocator_api`; stable cannot build the workspace), so CI and the local gate cannot diverge. It covers the whole workspace, not `default-members`, except `zed_ry` (wasm).
- **GNU R is required,** because the runtime's suites skip without one; it is 4.6.2, the release the oracle records with and ry bundles.
- **Ignored suites stay manual** (`just fuzz-deep`, `just perf`): `ide::fuzz_deep` alone takes about an hour, and the corpus benchmarks need fetched data.
- **Triggers (user decision):** `workflow_dispatch` only, never push or pull request; the orchestrator starts it on main when main has new commits.

## The tests that need no GNU R run by hand and gate a release, never on push

**Status:** planned. Both workflow files are staged in `.github/pending/` and run nothing until the user runs `just accept`.

- **Triggers (user decision):** `workflow_dispatch` and `workflow_call`, never `push`, because agents commit to main many times an hour.
- **`test.yml` has two jobs, each one `just` recipe.** `tests` runs `just test-fast` (the crates that build without the interpreter, libR and Polars) after checking the runner has no `R`, so GNU R comparisons skip rather than run against the image's R. `fuzz` runs `just fuzz-all 60 runtime`.
- **The bound is `timeout`, and a target cut off there has passed,** because libFuzzer's `-max_total_time` starts only after the corpus replays (about 15 minutes for `ide`). Every target runs before the job fails.
- **The `polars` fuzz targets (`csv`, `unserialize`) run only by hand,** because a coverage build of Polars kills the compiler in a 15 GB container.
- **The staged `release.yml` calls it and `publish` needs it,** so a failing tag publishes nothing; `nightly.yml` is not gated because main may be broken.
- **A separate workflow, not a job in `ci.yml`,** because it needs neither Nix nor R, so a release can wait for it on a hosted runner. It does not replace the gate, which alone tests the runtime and the oracle.

## Optimized tests build with the bench profile, not fat LTO

**Status:** superseded by "Build profiles: release for numbers, iterate for loops". Kept for why optimized tests avoid fat LTO and for the dev profile's `line-tables-only` debug info.

- **Fat LTO stays in release,** because shipped binaries and published numbers come from it, and `cargo test --release` always uses the release profile. Its link of libr's `test_linpack` peaked at 11 GiB and was OOM-killed on 16 GB machines, so optimized tests use a profile without fat LTO.
- **`[profile.dev] debug = "line-tables-only"`** roughly halves debug artifacts; an interp test binary barely shrinks because its `.text` and relocations dominate.

## How bench-real measures

**Status:** superseded by "Four published benchmark suites, one runner". Kept for the measured cause of the noise and the rejected estimators, which explain why the runner samples in paired rounds.

- **Cause:** the cloud VMs' effective CPU speed drifts with other tenants, invisibly in steal time or load (a fixed C loop took 0.30 to 0.39 s across processes on a quiet VM). Ordering, cold runs and frequency scaling are not the cause.
- **Hence paired rounds:** each round runs every engine once in shuffled order and ratios form within a round, so drift longer than a round cancels. Compare two builds only in the same rounds (`--baseline`), never across sittings.
- **Rejected:** pinning cores and disabling ASLR (no effect); the fastest of N runs (as noisy as paired medians, and cannot be paired).

## Published benchmark numbers come only from the results files

**Status:** built, except the renderer-side too-short floor and workload rule, which "Benchmark suites are directories the renderers only display" supersedes.

- `docs/src/lib/benchmarks.ts` and `tools/bench/readme.py` render every number; prose names categories, never figures, so a fresh `--write` cannot contradict the text. Every rendering carries the metadata line.
- **Too short is judged on GNU R's time,** because it measures the work; flagging a short ry time would drop the speed-ups being measured.

## Benchmark suites are directories the renderers only display

**Status:** built, except the list of four suites, which "The typed suite scores typed ry against Rust" supersedes.

- A suite is `tools/bench/<suite>/` with `suite.toml` (the only source of labels) and a schema-1 `results.json`; renderers glob the directories, and a suite without results gets no row or section.
- **The runner judges, the renderers display:** flags and geometric means come from the runner, so site and README cannot disagree. Only a GNU R time under `meta.too_short_seconds` leaves a program out of the means (`isTooShort`).
- **One schema, no adapters;** renderers fail on anything else. The method table shows `just bench <suite> --write`, not `meta.command`, which holds the measuring machine's paths.

## Four published benchmark suites, one runner

**Status:** built, except typed as a column only, which "The typed suite scores typed ry against Rust" supersedes.

- **One runner, `tools/bench/bench.py` (`just bench <suite>`), one schema.** A typed program is `<name>.typed.R` beside `<name>.R`, so the two forms cannot drift. `internal/` holds unpublished benchmarks, `tooling/` the generator and corpus installer.
- **Method:** paired rounds with shuffled engine and program order; a program's ratio is the median per-round ratio with a 95% bootstrap interval. `--write` needs 10 rounds and every program, and adds rounds where an interval reaches over 10% from its median.
- **Gates:** a crash or a checksum differing from GNU R's (relative 1e-10, since typed code may sum in another order) writes nothing. Package suites gate on neither: differing outcomes are `tests_differ`, and a crash or timeout is a recorded failure left out of the mean. A test script exiting with status 1 counts as one failed test, because that is how a script without testthat reports failure (`test.data.table()`). A `gnu_r` failure tells a broken pin from a ry gap.
- **Kernel or application:** a kernel spends its time on arithmetic over numbers and numeric vectors; an application on lists, attributes, dispatch, strings and tables.

## Flake inputs are git URLs

**Status:** built, except "No workflow uses the flake", which "CI runs in the flake's dev shell" supersedes.

- Every input is `git+https://github.com/owner/repo?shallow=1` (nixpkgs with `ref=nixos-unstable`), because the cloud sessions' proxy serves git clones but answers `github:` tarball URLs with 403, so `nix develop` could not start. Only the git form pins a commit, and it was faster for nixpkgs. **Do not switch back to `github:` inputs.**

## Cloud sessions are provisioned by tools/dev-setup.sh, not the nix shell

**Status:** built.

- `tools/dev-setup.sh` (idempotent) installs apt libraries, a recent `just`, and through `tools/reference-r.sh` the reference GNU R 4.6.2 in `/opt/R/4.6.2` with the oracle's packages; `just doctor` (`tools/doctor.py`) checks each and names the fix.
- **`--enable-R-shlib` is required:** without `libR.so` ry's GNU R console cannot load that R and Posit's binary packages cannot link. **Posit's mirror serves binaries only to a user agent starting `R/<version>`,** which R's default lacks.
- **Why not nix here:** the binary cache restores the prebuilt R in seconds without a nix store in the container's 40 GB quota. CI uses the flake's `ci` shell, whose R is built the same way.

## CI runs in the flake's dev shell

**Status:** built.

- **`.github/workflows/ci.yml` only installs nix, reads ry.cachix.org and runs `nix develop .#ci -c just ci`,** so agents change CI by editing the `ci` recipe or the flake, without the user's `just accept`. A setup-r GNU R differed from the oracle's recordings, so CI failed on build differences, not ry bugs.
- **`flake.nix` builds `lib.gnuR` as `tools/reference-r.sh` builds the reference R on Ubuntu 24.04.** Each pin answers a measured oracle difference:
  - GCC 13.3, because nixpkgs' 13.4 stops vectorizing complex addition and swaps NA and NaN (`kernels__complex_na_nan_sums`);
  - a position-independent `R`, because a non-PIE heap at low addresses turns a long-vector error into a short vector (`base__replacement_promises_and_errors`);
  - Debian's `which` (`system__sys_which`) and a pager (`tools__help_text`);
  - Ubuntu's LAPACK and BLAS debs, because without a `libblas.so` link configure uses R's internal LAPACK and `La_library()` changes.
- **`LD_LIBRARY_PATH` points at those LAPACK and BLAS,** because ry dlopens them by soname and Nix's loader never searches `/usr/lib`.
- **The battery's `target/` reaches 22 GB,** so the workflow's free-disk step matters.

## Workspace-wide feature unification

**Status:** built.

- `.cargo/config.toml` sets the nightly `resolver.feature-unification = "workspace"`, and `crates/features` carries as normal dependencies the features only dev-dependencies enable, so building and testing share one dependency tree (otherwise each command rebuilt Polars and everything above the split).
- **Trap:** a new dev-dependency that brings a feature reopens the split; `crates/features/Cargo.toml` says how to spot it.

## Prebuilt trees come from the ry.cachix.org binary cache

**Status:** built.

- `tools/cache.sh` packs a finished tree (reference R, corpus library) as a zstd tarball, adds it as a store path and pushes to `ry.cachix.org`; `tools/cache.lock` maps a hash of its inputs to that path. `tools/reference-r.sh` and `just bench-packages install` restore before building.
- **Tarballs, not derivations,** because the trees must be the Ubuntu builds the oracle recorded; a rebuild never reproduces the bytes, so the lock records the path.
- **No workflow pushes trees,** because it would have to commit the lock back to main; whoever changes the inputs runs `just cache-push <tree>` and commits the lock.
- **Pushes pass `--host https://app.cachix.org`,** the only host the cloud proxy injects the token for.
- **Release builds share the cache (user directive):** `.github/workflows/cachix.yml` pushes `.#ry` and the dev shell per release and nightly, not per main push (5 GB limit), filtering out `ry-deps-*`. It follows `workflow_run`, because releases published with `GITHUB_TOKEN` fire no `release: published`.

## Build profiles: release for numbers, iterate for loops

**Status:** built.

- **`release` (fat LTO) is the only source of published or quoted numbers.** `iterate` (no LTO, 16 codegen units) serves `cargo test-release`, `just perf` and `just simulate`; it rebuilds far faster but single kernels range from 24% faster to 28% slower, so it is no stand-in. `crates/ry/build.rs` names the profile in any non-release version, and `tools/bench/bench.py` refuses such a binary. `[profile.bench]` is gone, because `--profile bench` wrote into `target/release` and its binaries were quoted as release.
- **`profiling` is release plus `debug = "line-tables-only"`,** keeping fat LTO because thin LTO changes cross-crate inlining, which is what a profile must show; line tables alone keep the link within 16 GB.

## Worktrees keep their own target directories and share sccache

**Status:** built.

- `tools/dev-setup.sh` sets `build.rustc-wrapper = "sccache"` in `~/.cargo/config.toml`, so dependencies compile once across worktrees while target directories stay per worktree (`just prune` trims them). A shared `CARGO_TARGET_DIR` was rejected: concurrent builds block on cargo's lock and rebuild each other's crates. sccache skips incremental compilations, so workspace-crate rebuilds are unchanged.

## Published benchmarks measure the release archive

**Status:** built.

- `bench.py --release ARCHIVE` unpacks the official Linux x86_64 archive privately and clears `RY_LIBR`, `RY_BASE_R` and `R_HOME`, so only its own runtime loads; `--write` requires it, refuses a nightly, and records `meta.release`. Recorded once per release, after the tag.
- **Why:** the release is profile-guided and BOLT-laid-out (`tools/layout/optimize.sh`), measurably faster than a plain build, and that pipeline does not fit a 15 GB machine, so the uploaded archive is the artifact to measure.
- `refuse_foreign_images` stops the run on a `.ryc` another build wrote. Changes are still judged on plain release builds by `Ir`. Pinned by `tools/bench/test_bench.py`.

## A fatal signal prints one report from the executable, which asks the runtime for the R stack

**Status:** built.

- **`crates/crash` installs handlers first thing in `ry`'s `main`;** `libR.so` exports `ry_crash_traceback` (`interp::crash::write_traceback`), registered by `repl` through `crash::set_traceback`, so one handler serves both. The runtime gets a writer and a memory probe (a `write` into a pipe fails with EFAULT where a load would fault), so a corrupt frame stack ends a line, not the report.
- **Must keep:** a 256 KiB `sigaltstack` (stack overflows report), no allocation, `SA_NODEFER` so a fault in the report prints the native stack; afterwards chain to the previous handler, restore `SIG_DFL` and re-raise, so exit status and core dump are unchanged. `backtrace_symbols_fd` is warmed at install, because its first call loads libgcc.
- **Left out:** SIGILL and SIGFPE, a Windows filter (backlog), and the GNU R runtime, which installs its own.
- Pinned by `crates/crash/tests/test_crash.rs` and `crates/ry/tests/scenarios/crash-report.scenario`.

## Platform code is type-checked from Linux before the release job builds it

**Status:** built.

- `cargo clippy --target aarch64-apple-darwin` and `--target x86_64-pc-windows-gnullvm` run on Linux, because a release round on those runners costs most of an hour. Nothing links, but C build scripts run, so `CC_<target>`/`CXX_<target>` point at a clang wrapper passing `--target`, with `llvm-ar-18` and `llvm-windres-18`.
- **Trap:** zig's Mach-O linker ignores `-exported_symbols_list`, so a zig-linked build cannot show a missing export; a real macOS cross-build uses `clang --target=arm64-apple-macos11 -isysroot $SDKROOT -fuse-ld=lld`.

## The dev shells' GNU R is cached on every change to its inputs

- `.github/workflows/cachix.yml` also runs on a push to main touching `flake.nix`, `flake.lock` or `tools/base-r/**` (the only inputs of `lib.gnuR`), builds the `default` and `ci` shells and pushes them without the Rust toolchain and `ry-deps-*`, so `nix develop` never compiles GNU R. The path filter keeps runs rare.

## The hot suite replays the package entries the code bases spend their time in

- **Why:** the published suites' means disagree with the three code bases, which are the goal and take minutes per run, so no suite was fast and representative. `tools/bench/hot/` (`just bench hot`) replays 100 captured package entries; `tools/bench/tooling/hot.py` builds and checks it; `testing.md`, "The hot suite", describes it.
- **The unit is a package entry,** a closure call from user code into package code, because there time can be attributed and the arguments are values user code built. Rejected: hot bodies anywhere, whose arguments are promises into package frames and whose times nest.
- **User code is code evaluating in an environment whose top level is the user's, however entered** (`capture::evaluating` from promise forcing, `eval`, `Rf_eval`, `R_tryEval`, `R_ParseEvalString`); counting only user closures charged `test_that()` blocks to `testthat::test_that`.
- **A weight is a ratio estimate:** a key's capture time times GNU R's over the capture's time for the same uniformly drawn samples, so recorder overhead cancels. The recorder (`crates/interp/src/capture.rs`) empties a reservoir slot for an oversize call rather than keep a biased earlier one, subtracts its own suspended work, records integer nanoseconds, makes a call it cannot inspect `uninspectable` instead of raising (a measurement must not change the program), and stops in a forked child via `pthread_atfork`.
- **Replays run in the code bases' session** (every imported package loaded first, `session` in `suite.toml`), because results otherwise depend on program order (`zoo::rollapply` pads differently once xts loads).
- **Checksums** (`replay.R`, `bench.same_checksum_part`) hash everything exact (types, attributes except data.table's `.internal.selfref`, text, NA/NaN/Inf counts) and compare numeric sums with one tolerance (absolute 1e-10 or relative 1e-13), weighted by a golden-ratio sequence; rounding then hashing failed on last-bit differences. The tolerance exists for `power.t.test`. Samples that differ between two GNU R calls are `varies`.
- **Temporary paths are relocated** under the replay's `tempdir()` (`relocate`), and the validator drops a sample that writes outside its own `TMPDIR`, leaves different files on a second call, or opens a graphics device.
- **A call that changes its arguments gets a fresh copy per call** (`fresh` in `suite.toml`), unserialized outside the clock, else a data.table `:=` replays on the previous result.
- **The deterministic metric is callgrind instructions per region** (`--instructions`, `.ry_bench_region`), because the VMs expose no hardware counters. Programs are dealt over `bench.INSTRUCTION_SHARDS` processes by position, never core count, and run on one OpenMP thread, because callgrind counts idle workers spinning.
- **Read changes between predictions, never a prediction against a code base's time:** the suite tracks the code bases' changes closely with the same sign, but its predicted level is pessimistic for reasons still open (`backlog.md`, "Open: the hot suite"). Wall time on a shared VM cannot show a change under about 10%, so smaller changes are decided on instruction counts.

## The code bases are a benchmark suite of their own, outside the headline

- `tools/bench/codebases` records each code base's time as the sum of its test blocks' times, with GNU R, ry and ry without the JIT in the same rounds, shown in its own section after the package suites.
- **No headline row,** because generated code says less about real code than the package suites; kept out by `outsideSummary` (`docs/src/lib/benchmarks.ts`) and `outside_summary` (`tools/bench/readme.py`). Reconsider after two release runs. Release runs use `--extra-rounds 0`, because a round takes about 22 minutes.
- **Trap:** the Posit snapshot's xts binary truncates `ATTRIB`'s result on R 4.6, so `tools/corpus/install.R` reinstalls xts from source.

## The profile summary hashes private package names with a secret key per project

- **`package_display` (`crates/interp/src/profile_summary.rs`) labels a private package with SipHash-2-4 under a 128-bit secret key,** because an unkeyed hash lets anyone confirm a guessed name. The key is `RY_PROFILE_KEY` (32 hex digits; invalid is an error) or a per-project file under the user's cache directory, `ry/profile-keys/`, created once with `getrandom`, mode 0600, by hard-linking a staged file so racing sessions agree. With no keepable file the run uses its own key and the summary says why.
- **Cache directory, not the project's `.ry/`,** because a file in the project can be committed and `ry run` writes nothing else there. Two machines label differently; `RY_PROFILE_KEY` from a CI secret gives a fleet one key.
