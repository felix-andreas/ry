# Decisions: Build, benchmarks and CI

Build profiles, releases, platforms, the dev shell, CI, fuzzing harnesses and benchmark suites.

## Every pipeline stage is fuzzed from its first commit

**Status:** built, except the "CI does not fuzz" paragraph, which "CI runs `just gate` against GNU R 4.6.2, and nothing ignored" supersedes.

- **User directive:** every stage (lowering, naming, inference, diagnostics, the incremental layer, the formatter, IDE features) gets fuzz and property coverage the day it exists, never later. The formatter is fuzzed for idempotence and losslessness, an IDE feature for never panicking at any cursor.
- **Template: `crates/semantics/tests/test_fuzz.rs`.** Over a generator biased toward live shapes plus a token-soup arm it asserts no panic (fixpoints converge), determinism across fresh databases, valid diagnostic ranges, and incremental equivalence with a fresh build. `FUZZ_ITERS` scales budgets; a bounded pass runs in each crate's tests and `fuzz_deep` variants carry long runs.
- The first runs found a non-converging cycle and inference variables leaking through exported schemes, the class of bug that only surfaces in the large.

## The shipping binary links no Apple frameworks

**Status:** built, except the `justfile` preflight for framework-linking crates, which does not exist.

- **Why (user criteria):** fewer dependencies and no licensing exposure. A redistributed macOS SDK (tried, then reverted) adds a large third-party artifact under Apple's gray license; vendoring it makes ry the redistributor.
- **`[patch.crates-io]` replaces iana-time-zone with `patches/iana-time-zone`,** a stub whose `get_timezone()` errors, cutting reedline → chrono → iana-time-zone → core-foundation-sys (`-framework CoreFoundation`). Safe because chrono consults it only after `TZ` and `/etc/localtime`, and the REPL does not use reedline's clock prompt. The stub must stay version- and feature-compatible with what chrono requests, or cargo silently prefers the real crate.
- **Nix trap:** crane's dependency-only build empties local `.rs` files, which would empty the patch crate; `flake.nix` restores `patches/` through `extraDummyScript`, interpolating only that directory so the cache stays source-independent.
- **Framework-free is sustainable,** because libR is dlopen'd at run time (no link-time dependency) and the REPL plots to a file, a terminal image protocol or a browser.
- **Revisit** when a dependency truly needs a framework (native windows, clipboard) or for signing and notarizing: build on a real macOS runner, the only license-clean use of Apple's SDK.

## The Linux release binary links glibc 2.17 through zig

**Status:** superseded by "Releases are built natively per platform, with the runtime beside the binary". Kept for the reasoning about loaders, musl and allocators.

