default:
    @just --list

# No recipe exports its parameters (`$name`): Windows environment names are case-insensitive, so an
# exported `version` merges into a workflow's `VERSION` and the recipe's bash never sees `version`.
# A script assigns each parameter to a shell variable with `quote()` instead.

# Run ry from source, e.g. `just ry check` or `just ry run script.R`. ry's runtime (libR) is a
# separate library that `ry run` loads at run time, so cargo would not build it for `cargo run`.
# Build warnings show only when the build fails, so they do not bury ry's own output.
# It runs in the directory you call it from, so relative paths mean what you typed.
[no-cd]
[no-exit-message]
ry *args:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! output=$(cargo build --color always -q --manifest-path "{{ justfile_directory() }}/Cargo.toml" -p ry-lang -p libr -p rcmd 2>&1); then
    	printf '%s\n' "$output" >&2
    	exit 1
    fi
    exec "${CARGO_TARGET_DIR:-{{ justfile_directory() }}/target}/debug/ry" {{ args }}

# Build ry and walk through checking, typing, formatting, running and package loading in a demo project.
try:
    @tools/try.sh

# Build ry and print how to point an editor's language client at it.
try-editor:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release -p ry-lang
    ry="${CARGO_TARGET_DIR:-$PWD/target}/release/ry"
    echo "ry's language server is \`$ry server\`. Point your editor at it:"
    echo
    echo "  VS Code:  install the ry extension, then set \"ry.path\": \"$ry\" in settings.json"
    echo "  Zed:      in settings.json, \"lsp\": { \"ry\": { \"binary\": { \"path\": \"$ry\" } } }"
    echo "  Neovim:   vim.lsp.config('ry', { cmd = { '$ry', 'server' }, filetypes = { 'r' } }); vim.lsp.enable('ry')"
    echo "  Helix:    [language-server.ry] command = \"$ry\", args = [\"server\"] in languages.toml"
    echo
    echo "Or put it on your PATH, which every editor integration searches: export PATH=\"$(dirname "$ry"):\$PATH\""

# Check the toolchain, the oracle's reference GNU R and its packages, the corpus library, that the
# built binaries match the working tree, and free disk. `sudo tools/dev-setup.sh` provisions it all.
doctor:
    @python3 -I tools/doctor.py

# Push prebuilt trees to the ry.cachix.org binary cache and record them in tools/cache.lock (commit
# it): `r` is the reference GNU R that tools/dev-setup.sh then restores in seconds, `corpus` the
# package-test corpus library. Never push ry's own builds; the cache holds 5 GB.
cache-push +trees="r":
    for tree in {{ trees }}; do bash tools/cache.sh push "$tree" || exit 1; done

# Free disk in target/ without forcing a full rebuild: unused build configurations and stale
# incremental caches. `--tests` also removes test executables, `--dry-run` only reports.
prune *args:
    python3 -I tools/prune.py {{ args }}

test *args:
    cargo test {{ args }}

# The pre-landing check: tests, clippy at -D warnings, fmt, the licence notices, the memory layout and comment length.
gate: base-r-source battery clippy fmt-check (notices "--check") (readme "--check") (memory "check") comments unsafe

# Fetch GNU R's pinned source release into the download cache, over the network when it is not
# there. The battery's check that the base packages build on this checkout's runtime
# (`test_base_build` in the oracle crate) skips without it, so the gate fails instead.
base-r-source:
    tools/base-r/build.sh --fetch

# CI's test job, step for step: .github/workflows/ci.yml runs `nix develop .#ci -c just ci`. Run it
# in that shell, whose GNU R, compiler, glibc and locales are the ones CI has. The variables are
# the CI runner's: `R_HOME` makes the runtime's suites load base R from the reference GNU R.
ci:
    #!/usr/bin/env bash
    set -euo pipefail
    export TZ=UTC NOT_CRAN=true CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0
    R_HOME=$(R RHOME)
    export R_HOME
    bash tools/oracle-locales.sh
    just base-r dev
    just fmt-check
    just clippy
    just battery

# CI's collection-stress job (ci.yml, job `stress`, once per seed), which the release gate requires
# next to `just ci` (docs: development/releasing.md, "The release gate"). It runs in the same shell
# with the same variables. Every oracle case runs twice in one process under stress seeded with
# `seed` with the heap checked, and 300 generated programs of their own per seed run under GNU R,
# tier 0, the JIT and the same repeated stress.
ci-stress seed="0":
    #!/usr/bin/env bash
    set -euo pipefail
    export TZ=UTC NOT_CRAN=true CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0
    R_HOME=$(R RHOME)
    export R_HOME
    bash tools/oracle-locales.sh
    just base-r dev
    FUZZ_SEED={{ seed }} just gc-torture repeat
    just fuzz-differential $(({{ seed }} * 300)) 300 ry-tier0,ry-jit,ry-repeat-gctorture-seeded

# Workspace tests; `zed_ry` needs a wasm toolchain, so it is excluded.
battery *args:
    cargo test --workspace --exclude zed_ry {{ args }}

# The tests that need no GNU R (test.yml, job `tests`): every test of the crates that build
# without the interpreter, libR and Polars. A test that compares with GNU R or runs ry's runtime skips when
# there is no `R` on the PATH and no built base R, so the recipe needs neither. `table` and the
# runtime crates are left to `just battery`: their builds link Polars or the interpreter.
test-fast:
    cargo test --no-fail-fast -p syntax -p format -p semantics -p ide -p sniff -p loader -p heap -p numeric -p ssa -p rdb -p crash -p oracle -p rcmd -p repl -p ry-lang

clippy:
    cargo clippy --workspace --exclude zed_ry --all-targets -- -D warnings

fmt-check:
    cargo fmt --all --check

# The fixture cases whose `group__case` id contains `case`: one case, or a whole group with
# `just fixture pipes__`.
fixture case binary="test_typing_fixtures" package="semantics":
    FIXTURE_FILTER={{ case }} cargo test -p {{ package }} --test {{ binary }}

# Re-bless expectations, e.g. `just bless -p semantics --test test_typing_fixtures`.
bless *args:
    RY_BLESS=1 cargo test {{ args }}

# Regenerate docs/formatter.md through the shipping formatter.
format-docs:
    RY_BLESS=1 cargo test -p format --test test_format_docs

# Run the docs' marked examples, or those of the pages whose path contains `page`.
docs-test page="":
    DOCS_PAGE={{ page }} cargo test -p ry-lang --test test_docs

