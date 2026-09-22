#
# DEVELOPMENT
#

default:
    @just --list

ry *args:
    @cargo run -q -- {{ args }}

test *args:
    cargo test -- --nocapture {{ args }}

# The full per-slice gate: the workspace battery, clippy at -D warnings,
# and a formatting check. Green gate = landable change.
gate: battery clippy fmt-check

# The workspace test battery. `zed_ry` needs a wasm toolchain, so this and CI
# exclude it.
battery *args:
    cargo test --workspace --exclude zed_ry {{ args }}

clippy:
    cargo clippy --workspace --exclude zed_ry --all-targets -- -D warnings

fmt-check:
    cargo fmt --all --check

# One focused fixture case, e.g.
#   just fixture pipes__pipe_chains_type_end_to_end
#   just fixture datatable__declared_dependency_activates_the_stub test_typing_fixtures
# (`binary` is the suite's test target; testing.md lists them per crate.)
fixture case binary="test_typing_fixtures" package="semantics":
    FIXTURE_FILTER={{ case }} cargo test -p {{ package }} --test {{ binary }} -- --nocapture

# Re-bless fixture expectations for the given test selection, e.g.
#   just bless -p semantics --test test_typing_fixtures
bless *args:
    RY_BLESS=1 cargo test {{ args }}

# Regenerate docs/formatter.md from the template through the shipping formatter.
format-docs:
    RY_BLESS=1 cargo test -p format --test test_format_docs

# The seeded deep fuzz run beyond the bounded passes already inside
# `cargo test -p semantics`; `FUZZ_ITERS` scales the budget.
fuzz-deep:
    cargo test -p semantics --test test_fuzz fuzz_deep -- --ignored --nocapture

# Coverage-guided libFuzzer targets (parse | format | semantics) — nightly
# toolchain; seed the corpus first with
# `cargo +nightly -Zscript scripts/seed-fuzz-corpus.rs`.
fuzz-run target="semantics" *args:
    cargo +nightly fuzz run {{ target }} {{ args }}

# The workspace performance diagnosis (phase timings, memory, typing bursts).
stats path=".":
    cargo run -q -p ry-lang -- debug analysis-stats {{ path }}

# The REPL's end-to-end tests: drive `ry repl` through a pty against the
# system R. Local-only — they skip (green) on machines without R.
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

#
# BUILD
#

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

#
# RELEASE
#

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

    # Versioning schema, and the marketplace constraint that drives it:
    #   The VS Marketplace only accepts a plain `X.Y.Z` and REJECTS semver pre-release
    #   suffixes such as `-alpha.3`. So releases use this schema:
    #     pre-release : `X.Y.Z-alpha` / `X.Y.Z-beta`  — the PATCH (Z) is a single
    #                   monotonically increasing counter and the suffix names the
    #                   channel, e.g. 0.2.1-alpha, 0.2.2-alpha, ..., 0.2.14-beta.
    #     stable      : a bare `X.Y.Z`.
    #   The extension version is the CLI version with the channel suffix stripped — a
    #   valid, monotonic `X.Y.Z` — and the `--pre-release` flag marks the channel on
    #   the marketplace. Keeping the counter in the PATCH (not the suffix) is what makes
    #   stripping collision-free: 0.2.1-alpha and 0.2.2-alpha map to distinct 0.2.1 / 0.2.2.
    cli_version="{{ version }}"
    vscode_version="${cli_version%%-*}"
    sed -i "s/\"version\": \"[a-zA-Z0-9._-]*\"/\"version\": \"$vscode_version\"/" editors/code/package.json

    # editors/zed/extension.toml is intentionally NOT version-bumped: the Zed extension
    # ships no binary — it locates one (settings path, then PATH, then the latest GitHub
    # release) — so it versions on its own changes, on its own plain-semver line. Bump it
    # by hand when the extension changes; do not drag it back onto the CLI's number.

    cargo check # bonus: also updates version in lock file
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

    # zig cross-links the macOS binaries without an Apple SDK, so a dependency
    # that emits `-framework ...` cannot link. Fail fast with a clear message
    # instead of deep inside the nix build (see patches/iana-time-zone).
    # Resolved once, so a broken invocation fails the recipe instead of being
    # mistaken for "the framework crate is absent".
    macos_tree=$(cargo tree --target aarch64-apple-darwin -p ry-lang -e normal --prefix none)

    for package in core-foundation-sys core-foundation objc objc2 security-framework; do
        if grep -qE "^$package v" <<< "$macos_tree"; then
            echo "error: the macOS dependency graph pulls in '$package', which links an Apple framework;"
            echo "       zig cannot link Apple frameworks without an SDK. See patches/iana-time-zone."
            exit 1
        fi
    done

    nix build .#ry-linux-x86_64 -o release/nix/x86_64-unknown-linux-musl
    nix build .#ry-macos-aarch64 -o release/nix/aarch64-apple-darwin
    nix build .#ry-windows-x86_64 -o release/nix/x86_64-pc-windows-gnu

    just package-tar x86_64-unknown-linux-musl $dir
    just package-tar aarch64-apple-darwin $dir
    just package-zip x86_64-pc-windows-gnu $dir

    # vscode extension (client only)
    rm -rf editors/code/bin
    just build-extension $kind --out ../../$dir/ry.vsix

    just build-platform-vsix x86_64-unknown-linux-musl linux-x64 ry $dir $kind
    just build-platform-vsix aarch64-apple-darwin darwin-arm64 ry $dir $kind
    just build-platform-vsix x86_64-pc-windows-gnu win32-x64 ry.exe $dir $kind