- Release derivations set `allowedReferences = [ ]`, so a `/nix/store` interpreter or RUNPATH fails the build. A binary using the FHS loader cannot run its tests in the Nix sandbox.
- **Rejected: static musl,** because a static binary cannot `dlopen` libR, losing `ry repl` and `ry run`. musl's own `malloc` also made `ry check` 2 to 5 times slower.
- **Rejected: static glibc,** whose `dlopen` runs libR against a second, uninitialized libc; and glibc plus musl downloads, because the directive is one release build per platform.
- **Accepted cost:** no Alpine, and NixOS needs nix-ld or the flake's `default`.
- **Allocator measurements** (`ry check`, against glibc's `malloc`): zig's libc (`SmpAllocator`) matched glibc's speed with 13 to 23% more peak memory; jemalloc was 10 to 32% faster but builds C through `configure` and `make`; mimalloc was 24 to 25% slower on the largest packages. The release keeps glibc's own.

## The benchmark harness compares test by test through `ry load`

**Status:** built.

- `oracle bench` (`crates/oracle/src/bench.rs`) runs a package's testthat suite as `ry load <package> --file bench.R` under `--runtime gnu-r` and `--runtime ry`, so loading is identical and a difference is an evaluation difference. One JSON line per `test_that()` block.
- **Per test, not per file,** because one missing native fails a whole file of tests. Per-test results also give the failure messages that the tally groups into fix classes.
- **GNU R runs twice;** a test whose value GNU R does not reproduce (UUID, timestamp, temp path) is compared by status alone, so no hand-kept ignore list is needed.

## Everything is fuzzable, simulatable and scriptable

**Status:** built.

- **User directive;** `docs/src/content/docs/development/robustness.md` is the contract.
- **Nondeterminism lives behind seeded seams.** The server's worker sends through an `Outbox` (socket or recorder) and its loop body is `run_job`, so `server::simulation` runs the real server single-threaded on a seeded schedule and cancels at the Nth checkpoint (`cancel_at_checkpoint`), always followed by a real notification because the server assumes a newer edit is queued when cancelled. The oracle is a fresh server on the same disk and buffers, compared by input state, diagnostics, random requests, range geometry and the push contract. Expensive incremental cross-checks run behind `syntax::testing::enable_invariants()` and are off in production. Scenario files script journeys through the simulator and the real binary.
- **A recorder and a step function, not a mock server,** so production and simulation share one code path through the real `ServerState` handlers.
- **Harness modules ship in the library, not behind a cargo feature,** because a package's integration tests see only default features and a gate would make bounded passes opt-in. They panic on a broken contract by design.
- **Gap:** the file-system seam is a directory only the simulator writes; injected IO errors are not covered.

## `just ry` builds ry's runtime; ry-lang does not depend on libr

**Status:** built.

- `ry run` loads `libR.so` beside the executable, and libr is a separate cdylib, so `cargo run -p ry-lang` never builds it; `just ry` builds both, `just build` the whole workspace, and the not-found error names both recipes.
- **Rejected:** a nightly artifact dependency (every checker-only build would compile libr and Polars, and `cargo install` would still lose the library), and embedding the library (doubles the binary for users who never run code).

## Releases are built natively per platform, with the runtime beside the binary

**Status:** built, except the runtime beside the binary, which "A release is `ry` alone, with the runtime embedded and the R front end folded in" supersedes.

- **Native runners, not cross-links,** because the runtime links zlib, bzip2 and xz, which zig's glibc 2.17 sysroot lacks, and macOS cross-linking has only libSystem stubs. `just package VERSION` is the one recipe CI and `just release-dry-run` share; `tools/smoke-test.sh` tests the archive. An archive is flat because the Zed extension expects `ry` at its root.
- **Targets:** Linux x86_64, macOS arm64, Windows x86_64. Linux arm64 and Intel macOS were dropped because their runtimes had never run; a matrix row brings them back.
- **Windows is `x86_64-pc-windows-gnullvm` with MSYS2's clang64,** so `ry.exe`, `R.dll` and `ry-rcmd.exe` link the UCRT as Rtools-built package DLLs do. `just package` refuses a binary importing a toolchain runtime the archive lacks.
- **The VS Code extension bundles the archive's `ry`:** `just build-platform-vsix` copies it into `editors/code/bin`, and `just build-extension` adds the license and notices.
- **One version per release:** `just bump-version` sets the CLI to `X.Y.Z-beta` and both extensions to `X.Y.Z`. The news page is the release record (no `CHANGELOG.md`).
- **Marketplace only, by user decision** (no Open VSX; others use the release's `.vsix`). Publishing uses trusted publishing (`vsce publish --oidc`, vsce 4.1), because the Marketplace's only token is an Azure DevOps global PAT, retired December 1, 2026; packaging stays on vsce 3.6, which accepts bun's `node_modules`. `VSCE_PAT`, when set, is a fallback.
- **glibc floor 2.35** (`ubuntu-22.04`), accepted for the beta; a manylinux-style container is the fix if users ask.
- **Nix** wraps `ry` with `R_HOME` defaulting to nixpkgs' R, because a Nix-built binary cannot load another distribution's libR; a user-set `R_HOME` wins, and `ry-unwrapped` skips the wrapper.

## The bench templates are inline-typed

**Status:** built.

- The templates use inline syntax, because comment annotations never reach the runtime and records and tags have no R spelling. An untyped run erases each file with `ry debug erase` (`syntax::erase_inline_annotations`) and drops the leftover `#: @type` lines, so there is one program.
- Where a record or tag union replaces S4, R6 or a named list, `#@ typed` / `#@ untyped` / `#@ end` blocks keep both; `generate.py` counts lines on the untyped version so both runs instantiate the same templates.

## Seeded batteries replay through one loop

**Status:** built.

- `syntax::testing::for_each_seed(default, |seed| ...)` runs seeds scaled by `FUZZ_ITERS`, only `FUZZ_SEED` when set, and on a panic prints the exact replay command, `FUZZ_SEED=<seed> cargo test -p <package> <target> <test> -- --exact`. An offset seed (`1000 + seed`) stays inside the closure so the printed seed is the one to pass.

## Every seeded harness rotates through `FUZZ_SEED`

**Status:** built.

- Each case is one seed of `for_each_seed`; a shared generator is `SplitMix64(CONST ^ seed)`, because one stream drawn across iterations neither replays a case nor reaches new inputs in a deep run.
- `tools/fuzz-deep.py` discovers harnesses from the sources (so a new one joins without registration) and runs each with a fresh `FUZZ_SEED` and `FUZZ_DEPTH`, which scales each loop's own default and so keeps the proportions of a test with several loops.

## CI runs `just gate` against GNU R 4.6.2, and nothing ignored

**Status:** built, except that the job sets up GNU R itself, which "CI runs in the flake's dev shell" supersedes: the job runs `just ci` in that shell.

- CI runs the recipes `just gate` runs, with the toolchain from `rust-toolchain.toml` (a pinned nightly for `allocator_api`; stable cannot build the workspace), so CI and the local gate cannot diverge. It covers the whole workspace, not `default-members`, except `zed_ry` (wasm).
- **GNU R is required,** because the runtime's suites skip without one; it is 4.6.2, the release the oracle records with and ry bundles.
- **Ignored suites stay manual** (`just fuzz-deep`, `just perf`): `ide::fuzz_deep` alone takes about an hour, the corpus benchmarks need a fetched corpus, and `format_directory` needs `RY_FORMAT_DIR`.
- **Triggers (user decision):** `workflow_dispatch` only, never push or pull request; the orchestrator starts it on main when main has new commits.

## The tests that need no GNU R run by hand and gate a release, never on push

**Status:** planned. Both workflow files are staged in `.github/pending/` and run nothing until the user runs `just accept`.

- **Triggers (user decision):** `workflow_dispatch` and `workflow_call`, never `push`, because agents commit to main many times an hour.
- **`test.yml` has two jobs, each one `just` recipe.** `tests` runs `just test-fast` (the crates that build without the interpreter, libR and Polars) after checking the runner has no `R`, so GNU R comparisons skip rather than run against the image's R. `fuzz` runs `just fuzz-all 60 runtime`, the 15 of 17 targets that build without Polars, and uploads a failing input.
- **The bound is `timeout`, and a target cut off there has passed,** because libFuzzer's `-max_total_time` starts only after the corpus replays (about 15 minutes for `ide`). Seeds replay in random order, so a slow target reads a different sample each run. Every target runs before the job fails.
- **The `polars` fuzz targets (`csv`, `unserialize`) run only by hand,** because a coverage build of Polars kills the compiler (on `polars-core` past 7 GB) in a 15 GB container. The fuzz crate's features split them out: `runtime` and `polars`.
- **The staged `release.yml` calls it and `publish` needs it,** so a failing tag publishes nothing; `nightly.yml` is not gated because main may be broken.
- **A separate workflow, not a job in `ci.yml`,** because it needs neither Nix nor R, so a release can wait for it on a hosted runner. It does not replace the gate, which alone tests the runtime and the oracle. Between releases nothing in CI catches a regression unless started, so the local gate is the only check on every change.

## Optimized tests build with the bench profile, not fat LTO

**Status:** superseded by "Build profiles: release for numbers, iterate for loops". Kept for why optimized tests avoid fat LTO and for the dev profile's `line-tables-only` debug info.

- **Fat LTO stays in release,** because shipped binaries and published numbers come from it, and `cargo test --release` always uses the release profile. Its link of libr's `test_linpack` peaked at 11 GiB and was OOM-killed on 16 GB machines, so optimized tests use a profile without fat LTO. Measured on that link: fat LTO 11.08 GiB in 1,797 s, no fat LTO and 16 codegen units 3.60 GiB in 685 s.
- **`[profile.dev] debug = "line-tables-only"`** roughly halves debug artifacts; an interp test binary barely shrinks because its `.text` and relocations dominate.

## How bench-real measures

**Status:** superseded by "Four published benchmark suites, one runner". Kept for the measured cause of the noise and the rejected estimators, which explain why the runner samples in paired rounds.

- **Symptom:** two measurements of the same commit with GNU R timed separately reported the awfy geometric mean as 1.90x and 1.58x.
- **Cause:** the cloud VMs' effective CPU speed drifts with other tenants, invisibly in steal time or load (steal at most 1.3%, its correlation with a run's deviation -0.16; a fixed C loop took 0.30 to 0.39 s across processes on a quiet VM). Ordering, cold runs and frequency scaling are not the cause.
- **Hence paired rounds:** each round runs every engine once in shuffled order and ratios form within a round, so drift longer than a round cancels. Compare two builds only in the same rounds (`--baseline`), never across sittings.
- **Rejected:** pinning cores and disabling ASLR (no effect); the fastest of N runs (as noisy as paired medians, about ±3% across sittings on awfy, and cannot be paired).
- **Measured noise with paired rounds** (5 rounds, 95% interval of an old build against main): 0.97 to 1.04 on a representative mix, 0.92 to 1.07 on awfy, 0.95 to 1.06 on rcp, 0.98 to 1.03 on shootout.

## Published benchmark numbers come only from the results files

**Status:** built, except the renderer-side too-short floor and workload rule, which "Benchmark suites are directories the renderers only display" supersedes.

- `docs/src/lib/benchmarks.ts` and `tools/bench/readme.py` render every number; prose names categories, never figures, so a fresh `--write` cannot contradict the text. Every chart, table, summary and README block carries a metadata line (date, ry commit, GNU R version, machine, rounds), and `just readme --check` in `just gate` catches drift.
- **Too short is judged on GNU R's time,** because it measures the work; flagging a short ry time would drop the speed-ups being measured.

## Benchmark suites are directories the renderers only display

**Status:** built, except the list of four suites, which "The typed suite scores typed ry against Rust" supersedes.

- A suite is `tools/bench/<suite>/` with `suite.toml` (the only source of labels) and a schema-1 `results.json`; renderers glob the directories, and a suite without results gets no row or section.
- **The runner judges, the renderers display:** flags and geometric means come from the runner, so site and README cannot disagree. Only a GNU R time under `meta.too_short_seconds` leaves a program out of the means (`isTooShort`).
- **One schema, no adapters;** renderers fail on anything else. A package `failures` entry renders as "fails on <engine>: <reason>" with no bar. The method table shows `just bench <suite> --write`, not `meta.command`, which holds the measuring machine's paths.

## Four published benchmark suites, one runner

**Status:** built, except typed as a column only, which "The typed suite scores typed ry against Rust" supersedes.

- **One runner, `tools/bench/bench.py` (`just bench <suite>`), one schema.** A typed program is `<name>.typed.R` beside `<name>.R`, so the two forms cannot drift. `internal/` holds unpublished benchmarks, `tooling/` the generator and corpus installer.
- **Method:** paired rounds with shuffled engine and program order; a program's ratio is the median per-round ratio with a 95% bootstrap interval. `--write` needs 10 rounds and every program, and adds rounds where an interval reaches over 10% from its median.
- **Gates:** a crash or a checksum differing from GNU R's (relative 1e-10, since typed code may sum in another order) writes nothing. Package suites gate on neither: differing outcomes are `tests_differ`, and a crash or timeout is a recorded failure left out of the mean. A test script exiting with status 1 counts as one failed test, because that is how a script without testthat reports failure (`test.data.table()`). The cost: a script stopped early by a ry bug also counts as finished, but its `tests_differ` flag shows it. A `gnu_r` failure tells a broken pin from a ry gap. `typed_refused` records what `RY_TYPED_LOG` reports for the typed program beyond the harness.
- **Kernels against applications:** a kernel spends its time on arithmetic over numbers and numeric vectors, an application workload on lists, attributes, dispatch, strings and tables.
- **A PR claims a speed-up only when the intervals separate.**

## Flake inputs are git URLs

**Status:** built, except "No workflow uses the flake", which "CI runs in the flake's dev shell" supersedes.

- Every input is `git+https://github.com/owner/repo?shallow=1` (nixpkgs with `ref=nixos-unstable`), because the cloud sessions' proxy serves git clones but answers `github:` tarball URLs with 403, so `nix develop` could not start. Only the git form pins a commit, and a shallow nixpkgs clone took 16 s against 29 s for the tarball; `flake.lock` keeps the same revs and narHashes, so pinned trees are byte-identical. **Do not switch back to `github:` inputs.**

## Agents work in the nix dev shell, loaded by tools/env.sh

**Status:** built.

- **Previous shape:** cloud containers ran `tools/dev-setup.sh` (apt, `just` from PyPI, GNU R restored into `/opt/R`), and mold needed a further environment setup script. Two provisioning paths described the same toolchain, and the container one drifted from CI's.
- **Chosen shape:** every cloud container has nix, so `source tools/env.sh` loads the flake's `default` shell, which carries the toolchain, the reference GNU R, `just` and mold (`pkgs.mold`, the wrapped linker, which sees the shell's library paths). No environment setup script.
- **Why sourced, not `nix develop -c`:** an agent's commands each start a new shell. `nix develop -c` costs 4.7 s per command; sourcing the saved `nix print-dev-env` (`target/nix-dev-env.sh`) costs 0.06 s.
- **Measured, 4 cores:** first load 98 s and 8.8 GB of store, nothing built (both binary caches cover it); first debug build in the shell 5 min 20 s.
- **Limits:** shell builds link nix's glibc, so a target directory is either shell-built or host-built. The package corpus links Ubuntu libraries the shell lacks, so that work uses `RY_ENV=system` and `tools/dev-setup.sh` (unverified in the shell).