# Rewrite the output of the docs' exact examples from the real tool; excerpts are left to you.
docs-bless page="":
    RY_BLESS=1 DOCS_PAGE={{ page }} cargo test -p ry-lang --test test_docs

# Every seeded harness at `depth` times its default seeds (`FUZZ_DEPTH`) for `minutes`, each round
# starting at a fresh seed from `base`, plus the
# libr package suites under collection stress; failures are logged with their replay commands to
# target/fuzz-deep.log. A scheduled run passes a fresh base, e.g. `just fuzz-deep 480 $(date +%s)`.
fuzz-deep minutes="120" base="1000000" depth="10" *args:
    python3 -I tools/fuzz-deep.py --minutes {{ minutes }} --base {{ base }} --depth {{ depth }} {{ args }}

# One libFuzzer target (`cargo fuzz list` names them all); seed with `cargo -Zscript scripts/seed-fuzz-corpus.rs`.
fuzz-run target="semantics" *args:
    cargo fuzz run {{ target }} {{ args }}

# Every libFuzzer target for `seconds` of wall clock each, after seeding the corpora and building
# the targets. `features` picks the targets by the features of fuzz/Cargo.toml: `just fuzz-all 60
# runtime` leaves out the two whose build links Polars. The bound is `timeout`, not libFuzzer's
# `-max_total_time`, because libFuzzer replays a target's whole corpus before its own limit counts
# and the slowest target needs about 15 minutes for that. A target that is interrupted at the bound
# has passed. Seeds are replayed in random order (`-prefer_small=0`), so a target that cannot get
# through its corpus in time reads a different sample on each run. Every target runs, and the
# recipe fails at the end if any found a failure. The lockfile is seeded from the workspace's,
# because a fresh resolution can pick a salsa-macro-rules that does not match the pinned salsa.
fuzz-all seconds="120" features="runtime,polars":
    #!/usr/bin/env bash
    set -euo pipefail
    cp Cargo.lock fuzz/Cargo.lock
    cargo -Zscript scripts/seed-fuzz-corpus.rs
    cargo fuzz build -O --no-default-features --features "{{ features }}"
    targets=$(cargo metadata --manifest-path fuzz/Cargo.toml --no-deps --format-version 1 | python3 -I -c '
    import json, sys
    enabled = set(sys.argv[1].split(","))
    for target in json.load(sys.stdin)["packages"][0]["targets"]:
        if set(target.get("required-features", [])) <= enabled:
            print(target["name"])
    ' "{{ features }}")
    failed=""
    for target in $targets; do
        echo "== $target"
        status=0
        timeout --signal=INT --kill-after=60 {{ seconds }} \
            cargo fuzz run -O --no-default-features --features "{{ features }}" "$target" -- \
            -prefer_small=0 -timeout=30 -rss_limit_mb=4096 || status=$?
        if [ "$status" -ne 0 ] && [ "$status" -ne 124 ]; then
            echo "== $target FAILED (exit status $status)"
            failed="$failed $target"
        fi
    done
    if [ -n "$failed" ]; then
        echo "fuzz targets that found a failure:$failed"
        exit 1
    fi

# Phase timings, memory and typing bursts for a workspace.
stats path=".":
    cargo run -q -p ry-lang -- debug analysis-stats {{ path }}

# Corpus perf and memory budgets, and the benchmark memory budget; fetch with `cargo -Zscript scripts/fetch-corpus.rs`.
perf:
    cargo test -p semantics --profile iterate --test test_perf -- --ignored --nocapture
    cargo test -p ry-lang --profile iterate --test test_perf -- --ignored --nocapture

# Times one published benchmark suite (public, packages, kernels, applications, codebases, hot, typed, or all). All but
# `typed` run on GNU R, ry and, where a program has a typed version, ry typed in strict mode; `typed` runs every typed
# program against its Rust twin and needs rustc, not GNU R. `hot` exists once `just bench-hot-capture` generated it. Rounds are paired and shuffled, with a checksum
# gate against the suite's baseline, e.g. `just bench kernels --rounds 3 --filter fib` on target/release,
# `just bench typed --ry target/iterate/ry --any-build` for a look at another build, or
# `just bench all --release ry-x86_64-unknown-linux-gnu.tar.gz --write` on a tagged release archive (10+ rounds,
# stores each suite's results.json, which the READMEs and the benchmarks page render).
bench suite *args:
    #!/usr/bin/env bash
    set -euo pipefail
    # A release archive or a named binary brings its own build; anything else measures target/release.
    case " {{ args }} " in *" --release "*|*" --ry "*) ;; *) cargo build --release -p ry-lang -p libr -p rcmd ;; esac
    python3 -I tools/bench/bench.py {{ suite }} {{ args }}

# The builtins sweep, every exported function of base, stats and utils timed on GNU R and ry (tools/bench/builtins/sweep.py):
# `just bench-builtins check` (coverage and slower.txt, seconds), `just bench-builtins ratchet --ci` (the CI subset, minutes),
# `just bench-builtins compare --filter '^base::paste'`, `just bench-builtins validate --filter '^stats::' --with-ry`.
bench-builtins command *args:
    #!/usr/bin/env bash
    set -euo pipefail
    case {{ quote(command) }} in list | check | weights) ;; *) cargo build --release -p ry-lang -p libr -p rcmd ;; esac
    python3 -I tools/bench/builtins/sweep.py {{ command }} {{ args }}

# The builtins ratchet as .github/workflows/builtins.yml runs it: a release build with its base R, then the CI subset
# of the sweep, which fails when a function that tools/bench/builtins/slower.txt does not list is slower than GNU R.
builtins-ci:
    just base-r release
    cargo build --release -p ry-lang -p libr -p rcmd
    python3 -I tools/bench/builtins/sweep.py check
    python3 -I tools/bench/builtins/sweep.py ratchet --ci

# Generates the hot suite (tools/bench/hot/suite.toml and samples/) from the code bases that tooling/generate.py wrote
# to DIR: captures their package calls on ry, validates and calibrates the samples under GNU R, then selects and weighs
# the programs, e.g. `just bench-hot-capture /tmp/codebases`. CAPTURES keeps the raw capture for another selection.
bench-hot-capture dir captures="target/hot-captures":
    cargo build --release -p ry-lang -p libr -p rcmd -p oracle
    python3 -I tools/bench/tooling/hot.py capture --codebases {{ dir }} --out {{ captures }}
    python3 -I tools/bench/tooling/hot.py validate --captures {{ captures }}
    python3 -I tools/bench/tooling/hot.py select --captures {{ captures }}

