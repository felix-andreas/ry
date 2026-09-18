---
title: Development
description: How to contribute to ry
---

This page takes you from a clone to a passing test run. It also explains a few
corners of the codebase that are not obvious from the source.

## Project layout

ry is a Rust workspace. The shipping language tool is six crates.

- `crates/syntax` holds the hand-written lexer and the recursive-descent
  parser. It produces lossless [rowan](https://crates.io/crates/rowan) syntax
  trees. A `#:` type annotation is part of the grammar, not comment text.
- `crates/semantics` is the analysis core, built on
  [salsa](https://crates.io/crates/salsa). It holds the item tree, HIR
  lowering, naming, the Hindley-Milner type checker, stubs, lints, and
  diagnostics.
- `crates/format` is the formatter. It depends on `syntax` alone.
- `crates/ide` implements the editor features as pure reads over `semantics`.
  Those features are hover, navigation, rename, completion, signature help,
  inlay hints, symbols, and code actions.
- `crates/ry` is the product binary. It holds the CLI, with the `check`,
  `fmt`, `server`, `repl`, and `run` commands, and the LSP server.
- `crates/repl` is the interactive R console behind `ry repl` and `ry run`. It
  embeds the system R by locating and loading it at runtime.

The workspace also contains the frozen legacy stack. It is the previous
implementation, kept in the tree only as the benchmark baseline for
`legacy/differential`. It consists of `legacy/analysis-legacy`,
`legacy/engine-legacy`, `legacy/roughly-legacy`, and the `legacy/fixtures`
harness. The parity program that once ran every fixture through both stacks is
complete and retired. Do not extend the legacy stack. Never share or abstract
code between the two stacks. Duplicating a data file between them is fine.

The console embeds R with no build-time link dependency. It locates the R
shared library through `R_HOME`, or through `R RHOME` on `PATH`, and loads it
at runtime. The whole workspace therefore builds, and its unit tests run, on a
machine with no R at all. Only running the console needs R. It runs on Unix
through the `ptr_R_ReadConsole` hook globals, and on Windows through the
`Rstart` callback struct. Its end-to-end tests drive the real binary through a
pseudo-terminal, either a Unix pty or a Windows ConPTY, and they skip cleanly
where no R exists. Run `just repl-e2e` locally before you touch the console.

The analysis design is documented in
[Architecture](/contributing/architecture), and the file layout in
[Structure](/contributing/structure). The editor extensions live under
`editors/`. The `code` directory holds the VS Code extension, and `zed` holds
the Zed extension.

## Build and test

The repository root holds a `justfile` that names every day-to-day action. Run
`just` with no arguments to list them. These matter most:

```sh
just gate                # the full gate: battery, clippy -D warnings, fmt check
just battery             # the workspace test battery, excluding zed_ry
just fixture <group__case>          # one focused fixture case
just bless -p semantics --test ...  # re-bless fixture expectations
just fuzz-deep           # the long-running seeded fuzz pass
just stats <path>        # the workspace performance diagnosis
```

Review the diff after you bless an expectation. The `just fixture` recipe
defaults to the `test_typing_fixtures` target in `semantics`, and takes the
target and the package as its second and third arguments.

Here are the raw commands, for an environment without `just`:

```sh
cargo build                                   # the product crate, the workspace default member
cargo test                                    # the product crate's suites
cargo test --workspace --exclude zed_ry       # everything: the six crates, the benchmark
                                              # harness, and the frozen legacy stack
cargo test -p semantics                       # the analysis core's fixture and fuzz suites
cargo test -p format --test test_format_fixtures
```

Most behavior is verified with fixture tests. A fixture is a human-readable
`.test` file rendered to its expected output. Read
[Testing](/contributing/testing) for the fixture contract before you add or
change a test. Two environment variables matter day to day.

- `RY_BLESS=1` rewrites the expected `#++++` blocks in place from the current
  output. Review the diff before you commit it.
- `FIXTURE_FILTER=group__case` runs a single fixture case.

Some suites and all measurement instruments read a corpus of real-world R
code. Fetch it with `scripts/fetch-corpus.rs`, a single-file script that needs
`cargo +nightly -Zscript`. It writes into the gitignored `corpus/` directory.
The resolved inventory is committed as `scripts/corpus-manifest.txt`. The
performance and memory instruments live in
`legacy/differential/tests/test_stats.rs`, and the
[Testing](/contributing/testing) page documents them.

## Debug mode

`ry server --debug` surfaces internal analysis facts in the editor. A hover
then gains a "Debug" section with the Lowering, Naming, and Parsing views of
the expression under the cursor. Setting `RY_DEBUG=1` in the server's
environment turns on the same thing, for a setup where editing the server's
arguments is awkward. This switch is deliberately not a `ry.toml` key. The
configuration file is user-facing contract, and this switch is an aid for
people working on ry itself.

## Diagnosing a slow workspace

`ry debug analysis-stats [path]` runs the full analysis pipeline over a
workspace, through the same queries the language server uses, and reports
where the time and the memory go. It prints the wall time and the resident-set
growth of each phase: load, parse, lower and naming, typecheck, and
diagnostics. It then prints the slowest files by typecheck time, and an
incremental typing probe on representative files. The probe reports keystroke
latency, item rechecks and resolve steps per keystroke, and the raw re-parse
floor. The command forces `[check] typing` on, because a diagnosis without the
type checker measures nothing interesting, and it says so when the
configuration had it off. Build with `--release` when the absolute numbers
matter. A debug build still shows honest ratios.

`ry debug ast <file>` prints a file's syntax tree.

## Working on this site

Run `just docs` for a live preview. Run `cd docs && npm run build` to build the
site.

The formatter reference at `docs/src/content/docs/reference/formatting-rules.md`
is generated. Edit `crates/format/tests/formatter.template.md` instead, then
regenerate the page with `just format-docs`.

## VS Code extension setup

- Run `cd editors/code && bun install`. This installs the npm modules.
- Press Ctrl+Shift+B in VS Code to compile the client in [watch mode](https://code.visualstudio.com/docs/editor/tasks#:~:text=The%20first%20entry%20executes,the%20HelloWorld.js%20file.).
- Switch to the Run and Debug view in the sidebar with Ctrl+Shift+D.
- Select `Launch Client` from the drop-down, if it is not already selected.
- Press F5 to run the launch configuration.
- Open a document with a `.R` extension in the [Extension Development Host](https://code.visualstudio.com/api/get-started/your-first-extension#:~:text=Then%2C%20inside%20the%20editor%2C%20press%20F5.%20This%20will%20compile%20and%20run%20the%20extension%20in%20a%20new%20Extension%20Development%20Host%20window.) window that opens.

### If the language server never spawns

`launch.json` sets `"autoAttachChildProcesses": true`. Some debugger
extensions intercept the child process, and the server then never starts.
`CodeLLDB` installed from `nixpkgs` is a known case. Disable the extension, or
turn that setting off.

## Why the formatter code is imperative

The main challenge in formatting R is comments. A comment may appear between
any two tokens, including inside an `if` header, between a call and its
argument list, and after an operator. A concise "format each field" style
drops such a comment silently. The formatter therefore walks the concrete
children token by token, and decides placement at the token level. The same
constraint shapes how the formatter handles the rowan tree. A trailing comment
attaches inside an expression node, so a decision about where to place a
closing token must look at tokens, never at whole elements. See
`crates/format/src/format.rs` and the formatter fixtures under
`crates/format/tests/format/`.

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
