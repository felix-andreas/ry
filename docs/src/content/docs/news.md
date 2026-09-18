---
title: News
description: Release announcements and project news
---

## ry 0.3.0, a complete rewrite

*Released in July 2026 as a pre-release.*

Version 0.3.0 replaces the previous implementation end to end. It brings a new
parser, a new analysis engine, and a static type checker, the first one for R.

- **The project is renamed from Roughly to ry.** The binary is `ry`, the
  configuration file is `ry.toml`, and a suppression comment reads
  `# ry: allow(...)`. The crate is published as `ry-lang`.
- **R gets a static type checker.** It is novel and experimental. It runs
  Hindley-Milner inference with numeric constraints and generics, driven by
  [`#:` annotation comments](/type-checking/tutorial). Type errors are opt-in
  through `[check] typing`. Hover, completion, signature help, and inlay hints
  use the inferred types either way.
- **The R parser is hand-written.** It builds lossless syntax trees, recovers
  from broken input, and produces precise diagnostics that feed straight into
  the analysis. One mistake therefore does not hide the findings in the rest
  of the file.
- **The analysis engine is incremental.** A memoized query core rechecks only
  what an edit could have affected. Each per-edit result is verified to be
  byte-identical to an analysis from scratch.
- **The standard library has type stubs.** Declaration-only `.Rtypes` stubs
  for base, stats, utils, and methods ship inside the binary, and a project
  can override any of them.
- **The editor features run on the new analysis.** They are hover,
  completion, go-to-definition, references, rename, document and workspace
  symbols, inlay hints, signature help, and semantic highlighting of `#:`
  annotations.
- **The formatter and the linter read the same syntax trees as the
  analysis.** Every tool therefore reports the same picture of your code.
- **ry ships an R console.** `ry repl` and `ry run` load your system R, and
  they are the only commands that do. Completion in the console is backed by
  the project analysis.
- **The documentation is new**, at [ry-lang.org](https://ry-lang.org). It has
  a type-checking tutorial, guides, and reference pages.

Binaries are on
[GitHub Releases](https://github.com/felix-andreas/ry/releases). The VS Code
extension is on the
[Marketplace](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry).