# Checks that the hot suite predicts the code bases in DIR: times both on each ry configuration (`NAME=RY[,VARIABLE=VALUE...]`,
# the first being the reference) and on GNU R, and prints the predicted against the measured change with their correlation, e.g.
# `just bench-hot-track /tmp/codebases head=target/release/ry nojit=target/release/ry,RY_JIT=off old=/tmp/ry-0.4.3/ry`.
bench-hot-track dir +configurations:
    cargo build --release -p oracle
    python3 -I tools/bench/tooling/hot.py track --codebases {{ dir }} --out target/hot-track.json {{ configurations }}

# Prints what each R-level mechanism costs on ry in instructions (a closure call, an S3 dispatch, a `tryCatch`, a builtin call),
# counted under callgrind, e.g. `just bench-mechanisms --ry target/profiling/ry --filter dispatch`.
bench-mechanisms *args:
    python3 -I tools/bench/tooling/mechanisms.py {{ args }}

# Generates the three 300K-line benchmark code bases and prints cold-check and language-server latency.
bench-check *args:
    cargo build --release -p ry-lang
    python3 -I tools/bench/tooling/check.py {{ args }}

# Scores typed ry's hill climb: speed against Rust (one round of the `typed` benchmark suite), friendliness cases, and seeded soundness fuzzing, e.g.
# `just climb`, `just climb --only friendly`, or `just climb --log "adapt literal arithmetic"` to append a step to tools/climb/log.tsv.
climb *args:
    cargo build --release -p ry-lang -p libr
    python3 -I tools/climb/run.py {{ args }}

# Keeps .agents/memory navigable: `split` moves a legacy decisions.md into decisions/<area>.md,
# `index` regenerates decisions/index.md, and `check` fails on a stale index or an over-budget file.
memory *args="check":
    python3 -I tools/memory.py {{ args }}

# Fails when a Rust file has more over-long, banner or process-history comments than its baseline,
# e.g. `just comments report 20` for the worst blocks or `just comments baseline` after shortening some.
comments *args="check":
    python3 -I tools/comments.py {{ args }}

# Fails when a file outside heap, libr, jit, typed and ssa has more `unsafe` than its baseline, e.g.
# `just unsafe stats` for the counts per crate or `just unsafe baseline` after removing some.
unsafe *args="check":
    python3 -I tools/unsafe.py {{ args }}

# Regenerates what the READMEs show: the benchmark blocks and chart from the committed results files,
# and the diagnostic panels docs/public/diagnostic*.svg from real `ry check` runs on the README's examples.
# `--check` only reports what differs.
readme *args:
    #!/usr/bin/env bash
    set -uo pipefail
    status=0
    python3 -I tools/bench/readme.py {{ args }} || status=1
    cargo build -q -p ry-lang || exit 1
    cargo -Zscript scripts/render-diagnostic-svg.rs "${CARGO_TARGET_DIR:-{{ justfile_directory() }}/target}/debug/ry" {{ args }} || status=1
    exit $status

# Manages the package-test corpus pinned in tools/bench/packages/lock.tsv: `just bench-packages install` fetches and
# installs it from the rrepo CRAN mirror into .corpus/packages, and `just bench-packages lock` re-pins it.
bench-packages *args:
    python3 -I tools/bench/tooling/packages.py {{ args }}

# Times base R kernels (sort, regex, grouping, xts/zoo rolling ops, ...) on GNU R and ry's runtime with checksums, e.g. `just bench-base --rows 1e6 --memory`. zoo and xts load from RY_BENCH_RLIB.
bench-base *args:
    cargo build --release -p ry-lang -p libr -p rcmd
    python3 -I tools/bench/internal/base/run.py {{ args }}

# Times the kernels cut from a user's profile of a package test suite (S4 construction, scalar calls of small base closures,
# date conversion, xts and data.table calls, parsing and reading source files) on GNU R and ry's runtime with checksums, e.g. `just bench-ci --only k1,k3`.
bench-ci *args:
    cargo build --release -p ry-lang -p libr -p rcmd
    python3 -I tools/bench/internal/ci/run.py {{ args }}

# Times each List, Str, Dict and Result function on ry against its fastest GNU R equivalent (base R, purrr, stringr, stringi from R_LIBS_USER), e.g.
# `just bench-stdlib` (every function at n = 1e6), `just bench-stdlib Str. 1e5`, or `just bench-stdlib "" 1e6 --sort ratio --write tools/bench/internal/stdlib/results.md`.
bench-stdlib filter="" n="1e6" *args:
    cargo build --release -p ry-lang -p libr
    python3 -I tools/bench/internal/stdlib/run.py --only "{{ filter }}" --n {{ n }} {{ args }}

# Runs the table benchmark against data.table and rewrites its table in r-runtime.md, e.g.
# `just bench-table .bench/table` (1e7 rows) or `just bench-table .bench/table1e8 --rows 1e8 --groupby-only`
# (`--joins-only` runs the join questions alone, `--updates-only` the two `:=` loops that write
# existing columns). A limited run needs its markers on the page first.
bench-table dir *args:
    cargo build --release -p ry-lang -p libr -p rcmd
    python3 -I tools/bench/internal/table/report.py {{ dir }} --write {{ args }}

# Drives `ry repl` through a pty against the system R; skips without R.
repl-e2e:
    cargo test -p ry-lang --test test_repl_e2e

docs:
    cd docs && bun dev

# Regenerate the standard library's API pages (docs/.../reference/std/) from types/std.Rtypes.
# The same test, run without RY_BLESS, fails when the committed pages are stale.
docs-std:
    RY_BLESS=1 cargo test -p semantics --test test_std_docs

# Build the docs site from this checkout and upload it to Cloudflare Pages as BRANCH: `main` is the
# production deploy (while main is the Pages project's production branch), any other name a preview.
# Needs CLOUDFLARE_API_TOKEN, CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_PAGES_PROJECT.
docs-deploy branch:
    #!/usr/bin/env bash
    set -euo pipefail
    branch={{ quote(branch) }}
    missing=""
    for variable in CLOUDFLARE_API_TOKEN CLOUDFLARE_ACCOUNT_ID CLOUDFLARE_PAGES_PROJECT; do
    	if [ -z "${!variable:-}" ]; then missing="$missing $variable"; fi
    done
    if [ -n "$missing" ]; then
    	echo "error: docs-deploy needs these environment variables:$missing (development/releasing.md, \"Deploying the docs site\")" >&2
    	exit 1
    fi
    commit=$(git rev-parse HEAD)
    cd docs
    bun install --frozen-lockfile
    bun run build
    bunx wrangler@4.151.3 pages deploy dist --project-name "$CLOUDFLARE_PAGES_PROJECT" --branch "$branch" --commit-hash "$commit"

