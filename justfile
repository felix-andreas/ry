default:
    @just --list

ry *args:
    @cargo run -q -- {{ args }}

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

# The seeded deep fuzz run; `FUZZ_ITERS` scales the budget.
fuzz-deep:
    cargo test -p semantics --test test_fuzz fuzz_deep -- --ignored --nocapture

# libFuzzer (parse | format | semantics); seed with `cargo +nightly -Zscript scripts/seed-fuzz-corpus.rs`.
fuzz-run target="semantics" *args:
    cargo +nightly fuzz run {{ target }} {{ args }}

# Phase timings, memory and typing bursts for a workspace.
stats path=".":
    cargo run -q -p ry-lang -- debug analysis-stats {{ path }}

# Corpus perf and memory budgets; fetch with `cargo +nightly -Zscript scripts/fetch-corpus.rs`.
perf:
    cargo test -p semantics --release --test test_perf -- --ignored --nocapture

# Generates the three 300K-line benchmark code bases and prints cold-check and language-server latency.
bench *args:
    cargo build --release -p ry-lang
    python3 -I tools/bench/run.py {{ args }}

# Regenerates the benchmark table in why-ry.md: each program untyped on GNU R, untyped on ry, and typed in strict mode on ry.
bench-why *args:
    cargo build --release -p ry-lang -p libr
    python3 -I tools/bench/why/run.py {{ args }}

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

build:
    cargo build

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

publish $version $notes="":
    #!/usr/bin/env bash
    set -euo pipefail

    just bump-version $version
    just release $version
    just publish-github $version "$notes"
    just publish-marketplace $version

publish-commit $version="":
    #!/usr/bin/env bash
    set -euo pipefail

    if [ -z "{{ version }}" ]; then
    	version=$(git rev-parse --short=6 HEAD)
    	echo "info: using git revision $version as version"
    fi
    # A bare git revision is not `X.Y.Z`, so `release-kind` classifies it as a pre-release.
    just release $version
    just publish-github $version

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
    git commit -m "chore: Release v{{ version }}"

release $version:
    #!/usr/bin/env bash
    set -euo pipefail

    kind=$(just release-kind $version)
    dir=release/$version

    mkdir -p release
    rm -rf $dir release/nix
    mkdir -p $dir release/nix

    cargo check

    # zig cross-links macOS without an Apple SDK, so fail fast on a crate that links an Apple
    # framework (see patches/iana-time-zone). Resolving the tree once makes a broken `cargo tree`
    # fail the recipe instead of reading as "no such crate".
    macos_tree=$(cargo tree --target aarch64-apple-darwin -p ry-lang -e normal --prefix none)

    for package in core-foundation-sys core-foundation objc objc2 security-framework; do
        if grep -qE "^$package v" <<< "$macos_tree"; then
            echo "error: the macOS dependency graph pulls in '$package', which links an Apple framework;"
            echo "       zig cannot link Apple frameworks without an SDK. See patches/iana-time-zone."
            exit 1
        fi
    done

    nix build .#ry-linux-x86_64 -o release/nix/x86_64-unknown-linux-gnu
    nix build .#ry-macos-aarch64 -o release/nix/aarch64-apple-darwin
    nix build .#ry-windows-x86_64 -o release/nix/x86_64-pc-windows-gnu

    just package-tar x86_64-unknown-linux-gnu $dir
    just package-tar aarch64-apple-darwin $dir
    just package-zip x86_64-pc-windows-gnu $dir

    rm -rf editors/code/bin
    just build-extension $kind --out ../../$dir/ry.vsix

    just build-platform-vsix x86_64-unknown-linux-gnu linux-x64 ry $dir $kind
    just build-platform-vsix aarch64-apple-darwin darwin-arm64 ry $dir $kind
    just build-platform-vsix x86_64-pc-windows-gnu win32-x64 ry.exe $dir $kind

publish-github $version $notes="":
    #!/usr/bin/env bash
    set -euo pipefail

    prerelease_flag=""
    if [ "$(just release-kind $version)" = "pre-release" ]; then
    	prerelease_flag="--prerelease"
    fi
    git push
    gh release create $version $prerelease_flag \
    	"release/$version/ry-x86_64-unknown-linux-gnu.tar.gz#ry CLI (linux-x64)" \
    	"release/$version/ry-aarch64-apple-darwin.tar.gz#ry CLI (darwin-arm64)" \
    	"release/$version/ry-x86_64-pc-windows-gnu.zip#ry CLI (win32-x64)" \
    	"release/$version/ry.vsix#VS Code extension (client only)" \
    	"release/$version/ry-linux-x64.vsix#VS Code extension (linux-x64)" \
    	"release/$version/ry-darwin-arm64.vsix#VS Code extension (darwin-arm64)" \
    	"release/$version/ry-win32-x64.vsix#VS Code extension (win32-x64)" \
    	--notes "$notes"

publish-github-update $version:
    gh release upload $version \
        "release/$version/ry-x86_64-unknown-linux-gnu.tar.gz#ry CLI (linux-x64)" \
        "release/$version/ry-aarch64-apple-darwin.tar.gz#ry CLI (darwin-arm64)" \
        "release/$version/ry-x86_64-pc-windows-gnu.zip#ry CLI (win32-x64)" \
        "release/$version/ry.vsix#VS Code extension (client only)" \
        "release/$version/ry-linux-x64.vsix#VS Code extension (linux-x64)" \
        "release/$version/ry-darwin-arm64.vsix#VS Code extension (darwin-arm64)" \
        "release/$version/ry-win32-x64.vsix#VS Code extension (win32-x64)" \
        --clobber

publish-marketplace $version:
    #!/usr/bin/env bash
    set -euo pipefail

    release_flag=$(just vsce-release-flag $(just release-kind $version))

    just vsce publish $release_flag --packagePath ../../release/$version/ry-linux-x64.vsix
    just vsce publish $release_flag --packagePath ../../release/$version/ry-darwin-arm64.vsix
    just vsce publish $release_flag --packagePath ../../release/$version/ry-win32-x64.vsix
    just vsce publish $release_flag --packagePath ../../release/$version/ry.vsix

package-tar target dir:
    mkdir -p {{ dir }}/ry-{{ target }}
    cp release/nix/{{ target }}/bin/ry {{ dir }}/ry-{{ target }}/ry
    tar -czf {{ dir }}/ry-{{ target }}.tar.gz -C {{ dir }}/ry-{{ target }} ry

package-zip target dir:
    mkdir -p {{ dir }}/ry-{{ target }}
    cp release/nix/{{ target }}/bin/ry.exe {{ dir }}/ry-{{ target }}/ry.exe
    zip -j {{ dir }}/ry-{{ target }}.zip {{ dir }}/ry-{{ target }}/ry.exe

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
