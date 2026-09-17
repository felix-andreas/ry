---
title: News
description: Release announcements and project news
---

## ry 0.3.0 — a complete rewrite

*July 2026 · currently in alpha*

Version 0.3.0 replaces the previous implementation end to end: a new parser, a new analysis engine,
and a static type checker — the first for R.

- **Renamed from Roughly to ry.** The binary is `ry`, the config file is `ry.toml`, and suppression
  comments are `# ry: allow(...)`. Nothing needs changing to upgrade: the old names are still
  honoured. The crate is published as `ry-lang`.
- **A static type checker for R** — novel and experimental. Hindley–Milner inference with numeric
  constraints and generics, driven by [`#:` annotation comments](/type-checking/tutorial). Type
  errors are opt-in via `[check] typing`; hover, completion, signature help, and inlay hints use the
  inferred types either way.
- **A hand-written R parser.** Lossless syntax trees, recovery from broken input, and precise
  diagnostics that feed directly into the analysis, so one mistake does not hide the findings in the
  rest of the file.
- **A new incremental analysis engine.** A memoized query core re-checks only what an edit could have
  affected; per-edit results are verified byte-identical to a from-scratch analysis.
- **Type stubs for the standard library.** Declaration-only `.Rtypes` stubs for base, stats, utils,
  and methods ship in the binary; a project can override any of them.
- **Editor features** on the new analysis: hover, completion, go-to-definition, references, rename,
  document and workspace symbols, inlay hints, signature help, and semantic highlighting of `#:`
  annotations.
- **Formatter and linter** run on the same syntax trees as the analysis, so every tool reports the
  same picture of your code.
- **An R console.** `ry repl` and `ry run` load your system R — the only commands that do — with
  completion backed by the project analysis.
- **New documentation** at [ry-lang.org](https://ry-lang.org): a type-checking tutorial, guides, and
  reference pages.

Binaries are on [GitHub Releases](https://github.com/felix-andreas/ry/releases), and the VS Code
extension is on the
[Marketplace](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry).