publish-github $version $notes="":
    #!/usr/bin/env bash
    set -euo pipefail

    # `release-kind` is the single source of truth for the channel: alpha/beta
    # versions (and the bare git revisions `publish-commit` uses) are marked
    # pre-release on GitHub exactly as they are on the marketplace.
    prerelease_flag=""
    if [ "$(just release-kind $version)" = "pre-release" ]; then
    	prerelease_flag="--prerelease"
    fi
    git push
    gh release create $version $prerelease_flag \
    	"release/$version/ry-x86_64-unknown-linux-musl.tar.gz#ry CLI (linux-x64)" \
    	"release/$version/ry-aarch64-apple-darwin.tar.gz#ry CLI (darwin-arm64)" \
    	"release/$version/ry-x86_64-pc-windows-gnu.zip#ry CLI (win32-x64)" \
    	"release/$version/ry.vsix#VS Code extension (client only)" \
    	"release/$version/ry-linux-x64.vsix#VS Code extension (linux-x64)" \
    	"release/$version/ry-darwin-arm64.vsix#VS Code extension (darwin-arm64)" \
    	"release/$version/ry-win32-x64.vsix#VS Code extension (win32-x64)" \
    	--notes "$notes"

publish-github-update $version:
    gh release upload $version \
        "release/$version/ry-x86_64-unknown-linux-musl.tar.gz#ry CLI (linux-x64)" \
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

#
# UTILS
#

package-tar target dir:
    mkdir -p {{ dir }}/ry-{{ target }}
    cp release/nix/{{ target }}/bin/ry {{ dir }}/ry-{{ target }}/ry
    tar -czf {{ dir }}/ry-{{ target }}.tar.gz -C {{ dir }}/ry-{{ target }} ry

package-zip target dir:
    mkdir -p {{ dir }}/ry-{{ target }}
    cp release/nix/{{ target }}/bin/ry.exe {{ dir }}/ry-{{ target }}/ry.exe
    zip -j {{ dir }}/ry-{{ target }}.zip {{ dir }}/ry-{{ target }}/ry.exe

# use rlib repos to test formatting
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

# The release channel implied by a version string: a bare `X.Y.Z` is a stable release; an
# `X.Y.Z-alpha`/`-beta` postfix (and any other non-`X.Y.Z` form, such as a bare git revision used
# by `publish-commit`) is a marketplace pre-release. This is the single source of truth for the
# channel, so `release`/`publish` never take a separate kind argument.
release-kind $version:
    #!/usr/bin/env bash
    set -euo pipefail

    case "{{ version }}" in
    	*-alpha|*-beta) echo pre-release ;;
    	*-*) echo "error: version suffix must be -alpha or -beta, got {{ version }}" >&2; exit 1 ;;
    	[0-9]*.[0-9]*.[0-9]*) echo release ;;
    	*) echo pre-release ;;
    esac
