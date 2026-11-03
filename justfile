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

test *args:
    cargo test -- --nocapture {{ args }}

# The pre-landing check: tests, clippy at -D warnings, fmt, and the licence notices.
gate: battery clippy fmt-check (notices "--check")

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

# Every seeded harness at `depth` times its default seeds (`FUZZ_DEPTH`) for `minutes`, each round
# starting at a fresh seed from `base`, plus the
# libr package suites under collection stress; failures are logged with their replay commands to
# target/fuzz-deep.log. A scheduled run passes a fresh base, e.g. `just fuzz-deep 480 $(date +%s)`.
fuzz-deep minutes="120" base="1000000" depth="10" *args:
    python3 -I tools/fuzz-deep.py --minutes {{ minutes }} --base {{ base }} --depth {{ depth }} {{ args }}

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

# Corpus perf and memory budgets, and the benchmark memory budget; fetch with `cargo -Zscript scripts/fetch-corpus.rs`.
perf:
    cargo test -p semantics --profile bench --test test_perf -- --ignored --nocapture
    cargo test -p ry-lang --profile bench --test test_perf -- --ignored --nocapture

# Generates the three 300K-line benchmark code bases and prints cold-check and language-server latency.
bench *args:
    cargo build --release -p ry-lang
    python3 -I tools/bench/run.py {{ args }}

# Measures the docs benchmark: each program untyped on GNU R, untyped on ry, and typed in strict mode on ry.
# `--write` stores the results in tools/bench/why/results.json, which every benchmark table on the site renders.
bench-why *args:
    cargo build --release -p ry-lang -p libr -p rcmd
    python3 -I tools/bench/why/run.py {{ args }}

# Times base R kernels (sort, regex, grouping, xts/zoo rolling ops, ...) on GNU R and ry's runtime with checksums, e.g. `just bench-base --rows 1e6 --memory`. zoo and xts load from RY_BENCH_RLIB.
bench-base *args:
    cargo build --release -p ry-lang -p libr -p rcmd
    python3 -I tools/bench/base/run.py {{ args }}

# Times each List, Str, Dict and Result function on ry against its fastest GNU R equivalent (base R, purrr, stringr, stringi from R_LIBS_USER), e.g.
# `just bench-stdlib` (every function at n = 1e6), `just bench-stdlib Str. 1e5`, or `just bench-stdlib "" 1e6 --sort ratio --write tools/bench/stdlib/results.md`.
bench-stdlib filter="" n="1e6" *args:
    cargo build --release -p ry-lang -p libr
    python3 -I tools/bench/stdlib/run.py --only "{{ filter }}" --n {{ n }} {{ args }}

# Runs the table benchmark against data.table and rewrites its table in r-runtime.md, e.g.
# `just bench-table .bench/table` (1e7 rows) or `just bench-table .bench/table5e7 --rows 5e7 --groupby-only`
# (`--joins-only` runs the join questions alone).
bench-table dir *args:
    cargo build --release -p ry-lang -p libr -p rcmd
    python3 -I tools/bench/table/report.py {{ dir }} --write {{ args }}

# Drives `ry repl` through a pty against the system R; skips without R.
repl-e2e:
    cargo test -p ry-lang --test test_repl_e2e -- --nocapture

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
    	echo "error: docs-deploy needs these environment variables:$missing (contributing/releasing.md, \"Deploying the docs site\")" >&2
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

# Build a release archive into release/VERSION: `ry` alone, with ry's runtime (libR) embedded in it,
# LICENSE and THIRD_PARTY_NOTICES. TARGET defaults to this machine; Windows builds for x86_64-pc-windows-gnullvm with
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
    # The runtime is built first, because `ry` embeds it (crates/repl/build.rs).
    cargo build --release --locked --target "$target" -p libr
    RY_EMBED_RUNTIME="$(cd "$build" && pwd)/$library" cargo build --release --locked --target "$target" -p ry-lang
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
# platform. A platform whose archive is missing is skipped with a note, so this runs locally on one
# machine's archive; the release workflow always has all three.
release-assets version:
    #!/usr/bin/env bash
    set -euo pipefail
    version={{ quote(version) }}
    dir="release/$version"
    kind=$(just release-kind "$version")
    (cd editors/code && bun install --frozen-lockfile)
    # The universal extension goes first, before build-platform-vsix fills editors/code/bin.
    rm -rf editors/code/bin
    just build-extension "$kind" --out "../../$dir/ry.vsix"
    for platform in x86_64-unknown-linux-gnu:linux-x64 aarch64-apple-darwin:darwin-arm64 x86_64-pc-windows-gnullvm:win32-x64; do
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
    	prerelease=""
    	if [ "$kind" = pre-release ]; then prerelease="--prerelease"; fi
    	gh release create "$version" $prerelease --target "${GITHUB_SHA:-$(git rev-parse HEAD)}" --title "ry $version" \
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
    python3 -I tools/bench/frames/run.py --write {{ args }}

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
    ./target/release/ry-rcmd assemble .corpus/ry-home --include crates/libr/include --libr target/release/libR.so

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
    ./target/release/ry-rcmd assemble .corpus-top/ry-home --include crates/libr/include --libr target/release/libR.so
    ./target/release/oracle install --corpus .corpus-top --timeout 3600 $(cat .corpus-top/install.txt)
    ORACLE_RY=$PWD/target/release/ry ./target/release/oracle packages --corpus .corpus-top --installed {{ args }}