vsce *args:
    @bun --cwd=editors/code run vsce -- {{ args }}

install-extension:
    mkdir -p release/dev
    just build-extension pre-release --out ../../release/dev/ry.vsix
    code --install-extension release/dev/ry.vsix

# Regenerate THIRD_PARTY_NOTICES from Cargo.lock, the crates' licence files and the GNU R
# provenance headers; `just notices --check` fails if it is stale or a crate's licence breaks the
# rule in LICENSE.
notices *args:
    python3 -I tools/third-party-notices.py {{ args }}

# Build every binary and ry's runtime (libR), e.g. `just build --release`.
build *args:
    cargo build --workspace --exclude zed_ry {{ args }}

zigbuild target *args:
    cargo zigbuild --target {{ target }} {{ args }}

# Package the VS Code extension for one platform from its unpacked archive in DIR/ry-TARGET: it
# bundles the archive's `ry` binary, and build-extension adds the licence and notices.
build-platform-vsix target vscode-target dir kind:
    rm -rf editors/code/bin
    mkdir -p editors/code/bin
    find {{ dir }}/ry-{{ target }} -maxdepth 1 -type f ! -name LICENSE ! -name THIRD_PARTY_NOTICES -exec cp {} editors/code/bin/ \;
    just build-extension {{ kind }} --target {{ vscode-target }} --out ../../{{ dir }}/ry-{{ vscode-target }}.vsix

build-extension kind *args:
    #!/usr/bin/env bash
    set -euo pipefail
    kind={{ quote(kind) }}

    release_flag=$(just vsce-release-flag "$kind")
    # The extension's own code is MIT, but a platform extension bundles the GPL `ry` binary, so
    # the vsix carries both licences and the binary's third-party notices.
    {
    	printf 'The ry extension for VS Code is licensed under the MIT License below. A platform-specific\n'
    	printf 'build of the extension also bundles the `ry` program in bin/, which is licensed under the\n'
    	printf 'GNU General Public License, version 2 or later, as described after the MIT License.\n'
    	printf 'THIRD_PARTY_NOTICES lists the third-party code in `ry`.\n\n'
    	cat editors/code/LICENSE-MIT
    	printf '\n\n'
    	cat LICENSE
    } > editors/code/LICENSE
    cp THIRD_PARTY_NOTICES editors/code
    just vsce package $release_flag {{ args }}

bump-version version:
    #!/usr/bin/env bash
    set -euo pipefail

    sed -i 's/^version = "[a-zA-Z0-9._-]*"/version = "{{ version }}"/' Cargo.toml

    # The VS Marketplace rejects pre-release suffixes, so the extension takes the CLI version
    # without its `-alpha`/`-beta`; the patch number is the one counter across channels, which
    # keeps stripped versions distinct (0.2.1-alpha and 0.2.2-alpha become 0.2.1 and 0.2.2).
    cli_version="{{ version }}"
    vscode_version="${cli_version%%-*}"
    sed -i "s/\"version\": \"[a-zA-Z0-9._-]*\"/\"version\": \"$vscode_version\"/" editors/code/package.json

    # The Zed extension takes the same suffix-free version, so one release names one version in
    # every channel.
    sed -i "s/^version = \"[a-zA-Z0-9._-]*\"/version = \"$vscode_version\"/" editors/zed/extension.toml editors/zed/Cargo.toml

    cargo check # updates Cargo.lock
    git add Cargo.toml Cargo.lock editors/code/package.json editors/zed/extension.toml editors/zed/Cargo.toml
    git commit -m "chore: release {{ version }}"

# Build ry's base R from GNU R's pinned source release (tools/base-r) into target/base-r, where
# every ry and test binary of this checkout finds it. `profile` picks the runtime that runs the build.
base-r profile="release" target="":
    #!/usr/bin/env bash
    set -euo pipefail
    profile={{ quote(profile) }}
    target={{ quote(target) }}
    target_root="${CARGO_TARGET_DIR:-target}"
    case "$profile" in dev | test) directory=debug ;; bench) directory=release ;; *) directory=$profile ;; esac
    if [ -n "$target" ]; then
    	cargo build --profile "$profile" --locked --target "$target" -p libr -p rcmd
    	build="$target_root/$target/$directory"
    	output="$target_root/$target/base-r"
    else
    	cargo build --profile "$profile" -p libr -p rcmd
    	build="$target_root/$directory"
    	output="$target_root/base-r"
    fi
    case "${target:-$(rustc -vV | sed -n 's/^host: //p')}" in
    	*windows*) library=R.dll rcmd=ry-rcmd.exe ;;
    	*apple*) library=libR.dylib rcmd=ry-rcmd ;;
    	*) library=libR.so rcmd=ry-rcmd ;;
    esac
    tools/base-r/build.sh "$output" "$build/$rcmd" "$build/$library"

# Write the start-up image of target/base-r again for this checkout's runtime. `just base-r` writes
# it too, but an image loads only into the runtime build that wrote it, so after an edit to the
# runtime every start is the slow one until this runs.
start-image profile="release":
    #!/usr/bin/env bash
    set -euo pipefail
    profile={{ quote(profile) }}
    target_root="${CARGO_TARGET_DIR:-target}"
    case "$profile" in dev | test) directory=debug ;; bench) directory=release ;; *) directory=$profile ;; esac
    cargo build --profile "$profile" -p libr -p rcmd
    case "$(rustc -vV | sed -n 's/^host: //p')" in
    	*windows*) library=R.dll rcmd=ry-rcmd.exe ;;
    	*apple*) library=libR.dylib rcmd=ry-rcmd ;;
    	*) library=libR.so rcmd=ry-rcmd ;;
    esac
    "$target_root/$directory/$rcmd" start-image "$target_root/base-r" --libr "$target_root/$directory/$library"

