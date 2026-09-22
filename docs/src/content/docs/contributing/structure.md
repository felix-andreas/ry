---
title: Structure
description: What each source file holds in the syntax, semantics, ide, format, ry, and repl crates
---

This page is a map: for every source file in the product crates, what it is responsible for. How the
crates depend on each other, and where one phase hands over to the next, is the subject of
[Architecture](/contributing/architecture). The frozen `*-legacy` crates keep their own layout and
are not covered here.

## `syntax`

The crate root is `src/syntax.rs`.

- **`lexer.rs`**: the hand-written lexer. A `#:` annotation region comes out as structured trivia,
  so the parser can give it a grammar of its own.
- **`parser.rs`**: a recursive-descent parser with Pratt parsing for expressions, building rowan
  green trees. It also parses the annotation type grammar, and it recovers from errors at statement
  boundaries.
- **`kind.rs`**: `SyntaxKind`, the enum naming every token and node kind, with a display name for
  each.
- **`ast.rs`**: typed views over the raw tree. Every accessor returns an `Option`, because a broken
  file can be missing any part.
- **`literate.rs`**: reads an `.Rmd`, `.qmd`, or `.Rnw` document as the R program its chunks contain.
  Every non-R character becomes a space, so the conversion preserves length and byte offsets need no
  translation.
- **`reparse.rs`**: incremental reparsing by splicing in a single statement. It is purely an
  optimization, and parse correctness never depends on it.
- **`testing.rs`**: the fixture harness every fixture suite shares, with the `.test` file format,
  the `RY_BLESS` and `FIXTURE_FILTER` variables, and the rejection of duplicate case ids.

## `semantics`

The crate root is `src/semantics.rs`.

- **`semantics.rs`**: the salsa database. It declares two of the four database inputs, `SourceFile`
  and `ProjectFiles` (the other two, `PackageMetadata` and `StubSources`, live with the modules that
  own them). It also holds the item tree and its insertion-stable identities, the anchoring of each
  item to its syntax, the package interface computed by the `global_scheme` fixpoint, and the
  item-span queries.
- **`hir.rs`**: lowering from the syntax tree to the per-item HIR, whose expression ranges are
  relative to their item.
- **`naming.rs`**: the variable model, in which a variable is a mutable slot. It resolves scopes,
  reaching-write flow, and captures, recognizes data-masked evaluation, and reports unused outputs.
- **`types.rs`**: the interned types `Ty` and `TyKind`, plus schemes, constraints, and union
  normalization.
- **`infer.rs`**: the inference table, with its union-find entries, unification, the directional
  compatibility relation, and memoized deep resolution.
- **`check.rs`**: the inference walk over one item. This is where the environment's undo log, calls
  and overload probing, control flow, annotation enforcement, and strict origins live.
- **`annotations.rs`**: lowers a `#:` annotation node to an interned type, and holds the rules for
  annotation blocks.
- **`stubs.rs`**: the `.Rtypes` corpus. It parses the stubs, assembles them into a library of
  schemes, nominal types, masked verbs, and namespace exports, and reports loader problems.
- **`metadata.rs`**: the `PackageMetadata` input. It parses `NAMESPACE` and `DESCRIPTION` and
  resolves imports and dependencies.
- **`lints.rs`**: the style lints and their configuration types.
- **`testing.rs`**: the invariant battery for the semantic pipeline, shared by the fuzz harness and
  the coverage-guided targets.
- **`diagnostics.rs`**: where diagnostics leave the crate. It produces the parse-stage set and the
  full per-file set, renders strict output, and holds `TypeRenderer`, the one place types are turned
  into user-facing text.

## `ide`

The whole crate is one file, `src/ide.rs`, and every editor feature in it is a read of `semantics`
queries: hover, goto-definition, references, rename, inlay hints, signature help, completion, code
actions, document and workspace symbols, and navigation to annotation types and S4 definitions.
Definition, references, and rename share one occurrence engine, and completion uses the shared
smart-case matcher.

## `format`

The whole crate is one file, `src/format.rs`: the formatter, which works on the syntax tree and
preserves your line breaks, together with its configuration types.

## `ry`

The library root is `src/ry.rs`, and the binary root is `src/main.rs`.

- **`main.rs`**: the CLI surface and the exit-code contract. Exit code 0 means no findings, 1 means
  findings, and 2 means a usage, configuration, or IO error.
- **`cli.rs`**: the implementations of `check`, `fmt`, and `ry debug ast`. `check` assembles the
  project, renders the output, and reports `NAMESPACE` problems and stub overrides.
- **`server.rs`**: the LSP server, all of it: the frontend and worker threads, document sync, push
  and pull diagnostics, every feature endpoint, semantic tokens, and the stub and `NAMESPACE`
  buffers.
- **`config.rs`**: finds and parses `ry.toml`.
- **`diagnostics.rs`**: assembles diagnostics for the server and the CLI alike, applying
  configuration gating, strict escalation, and suppression comments.
- **`namespace.rs`**: parses and validates `NAMESPACE` imports.
- **`position.rs`**: the line index, converting between byte offsets and line and column. A column
  can be counted in bytes, characters, or UTF-16 code units: the CLI reports characters, and LSP
  defaults to UTF-16.
- **`stats.rs`**: the performance diagnosis behind `ry debug analysis-stats`.
- **`repl_completer.rs`**: completion for the interactive console.

## `repl`

The crate root is `src/repl.rs`. This crate backs `ry repl` and `ry run`, and it finds and loads the
system's R at runtime. That is why the rest of the workspace builds, and analyzes R, on a machine
with no R installed.

- **`repl.rs`**: the session, with evaluation, the read-eval-print loop, and the exit codes.
- **`libr.rs`**: the binding to R's shared library, resolved at runtime with no build-time link.
- **`console.rs`**: the terminal front end.
