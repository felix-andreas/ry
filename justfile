default:
    @just --list

# Run ry from source, e.g. `just ry check` or `just ry run script.R`. ry's runtime (libR) is a
# separate library that `ry run` loads at run time, so cargo would not build it for `cargo run`.
# Build warnings show only when the build fails, so they do not bury ry's own output.
# It runs in the directory you call it from, so relative paths mean what you typed.
[no-cd]
[no-exit-message]
ry *args:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! output=$(cargo build --color always -q --manifest-path "{{ justfile_directory() }}/Cargo.toml" -p ry-lang -p libr 2>&1); then
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

test *args:
    cargo test -- --nocapture {{ args }}

# The pre-landing check: tests, clippy at -D warnings, and fmt.
gate: battery clippy fmt-check

# Workspace tests; `zed_ry` needs a wasm toolchain, so it is excluded.
battery *args:
    cargo test --workspace --exclude zed_ry {{ args }}

clippy:
    cargo clippy --workspace --exclude zed_ry --all-targets -- -D warnings

fmt-check:
    cargo fmt --all --check

# One fixture case, e.g. `just fixture pipes__pipe_chains_type_end_to_end`.
fixture case binary="test_typing_fixtures" package="semantics":
    FIXTURE_FILTER={{ case }} cargo test -p {{ package }} --test {{ binary }} -- --nocapture

# Re-bless expectations, e.g. `just bless -p semantics --test test_typing_fixtures`.
bless *args:
    RY_BLESS=1 cargo test {{ args }}

# Regenerate docs/formatter.md through the shipping formatter.
format-docs:
    RY_BLESS=1 cargo test -p format --test test_format_docs

# Run the docs' marked examples, or those of the pages whose path contains `page`.
docs-test page="":
    DOCS_PAGE={{ page }} cargo test -p ry-lang --test test_docs -- --nocapture

# Rewrite the output of the docs' exact examples from the real tool; excerpts are left to you.
docs-bless page="":
    RY_BLESS=1 DOCS_PAGE={{ page }} cargo test -p ry-lang --test test_docs -- --nocapture

# The seeded deep fuzz run; `FUZZ_ITERS` scales the budget.
fuzz-deep:
    cargo test -p semantics --test test_fuzz fuzz_deep -- --ignored --nocapture

# One libFuzzer target (`cargo fuzz list` names them all); seed with `cargo -Zscript scripts/seed-fuzz-corpus.rs`.
fuzz-run target="semantics" *args:
    cargo fuzz run {{ target }} {{ args }}

# Every libFuzzer target for `seconds` each, after seeding the corpora. The lockfile is seeded
# from the workspace's, because a fresh resolution can pick a salsa-macro-rules that does not
# match the pinned salsa. Stops at the first target that finds a failure.
fuzz-all seconds="120":
    cp Cargo.lock fuzz/Cargo.lock
    cargo -Zscript scripts/seed-fuzz-corpus.rs
    for target in $(cargo fuzz list); do \
        echo "== $target"; \
        cargo fuzz run -O "$target" -- -max_total_time={{ seconds }} -timeout=30 -rss_limit_mb=4096 || exit 1; \
    done

# Phase timings, memory and typing bursts for a workspace.
stats path=".":
    cargo run -q -p ry-lang -- debug analysis-stats {{ path }}

# Corpus perf and memory budgets; fetch with `cargo -Zscript scripts/fetch-corpus.rs`.
perf:
    cargo test -p semantics --release --test test_perf -- --ignored --nocapture

# Generates the three 300K-line benchmark code bases and prints cold-check and language-server latency.
bench *args:
    cargo build --release -p ry-lang
    python3 -I tools/bench/run.py {{ args }}

# Regenerates the benchmark table in why-ry.mdx: each program untyped on GNU R, untyped on ry, and typed in strict mode on ry.
bench-why *args:
    cargo build --release -p ry-lang -p libr
    python3 -I tools/bench/why/run.py {{ args }}