# Build a release archive into release/VERSION: `ry` alone, with ry's runtime (libR) and base R
# embedded in it, LICENSE and THIRD_PARTY_NOTICES. TARGET defaults to this machine; Windows builds for x86_64-pc-windows-gnullvm with
# an LLVM mingw toolchain (r-runtime.md, section 11), so `ry.exe`, `R.dll` and Rtools-built packages
# share the UCRT.
package version target="":
    #!/usr/bin/env bash
    set -euo pipefail
    target="{{ target }}"
    [ -n "$target" ] || target=$(rustc -vV | sed -n 's/^host: //p')
    build="${CARGO_TARGET_DIR:-target}/$target/release"
    dir="release/{{ version }}"
    stage="$dir/ry-$target"
    rm -rf "$stage"
    mkdir -p "$stage"
    case "$target" in
    	*windows*) library=R.dll executable=ry.exe ;;
    	*apple*) library=libR.dylib executable=ry ;;
    	*) library=libR.so executable=ry ;;
    esac
    # The runtime and base R are built first, because `ry` embeds both (crates/repl/build.rs). On
    # x86_64 Linux the build is profile-guided and libR is laid out by BOLT (tools/layout/optimize.sh,
    # decisions/heap-gc.md, "The runtime's object model, collector and native boundary", item "The
    # Linux release build is profile-guided"); RY_LAYOUT=off builds it plainly.
    if [ "$target" = x86_64-unknown-linux-gnu ] && [ "${RY_LAYOUT:-}" != off ]; then
    	tools/layout/optimize.sh "$target"
    else
    	just base-r release "$target"
    	base="$(cd "${CARGO_TARGET_DIR:-target}/$target/base-r" && pwd)"
    	RY_EMBED_RUNTIME="$(cd "$build" && pwd)/$library" RY_EMBED_BASE="$base" cargo build --release --locked --target "$target" -p ry-lang
    fi
    # Without PT_GNU_STACK glibc 2.41 and later refuse to load libR (BOLT drops the header).
    case "$target" in
    	*linux*) python3 -I tools/layout/gnu_stack.py check "$build/$library" "$build/$executable" ;;
    esac
    case "$target" in
    	*windows*)
    		# The archive carries no toolchain runtime, so the binaries may import only Windows and
    		# the UCRT. Rust links libunwind statically on gnullvm, and this keeps it that way.
    		runtimes=$("${OBJDUMP:-llvm-objdump}" -p "$build/ry.exe" "$build/R.dll" | sed -n 's/^ *DLL Name: //p' |
    			grep -iE '^(libunwind|libc\+\+|libwinpthread|libgcc|libstdc|msvcrt|vcruntime)' || true)
    		if [ -n "$runtimes" ]; then
    			echo "error: ry.exe or R.dll imports a toolchain runtime the archive lacks: $runtimes" >&2
    			exit 1
    		fi
    		cp "$build/ry.exe" LICENSE THIRD_PARTY_NOTICES "$stage/"
    		(cd "$stage" && 7z a -tzip "../ry-$target.zip" ry.exe LICENSE THIRD_PARTY_NOTICES >/dev/null)
    		;;
    	*)
    		cp "$build/ry" LICENSE THIRD_PARTY_NOTICES "$stage/"
    		tar -czf "$dir/ry-$target.tar.gz" -C "$stage" .
    		;;
    esac
    echo "$dir/ry-$target.$(case "$target" in *windows*) echo zip ;; *) echo tar.gz ;; esac)"

# Print the name of a nightly build of COMMIT: `nightly.<UTC date>.<HHMM>.<six hex digits of the hash>`.
nightly-name commit="HEAD":
    #!/usr/bin/env bash
    set -euo pipefail
    stamp=$(TZ=UTC0 git log -1 --format='%cd' --date=format-local:'%Y%m%d.%H%M' {{ quote(commit) }})
    hash=$(git rev-parse {{ quote(commit) }})
    echo "nightly.$stamp.${hash:0:6}"

# Publish release/NAME as a nightly GitHub release, then delete all but the newest five nightlies.
# It is a normal release with `--latest=false`, never a pre-release: the Zed extension installs the
# newest pre-release, and GitHub's "latest" must keep pointing at a real release.
# Needs `gh` signed in (GH_TOKEN in CI).
nightly-publish name:
    #!/usr/bin/env bash
    set -euo pipefail
    name={{ quote(name) }}
    sha="${GITHUB_SHA:-$(git rev-parse HEAD)}"
    asset="release/$name/ry-x86_64-unknown-linux-gnu.tar.gz"
    repo=$(gh repo view --json nameWithOwner --jq .nameWithOwner)
    url="https://github.com/$repo/releases/download/$name/ry-x86_64-unknown-linux-gnu.tar.gz"
    notes="An untested snapshot of [${sha:0:7}](https://github.com/$repo/commit/$sha), not a release. Install on Linux x86_64 with: \`curl -fsSL $url | tar xz\`"
    if gh release view "$name" >/dev/null 2>&1; then
    	gh release upload "$name" --clobber "$asset"
    else
    	gh release create "$name" --prerelease --latest=false --target "$sha" --title "ry $name" --notes "$notes" "$asset"
    fi
    # The names start with a date, so the newest sort last.
    gh release list --limit 200 --json tagName --jq '.[].tagName | select(startswith("nightly."))' |
    	sort -r | tail -n +6 |
    	while read -r tag; do gh release delete "$tag" --cleanup-tag --yes; done

# Package this machine's release archive, then install it into a scratch directory and smoke-test it.
release-dry-run version="0.0.0-dry-run":
    #!/usr/bin/env bash
    set -euo pipefail
    archive=$(just package {{ version }} | tail -n 1)
    tools/smoke-test.sh "$archive"
    if command -v bun >/dev/null 2>&1; then
    	(cd editors/code && bun install --frozen-lockfile >/dev/null)
    	just build-extension pre-release --out "../../release/{{ version }}/ry.vsix"
    	echo "release/{{ version }}/ry.vsix"
    else
    	echo "note: bun is not installed, so the VS Code extension was not packaged"
    fi
    if command -v nix >/dev/null 2>&1; then
    	nix build .#ry --no-link --print-out-paths
    else
    	echo "note: nix is not installed, so the nix package was not built"
    fi

# Fail unless VERSION is a valid release version and Cargo.toml, the VS Code extension and the Zed
# extension all carry it (the extensions without its -alpha/-beta suffix). The `::error::` prefix
# makes GitHub Actions show the message as an annotation.
release-check version:
    #!/usr/bin/env bash
    set -euo pipefail
    version={{ quote(version) }}
    workspace=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
    extension=$(sed -n 's/^[[:space:]]*"version": "\(.*\)",$/\1/p' editors/code/package.json | head -n 1)
    zed=$(sed -n 's/^version = "\(.*\)"/\1/p' editors/zed/extension.toml)
    if [ "$workspace" != "$version" ] || [ "$extension" != "${version%%-*}" ] || [ "$zed" != "${version%%-*}" ]; then
    	echo "::error::releasing $version, but Cargo.toml is at $workspace, the VS Code extension at $extension and the Zed extension at $zed; run \`just bump-version $version\` first"
    	exit 1
    fi
    just release-kind "$version" >/dev/null