## tools/dev-setup.sh provisions the host for package-corpus work

**Status:** built. Superseded as the default for agents by the entry above.

- `tools/dev-setup.sh` (idempotent) installs apt libraries, `just` 1.59.0 from PyPI, and through `tools/reference-r.sh` the reference GNU R 4.6.2 in `/opt/R/4.6.2` with the oracle's packages (rlang 1.3.0); `just doctor` (`tools/doctor.py`) checks each, plus binary freshness by recomputing the runtime identity, and names the fix.
- **`--enable-R-shlib` is required:** without `libR.so` ry's GNU R console cannot load that R and Posit's binary packages cannot link. **Posit's mirror serves binaries only to a user agent starting `R/<version>`,** which R's default lacks (about 10 s for the set against 5 minutes compiling).
- CI uses the flake's `ci` shell, whose R is built the same way.

## CI runs in the flake's dev shell

**Status:** built.

- **`.github/workflows/ci.yml` only installs nix, reads ry.cachix.org and runs `nix develop .#ci -c just ci`,** so agents change CI by editing the `ci` recipe or the flake, without the user's `just accept`. A setup-r GNU R differed from the oracle's recordings, so CI failed on build differences, not ry bugs.
- **`flake.nix` builds `lib.gnuR` as `tools/reference-r.sh` builds the reference R on Ubuntu 24.04.** Each pin answers a measured oracle difference:
  - GCC 13.3, because nixpkgs' 13.4 stops vectorizing complex addition and swaps NA and NaN (`kernels__complex_na_nan_sums`);
  - a position-independent `R`, because a non-PIE heap at low addresses turns a long-vector error into a short vector (`base__replacement_promises_and_errors`);
  - Debian's `which` (`system__sys_which`) and a pager (`tools__help_text`);
  - Ubuntu's LAPACK and BLAS debs, because without a `libblas.so` link configure uses R's internal LAPACK and `La_library()` changes;
  - Ubuntu's hardening flags instead of Nix's, and glibc's `*pi` functions and `pown` hidden from configure.