# Times base R kernels (sort, regex, grouping, xts/zoo rolling ops, ...) on GNU R and ry's runtime with checksums, e.g. `just bench-base --rows 1e6 --memory`. zoo and xts load from RY_BENCH_RLIB.
bench-base *args:
    cargo build --release -p ry-lang -p libr
    python3 -I tools/bench/base/run.py {{ args }}

# Drives `ry repl` through a pty against the system R; skips without R.
repl-e2e:
    cargo test -p ry-lang --test test_repl_e2e -- --nocapture

docs:
    cd docs && bun dev

vsce *args:
    @bun --cwd=editors/code run vsce -- {{ args }}

install-extension:
    mkdir -p release/dev
    just build-extension pre-release --out ../../release/dev/ry.vsix
    code --install-extension release/dev/ry.vsix

# Build every binary and ry's runtime (libR), e.g. `just build --release`.
build *args:
    cargo build --workspace --exclude zed_ry {{ args }}

zigbuild $target *args:
    cargo zigbuild --target {{ target }} {{ args }}

build-platform-vsix target vscode-target binary-name dir kind:
    rm -rf editors/code/bin
    mkdir -p editors/code/bin
    cp {{ dir }}/ry-{{ target }}/{{ binary-name }} editors/code/bin
    just build-extension {{ kind }} --target {{ vscode-target }} --out ../../{{ dir }}/ry-{{ vscode-target }}.vsix

build-extension $kind *args:
    #!/usr/bin/env bash
    set -euo pipefail

    release_flag=$(just vsce-release-flag $kind)
    cp LICENSE editors/code
    just vsce package $release_flag {{ args }}

bump-version $version:
    #!/usr/bin/env bash
    set -euo pipefail

    sed -i 's/^version = "[a-zA-Z0-9._-]*"/version = "{{ version }}"/' Cargo.toml

    # The VS Marketplace rejects pre-release suffixes, so the extension takes the CLI version
    # without its `-alpha`/`-beta`; the patch number is the one counter across channels, which
    # keeps stripped versions distinct (0.2.1-alpha and 0.2.2-alpha become 0.2.1 and 0.2.2).
    cli_version="{{ version }}"
    vscode_version="${cli_version%%-*}"
    sed -i "s/\"version\": \"[a-zA-Z0-9._-]*\"/\"version\": \"$vscode_version\"/" editors/code/package.json

    # The Zed extension ships no binary, so editors/zed/extension.toml is bumped by hand on
    # its own version line.

    cargo check # updates Cargo.lock
    git add Cargo.toml Cargo.lock editors/code/package.json
    git commit -m "chore: release {{ version }}"

# Build a release archive into release/VERSION: `ry` and ry's runtime (libR) side by side. TARGET
# defaults to this machine; Windows builds for x86_64-pc-windows-gnullvm with an LLVM mingw toolchain
# (r-runtime.md, section 11), so `ry.exe`, `R.dll` and Rtools-built packages share the UCRT.
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
    cargo build --release --locked --target "$target" -p ry-lang -p libr
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
    		cp "$build/ry.exe" "$build/R.dll" LICENSE "$stage/"
    		(cd "$stage" && 7z a -tzip "../ry-$target.zip" ry.exe R.dll LICENSE >/dev/null)
    		;;
    	*)
    		library=libR.so
    		case "$target" in *apple*) library=libR.dylib ;; esac
    		cp "$build/ry" "$build/$library" LICENSE "$stage/"
    		tar -czf "$dir/ry-$target.tar.gz" -C "$stage" .
    		;;
    esac
    echo "$dir/ry-$target.$(case "$target" in *windows*) echo zip ;; *) echo tar.gz ;; esac)"

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

vsce-release-flag $kind:
    @echo {{ if kind == "release" { "" } else if kind == "pre-release" { "--pre-release" } else { error("kind must be either release or pre-release") } }}