# Unpack the archives in release/VERSION and package the VS Code extension from them: one `.vsix`
# per platform, bundling that archive's binaries, plus `ry.vsix` without a binary for every other
# platform. A platform whose archive is missing is skipped with a note, and then `ry.vsix` is not
# built either: VS Code on that platform would install it, find no binary and fail, whereas with
# no package at all it reports the extension as unavailable there. This runs locally on one
# machine's archive and in a release workflow started with `platforms` set to `unix` or `linux`.
release-assets version:
    #!/usr/bin/env bash
    set -euo pipefail
    version={{ quote(version) }}
    dir="release/$version"
    kind=$(just release-kind "$version")
    (cd editors/code && bun install --frozen-lockfile)
    platforms=(x86_64-unknown-linux-gnu:linux-x64 aarch64-apple-darwin:darwin-arm64 x86_64-pc-windows-gnullvm:win32-x64)
    complete=1
    for platform in "${platforms[@]}"; do
    	target=${platform%%:*}
    	if [ ! -f "$dir/ry-$target.tar.gz" ] && [ ! -f "$dir/ry-$target.zip" ]; then complete=0; fi
    done
    # The universal extension goes first, before build-platform-vsix fills editors/code/bin.
    rm -rf editors/code/bin "$dir/ry.vsix"
    if [ "$complete" = 1 ]; then
    	just build-extension "$kind" --out "../../$dir/ry.vsix"
    else
    	echo "note: an archive is missing, so the universal ry.vsix was not packaged" >&2
    fi
    for platform in "${platforms[@]}"; do
    	target=${platform%%:*}
    	unpacked="$dir/ry-$target"
    	if [ -f "$unpacked.tar.gz" ]; then
    		rm -rf "$unpacked" && mkdir -p "$unpacked" && tar -xzf "$unpacked.tar.gz" -C "$unpacked"
    	elif [ -f "$unpacked.zip" ]; then
    		rm -rf "$unpacked" && mkdir -p "$unpacked" && unzip -q "$unpacked.zip" -d "$unpacked"
    	else
    		echo "note: $dir has no archive for $target, so its extension was not packaged" >&2
    		continue
    	fi
    	just build-platform-vsix "$target" "${platform#*:}" "$dir" "$kind"
    done
    rm -rf editors/code/bin

# Publish release/VERSION: create the GitHub release with every archive and `.vsix` it holds (or
# refresh the assets of one a failed run left behind), then publish each `.vsix` to the VS Code
# Marketplace.
# Needs `gh` signed in (GH_TOKEN in CI) and, outside CI, a VSCE_PAT.
release-publish version:
    #!/usr/bin/env bash
    set -euo pipefail
    version={{ quote(version) }}
    dir="release/$version"
    kind=$(just release-kind "$version")
    # A release limited to some platforms, or without the extension, lacks some of these files.
    shopt -s nullglob
    assets=("$dir"/ry-*.tar.gz "$dir"/ry-*.zip "$dir"/*.vsix)
    if gh release view "$version" >/dev/null 2>&1; then
    	gh release upload "$version" --clobber "${assets[@]}"
    else
    	# A full release claims "latest" explicitly, because GitHub's automatic choice compares
    	# semantic versions and an older nightly published as a normal release has a non-semver tag.
    	flag="--latest"
    	if [ "$kind" = pre-release ]; then flag="--prerelease"; fi
    	gh release create "$version" "$flag" --target "${GITHUB_SHA:-$(git rev-parse HEAD)}" --title "ry $version" \
    		--notes "See the [news page](https://ry-lang.org/news/) for what changed." "${assets[@]}"
    fi
    # Without a VSCE_PAT, vsce publishes with trusted publishing (`--oidc`), which needs vsce 4.1 and
    # a policy for the release workflow on the publisher's Marketplace page. Packaging stays on the
    # locked vsce, because 4.1 rejects bun's node_modules layout when it lists dependencies, and
    # publishing a finished .vsix lists none. `--skip-duplicate` lets a rerun finish a partly
    # published release.
    options=(--skip-duplicate)
    if [ "$kind" = pre-release ]; then options+=(--pre-release); fi
    if [ -z "${VSCE_PAT:-}" ]; then options+=(--oidc); fi
    for vsix in "$dir"/ry-*.vsix "$dir"/ry.vsi[x]; do
    	npx --yes @vscode/vsce@4.1.0 publish "${options[@]}" --packagePath "$vsix"
    done

# Clone r-lib repos to test formatting against.
rlib-clone:
    #!/usr/bin/env bash
    mkdir -p .local/rlib
    for repo in devtools lintr actions httr testthat usethis styler pkgdown; do
    	if [ ! -d ".local/rlib/$repo" ]; then
    		echo "cloning $repo..."
    		git clone --depth 1 "https://github.com/r-lib/$repo.git" ".local/rlib/$repo"
    	fi
    done

rlib *args:
    cd .local/rlib && for dir in */; do (echo "$dir" && cd "$dir" && git {{ args }}); done

vsce-release-flag kind:
    @echo {{ if kind == "release" { "" } else if kind == "pre-release" { "--pre-release" } else { error("kind must be either release or pre-release") } }}

# The channel: a bare `X.Y.Z` is a release; anything else, a git revision included, is a pre-release.
release-kind version:
    #!/usr/bin/env bash
    set -euo pipefail
    version={{ quote(version) }}

    case "$version" in
    	*-alpha|*-beta) echo pre-release ;;
    	*-*) echo "error: version suffix must be -alpha or -beta, got $version" >&2; exit 1 ;;
    	[0-9]*.[0-9]*.[0-9]*) echo release ;;
    	*) echo pre-release ;;
    esac

# Times the benchmark code bases' table modules end to end (data.table on GNU R and on ry, typed DataFrame on ry) and rewrites their table in r-runtime.md, e.g. `just bench-frames --rows 1e7`.
bench-frames *args:
    cargo build --release -p ry-lang
    cargo build --release -p libr
    python3 -I tools/bench/internal/table/frames.py --write {{ args }}

