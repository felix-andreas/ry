---
title: News
description: Release announcements and project news
---

## ry 0.3.0, a complete rewrite

*Released in July 2026 as a pre-release.*

Version 0.3.0 replaces the previous implementation end to end, with a new parser, a new analysis
engine, and a static type checker, the first one for R.

- **The project is renamed from Roughly to ry.** The binary is `ry`, the
  configuration file is `ry.toml`, and a suppression comment reads
  `# ry: allow(...)`. The crate is published as `ry-lang`.
- **R gets a static type checker**, new and experimental: Hindley-Milner inference with numeric
  constraints and generics, guided by [`#:` annotation comments](/type-checking/tutorial). Type
  errors are opt-in through `[check] typing`, but hover, completion, signature help, and inlay hints
  use the inferred types either way.
- **The R parser is hand-written.** It builds lossless syntax trees, recovers from broken input, and
  produces precise diagnostics that feed straight into the analysis, so one mistake no longer hides
  the findings in the rest of the file.
- **The analysis engine is incremental.** A memoized query core rechecks only what an edit could have
  affected, and incremental results are verified to be byte-identical to an analysis from scratch.
- **The standard library has type stubs.** Declaration-only `.Rtypes` stubs for base, stats, utils,
  and methods ship inside the binary, and a project can override any of them.
- **The editor features run on the new analysis**: hover, completion, go-to-definition, references,
  rename, document and workspace symbols, inlay hints, signature help, and semantic highlighting of
  `#:` annotations.
- **The formatter and the linter read the same syntax trees as the analysis**, so every tool sees
  your code the same way.
- **ry ships an R console.** `ry repl` and `ry run` load your system R (they are the only commands
  that do), and completion in the console is backed by the project analysis.
- **The documentation is new**, at [ry-lang.org](https://ry-lang.org), with a type-checking tutorial,
  guides, and reference pages.

Binaries are on
[GitHub Releases](https://github.com/felix-andreas/ry/releases). The VS Code
extension is on the
[Marketplace](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry).