# The channel: a bare `X.Y.Z` is a release; anything else, a git revision included, is a pre-release.
release-kind $version:
    #!/usr/bin/env bash
    set -euo pipefail

    case "{{ version }}" in
    	*-alpha|*-beta) echo pre-release ;;
    	*-*) echo "error: version suffix must be -alpha or -beta, got {{ version }}" >&2; exit 1 ;;
    	[0-9]*.[0-9]*.[0-9]*) echo release ;;
    	*) echo pre-release ;;
    esac

# Install the benchmark dependency closure into .corpus/library (needs GNU R, network access and tools/corpus/system-packages.txt).
corpus:
    Rscript tools/corpus/install.R

# Download the corpus packages' source tarballs, which carry the tests binary installs lack.
corpus-sources:
    Rscript tools/corpus/sources.R

# Run corpus packages' own tests under GNU R and ry's runtime: `just package-tests [--runtime ry] [PKG...]`.
package-tests *args:
    cargo build -q --release -p ry-lang -p libr
    ORACLE_RY=target/release/ry cargo run -q --release -p oracle -- packages {{ args }}

# Assemble ry's R home in .corpus/ry-home: bin/R, bin/Rscript and R CMD run on ry's runtime.
ry-home:
    cargo build -q --release -p libr -p rcmd
    ./target/release/ry-rcmd assemble .corpus/ry-home --include crates/libr/include --libr target/release/libR.so

# Install the closure from source on ry's runtime into .corpus/ry-library: `just corpus-ry-install [--jobs N] [PKG...]`.
corpus-ry-install *args: ry-home
    cargo run -q --release -p oracle -- install {{ args }}

# The differential oracle: `just oracle verify`, `record`, `compare` or `show FILE`.
oracle *args:
    cargo run -q -p oracle -- {{ args }}

# Generate the synthetic benchmark code bases, e.g. `just bench-gen --out .bench --lines 10000`.
bench-gen *args:
    python3 -I tools/bench/generate.py {{ args }}

# Compare generated packages' tests under GNU R and ry's runtime test by test, e.g. `just bench-compare .bench/finbench`.
bench-compare *args:
    cargo build -q --release -p ry-lang -p libr -p oracle
    R_LIBS=$PWD/.corpus/library ./target/release/oracle bench --ry target/release/ry {{ args }}

# Run a generated package's tests under GNU R, e.g. `just bench-test .bench/finbench`.
bench-test package reporter="summary":
    Rscript tools/bench/run-tests.R {{ package }} {{ reporter }}

# Differential fuzzing of the runtime: random programs under GNU R, ry's tier 0 and the forced JIT.
# `just fuzz-differential 0 500` runs seeds 0 to 499; add `ry-gctorture` to the engines for stress.
fuzz-differential seed="0" count="200" engines="ry-tier0,ry-jit":
    cargo build -q -p ry-lang -p libr -p oracle
    ./target/debug/oracle fuzz --seed {{ seed }} --count {{ count }} --engines {{ engines }}

# Every oracle case under collection stress (`RY_GC_STRESS`), compared with ry unstressed;
# `ORACLE_ENGINE=ry-gctorture cargo test -p oracle` compares them with GNU R instead.
gc-torture:
    cargo build -q -p ry-lang -p libr
    FUZZ_ITERS=1000000 cargo test -p oracle --test test_cases cases_survive_gc_torture -- --nocapture

# The language-server simulator over many seeds: `just simulate 1000 120` runs seeds 0 to 999 with
# 120 actions each. Replay one failure with `SIMULATION_SEED=<seed> cargo test -p ry-lang --test test_simulation`.
simulate count="200" actions="60" base="0":
    SIMULATION_BASE={{ base }} FUZZ_ITERS={{ count }} SIMULATION_ACTIONS={{ actions }} cargo test -p ry-lang --release --test test_simulation -- --nocapture

# The scripted user journeys in crates/ry/tests/scenarios; `just scenarios edit-and-rename` runs one.
scenarios name="":
    cargo build -q -p libr
    SCENARIO={{ name }} cargo test -p ry-lang --test test_scenarios -- --nocapture
