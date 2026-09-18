---
title: Development
description: How to contribute to ry
---

This page helps you get from a clone to passing tests and explains a few non-obvious corners of the
codebase.

## Project layout

ry is a Rust workspace. The shipping language tool is six crates:

- **`crates/syntax`** — the hand-written lexer and recursive-descent parser, producing lossless
  [rowan](https://crates.io/crates/rowan) syntax trees; `#:` type annotations are first-class
  grammar, not comment text.
- **`crates/semantics`** — the [salsa](https://crates.io/crates/salsa)-based analysis core: the item
  tree, HIR lowering, naming, the Hindley–Milner type checker, stubs, lints, and diagnostics.
- **`crates/format`** — the non-invasive formatter (depends only on `syntax`).
- **`crates/ide`** — editor features as pure reads over `semantics`: hover, navigation, rename,
  completion, signature help, inlay hints, symbols, code actions.
- **`crates/ry`** — the product binary: the CLI (`check`, `fmt`, `server`, `repl`, `run`) and
  the LSP server.
- **`crates/repl`** — the interactive R console behind `ry repl` and `ry run`, embedding
  the system R by locating and loading it at runtime.

The workspace also contains the **frozen legacy stack** (`legacy/analysis-legacy`,
`legacy/engine-legacy`, `legacy/roughly-legacy`, plus its `legacy/fixtures` harness): the previous
implementation, kept in-tree only as the benchmark baseline for `legacy/differential`. The parity
program that once ran every fixture through both stacks is complete and retired. Do not extend the
legacy stack, and never share or abstract code between the two stacks — data files may be
duplicated freely instead.

The console embeds R **without any build-time link dependency**: the R shared library is located (`R_HOME`, or
`R RHOME` from `PATH`) and loaded at runtime, so the whole workspace builds and its unit tests run
on machines with no R at all. Only *running* the console needs R; it runs on Unix (through the
`ptr_R_ReadConsole` hook globals) and on Windows (through the `Rstart` callback struct). Its
end-to-end tests (`cargo test -p ry-lang --test test_repl_e2e`) drive the real binary through a
pseudo-terminal (a Unix pty or Windows ConPTY) and skip cleanly where no R exists — run them
locally before touching the REPL. A predecessor experiment linked R at build time through
`extendr`; it was deleted once the runtime-binding console reached and passed parity, which is what
removed the workspace's build-time dependency on a local R.

The analysis design is documented in [Architecture](/contributing/architecture) and the file layout in
[Structure](/contributing/structure). The editor extensions live under `editors/` (`code` for VS Code, `zed` for
Zed).

## Build and test

The repo-root `justfile` names every day-to-day action; `just` (with no arguments) lists them.
The ones that matter most:

```sh
just gate                # the full per-slice gate: battery, clippy -D warnings, fmt check
just battery             # the workspace test battery (excludes zed_ry)
just fixture <group__case>          # one focused fixture case
just bless -p semantics --test ...  # re-bless fixture expectations (review the diff!)
just fuzz-deep           # the long-running seeded fuzz pass (FUZZ_ITERS scales)
just fuzz-run <target>   # one coverage-guided fuzz target
just stats <path>        # the workspace performance diagnosis
```

The raw commands, for environments without `just`:

```sh
cargo build                                   # the product crate (workspace default member)
cargo test                                    # the product crate's suites
cargo test --workspace --exclude zed_ry       # everything: all six crates, the benchmark
                                              # harness, and the frozen legacy stack
cargo test -p semantics                       # the analysis core's fixture + fuzz suites
cargo test -p format --test test_format_fixtures
```

Most behavior is verified with **fixture tests** — human-readable `.test` files rendered to expected
output. Read [Testing](/contributing/testing) for the fixture contract before adding or changing tests. Two
environment variables matter day to day:

- `RY_BLESS=1` rewrites the expected `#++++` blocks in place from the current output (review the
  diff before committing).
- `FIXTURE_FILTER=group__case` runs a single fixture case.

The real-world corpus some suites and all measurement instruments use is fetched with
`scripts/fetch-corpus.rs` (a `cargo +nightly -Zscript` single-file script) into the gitignored
`corpus/`; the resolved inventory is committed as `scripts/corpus-manifest.txt`. The performance and memory instruments live in
`legacy/differential/tests/test_stats.rs` and are documented on the [Testing](/contributing/testing) page.

## Debug mode

`ry server --debug` surfaces internal analysis facts in the editor — hovers gain a "Debug"
section with the Lowering, Naming, and Parsing views of the expression under the cursor.
`RY_DEBUG=1` in the server's environment is the equivalent for setups where editing the
server's arguments is awkward; the flag takes precedence. This is deliberately **not** a
`ry.toml` key: the config file is user-facing contract, and this switch is an aid for people
working on ry itself.

## Diagnosing a slow workspace

`ry debug analysis-stats [path]` runs the full analysis pipeline over a workspace through the
same queries the language server uses and reports where the time and memory go: per-phase wall time
with resident-set growth (load, parse, lower + naming, typecheck, diagnostics), the slowest files
by typecheck, and an incremental typing probe on representative files (keystroke latency, item
rechecks and resolve steps per keystroke, and the raw re-parse floor). It forces `[check] typing`
on — a diagnosis without the type checker measures nothing interesting — and says so when the
configuration had it off. Build with `--release` when the absolute numbers matter; a debug build
still shows honest ratios. `ry debug ast <file>` prints a file's syntax tree.

To work on this documentation site, run `just docs` (a live preview) or `cd docs && npx astro build`.
The formatter page (`docs/src/content/docs/reference/formatting-rules.md`) is generated — edit
`crates/format/tests/formatter.template.md` and regenerate with
`RY_BLESS=1 cargo test -p format --test test_format_docs` instead.

## VS Code Extension Setup

- Run `cd editors/code && bun install`. This installs all necessary npm modules
- Press Ctrl+Shift+B in VS Code to start compiling the client in [watch mode](https://code.visualstudio.com/docs/editor/tasks#:~:text=The%20first%20entry%20executes,the%20HelloWorld.js%20file.).
- Switch to the Run and Debug View in the Sidebar (Ctrl+Shift+D).
- Select `Launch Client` from the drop down (if it is not already).
- Press ▷ to run the launch config (F5).
- In the [Extension Development Host](https://code.visualstudio.com/api/get-started/your-first-extension#:~:text=Then%2C%20inside%20the%20editor%2C%20press%20F5.%20This%20will%20compile%20and%20run%20the%20extension%20in%20a%20new%20Extension%20Development%20Host%20window.) instance of VSCode, open a document with a `.R` extension.

### If the language server never spawns

`launch.json` sets `"autoAttachChildProcesses": true`. Some debugger extensions — `CodeLLDB`
installed from `nixpkgs` is a known case — intercept the child process and the server never
starts. Disable the extension or turn that setting off.

## Formatting: why the code is imperative

The main challenge in formatting R is comments, which may appear between any two tokens — inside an
`if` header, between a call and its argument list, after an operator. A concise "format each field"
style silently drops them, so the formatter walks concrete children token-by-token and makes
placement decisions at the token level. The same constraint shapes the rowan tree handling: trailing
comments attach *inside* expression nodes, so closer-placement decisions must look at tokens, never
whole elements. See `crates/format/src/format.rs` and the formatter fixtures under
`crates/format/tests/format/`.

## References

### R

* https://github.com/wch/r-source/blob/trunk/src/main/gram.y
* https://cran.r-project.org/doc/manuals/r-release/R-lang.html
* https://www.reddit.com/r/rust/comments/uu47mk/comment/i9dn0yg/

### VS Code Extensions

* docs:
  * https://code.visualstudio.com/api/references/vscode-api
  * https://code.visualstudio.com/api/language-extensions
* examples:
  * https://github.com/microsoft/vscode-extension-samples/tree/main/lsp-sample
  * https://github.com/semanticart/lsp-from-scratch
  * https://github.com/nix-community/vscode-nix-ide
  * https://github.com/ziglang/vscode-zig/

### Other Language Servers written in Rust

* https://github.com/gleam-lang/gleam/tree/main/compiler-core/src/language_server
* https://github.com/supabase-community/postgres-language-server
* tower-lsp
  * https://github.com/Desdaemon/-lsp/
  * https://github.com/FuelLabs/sway
  * https://github.com/IWANABETHATGUY/tower-lsp-boilerplate
  * https://github.com/TenStrings/glicol-lsp/blob/77e97d9c687dc5d66871ad5ec91b6f049de2b8e8/src/main.rs#L16
  * https://github.com/Automattic/harper
  * https://github.com/jfecher/ante/blob/5f7446375bc1c6c94b44a44bfb89777c1437aaf5/ante-ls/src/main.rs#L163
* async_lsp
  * https://github.com/oxalica/nil

### Formatting

* https://homepages.inf.ed.ac.uk/wadler/papers/prettier/prettier.pdf

### Language Design / Typing

* https://github.com/Glyphack/enderpy
* https://github.com/dgkf/R
* https://github.com/fabriceHategekimana/typr
* https://github.com/salsa-rs/salsa/blob/master/examples/calc/type_check.rs

### Out of order issue

* https://github.com/ebkalderon/tower-lsp/issues/284
* https://github.com/ethereum/fe/pull/1022
* https://github.com/oxalica/async-lsp
* https://github.com/tower-lsp-community/tower-lsp-server/issues/36