- **The `ci` shell puts that GCC first on PATH** (the toolchain propagates nixpkgs' GCC 15) and sets `TZDIR` and a locale archive with the oracle's locales.
- **`LD_LIBRARY_PATH` points at those LAPACK and BLAS,** because ry dlopens them by soname and Nix's loader never searches `/usr/lib`.
- **Measured:** with the shell's R, `just oracle verify` passes all 440 cases (17 skip for packages the shell lacks). The closure is 2.3 GiB. `just ci` takes about 43 minutes cold and 31 warm on four cores, 8 of them before the tests even when warm, because `just base-r` rebuilds base R every run.
- **The battery's `target/` reaches 22 GB,** so the workflow's free-disk step matters.

## Workspace-wide feature unification

**Status:** built.

- `.cargo/config.toml` sets the nightly `resolver.feature-unification = "workspace"`, and `crates/features` carries as normal dependencies the features only dev-dependencies enable, so building and testing share one dependency tree (otherwise each command rebuilt Polars and everything above the split). Verified with `cargo --unit-graph`: the ry build and `cargo test -p interp`, `-p libr` and `-p semantics` share the same dependency units.
- **Trap:** a new dev-dependency that brings a feature reopens the split; `crates/features/Cargo.toml` says how to spot it.

## Prebuilt trees come from the ry.cachix.org binary cache

**Status:** built.

- `tools/cache.sh` packs a finished tree (reference R, corpus library) as a zstd tarball, adds it as a store path and pushes to `ry.cachix.org`; `tools/cache.lock` maps a hash of its inputs to that path. `tools/reference-r.sh` and `just bench-packages install` restore before building: the reference R in 5.5 s against 3.5 minutes to build, the corpus (119 packages) in 8.8 s against 8 minutes. Reading needs no token.
- **Tarballs, not derivations,** because the trees must be the Ubuntu builds the oracle recorded; a rebuild never reproduces the bytes, so the lock records the path.
- **No workflow pushes trees,** because it would have to commit the lock back to main; whoever changes the inputs runs `just cache-push <tree>` and commits the lock; `just doctor` warns while the lock has no build of the current inputs.
- **Pushes pass `--host https://app.cachix.org`,** the only host the cloud proxy injects the token for.
- **Release builds share the cache (user directive):** `.github/workflows/cachix.yml` pushes `.#ry` and the dev shell per release and nightly, not per main push (5 GB limit), filtering out `ry-deps-*`. It follows `workflow_run`, because releases published with `GITHUB_TOKEN` fire no `release: published`, and builds only when the run's `publish` job succeeded.

## Build profiles: release for numbers, iterate for loops

**Status:** built.

- **`release` (fat LTO) is the only source of published or quoted numbers.** `iterate` (no LTO, 16 codegen units) serves `cargo test-release`, `just perf` and `just simulate`; after an interp edit it rebuilds `ry` and `libR.so` in 2m06s (2.1 GB peak) against release's 5m51s (6.3 GB), but single kernels range from 24% faster to 28% slower (geometric mean 4.5% slower), so it is no stand-in. `crates/ry/build.rs` names the profile in any non-release version, and `tools/bench/bench.py` refuses such a binary. `[profile.bench]` is gone, because `--profile bench` wrote into `target/release` and its binaries were quoted as release.
- **`profiling` is release plus `debug = "line-tables-only"`,** keeping fat LTO because thin LTO changes cross-crate inlining, which is what a profile must show; line tables alone keep the link within 16 GB (peak 10.8 GB), and its `.text` is within 0.2% of release's.

## Worktrees keep their own target directories and share sccache

**Status:** built.

- `tools/dev-setup.sh` sets `build.rustc-wrapper = "sccache"` in `~/.cargo/config.toml`, so dependencies compile once across worktrees while target directories stay per worktree (`just prune` trims them). A shared `CARGO_TARGET_DIR` was rejected: concurrent builds block on cargo's lock and rebuild each other's crates. sccache skips incremental compilations, so workspace-crate rebuilds are unchanged. A cold debug build with a warm cache takes 1m13s against 4m28s; the cache holds 219 MB.

## Published benchmarks measure the release archive

**Status:** built.

- `bench.py --release ARCHIVE` unpacks the official Linux x86_64 archive privately, points `XDG_CACHE_HOME` there and clears `RY_LIBR`, `RY_BASE_R` and `R_HOME`, so only its own runtime loads; `--write` requires it, refuses a nightly, and records `meta.release` (tag, archive name, sha256). Recorded once per release, after the tag.
- **Why:** the release is profile-guided and BOLT-laid-out (`tools/layout/optimize.sh`), measurably faster than a plain build (1.12x on `lapply`, 1.30x on the validation workload), and that pipeline does not fit a 15 GB machine (the instrumented link reaches about 14 GB), so the uploaded archive is the artifact to measure.
- `refuse_foreign_images` stops the run on a `.ryc` another build wrote. Changes are still judged on plain release builds by `Ir`, with the no-op control for layout effects. Pinned by `tools/bench/test_bench.py`, run from `test_release_metadata::the_benchmark_runner_records_the_release_archive_and_refuses_foreign_images`.

## A fatal signal prints one report from the executable, which asks the runtime for the R stack

**Status:** built.

- **`crates/crash` installs handlers first thing in `ry`'s `main`;** `libR.so` exports `ry_crash_traceback` (`interp::crash::write_traceback`), registered by `repl` through `crash::set_traceback`, so one handler serves both. The runtime gets a writer and a memory probe (a `write` into a pipe fails with EFAULT where a load would fault), so a corrupt frame stack ends a line, not the report.
- **Must keep:** a 256 KiB `sigaltstack` (stack overflows report), no allocation, `SA_NODEFER` so a fault in the report prints the native stack; afterwards chain to the previous handler, restore `SIG_DFL` and re-raise, so exit status and core dump are unchanged. `backtrace_symbols_fd` is warmed at install, because its first call loads libgcc. Release binaries are stripped, so frames read `module(+offset)`.
- **Left out:** SIGILL and SIGFPE (GNU R's handler covers SIGILL only), a Windows filter (backlog), and the GNU R runtime, which installs its own.
- Pinned by `crates/crash/tests/test_crash.rs` and `crates/ry/tests/scenarios/crash-report.scenario`.

## Platform code is type-checked from Linux before the release job builds it

**Status:** built.

- `cargo clippy --target aarch64-apple-darwin` and `--target x86_64-pc-windows-gnullvm` run on Linux, because a release round on those runners costs most of an hour. Nothing links, but C build scripts run, so `CC_<target>`/`CXX_<target>` point at a clang wrapper passing `--target`, with `llvm-ar-18` and `llvm-windres-18`. It catches `pipe2`, `sched_setaffinity`/`sched_getaffinity`, `cpu_set_t` and Apple's `iconv` signatures. The Windows CLI alone type-checks with `zig cc -target x86_64-windows-gnu`.
- **Trap:** zig's Mach-O linker ignores `-exported_symbols_list`, so a zig-linked build cannot show a missing export; a real macOS cross-build uses `clang --target=arm64-apple-macos11 -isysroot $SDKROOT -fuse-ld=lld`.

## The dev shells' GNU R is cached on every change to its inputs

**Status:** built.

- `.github/workflows/cachix.yml` also runs on a push to main touching `flake.nix`, `flake.lock` or `tools/base-r/**` (the only inputs of `lib.gnuR`), builds the `default` and `ci` shells and pushes them without the Rust toolchain and `ry-deps-*`, so `nix develop` never compiles GNU R. The path filter keeps runs rare; the `ry` package is pushed only at releases.

## The hot suite replays the package entries the code bases spend their time in

**Status:** built.

- **Why:** the published suites' means disagree with the three code bases, which are the goal and take minutes per run, so no suite was fast and representative. `tools/bench/hot/` (`just bench hot`) holds 100 programs, each one package entry with up to three captured calls, replayed by `harness.R` in batches of about 20 ms of GNU R time; `tools/bench/tooling/hot.py` builds and checks it; `testing.md`, "The hot suite", describes it.
- **The unit is a package entry,** a closure call from user code into package code, keyed by the callee's namespace and the name it was called by, because there time can be attributed and the arguments are values user code built. Rejected: hot bodies anywhere, whose arguments are promises into package frames and whose times nest.
- **User code is code evaluating in an environment whose top level is the user's, however entered** (`capture::evaluating` from promise forcing, `eval`, `Rf_eval`, `R_tryEval`, `R_ParseEvalString`); counting only user closures charged `test_that()` blocks to `testthat::test_that`, and the rule raised the captured share of the three runs from 68%, 50% and 41% to 80%, 71% and 72%.
- **A weight is a ratio estimate:** a key's capture time times GNU R's over the capture's time for the same uniformly drawn samples, so recorder overhead cancels. A code base's predicted speed-up is the weighted harmonic mean of its programs' speed-ups.
- **The recorder (`crates/interp/src/capture.rs`) keeps the draw uniform and its own cost out.** It draws samples by reservoir; a selected call too large to store empties its slot (counted in `oversize`) rather than keep the earlier sample, which would bias toward small calls. Its own work runs suspended and is subtracted from every open record and the total. Times are integer nanoseconds, because six decimal places of seconds keep one digit of a 2 µs call. A failure inside it makes the call uncapturable (`uninspectable`) and never raises, because a measurement must not change the program. A forked child stops recording in a `pthread_atfork` child handler and writes nothing.
- **Replays run in the code bases' session** (every imported package loaded first, `session` in `suite.toml`), because results otherwise depend on program order (`zoo::rollapply` pads differently once xts loads, and ompr finds no solver before `ROI.plugin.highs` loads).
- **Checksums hash everything exact and compare numeric sums with one tolerance** (`replay.R`, `bench.same_checksum_part`), because rounding then hashing failed on last-bit differences (`power.t.test` differs by a few ulp). The hash skips data.table's `.internal.selfref` (an address); each numeric vector is weighted by its position with the golden-ratio sequence `1 + frac(k * phi)` (no short period), and the two numeric fields compare at an absolute 1e-10 or a relative 1e-13. A sample whose value differs between two GNU R calls is marked `varies` and not compared.
- **Captured temporary paths are relocated** under the replay's `tempdir()` (`relocate`). The validator runs GNU R with its own `TMPDIR` and drops a sample that touches any other path, leaves a different set of paths on its second call, or opens a graphics device.
- **A call that changes its arguments gets a fresh copy per call** (`fresh` in `suite.toml`), unserialized outside the clock, else a data.table `:=` replays on the previous result. The validator replays with the JIT off (so bytecode attached to a closure does not show), and its calibration is the median of five batches, like the runner.
- **The deterministic metric is callgrind instructions per region** (`--instructions`, `.ry_bench_region`), because the VMs expose no hardware counters (`perf_event_open` fails). A dump after instrumentation stops carries its count on the `totals:` line (`summary:` reads zero). Programs are dealt over `bench.INSTRUCTION_SHARDS` processes by position, never core count, and run on one OpenMP thread, because callgrind counts idle workers spinning (a grouped data.table `:=` counted 80 to 96 million instructions on four threads, 45.8 million on one). Two passes agree to 0.01% on a code base's weighted total, 0.013% on the median program and 0.41% on the worst. The suite takes about 80 s.
- **The suite tracks the code bases at r = 0.976 over 33 changes** (`hot.py track`), and a predicted change shows up about 0.85 times in the code bases, because the programs hold 77 to 80% of a run. On dsbench's whole run under callgrind, the JIT off measured -0.21% (suite: -0.27%) and the builtin fast paths off +1.51% (suite: +2.95%); each ran once, so a code base's count repeatability is unmeasured.
- **Read changes between predictions, never a prediction against a code base's time:** the predicted level is pessimistic (1.34x, 1.39x and 1.47x predicted against 1.62x, 1.68x and 1.62x measured), for reasons still open (`backlog.md`, "Open: the hot suite").
- **Wall time on a shared VM cannot show a change under about 10%:** the same build timed twice 100 minutes apart was 14 to 16% slower on the code bases and 21 to 27% slower in the suite. Wall-time comparisons share rounds (`--baseline`), and smaller changes are decided on instruction counts.

## The code bases are a benchmark suite of their own, outside the headline

**Status:** built.

- `tools/bench/codebases` records each code base's time as the sum of its test blocks' times, with GNU R, ry and ry without the JIT in the same rounds, plus each code base's per-block geometric mean and each engine's slowest code base, shown in its own section after the package suites.
- **No headline row,** because generated code says less about real code than the package suites; kept out by `outsideSummary` (`docs/src/lib/benchmarks.ts`) and `outside_summary` (`tools/bench/readme.py`). Release runs use `--extra-rounds 0`, because a round takes about 22 minutes (a release run about 4 hours). The corpus lacks polars, Rblpapi and esgtoolkit, so the code bases run without them.
- **Revisit** a headline row after two release runs, once the intervals are known.
- **Trap:** the Posit snapshot's xts binary truncates `ATTRIB`'s result on R 4.6, so `tools/corpus/install.R` reinstalls xts from source.

## The profile summary hashes private package names with a secret key per project

**Status:** built.

- **`package_display` (`crates/interp/src/profile_summary.rs`) labels a private package with SipHash-2-4 under a 128-bit secret key,** because an unkeyed hash lets anyone confirm a guessed name. The key is `RY_PROFILE_KEY` (32 hex digits; invalid is an error) or a per-project file under the user's cache directory, `ry/profile-keys/`, named by an unkeyed SipHash of the project's canonical path (`outside-projects` outside any project; the project is the nearest directory with `ry.toml`, `roughly.toml` or `DESCRIPTION`), created once with `getrandom`, mode 0600, by hard-linking a staged file so racing sessions agree. With no keepable file the run uses its own key and the summary says why.
- **Cache directory, not the project's `.ry/`,** because a file in the project can be committed and `ry run` writes nothing else there. Two machines label differently; `RY_PROFILE_KEY` from a CI secret gives a fleet one key.
- **Randomness seam:** tests call `package_display` with a fixed key, and the scenario sets `RY_PROFILE_KEY`.

## The builtins sweep times every export of base, stats and utils, behind a shrinking ratchet

**Status:** built.

- **`tools/bench/builtins`** holds one or more cases per exported function (`cases/*.R`, a scalar, a 1e3 and a 1e6 input where meaningful, plus character, list, matrix and data frame variants) and a skip with a reason for the rest (I/O, nondeterminism, language constructs, session changes). `sweep.py list` and `check` keep every export covered, so a new export in base R fails the check until it gets a case or a skip.
- **Why per function:** suites and kernels find a slow builtin only when a workload happens to hit it. The sweep finds it by construction, and `sweep.py weights` (call counts in the code-base templates and the oracle corpus) orders the fixes.
- **The ratchet is `slower.txt`,** the functions slower than GNU R in any case on ry with or without the JIT. `ratchet` fails on an unlisted function below `1 / (1 + tolerance)` after confirming rounds, `--update` removes functions that pass, and `check` refuses a list longer than main's. `--seed` rewrites it, which only a change to the cases justifies. `--ci` drops the 1e5+ inputs and shortens the batches; the workflow is `.github/workflows/builtins.yml`, run by hand like `ci.yml`.
- **bench.py runs it as the `builtins` suite** with its own round counts (`SUITE_ROUNDS`), because the harness repeats each case in five batches inside one process and a round takes most of an hour.
- **The large fixtures are dropped after each case and rebuilt on demand**, with a collection between cases. Hundreds of megabytes of live 1e6 inputs made every allocating case on ry pay for collections that rescan them, so allocation-heavy calls measured 2 to 4 times slower than they run in real code: before the change 577 functions looked slower than GNU R, after it 81 did. A case's time must not depend on which cases ran before it.
- **Harness traps:** `bench(.name, ...)` so a variant named `n` or `na` is not partially matched to the name; every timed call reads a variable, because both engines fold calls of constants; a connection left open in one case warns inside a later one. `compare --parallel` runs the engines side by side for a fast triage, at the cost of noise.

## The edit-to-signal loop is tuned for agents

**Status:** built. Commands and timings live in `docs/src/content/docs/development/testing.md`, "The fast loop".

- **Legacy symbol mangling in `[profile.dev]`** (`Cargo.toml`'s `[profile.dev] rustflags`, which needs the `profile-rustflags` feature `.cargo/config.toml` turns on): the debug `libR.so` is 623 MB instead of 894 MB with v0, and an interpreter rebuild is about 1 s faster. Release keeps the default.
- **mold through `RUSTC_WRAPPER=tools/mold-rustc`, set by `tools/env.sh`,** not through `CARGO_TARGET_<triple>_LINKER`: cargo fingerprints the linker setting, so toggling it rebuilt 427 crates, while the wrapper is outside the fingerprint and toggling it rebuilds nothing. The wrapper adds the flag only for Linux targets, and `tools/env.sh` sets it only on Linux. mold cuts the link's peak memory from 3.1 GB to 0.9 GB.
- **`doctest = false` in every crate,** because no crate has doctests and each doctest pass costs a rustdoc invocation.
- **One scoped-thread pool, `syntax::testing::in_parallel`,** used by the fixture harness, the oracle, `test_scenarios` and `test_format_range`. Results return in item order. Items are dealt out statically (worker `w` of `n` takes `w`, `w + n`, ...), not to whichever worker is free, because fixture renders reuse a per-thread database (`semantics::testing`) and a work-stealing order would give each database a different history on every run; `RY_TEST_WORKERS` pins `n` and failures print it. Each item's panic is caught and returned as that item's `Err(message)`, so one panic neither loses its case's name nor discards the other results.
- **`FIXTURE_FILTER` is a substring of `group__case`,** so a group prefix runs a group; a filter matching nothing in any suite fails. Every failure prints a one-line rerun command.
- **The oracle builds the workspace ry itself** (`cargo build -p ry-lang -p libr` unless `ORACLE_RY`), so it never compares a stale `libR.so`, and prints failures only unless `--verbose`. `cargo test -p oracle` compares a seeded sample by default and every case only with `ORACLE_ENGINE` set; it skips when no base R is built, except with `ORACLE_ENGINE` set, where it fails. The target directory comes from `oracle::target_directory` (honoring `CARGO_TARGET_DIR`) for both the build and the lookup.
- **Nested `cargo build` calls in tests stay,** because a no-op build costs 0.35 s once per process (`syntax::testing::cargo_build`) and is the only freshness guarantee. Caching `build_package` across processes would save about 2 s, not worth a staleness risk.
- **Tests that link `frames` live in `crates/frames/tests`,** so interp's test binaries link no Polars (each moved binary was 748 MB).
- **interp's integration tests stay separate binaries,** because the interpreter is a process-wide singleton (`interp::install`) and tests sharing one process would share its state. typed's tests are one binary (`tests/test_typed/`), table's engine tests one (`tests/test_table/`); `test_memory` keeps its own `#[global_allocator]`.

## The interpreter stays one crate, because the edit loop is bound by links

**Status:** built (a decision not to split).

- **Measured** (4 cores, debug, one-line body edit, then `cargo build -p ry-lang -p libr`): interp 11.0 s, heap 11.5 s, jit 11.7 s, semantics 14.4 s, ry 9.9 s; `cargo check -p <crate>` 0.8 to 3.9 s. A clean build is 56 s for `ry-lang` and 145 s more for `libr`, whose critical path is polars-core, polars-ops, table, frames, libr.
- **Where the time goes.** After an interp edit, interp's rustc takes 3.4 s with 1 of 256 codegen units recompiled; the rest is fixed front-end cost. The `libR.so` link alone takes 3.6 s, and the binary was 1.38 GB, 905 MB of it symbol names.
- **Rejected: splitting `interp`.** 58 of its 69 modules form one dependency cycle through `use crate::`, which a split would have to break first, for an estimated 1 to 2 s of a 10 to 15 s loop.
- **Rejected by measurement.** The Cranelift codegen backend cannot build interp's explicit tail calls or the variadic `f64` calls in `numeric` and `stats`, was no faster with those three on LLVM, and broke unwinding for salsa cancellation in 9 language-server tests. `-Zthreads=4`, `debug = 0` and `split-debuginfo = "unpacked"` changed nothing. GNU `bfd` took 30 s per `libR.so` link.
- **Not tried.** sccache: containers are fresh and have no persistent cache, so it would need a remote backend and would help only clean builds.
- **What was done instead.** Legacy mangling in the dev profile, mold where installed, and keeping Polars out of interp's test links (the entries on those in this file).

## Agent-written notes are not mass-rewritten for brevity

**Status:** built (a process rule).

- **What happened.** A terse rewrite of the decision log by agents that had not written it halved its size and dropped a load-bearing fact (a number behind a decision, a rejected alternative, a trap, a code pointer) from about 421 of 509 entries; an entry-by-entry comparison against the old text put them back. A rewrite of long source comments lost invariants and the reasons behind constants in about a dozen blocks while saving 20 KB of 3.26 MB.
- **Rule.** Cut context cost by structure (area files, an index of headings, byte budgets, a comment lint that ratchets) and by deleting what is superseded, not by rewording dense notes. A rewording pass needs an entry-by-entry comparison against the original, which costs more than it saves.

## Shipped binaries keep their symbol table, and crash reports name functions from it

**Status:** built; the release size cost is unmeasured.

- **Rule.** Release builds strip debug info only (`[profile.release] strip = "debuginfo"`, `strip --strip-debug` in `tools/layout/optimize.sh` after BOLT), so `.symtab` ships in `ry` and `libR`. Release uses legacy symbol mangling, like dev, because v0 names roughly double `.strtab`.
- **Why.** `dladdr` sees only `.dynsym`, which names the few exported functions, so a stripped report attributed every internal frame to the nearest export, and a test user's crash report could not be read.
- **Shape.** On Linux with glibc, `crash` maps each frame's file read-only in the signal handler (the executable through `/proc/self/exe`, since R's `setwd` breaks a relative `argv[0]`), scans `.symtab` for the `STT_FUNC` symbol holding the address, and demangles it with `rustc-demangle` without its hash, allocating nothing. macOS keeps `backtrace_symbols_fd`.
- **Rejected.** Separate symbol files published as release assets: smaller archives, but every report needs an offline symbolization step with the matching build, which a user filing a report does not have. `optimize.sh` prints `.symtab` and `.strtab` sizes so the cost shows in each release build log.

## The differential fuzzer compares canonical dumps, over four kinds of program

**Status:** built.

- **What was weak.** `oracle fuzz` generated one kind of program from an ASCII grammar and compared printed output and deparse text. Two bugs a test user hit were out of its reach: a crash on a closure's second call with a matrix of 64 or more elements (no program called one closure twice on a value at a storage threshold), and a lookup by a non-ASCII name that returned `NA` (printing shows no encoding mark, and no name was non-ASCII).
- **The observer is R code in the driver** (`crates/oracle/src/observe.R`), not `serialize()` bytes: a text dump diffs, names the differing node, and can leave out what is not a contract (an environment's address, a compact sequence's storage). It dumps each top-level value, its visibility and each condition's classes and call, numbered by top-level expression so a report names the expression.
- **A non-ASCII string's mark is lenient by default** (`u` for marked UTF-8 and for unmarked valid UTF-8). Which of the two a builtin returns differs between ry and GNU R in dozens of builtins and changes no result in a UTF-8 session, so strict comparison buried every other finding; `ORACLE_OBSERVE=strict` is the audit mode. The name bug is found through its result (`x[name]` is `NA`), not through the mark.
- **Metamorphic programs need no reference engine.** A trial runs one body on one value in several forms (repeated calls, other scopes, a warmed closure, a shared or serialized value) and prints `DIFFERENT` when a form disagrees with the first run by `identical()`. That is a bug in the engine that printed it, so the default suite checks the kind on ry alone, tier 0 against the forced JIT, and the comparison with GNU R is the long run's. Why not GNU R in the default suite: ry differs from GNU R on about half of the builtin programs (error messages, accepted types), and a default test must pass.
- **Programs are dense.** An engine takes about 0.6 s to start in a debug build and a trial takes milliseconds, so a program holds six to twelve trials. At two to four trials, 440 seeds found neither user bug; at six to twelve, both are found in seeds 0 to 99.
- **Swarm testing.** Each seed enables each of 33 generator features with probability one half, so a feature pair that is rare under uniform choice dominates some programs.
- **A corpus seed is a finding only when the unmutated snippet agrees.** Real code already differs from GNU R in known ways; those snippets go to `target/fuzz-corpus/base-differs.txt`, and a diverging mutant runs twice under GNU R so a snippet that depends on the run is dropped.
- **The reducer is delta debugging over the syntax tree with a signature** (which of crash, error, warnings, messages, output, value or dump differs), so a reduction cannot drift from a crash to an unrelated message difference. It is slow on a large program (about one candidate per second in a debug build), so cut a metamorphic finding to its failing trial by hand before reducing it.
- **One runner for every kind** (`oracle fuzz --kind`): workers take (seed, kind) pairs from one counter, so `--minutes` covers a prefix of the seeds whatever `--jobs` is, and a run is replayed from its base seed.
