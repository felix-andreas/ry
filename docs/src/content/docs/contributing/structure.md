---
title: Structure
description: What each source file holds in the syntax, semantics, ide, format, ry, and repl crates
---

This page records what each source file holds. The crate graph and the phase
boundaries live in [Architecture](/contributing/architecture). The frozen
`*-legacy` crates keep their own layout, and this page does not cover them.

## `syntax`

The crate root is `src/syntax.rs`.

- `lexer.rs` is the hand-written lexer. It lexes an `#:` annotation region as
  structured trivia.
- `parser.rs` is the parser. It is recursive descent with Pratt expression
  parsing, and it builds rowan green trees. It also parses the annotation type
  grammar. Recovery is anchored at statement boundaries.
- `kind.rs` defines `SyntaxKind`. That enum names every token kind and node
  kind, and it carries the display name for each one.
- `ast.rs` holds the typed views over the raw tree. Every accessor returns an
  `Option`.
- `literate.rs` reads an `.Rmd`, `.qmd`, or `.Rnw` document as the R program
  its chunks contain. The conversion preserves length by replacing every
  non-R character with a space, so byte offsets need no translation.
- `reparse.rs` reparses incrementally by splicing one statement. It is an
  optimization. Parse correctness never depends on it.
- `testing.rs` is the fixture harness that every fixture suite shares. It
  defines the `.test` file format, the `RY_BLESS` and `FIXTURE_FILTER`
  variables, and the rejection of duplicate case ids.

## `semantics`

The crate root is `src/semantics.rs`.

- `semantics.rs` defines the salsa database. It holds two of the four database
  inputs, `SourceFile` and `ProjectFiles`. The other two, `PackageMetadata`
  and `StubSources`, live with their own modules. It also holds the item tree
  with its insertion-stable identities, the anchoring of an item to its
  syntax, the package interface through the `global_scheme` fixpoint, and the
  item-span queries.
- `hir.rs` lowers the syntax tree to the per-item HIR. An expression range in
  the HIR is relative to its item.
- `naming.rs` is the variable model. A variable is a mutable slot. The module
  resolves scopes, reaching-write flow, and captures. It also recognizes
  data-masked evaluation and reports unused outputs.
- `types.rs` defines the interned types `Ty` and `TyKind`, along with schemes,
  constraints, and union normalization.
- `infer.rs` is the inference table. It holds the union-find entries,
  unification, the directional compatibility relation, and memoized deep
  resolution.
- `check.rs` runs the inference walk for one item. It covers the environment
  undo log, calls and overload probing, control flow, annotation enforcement,
  and strict origins.
- `annotations.rs` lowers an `#:` annotation node onto an interned type. It
  also holds the rules for the block form.
- `stubs.rs` owns the `.Rtypes` corpus. It parses the corpus, assembles the
  library of schemes, nominals, masked verbs, and namespace exports, and
  reports loader problems.
- `metadata.rs` provides the `PackageMetadata` input. It parses NAMESPACE and
  DESCRIPTION, then resolves imports and dependencies.
- `lints.rs` holds the style lints and their configuration types.
- `testing.rs` is the invariant battery for the semantic pipeline. The fuzz
  harness and the coverage-guided targets share it.
- `diagnostics.rs` is the diagnostics edge. It produces the parse-stage set
  and the full per-file set, renders strict output, and holds the one
  user-facing `TypeRenderer`.

## `ide`

The crate is one file, `src/ide.rs`. Every editor feature is a read of
`semantics` queries. The features are hover, goto-definition, references,
rename, inlay hints, signature help, completion, code actions, document and
workspace symbols, and navigation to annotation types and S4 definitions.
Definition, references, and rename share one occurrence engine. Completion
uses the shared smart-case matcher.

## `format`

The crate is one file, `src/format.rs`. It holds the preserving formatter
over the syntax tree and the formatter's configuration types.

## `ry`

The library root is `src/ry.rs`. The binary root is `src/main.rs`.

- `main.rs` defines the CLI surface and the exit-code contract. An exit code
  of 0 means no findings, 1 means findings, and 2 means a usage,
  configuration, or IO error.
- `cli.rs` implements `check`, `fmt`, and `ry debug ast`. The `check`
  implementation assembles the project, renders the output, and reports
  NAMESPACE problems and stub overrides.
- `server.rs` is the LSP server. It holds the frontend and worker threading,
  document sync, push and pull diagnostics, every feature endpoint, semantic
  tokens, and the stub and NAMESPACE buffers.
- `config.rs` discovers and parses `ry.toml`.
- `diagnostics.rs` assembles diagnostics for both the server and the CLI. It
  applies configuration gating, strict escalation, and suppression comments.
- `namespace.rs` parses and validates NAMESPACE imports.
- `position.rs` is the line index. It converts between a byte offset and a
  line and column. The column is measured in bytes, in characters, or in
  UTF-16 code units. The CLI reports characters. The LSP default is UTF-16
  code units.
- `stats.rs` implements the performance diagnosis behind
  `ry debug analysis-stats`.
- `repl_completer.rs` provides completion for the interactive console.

## `repl`

The crate root is `src/repl.rs`. This crate backs `ry repl` and `ry run`. It
locates and loads the system R at runtime, so the rest of the workspace
builds and analyzes R on a machine with no R installed.

- `repl.rs` is the session. It holds evaluation, the read-eval-print loop,
  and the exit codes.
- `libr.rs` binds to R's shared library at runtime. There is no build-time
  link.
- `console.rs` is the terminal front end.