# Install the benchmark dependency closure into .corpus/library (needs GNU R, network access and tools/corpus/system-packages.txt).
corpus:
    Rscript tools/corpus/install.R

# Download the corpus packages' source tarballs, which carry the tests binary installs lack.
corpus-sources:
    Rscript tools/corpus/sources.R

# Run corpus packages' own tests under GNU R and ry's runtime: `just package-tests [--runtime ry] [PKG...]`.
package-tests *args:
    cargo build -q --release -p ry-lang -p libr -p rcmd
    ORACLE_RY=target/release/ry cargo run -q --release -p oracle -- packages {{ args }}

# Assemble ry's R home in .corpus/ry-home: bin/R, bin/Rscript and R CMD run on ry's runtime.
ry-home:
    cargo build -q --release -p libr -p rcmd
    ./target/release/ry-rcmd assemble .corpus/ry-home --include crates/libr/include --libr target/release/libR.so --base "${R_HOME:-target/base-r}"

# Install the closure from source on ry's runtime into .corpus/ry-library: `just corpus-ry-install [--jobs N] [PKG...]`.
corpus-ry-install *args: ry-home
    cargo run -q --release -p oracle -- install {{ args }}

# Install the top CRAN packages outside the closure (tools/corpus/top-packages.txt) and their Suggests into .corpus-top/library with GNU R, and download their sources.
top-packages-setup:
    RY_CORPUS_DIR=.corpus-top RY_CORPUS_ROOTS=tools/corpus/top-packages.txt Rscript tools/corpus/install.R
    RY_CORPUS_DIR=.corpus-top RY_CORPUS_ROOTS=tools/corpus/top-packages.txt Rscript tools/corpus/sources.R

# Build the top packages from source on ry and run their tests under both runtimes: `just top-packages [--jobs N] [PKG...]`.
top-packages *args:
    cargo build -q --release -p ry-lang -p libr -p rcmd -p oracle
    ./target/release/ry-rcmd assemble .corpus-top/ry-home --include crates/libr/include --libr target/release/libR.so --base "${R_HOME:-target/base-r}"
    ./target/release/oracle install --corpus .corpus-top --timeout 3600 $(cat .corpus-top/install.txt)
    ORACLE_RY=$PWD/target/release/ry ./target/release/oracle packages --corpus .corpus-top --installed {{ args }}

# The differential oracle: `just oracle verify`, `record`, `compare` or `show FILE`.
oracle *args:
    cargo run -q -p oracle -- {{ args }}

# Generates the seeded S4-heavy package (600 classes, 1,900 methods) into `dir` and times its first load on ry's runtime and on GNU R, split by S4 call, e.g. `just bench-s4-load .bench/s4heavy`.
bench-s4-load dir="target/s4heavy" seed="20261102":
    cargo build --release -p ry-lang -p libr
    python3 -I tools/loader-bench/s4_package.py {{ dir }} --seed {{ seed }}
    cd {{ dir }} && echo 'q()' | {{ justfile_directory() }}/target/release/ry run
    cd {{ justfile_directory() }}/target && PKG={{ absolute_path(dir) }} ./release/ry run {{ justfile_directory() }}/tools/loader-bench/s4_profile.R
    @echo "== GNU R"
    cd {{ justfile_directory() }}/target && PKG={{ absolute_path(dir) }} Rscript {{ justfile_directory() }}/tools/loader-bench/s4_profile.R

# Times single S4 definitions in the global environment on GNU R and ry, e.g. `just bench-s4-micro 100`.
bench-s4-micro n="200":
    cargo build --release -p ry-lang -p libr
    @echo "== GNU R"
    S4_MICRO_N={{ n }} Rscript tools/loader-bench/s4_micro.R
    @echo "== ry"
    cd target && S4_MICRO_N={{ n }} ./release/ry run {{ justfile_directory() }}/tools/loader-bench/s4_micro.R

# Times each phase of `ry load` on the S4-heavy package and its first S4 use, once with an empty cache and twice warm, e.g. `just bench-s4-first-use target/s4heavy lubridate,data.table,xts`.
bench-s4-first-use dir="target/s4heavy" imports="" seed="20261102":
    cargo build --release -p ry-lang -p libr
    python3 -I tools/loader-bench/s4_package.py {{ dir }} --seed {{ seed }} {{ if imports == "" { "" } else { "--imports " + imports } }}
    rm -rf target/s4-first-use-cache
    for run in cold warm warm; do echo "== $run"; XDG_CACHE_HOME={{ absolute_path("target/s4-first-use-cache") }} RY_LOAD_VERBOSE=true ./target/release/ry load {{ dir }} -e 'started <- proc.time()[[3]]; x <- new("Cls100"); message(sprintf("  first new()    %7.1f ms", (proc.time()[[3]] - started) * 1000))' -e 'message(sprintf("  at exit        %7.1f ms", proc.time()[[3]] * 1000))'; done

# Generate the synthetic benchmark code bases, e.g. `just bench-gen --out .bench --lines 10000`.
bench-gen *args:
    python3 -I tools/bench/tooling/generate.py {{ args }}

# Compare generated packages' tests under GNU R and ry's runtime test by test, e.g. `just bench-compare .bench/finbench`.
bench-compare *args:
    cargo build -q --release -p ry-lang -p libr -p rcmd -p oracle
    R_LIBS=$PWD/.corpus/library ./target/release/oracle bench --ry target/release/ry {{ args }}

# Run a generated package's tests under GNU R, e.g. `just bench-test .bench/finbench`.
bench-test package reporter="summary":
    Rscript tools/bench/tooling/run-tests.R {{ package }} {{ reporter }}

# Differential fuzzing of the runtime: random programs under GNU R, ry's tier 0 and the forced JIT.
# `just fuzz-differential 0 500` runs seeds 0 to 499; add `ry-gctorture` or `ry-gctorture-young`
# to the engines for stress.
fuzz-differential seed="0" count="200" engines="ry-tier0,ry-jit":
    cargo build -q -p ry-lang -p libr -p rcmd -p oracle
    ./target/debug/oracle fuzz --seed {{ seed }} --count {{ count }} --engines {{ engines }}