# The differential oracle: `just oracle verify`, `record`, `compare` or `show FILE`.
oracle *args:
    cargo run -q -p oracle -- {{ args }}

# Generates the seeded S4-heavy package (600 classes, 1,900 methods) into `dir` and times its first load on ry's runtime, split by S4 call, e.g. `just bench-s4-load .bench/s4heavy`.
bench-s4-load dir="target/s4heavy" seed="20261102":
    cargo build --release -p ry-lang -p libr
    python3 -I tools/loader-bench/s4_package.py {{ dir }} --seed {{ seed }}
    cd {{ dir }} && echo 'q()' | {{ justfile_directory() }}/target/release/ry run
    cd {{ justfile_directory() }}/target && PKG={{ absolute_path(dir) }} ./release/ry run {{ justfile_directory() }}/tools/loader-bench/s4_profile.R

# Times each phase of `ry load` on the S4-heavy package and its first S4 use, once with an empty cache and twice warm, e.g. `just bench-s4-first-use target/s4heavy lubridate,data.table,xts`.
bench-s4-first-use dir="target/s4heavy" imports="" seed="20261102":
    cargo build --release -p ry-lang -p libr
    python3 -I tools/loader-bench/s4_package.py {{ dir }} --seed {{ seed }} {{ if imports == "" { "" } else { "--imports " + imports } }}
    rm -rf target/s4-first-use-cache
    for run in cold warm warm; do echo "== $run"; XDG_CACHE_HOME={{ absolute_path("target/s4-first-use-cache") }} RY_LOAD_VERBOSE=true ./target/release/ry load {{ dir }} -e 'started <- proc.time()[[3]]; x <- new("Cls100"); message(sprintf("  first new()    %7.1f ms", (proc.time()[[3]] - started) * 1000))' -e 'message(sprintf("  at exit        %7.1f ms", proc.time()[[3]] * 1000))'; done

# Generate the synthetic benchmark code bases, e.g. `just bench-gen --out .bench --lines 10000`.
bench-gen *args:
    python3 -I tools/bench/generate.py {{ args }}

# Compare generated packages' tests under GNU R and ry's runtime test by test, e.g. `just bench-compare .bench/finbench`.
bench-compare *args:
    cargo build -q --release -p ry-lang -p libr -p rcmd -p oracle
    R_LIBS=$PWD/.corpus/library ./target/release/oracle bench --ry target/release/ry {{ args }}

# Run a generated package's tests under GNU R, e.g. `just bench-test .bench/finbench`.
bench-test package reporter="summary":
    Rscript tools/bench/run-tests.R {{ package }} {{ reporter }}

# Differential fuzzing of the runtime: random programs under GNU R, ry's tier 0 and the forced JIT.
# `just fuzz-differential 0 500` runs seeds 0 to 499; add `ry-gctorture` to the engines for stress.
fuzz-differential seed="0" count="200" engines="ry-tier0,ry-jit":
    cargo build -q -p ry-lang -p libr -p rcmd -p oracle
    ./target/debug/oracle fuzz --seed {{ seed }} --count {{ count }} --engines {{ engines }}

# Every oracle case under collection stress (`RY_GC_STRESS`), compared with ry unstressed;
# `ORACLE_ENGINE=ry-gctorture cargo test -p oracle` compares them with GNU R instead.
gc-torture:
    cargo build -q -p ry-lang -p libr -p rcmd
    FUZZ_ITERS=1000000 cargo test -p oracle --test test_cases cases_survive_gc_torture -- --nocapture

# The language-server simulator over many seeds: `just simulate 1000 120` runs seeds 0 to 999 with
# 120 actions each. Replay one failure with `SIMULATION_SEED=<seed> cargo test -p ry-lang --test test_simulation`.
simulate count="200" actions="60" base="0":
    SIMULATION_BASE={{ base }} FUZZ_ITERS={{ count }} SIMULATION_ACTIONS={{ actions }} cargo test -p ry-lang --profile bench --test test_simulation -- --nocapture

# The scripted user journeys in crates/ry/tests/scenarios; `just scenarios edit-and-rename` runs one.
scenarios name="":
    cargo build -q -p libr
    SCENARIO={{ name }} cargo test -p ry-lang --test test_scenarios -- --nocapture

# Fuzz `ry migrate data-table --write` over the generated benchmark code bases and the data.table users: parse, strict checks and checksums, e.g. `just bench-migrate --lines 20000`.
bench-migrate *args:
    cargo build --release -p ry-lang
    cargo build --release -p libr
    python3 -I tools/bench/migrate/fuzz.py {{ args }}

# Apply what agents staged in .github/pending for a maintainer (their token cannot write
# .github/workflows): move each workflow into place, then commit and push.
accept:
    git mv -f .github/pending/* .github/workflows/
    git commit -m "ci: accept the staged workflows"
    git push
