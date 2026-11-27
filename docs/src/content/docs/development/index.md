---
title: Development
description: How to build, test, and debug ry
---

ry is a Rust workspace whose crates live under `crates/`. [Structure](/development/structure) maps
every source file, and [Architecture](/development/architecture) explains how the analysis fits
together. The editor extensions live in `editors/code` (VS Code) and `editors/zed` (Zed).

## Set up a machine

With nix installed, nothing needs provisioning for the build, the tests and the oracle:
`source tools/env.sh` loads the flake's `default` dev shell (the Rust toolchain, the reference GNU R,
`just`, mold), and a Claude cloud container needs no setup script. The first run downloads 8.8 GB and
takes about 100 seconds; [the fast loop](/development/testing/#the-fast-loop) has the details.

The package corpus and the benchmarks against GNU R need the host's libraries. On Ubuntu 24.04, one
script provisions everything the build, the tests, the oracle and the
benchmarks need, and `just doctor` checks the result:

```sh
sudo tools/dev-setup.sh   # apt libraries, just, GNU R 4.6.2 in /opt/R, the oracle's packages
just doctor               # each check prints ok, warn or FAIL with the command that fixes it
```

The script is idempotent, so a cloud environment can run it as its setup script in every fresh
container. It installs `just` 1.59.0 from PyPI, because the justfile needs 1.40 or newer and
Ubuntu's package is older, and it wraps rustc in sccache, so a fresh worktree compiles its
dependencies from a shared cache (a cold debug build of `ry` takes about 1 minute instead of 4.5).
The cache lives in `~/.cache/sccache` and grows to sccache's default cap of 10 GB; set
`SCCACHE_CACHE_SIZE` to change it. The oracle's [reference GNU R](/development/corpus-and-oracle#the-reference-gnu-r)
and the packages its cases name (rlang pinned to 1.3.0) come from `tools/reference-r.sh`, which
restores a prebuilt copy from the public binary cache `ry.cachix.org` in about five seconds and
builds from source (about three and a half minutes on four cores) when the cache holds no build of
the current scripts. The script also restores the package-test corpus library from the same cache
(about ten seconds). When the cache has no build of the pinned set, `just bench-packages install`
installs it from source in about eight minutes.

Restoring needs nix, which the cloud image has, and takes zstd from nixpkgs when it is not installed; without nix, the script builds R from source.
`tools/cache.lock` names each prebuilt tree by a hash of the files it was built from, so editing
`tools/reference-r.sh` or a pinned version makes the next container build from source, and
`just doctor` warns until the lock has a build of the current inputs. After such a change, push the
new build and commit the lock:

```sh
sudo tools/dev-setup.sh   # builds R from source, since the cache has no build of the new inputs
just cache-push r         # packs /opt/R/4.6.2, pushes it to ry.cachix.org, rewrites tools/cache.lock
```

`just cache-push` refuses a tree that is not what its inputs build (an R that fails
`tools/reference-r.sh check`, a library that lacks a locked version), and a push that would take the
trees in the lock past 2.5 GB, half of the cache's 5 GB. Anyone can read the cache, so push only
these trees, which hold GNU R and CRAN packages, and never ry's own builds or anything private. A
cloud session's proxy holds the write token for `app.cachix.org`, which is why the push names that
host; elsewhere, set `CACHIX_AUTH_TOKEN`.

`nix develop` (the `default` and `ci` shells) downloads everything from `cache.nixos.org` or
`ry.cachix.org` and builds nothing. The custom GNU R (`lib.gnuR`) and the R packages built against it
are not in nixpkgs, so `ry.cachix.org` holds them. The Cachix workflow rebuilds and pushes the two
shells when `flake.nix`, `flake.lock` or `tools/base-r` changes on main; the Rust toolchain and
crane's `ry-deps-*` build are left out.

`just doctor` also checks that `ry`, `libR.so` and the start-up image were built from the working
tree, by recomputing the runtime hash they embed, and it reports free disk and the size of each
`target/` profile. A cloud container's disk quota is about 40 GB whatever `df` says. `just prune`
frees space without forcing a full rebuild: it removes build configurations nothing has read for two
days and incremental caches older than an hour, and `just prune --tests` also removes test
executables.

The flake's dev shells carry the same toolchain and a GNU R built like the reference one, and CI
runs in them: `nix develop .#ci` holds what `just ci` needs, and `nix develop` adds the editing
tools. `tools/env.sh` loads the `default` shell for agents.

## Build and test

The workspace builds on the dated nightly that `rust-toolchain.toml` pins, because std's allocator
API and other unstable features are worth having before they stabilize (the policy is in
`.agents/memory/decisions/docs-process.md`, "Data-oriented design and explicit allocators").
With rustup, cargo downloads that toolchain, with clippy and rustfmt, on the first build. The Zed extension is the
exception: `editors/zed/rust-toolchain.toml` pins a stable toolchain, because the nightly's
`wasm32-wasip2` std imports a WASI function that Zed's host does not provide yet. Stable cargo
cannot read the root manifest's profile `rustflags`, so `editors/zed` is its own workspace with
its own `Cargo.lock`, outside the root workspace.
The nix dev shell reads the same file. Nightly rustfmt also applies the import settings in
`rustfmt.toml`, so `cargo fmt` keeps a single `use { ... }` tree per file.

The `justfile` at the repository root names every day-to-day action, and `just` with no arguments
lists them all. The common ones:

```sh
just ry check                       # build ry and its runtime if needed, then run `ry check`
just build                          # every binary and ry's runtime (`just build --release`)
just try                            # build ry and walk through every command in a demo project
just try-editor                     # build ry and print how to point an editor at it
just gate                           # tests, clippy -D warnings, and fmt --check
just battery                        # all workspace tests
just fixture <group__case>          # one fixture case
just bless -p semantics --test ...  # rewrite fixture expectations from current output
just docs-test [page]               # run the docs' marked examples through the real binary
just stats <path>                   # phase timings and memory for one workspace
just perf                           # corpus perf and memory budgets (needs corpus/)
just repl-e2e                       # drive `ry R` and `ry run` through a pty
just release-dry-run                # package and smoke-test this machine's release archive
just docs-deploy <branch>           # build the docs site and deploy it (main is production)
just doctor                         # check the toolchain, GNU R, the corpus and binary freshness
just prune                          # free disk in target/ without forcing a full rebuild
just cache-push r                   # push the reference GNU R to ry.cachix.org (or `corpus`)
```

The simulators, fuzzers and long oracle runs have their own recipes, which
[Robustness](/development/robustness#running-the-long-harnesses) lists.

`just fixture` runs a case from the `test_typing_fixtures` target in `semantics` unless you pass a
different target and package as its second and third arguments. Review the diff after every bless:
bless records whatever the code produces, right or wrong.

Without `just`:

```sh
cargo build              # the ry binary (the workspace default member)
cargo test               # the ry crate's tests
cargo test --workspace   # every crate
cargo test -p semantics  # the analysis core, the usual loop while iterating
```

Most behavior is pinned by fixture tests, plain-text `.test` files whose rendered output is compared
to an expected block. Read [Testing](/development/testing) before you add or change one. Two
environment variables come up every day:

- `RY_BLESS=1` rewrites the expected `#++++` blocks in place.
- `FIXTURE_FILTER=group__case` runs a single case.

Some suites and every performance instrument read a corpus of real-world R code. Fetch it with
`cargo -Zscript scripts/fetch-corpus.rs`, which writes into the gitignored `corpus/`
directory. The resolved inventory is committed as `scripts/corpus-manifest.txt`.

### Build profiles

| Profile | Command | Use it for |
| --- | --- | --- |
| `dev` | `cargo build` | everyday work and tests |
| `release` | `cargo build --release` | shipped binaries and every number you publish or quote |
| `iterate` | `cargo build --profile iterate`, `cargo test-release` | optimized tests and quick performance loops |
| `profiling` | `cargo build --profile profiling` | perf, callgrind and samply |

`iterate` is release without cross-crate link-time optimization (`lto = false`, which keeps
cargo's thin LTO inside each crate). After an edit to the interpreter it rebuilds
`ry` and `libR.so` in about 2 minutes on four cores, against about 6 for release, and its link stays
under 16 GB of memory. No inlining then crosses a crate boundary, so a single program can run
a quarter faster or slower than under release: `ry --version` names the profile
(`ry 0.4.3-beta (iterate build)`), and `tools/bench/bench.py` refuses any binary that is not a
release build. Its `--any-build` flag lifts that refusal for a look at where a build stands, for
example `just bench typed --ry target/iterate/ry --any-build`, and such a run cannot be stored. `profiling` is release with line tables kept and the same fat LTO, so the profiler sees
release's inlining: the `.text` of `ry` and `libR.so` is within 0.2% of release's, and its link
peaks near 11 GB.

### The R console needs no R to build

The `repl` crate loads R's shared library at runtime instead of linking against it, so the whole
workspace builds and its tests run on a machine with no R. Only running the console needs R. The
end-to-end tests skip cleanly where R is missing; run `just repl-e2e` before you touch the console.
[REPL design](/development/design/repl/) records how the embedding works.

### ry's runtime is built separately

`ry run` and `ry repl --runtime ry` load ry's own runtime, `libR.so`, from the directory of the
`ry` executable. It is a separate crate (`libr`), so `cargo build -p ry-lang` does not rebuild it;
`just ry` and `just build` do. To keep a leftover library from running old code, `ry` compares a
hash of the runtime's sources with the one stored in `libR.so` right after loading it, and refuses
a mismatch with exit status 2:

```text
  x ry's runtime (target/debug/libR.so) is out of date: it has build 8fcdf6d9f4862983, but this ry is build
  | 2588183553632a45; rebuild it with `just build` or `cargo build -p libr` (same profile as ry)
```

Build `libr` with the same profile as `ry` and the error goes away. The hash covers the sources
of `libr` and the crates it links, so editing the formatter or the IDE layer never triggers it.

## Debugging

- `ry server --debug`, or `RY_DEBUG=1` in the server's environment, adds a "Debug" section to every
  hover with the Lowering, Naming, and Parsing views of the expression under the cursor. It is
  deliberately not a `ry.toml` key, because the configuration file is a user-facing contract.
- `ry debug ast <file>` prints a file's syntax tree.
- `ry debug erase <file>...` rewrites files in place as their erasure, the plain R that `ry R` runs.
- `ry debug analysis-stats [path]` (or `just stats`) runs the full pipeline over a workspace through
  the language server's queries, with `[check] typing` forced on, and prints:
  - the wall time and resident-memory growth of each phase, and the slowest files to type-check;
  - keystroke latency and rechecks per keystroke, from an incremental probe;
  - how often hover, goto-definition and signature help answer at sampled identifiers, by role
    (call head, variable, argument name, `$` field, `pkg::name`);
  - how often completion of a truncated identifier puts the original name in the top five, with
    latency percentiles;
  - whether find-references on sampled package bindings agrees with an independent lexical resolver
    (`crates/ry/src/scope_oracle.rs`), and whether renaming a binding and back restores the text.

  Build with `--release` when absolute numbers matter; a debug build gives honest ratios.

## The docs site

The sidebar (`docs/astro.config.ts`) has four sections, and detail belongs in Reference or
Development, never in the introduction or the guide:

| Section | Pages |
| --- | --- |
| Introduction | `getting-started.mdx`, the landing page: a short pitch, what ry does, the benchmark summary and next steps, with no code example (by user direction); `installation.md` (binary, R, editors); `benchmarks.mdx` (bar charts, what each suite contains, how it is measured) |
| Learn | `guide.mdx` ("Adopting ry": one H2 per adoption step, each a sequence of snippets with real output); `ry.mdx`, the language tour (every feature once, code first, also served as `/ry.md`), kept current when a feature changes |
| Reference | `reference/`, the contracts: `type-system.mdx` is the semantics contract, and the other pages document the tool |
| Development | `development/`, for contributors; `development/design/` holds unsettled drafts, the one place allowed to describe behavior that does not exist, kept out of the sidebar and listed on that folder's index page |

A moved or deleted page gets an entry in the
`redirects` map there. `docs/src/components/Sidebar.astro` keeps the sidebar's scroll position
across page loads, because the theme scrolls `.sidebar-content` rather than the element
Starlight's own persister restores.

Typed examples are tab groups inside `<Tabs syncKey="annotation-style">`, so a reader's choice
follows them across pages: `Untyped` (the plain R a reader starts from), then `Comments` and
`Inline`, with the form the page teaches second: `Comments` in the guide's step 2 and `Inline`
elsewhere. An example drops `Untyped` when its plain R would not be R (records, tags,
`match`, Arrays, `DataFrame`, the standard library, nominal types) or when its subject is an
annotation itself. Pages with tabs are `.mdx`. Use one tab group per idea, not one per snippet.

Every block of `ry` output on a page is a test that `just docs-test` runs through the real binary;
[Docs examples](/development/testing#docs-examples) has the marker format and the rules for tabs.

`benchmarks.mdx` has one section per measured suite, in this order: public R JIT benchmarks,
package test suites, numeric kernels (ours), application workloads (ours), hot package entries, and
typed ry against Rust, with typed as a column where a typed version exists. A suite without
committed results gets no section and no summary row. The suite against Rust reports typed ry's
time over Rust's, where lower is faster, so it never shares a table, a chart or a sentence with the
speed-ups over GNU R. Each suite is a directory under `tools/bench/` holding `suite.toml` (its `id`,
names, summaries, source paths) and `results.json`, which `just bench <suite> --release ARCHIVE
--write` writes on the tagged release archive. The runner, `tools/bench/bench.py`, owns every
too-short or refused flag and every geometric mean, and `docs/src/lib/benchmarks.ts` and
`tools/bench/readme.py` only render those files, so never type a benchmark number into prose.
`just readme --check` (in `just gate`) catches a stale README block or diagnostic image.

Agents read the site as Markdown. Every page is also served at its URL plus `.md` (for example
`/guide#step-2-type-your-code.md`), with the frontmatter, imports and components removed and each tab
group turned into one subheading per tab. `/llms.txt` lists the pages with their descriptions in
sidebar order, and `/llms-full.txt` concatenates them all. Each page's head
links its Markdown version with `<link rel="alternate" type="text/markdown">`, and the breadcrumb row
has a "View as Markdown" link. The code is `docs/src/lib/markdown.ts`, the three endpoints in
`docs/src/pages/`, and `docs/src/routeData.ts`. A new MDX component must be handled in
`bodyToMarkdown`, or it leaks into the Markdown as raw tags. A `description` containing ` #` must be
quoted, because YAML reads the rest of the line as a comment.

Run `just docs` for a live preview, and `cd docs && bun run build` (or `pnpm build`) for a full
build, which needs Node 22.23.4 or newer. The live site changes only when someone deploys it with
`just docs-deploy` or the Docs workflow ([Releasing](/development/releasing#deploying-the-docs-site)).

[Formatting rules](/reference/formatting-rules) is generated, so do not edit it by hand. Edit
`crates/format/tests/formatter.template.md` and regenerate the page with `just format-docs`.

## The VS Code extension

1. Run `cd editors/code && bun install`.
2. Open the repository in VS Code and press Ctrl+Shift+B to compile the client in watch mode.
3. In the Run and Debug view (Ctrl+Shift+D), select `Launch Client` and press F5.
4. Open any `.R` file in the Extension Development Host window that appears.

## Why the formatter code is imperative

A comment in R can sit between any two tokens: inside an `if` header, between a call and its
argument list, after an operator. A declarative "format each field" style silently drops such a
comment, so the formatter walks the concrete children token by token and decides placement at the
token level. The parser attaches a trailing comment *inside* an expression node, so a decision about
where a closing token goes has to look at tokens, never at whole elements. See
`crates/format/src/format.rs` and the fixtures under `crates/format/tests/format/`.

## Writing: one idea three ways

`AGENTS.md`, "How to write", sets the rules for everything an agent writes. Here is one idea written
three ways. The first packs five facts into one sentence behind a colon:

> `x |> f(y)` is syntax R's own parser rewrites to `f(x, y)` before evaluation, and it types as exactly that call: the piped value becomes the first positional argument, every call rule above applies (arity, argument compatibility, overload selection), chains compose left to right, and a type error on the piped value blames the left-hand expression.

The second overcorrects. Every sentence is short, and none of them says how it relates to the one before:

> R's parser rewrites `x |> f(y)` into `f(x, y)` before it evaluates the code. ry types the pipe as that call and nothing else. The piped value becomes the first positional argument. All call rules above apply to it: arity, argument compatibility, and overload selection. Chains compose from left to right. If the piped value has a type error, ry reports it on the left-hand expression.

The third is the one to write:

> R's parser rewrites `x |> f(y)` into `f(x, y)` before anything runs, so the pipe types exactly like that call. The piped value becomes the first positional argument, and every call rule applies to it: arity, argument compatibility, and overload selection. A chain composes from left to right, and a type error in the piped value is reported on the left-hand expression.

## Comments

Agents read whole source files, so every comment line costs context on every read. A Rust comment
explains why the code is written as it is, and only when the reason is not obvious. `just comments`
(part of `just gate`) runs `tools/comments.py check`, which flags a `//` or `///` block longer
than 12 lines, a `//!` module doc longer than 25, a banner (`// ----`, `// ==== Section ====`), a
section heading (a short `// Allocation.` line between blank lines), and process words such as a
milestone, a pull request number or a hash cited as a commit. A license header at the top of a file is exempt,
as are `patches/` and `target/`.

The check is a ratchet: `tools/comments-baseline.txt` lists the files still allowed some violations.
A file over its count fails. A file under its count also fails, with a hint to run
`python3 tools/comments.py baseline` so the count shrinks. `python3 tools/comments.py report` lists
the worst blocks with their file, line and size. A block whose facts do not fit the limit stays over
it and is carried by the baseline: shortening a comment must never drop a reason, an invariant or a
measured cost.

## Agent memory layout

The agent knowledge base is `.agents/memory/`, indexed by `MEMORY.md`. Settled decisions live in
`.agents/memory/decisions/`, one file per area (`syntax-semantics.md`, `interp.md`, `jit-ssa.md`
and nine more), and `decisions/index.md` lists every entry with a link and its status. A new
decision is an H2 section with a `**Status:**` line, appended to the area file it belongs to, after
which `python3 tools/memory.py index` regenerates the index. A crate's own traps live in
`crates/<crate>/AGENTS.md`.

`python3 tools/memory.py check`, part of `just gate`, fails when a single `decisions.md` reappears,
when the index is stale, when one entry heading appears in two area files, when a file outgrows its
byte budget in `tools/memory.py` (`AGENTS.md`, `MEMORY.md`, `backlog.md`, `worklog.md` and
`.agents/orchestrator.md`, each about 5% above its size when the budget was set, and pruned rather
than given more room), when a decisions area file passes the shared 260 KB cap (settled decisions are
not pruned, so the area is split into two files), or when a line outgrows its file's line limit (400
bytes in `worklog.md`, 2 KB in the orchestrator file). `python3 tools/memory.py split` moves the
entries of a stray `decisions.md` into the area files.

## References

- R grammar and language: [gram.y](https://github.com/wch/r-source/blob/trunk/src/main/gram.y),
  [R Language Definition](https://cran.r-project.org/doc/manuals/r-release/R-lang.html)
- Formatting: [Wadler, A prettier printer](https://homepages.inf.ed.ac.uk/wadler/papers/prettier/prettier.pdf)
- VS Code extensions: [API reference](https://code.visualstudio.com/api/references/vscode-api),
  [language extensions](https://code.visualstudio.com/api/language-extensions),
  [lsp-sample](https://github.com/microsoft/vscode-extension-samples/tree/main/lsp-sample)
- Language servers in Rust: [gleam](https://github.com/gleam-lang/gleam/tree/main/compiler-core/src/language_server),
  [postgres-language-server](https://github.com/supabase-community/postgres-language-server),
  [nil](https://github.com/oxalica/nil) (async-lsp), [harper](https://github.com/Automattic/harper)
- Typing R and related languages: [enderpy](https://github.com/Glyphack/enderpy),
  [dgkf/R](https://github.com/dgkf/R), [typr](https://github.com/fabriceHategekimana/typr),
  [salsa's type-check example](https://github.com/salsa-rs/salsa/blob/master/examples/calc/type_check.rs)
- Out-of-order LSP messages: [tower-lsp#284](https://github.com/ebkalderon/tower-lsp/issues/284),
  [fe#1022](https://github.com/ethereum/fe/pull/1022),
  [tower-lsp-server#36](https://github.com/tower-lsp-community/tower-lsp-server/issues/36)
