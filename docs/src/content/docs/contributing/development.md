---
title: Development
description: How to contribute to ry
---

This page takes you from a fresh clone to a passing test run, and then explains a few corners of the
codebase that the source alone does not make obvious.

## Project layout

ry is a Rust workspace, and the tool it ships is six crates:

- `crates/syntax`: the hand-written lexer and recursive-descent parser, producing lossless
  [rowan](https://crates.io/crates/rowan) syntax trees. A `#:` type annotation is part of the
  grammar, not comment text.
- `crates/semantics`: the analysis core, built on [salsa](https://crates.io/crates/salsa). It holds
  the item tree, HIR lowering, naming, the Hindley-Milner type checker, stubs, lints, and
  diagnostics.
- `crates/format`: the formatter, which depends on `syntax` alone.
- `crates/ide`: the editor features (hover, navigation, rename, completion, signature help, inlay
  hints, symbols, and code actions), all implemented as pure reads over `semantics`.
- `crates/ry`: the product binary, with the CLI (`check`, `fmt`, `server`, `repl`, and `run`) and the
  LSP server.
- `crates/repl`: the interactive R console behind `ry repl` and `ry run`, which finds the system's R
  and loads it at runtime.

The workspace also contains the frozen legacy stack: the previous implementation
(`legacy/analysis-legacy`, `legacy/engine-legacy`, `legacy/roughly-legacy`, and the
`legacy/fixtures` harness), kept only as the benchmark baseline for `legacy/differential`. The
parity program that once ran every fixture through both stacks is complete and retired. Do not
extend the legacy stack, and never share or abstract code between the two stacks, although
duplicating a data file between them is fine.

The console embeds R without linking against it at build time. It finds R's shared library through
`R_HOME` (or by asking `R RHOME` on the `PATH`) and loads it at runtime, so the whole workspace
builds, and its unit tests run, on a machine with no R at all. Only running the console needs R. On
Unix the console hooks in through the `ptr_R_ReadConsole` globals, and on Windows through the
`Rstart` callback struct. Its end-to-end tests drive the real binary through a pseudo-terminal (a
Unix pty or a Windows ConPTY) and skip cleanly where R is missing. Run `just repl-e2e` locally before
you touch the console.

[Architecture](/contributing/architecture) describes how the analysis works, and
[Structure](/contributing/structure) maps every source file. The editor extensions live under
`editors/`: `code` is the VS Code extension, and `zed` is the Zed extension.

## Build and test

The `justfile` at the repository root names every day-to-day action, and running `just` with no
arguments lists them. These are the ones you will reach for most:

```sh
just gate                # the full gate: battery, clippy -D warnings, fmt check
just battery             # the workspace test battery, excluding zed_ry
just fixture <group__case>          # one focused fixture case
just bless -p semantics --test ...  # re-bless fixture expectations
just fuzz-deep           # the long-running seeded fuzz pass
just stats <path>        # the workspace performance diagnosis
```

`just fixture` runs a case from the `test_typing_fixtures` target in `semantics` unless you pass a
different target and package as its second and third arguments. Whenever you bless expectations,
review the diff before you commit it.

Without `just`, these are the raw commands:

```sh
cargo build                                   # the product crate, the workspace default member
cargo test                                    # the product crate's suites
cargo test --workspace --exclude zed_ry       # everything: the six crates, the benchmark
                                              # harness, and the frozen legacy stack
cargo test -p semantics                       # the analysis core's fixture and fuzz suites
cargo test -p format --test test_format_fixtures
```

Most behavior is verified by fixture tests: human-readable `.test` files, each rendered and compared
to its expected output. Read [Testing](/contributing/testing) for the fixture contract before you
add or change one. Two environment variables come up every day:

- `RY_BLESS=1` rewrites the expected `#++++` blocks in place from the current output.
- `FIXTURE_FILTER=group__case` runs a single fixture case.

Some suites, and every measurement instrument, read a corpus of real-world R code. Fetch it with
`scripts/fetch-corpus.rs`, a single-file script that needs `cargo +nightly -Zscript` and writes into
the gitignored `corpus/` directory. The resolved inventory is committed as
`scripts/corpus-manifest.txt`. The performance and memory instruments live in
`legacy/differential/tests/test_stats.rs`, and [Testing](/contributing/testing) documents them.

## Debug mode

`ry server --debug` surfaces internal analysis facts in the editor: every hover gains a "Debug"
section showing the Lowering, Naming, and Parsing views of the expression under the cursor. Setting
`RY_DEBUG=1` in the server's environment does the same, for setups where changing the server's
arguments is awkward. It is deliberately not a `ry.toml` key, because the configuration file is a
user-facing contract and this switch is a tool for people working on ry itself.

## Diagnosing a slow workspace

`ry debug analysis-stats [path]` runs the full analysis pipeline over a workspace, through the same
queries the language server uses, and reports where the time and memory go. For each phase (load,
parse, lowering and naming, type checking, diagnostics) it prints the wall time and how much the
resident set grew. After that come the slowest files by type-checking time, and an incremental probe
on representative files that reports keystroke latency, item rechecks and resolve steps per
keystroke, and the raw re-parse time as a floor to compare against.

The command forces `[check] typing` on, since a diagnosis without the type checker measures nothing
interesting, and it tells you when your configuration had it off. Build with `--release` when the
absolute numbers matter; a debug build still gives honest ratios.

`ry debug ast <file>` prints a file's syntax tree.

## Working on this site

Run `just docs` for a live preview, and `cd docs && npm run build` for a full build.

The formatter reference at `docs/src/content/docs/reference/formatting-rules.md` is generated, so
do not edit it by hand. Edit `crates/format/tests/formatter.template.md` instead, and regenerate the
page with `just format-docs`.

## Setting up the VS Code extension

1. Run `cd editors/code && bun install` to install the npm modules.
2. Press Ctrl+Shift+B in VS Code to compile the client in
   [watch mode](https://code.visualstudio.com/docs/editor/tasks#:~:text=The%20first%20entry%20executes,the%20HelloWorld.js%20file.).
3. Open the Run and Debug view with Ctrl+Shift+D.
4. Select `Launch Client` from the drop-down, if it is not already selected.
5. Press F5 to run the launch configuration.
6. In the
   [Extension Development Host](https://code.visualstudio.com/api/get-started/your-first-extension#:~:text=Then%2C%20inside%20the%20editor%2C%20press%20F5.%20This%20will%20compile%20and%20run%20the%20extension%20in%20a%20new%20Extension%20Development%20Host%20window.)
   window that opens, open any `.R` file.

### If the language server never starts

`launch.json` sets `"autoAttachChildProcesses": true`, and some debugger extensions then intercept
the child process so that the server never starts. `CodeLLDB` installed from `nixpkgs` is a known
culprit. Disable that extension, or turn the setting off.

## Why the formatter code is imperative

The hard part of formatting R is comments. A comment can sit between any two tokens: inside an `if`
header, between a call and its argument list, after an operator. A concise "format each field" style
drops such a comment without a word, so the formatter walks the concrete children token by token and
decides placement at the token level.

The rowan tree adds a twist. A trailing comment attaches *inside* an expression node, so any
decision about where a closing token goes has to look at tokens, never at whole elements. See
`crates/format/src/format.rs` and the formatter fixtures under `crates/format/tests/format/`.

## References

### R

- https://github.com/wch/r-source/blob/trunk/src/main/gram.y
- https://cran.r-project.org/doc/manuals/r-release/R-lang.html
- https://www.reddit.com/r/rust/comments/uu47mk/comment/i9dn0yg/

### VS Code extensions

- docs:
  - https://code.visualstudio.com/api/references/vscode-api
  - https://code.visualstudio.com/api/language-extensions
- examples:
  - https://github.com/microsoft/vscode-extension-samples/tree/main/lsp-sample
  - https://github.com/semanticart/lsp-from-scratch
  - https://github.com/nix-community/vscode-nix-ide
  - https://github.com/ziglang/vscode-zig/

### Other language servers written in Rust

- https://github.com/gleam-lang/gleam/tree/main/compiler-core/src/language_server
- https://github.com/supabase-community/postgres-language-server
- tower-lsp
  - https://github.com/FuelLabs/sway
  - https://github.com/IWANABETHATGUY/tower-lsp-boilerplate
  - https://github.com/TenStrings/glicol-lsp/blob/77e97d9c687dc5d66871ad5ec91b6f049de2b8e8/src/main.rs#L16
  - https://github.com/Automattic/harper
  - https://github.com/jfecher/ante/blob/5f7446375bc1c6c94b44a44bfb89777c1437aaf5/ante-ls/src/main.rs#L163
- async_lsp
  - https://github.com/oxalica/nil

### Formatting

- https://homepages.inf.ed.ac.uk/wadler/papers/prettier/prettier.pdf

### Language design and typing

- https://github.com/Glyphack/enderpy
- https://github.com/dgkf/R
- https://github.com/fabriceHategekimana/typr
- https://github.com/salsa-rs/salsa/blob/master/examples/calc/type_check.rs

### Out-of-order issues

- https://github.com/ebkalderon/tower-lsp/issues/284
- https://github.com/ethereum/fe/pull/1022
- https://github.com/oxalica/async-lsp
- https://github.com/tower-lsp-community/tower-lsp-server/issues/36
