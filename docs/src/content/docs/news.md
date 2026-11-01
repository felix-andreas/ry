---
title: News
description: Release announcements and project news
---

## ry 0.4.0-beta, the first release that runs R

*Not released yet: this entry is prepared for the beta.*

Version 0.4.0-beta adds ry's own R runtime beside the checker, plus the language features that only
that runtime runs.

- **`ry run script.R` runs R on ry's runtime**, and `ry R script.R` runs the same script on GNU R.
  ry's runtime still reads base R from your GNU R installation, so both need one.
- **Inline annotations** such as `function(price: double) -> double` sit beside `#:` comments. ry's
  runtime checks them at function boundaries, and GNU R runs the code with them erased.
- **Records, tuples, tagged unions and `match`**, checked for exhaustiveness, are ry's recommended
  way to model data.
- **`DataFrame`** is a typed table with a Polars-style API that runs on the Polars engine.
- **`ry load`** loads the package you are developing from source, lazily, and the `ry run` and `ry R`
  consoles reload what you edited before each command.
- **`ry check --fix`** applies the safe fixes (`T` to `TRUE`, `=` to `<-`, a trailing comma), and
  `--unsafe-fixes` also applies spelling suggestions. The editor gets the same quick fixes plus a
  fix-all action, a call hierarchy, an outline with sections, folding, selection ranges, semantic
  highlighting and parameter-name hints.

Each release archive now holds `ry` and ry's runtime library side by side, for Linux (x86_64 and
aarch64), macOS (Apple silicon and Intel) and Windows (x86_64). The macOS and Windows runtimes have
not been run on a real Mac or Windows machine yet (Windows only under Wine), so treat them as
experimental.

## ry 0.3.0, a complete rewrite

*Released in July 2026 as a pre-release.*

Version 0.3.0 replaces the previous implementation end to end, with a new parser, a new analysis
engine, and a static type checker, the first one for R.

- **The project is renamed from Roughly to ry.** The binary is `ry`, the configuration file is
  `ry.toml`, and a suppression comment reads `# ry: allow(...)`. The crate is published as `ry-lang`.
- **R gets a static type checker**, new and experimental: Hindley-Milner inference with numeric
  constraints and generics, guided by [`#:` annotation comments](/type-checking/annotations). Type
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
- **ry ships an R console.** `ry repl` and `ry run` load your system R (they are the only commands
  that do), and completion in the console is backed by the project analysis.

Binaries are on [GitHub Releases](https://github.com/felix-andreas/ry/releases), and the VS Code
extension is on the [Marketplace](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry).
