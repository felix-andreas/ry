---
title: Development
description: How to build, test, and debug ry
---

ry is a Rust workspace of six crates under `crates/`. [Structure](/contributing/structure) maps
every source file, and [Architecture](/contributing/architecture) explains how the analysis fits
together. The editor extensions live in `editors/code` (VS Code) and `editors/zed` (Zed).

## Build and test

The `justfile` at the repository root names every day-to-day action, and `just` with no arguments
lists them all. The common ones:

```sh
just gate                           # tests, clippy -D warnings, and fmt --check
just battery                        # all workspace tests, excluding zed_ry
just fixture <group__case>          # one fixture case
just bless -p semantics --test ...  # rewrite fixture expectations from current output
just fuzz-deep                      # the long seeded fuzz run
just stats <path>                   # phase timings and memory for one workspace
just perf                           # corpus perf and memory budgets (needs corpus/)
just repl-e2e                       # drive `ry repl` through a pty against the system R
```

`just fixture` runs a case from the `test_typing_fixtures` target in `semantics` unless you pass a
different target and package as its second and third arguments. Review the diff after every bless:
bless records whatever the code produces, right or wrong.

Without `just`:

```sh
cargo build                              # the ry binary (the workspace default member)
cargo test                               # the ry crate's tests
cargo test --workspace --exclude zed_ry  # every crate
cargo test -p semantics                  # the analysis core, the usual loop while iterating
```

Most behavior is pinned by fixture tests, plain-text `.test` files whose rendered output is compared
to an expected block. Read [Testing](/contributing/testing) before you add or change one. Two
environment variables come up every day:

- `RY_BLESS=1` rewrites the expected `#++++` blocks in place.
- `FIXTURE_FILTER=group__case` runs a single case.

Some suites and every performance instrument read a corpus of real-world R code. Fetch it with
`cargo +nightly -Zscript scripts/fetch-corpus.rs`, which writes into the gitignored `corpus/`
directory. The resolved inventory is committed as `scripts/corpus-manifest.txt`.

### The R console needs no R to build

The `repl` crate loads R's shared library at runtime instead of linking against it, so the whole
workspace builds and its tests run on a machine with no R. Only running the console needs R. The
end-to-end tests skip cleanly where R is missing; run `just repl-e2e` before you touch the console.
[REPL design](/contributing/design/repl/) records how the embedding works.

## Debugging

- `ry server --debug`, or `RY_DEBUG=1` in the server's environment, adds a "Debug" section to every
  hover with the Lowering, Naming, and Parsing views of the expression under the cursor. It is
  deliberately not a `ry.toml` key, because the configuration file is a user-facing contract.
- `ry debug ast <file>` prints a file's syntax tree.
- `ry debug analysis-stats [path]` (or `just stats`) runs the full pipeline over a workspace,
  through the same queries the language server uses. It prints the wall time and resident-memory
  growth of each phase, the slowest files by type-checking time, and an incremental probe that
  reports keystroke latency and rechecks per keystroke. It forces `[check] typing` on. Build with
  `--release` when absolute numbers matter; a debug build still gives honest ratios.

## The docs site

Every typed example in the docs is a pair of tabs, `Inline` first and `Comments` second, inside a
Starlight `<Tabs syncKey="annotation-style">`, so a reader's choice follows them across pages. Pages
with tabs are `.mdx`. After editing one, run `python3 docs/check-tabs.py docs/src/content/docs/**/*.mdx`
with a built `ry` (set `RY` if it is not at `target/debug/ry`): it checks both tabs of each pair and
fails when their diagnostics differ. Every annotation form has both spellings, type definitions
included, so the Inline tab carries no `#:` line except to show the two spellings together.

Agents read the site as Markdown. Every page is also served at its URL plus `.md` (for example
`/type-checking/overview.md`), with the frontmatter, imports and components removed and each tab
group turned into consecutive `Inline` and `Comments` subheadings. `/llms.txt` lists the pages with
their descriptions in sidebar order, and `/llms-full.txt` concatenates them all. Each page's head
links its Markdown version with `<link rel="alternate" type="text/markdown">`, and the breadcrumb row
has a "View as Markdown" link. The code is `docs/src/lib/markdown.ts`, the three endpoints in
`docs/src/pages/`, and `docs/src/routeData.ts`. A new MDX component must be handled in
`bodyToMarkdown`, or it leaks into the Markdown as raw tags. A `description` containing ` #` must be
quoted, because YAML reads the rest of the line as a comment.

Run `just docs` for a live preview, and `cd docs && bun run build` for a full build, which needs
Node 22.23.4 or newer.

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
token level. Rowan attaches a trailing comment *inside* an expression node, so a decision about
where a closing token goes has to look at tokens, never at whole elements. See
`crates/format/src/format.rs` and the fixtures under `crates/format/tests/format/`.

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