# The other program kinds of the differential fuzzer: `metamorphic` (one value, one body, and the
# forms that must agree), `builtins` (builtins on a zoo of values), `api` (calls drawn from every
# function of base, stats and utils), `io` (round trips through files and connections), `language`
# (closures over promises, frames, conditions, dispatch and quoted code, called repeatedly),
# `perturbed` (a generated program with dead and effect-free statements inserted into its function
# bodies), `corpus`, or `all`, e.g. `just fuzz-kind metamorphic 0 500`.
# Diverging programs and their reports go to target/fuzz-findings.
fuzz-kind kind="all" seed="0" count="200" engines="ry-tier0,ry-jit":
    cargo build -q -p ry-lang -p libr -p rcmd -p oracle
    ./target/debug/oracle fuzz --kind {{ kind }} --seed {{ seed }} --count {{ count }} --engines {{ engines }} --findings target/fuzz-findings

# How many of the functions base, stats and utils export the programs of COUNT seeds of each kind
# name in a call. It runs no engine, so it measures the generators' reach in seconds.
fuzz-coverage count="500":
    cargo build -q -p oracle
    ./target/debug/oracle fuzz --kind all --seed 0 --count {{ count }} --coverage

# Every program kind of the differential fuzzer against GNU R for HOURS of wall clock, from seed
# BASE on. A scheduled run passes a fresh base, e.g. `just fuzz-long 8 $(date +%s)`. Diverging
# programs and their reports go to target/fuzz-findings, and the run exits 1 when there are any.
fuzz-long hours="1" base="1000000" engines="ry-tier0,ry-jit,ry-gctorture-seeded":
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -q -p ry-lang -p libr -p rcmd -p oracle
    minutes=$(python3 -I -c 'import sys; print(max(1, round(float(sys.argv[1]) * 60)))' "{{ hours }}")
    ./target/debug/oracle fuzz --kind all --seed {{ base }} --minutes "$minutes" --engines {{ engines }} --findings target/fuzz-findings

# The Fuzz workflow's job (.github/workflows/fuzz.yml), in the `ci` dev shell: `just fuzz-long`
# after the set-up `just ci` does, from the start time as the seed base unless one is given.
fuzz-ci hours="4" base="":
    #!/usr/bin/env bash
    set -euo pipefail
    export TZ=UTC NOT_CRAN=true
    R_HOME=$(R RHOME)
    export R_HOME
    bash tools/oracle-locales.sh
    just base-r dev
    base="{{ base }}"
    just fuzz-long "{{ hours }}" "${base:-$(date +%s)}"

# Shrink a program on which an engine diverges from GNU R to a minimal one that diverges the same
# way, e.g. `just fuzz-reduce fuzz_7.R ry-jit`; the result goes to target/fuzz-findings/reduced.R.
fuzz-reduce file engine="ry-tier0":
    cargo build -q -p ry-lang -p libr -p rcmd -p oracle
    mkdir -p target/fuzz-findings
    ./target/debug/oracle reduce --engine {{ engine }} --output target/fuzz-findings/reduced.R {{ file }}

# Differential fuzzing on real R code: mutated snippets of base R's help examples, R's regression
# tests and the oracle cases, under GNU R and ry's engines with the deep observer. Snippets that
# differ before mutation go to target/fuzz-corpus/base-differs.txt instead of the findings.
fuzz-corpus seed="0" count="200" engines="ry-tier0,ry-jit":
    cargo build -q -p ry-lang -p libr -p rcmd -p oracle
    ./target/debug/oracle fuzz --kind corpus --seed {{ seed }} --count {{ count }} --engines {{ engines }}

# Every oracle case under collection stress (`RY_GC_STRESS`), compared with ry unstressed;
# `just gc-torture young` keeps survivors collectable (`RY_GC_STRESS=young`), which finds an
# object only a Rust local holds, and `just gc-torture seeded` collects at random gaps with the
# write barrier checked (`RY_GC_STRESS=seed:N`, `RY_GC_VERIFY`), which finds a store that skipped
# the barrier; `FUZZ_SEED` picks its gaps. `just gc-torture repeat` adds a second pass to the
# seeded mode: each case runs twice in one process, which finds what a closure's first call left
# behind for its second (a reused frame, a call site's plan). `ORACLE_ENGINE=ry-gctorture cargo
# test -p oracle` compares the cases with their recorded outcomes instead.
gc-torture mode="plain":
    cargo build -q -p ry-lang -p libr -p rcmd
    FUZZ_ITERS=1000000 cargo test -p oracle --test test_cases \
        {{ if mode == "young" { "cases_survive_young_gc_torture" } else if mode == "seeded" { "native_cases_keep_the_write_barrier_under_seeded_gc_torture" } else if mode == "repeat" { "cases_survive_a_second_pass_under_seeded_gc_torture" } else { "cases_survive_gc_torture" } }} \
        -- --exact --nocapture

# The heap's and the interpreter's tests under AddressSanitizer (docs: development/robustness.md,
# "Each layer"). The heap tells the sanitizer which memory holds no object, so a read or write of
# a freed object stops at the access. `--target` keeps the flag off build scripts and proc macros,
# and leaks are not reported because the interpreter lives as long as its process.
asan *args:
    RUSTFLAGS="-Zsanitizer=address" ASAN_OPTIONS=detect_leaks=0 cargo test -p heap -p interp \
        --target "$(rustc -vV | sed -n 's/^host: //p')" --target-dir target/asan {{ args }}

# The language-server simulator over many seeds: `just simulate 1000 120` runs seeds 0 to 999 with
# 120 actions each. Replay one failure with `SIMULATION_SEED=<seed> cargo test -p ry-lang --test test_simulation`.
simulate count="200" actions="60" base="0":
    SIMULATION_BASE={{ base }} FUZZ_ITERS={{ count }} SIMULATION_ACTIONS={{ actions }} cargo test -p ry-lang --profile iterate --test test_simulation

# The scripted user journeys in crates/ry/tests/scenarios; `just scenarios edit-and-rename` runs one.
scenarios name="":
    SCENARIO={{ name }} cargo test -p ry-lang --test test_scenarios scenarios_hold

# Fuzz `ry migrate data-table --write` over the generated benchmark code bases and the data.table users: parse, strict checks and checksums, e.g. `just bench-migrate --lines 20000`.
bench-migrate *args:
    cargo build --release -p ry-lang
    cargo build --release -p libr
    python3 -I tools/bench/tooling/migrate_fuzz.py {{ args }}

# Apply what agents staged in .github/pending for a maintainer (their token cannot write
# .github/workflows): move each workflow into place, then commit and push.
accept:
    git mv -f .github/pending/* .github/workflows/
    git commit -m "ci: accept the staged workflows"
    git push
