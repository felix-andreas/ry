---
title: Architecture
description: How ry builds its analysis stack, from the hand-written parser to the salsa semantics database and the language server
---

This page is the authoritative implementation architecture for ry. The
[typing reference](/reference/type-system) is the authoritative user-facing
typing contract. This page defines the implementation boundaries that realize
that contract.

## Crate graph

The crate graph enforces the layering. A crate boundary is the
compiler-checked form of making an illegal state unrepresentable.

- `syntax` is a hand-written lexer and a recursive-descent parser with Pratt
  expression parsing. It emits [rowan](https://github.com/rust-analyzer/rowan)
  green and red trees. The trees are lossless, so every byte reprints exactly,
  including trivia. Parsing is error-resilient, so every parse yields a tree
  and an error stays local to the break. Parsing is also position-independent:
  a green node stores a width and no absolute offset, so an untouched
  statement's subtree is structurally equal across an edit elsewhere in the
  file. An `#:` annotation is lexed as structured trivia and parsed into
  first-class nodes with real spans. Type syntax is grammar, not scraped
  comment text. This crate depends on nothing semantic.
- `semantics` holds the salsa database and every analysis query: the per-item
  item tree and HIR, naming, interned types, Hindley-Milner inference, the
  stub corpus, the lints, and the diagnostics edge. It depends on `syntax` and
  on salsa.
- `ide` implements the editor features as pure reads of `semantics` queries at
  byte offsets. Those features are hover, navigation, rename, completion,
  inlay hints, signature help, code actions, and symbols. Editor-protocol
  concerns live in the server and never here.
- `format` is the preserving formatter. It depends on `syntax` alone, which
  the compiler enforces, so it can never depend on an analysis result.
- `ry` is the product surface: the LSP server and the CLI, with the `check`,
  `fmt`, `server`, and `debug` commands. It owns configuration,
  position-encoding conversion, diagnostics assembly and publication, and
  suppression comments.

The `*-legacy` crates are the previous stack. They are `roughly-legacy`,
`analysis-legacy`, and `engine-legacy`, and they stay frozen in the tree as
the baseline the performance benchmarks measure against. The two stacks share
no code, by design.

## The semantics database

Analysis is incremental at item granularity, on salsa.

- The database has four inputs. `SourceFile` carries a file's text and
  document kind. `ProjectFiles` is a singleton that lists the project's files,
  package documents first, in workspace-relative path order. That order is the
  one the last-writer-wins symbol index and the CLI agree on.
  `PackageMetadata` is a singleton carrying the imports from `NAMESPACE` and
  the dependency universe from `DESCRIPTION`. `StubSources` is a set-once
  singleton carrying the `.Rtypes` corpus.
- `parse(file)` is a per-file query, accelerated by a splice cache. The
  cache keeps the file's previous text and tree beside the database, shared
  across storage-handle clones. It derives the current edit as the longest
  common-prefix and common-suffix delta, and `syntax::reparse` then reparses
  only the touched statement region, sharing the untouched green subtrees by
  pointer. The spliced result is byte-identical and error-identical to a parse
  from scratch. The edit-stream fuzzer pins that equivalence, and the splice
  refuses and falls back whenever the equivalence is not provable. The cache
  is therefore invisible to every downstream query. Sub-file parse
  invalidation stays out of salsa by design.
- Semantic incrementality happens one level down, in the item tree. An item's
  identity is insertion-stable, because it interns the item's kind, name,
  parent, and a disambiguator. An edit inside one item therefore leaves a
  sibling item's derived values equal, and salsa's early cutoff prunes all
  downstream work.
- Each item flows through four stages. The green subtree lowers to HIR, whose
  spans are relative to the item. Naming then runs the mutable-slot variable
  model with reaching-write flow. The inference walk, `item_check`, produces
  the expression types, the type errors, the strict origins, and the exported
  scheme.
- The package interface resolves a cyclic definition group through one
  canonical fixpoint. The result does not depend on which member is queried
  first. `interface_sccs` condenses the static graph of references from an
  item to its winner, using iterative Tarjan in canonical project order.
  `scc_schemes` runs Jacobi rounds from an all-`Unknown` start until the
  scheme table converges, and its round cap pins every member. `item_check`
  then adopts the canonical scheme as the single exported truth. A member
  check inside the fixpoint never re-enters `item_check`, so no salsa cycle
  forms. Salsa's own cycle recovery remains only as a backstop for a reference
  edge the static graph cannot see.
- A cycle recovery's refusal must not depend on the round that produced it.
  Salsa stops iterating when a recovery returns a value equal to the last one,
  so the value a recovery pins at its round cap has to be reachable again
  unchanged. A pin computed from the freshly recomputed value is not
  reachable again. The parts it leaves alone keep moving with the cycle, every
  round then differs, and salsa runs to its own `MAX_ITERATIONS` and panics.
  Pin a constant, or re-pin what was already returned. Where the pinned type
  is a composite, cut every exported surface, not only the obvious one.
- A whole-project walk reads a per-item projection, not full naming.
  `item_interface_reads` is the set of read names that feeds `interface_sccs`.
  `item_top_level_names` is the set of binding names that feeds
  conditional-slot resolution. Both are small tracked queries, and their
  values survive a body edit that only shifts ranges. The common keystroke
  therefore backdates the projection, and the project-wide graph walks stay
  green instead of re-executing.
- Types are interned, so equality is id equality and nothing deep-clones. Deep
  resolution over the interned type DAG is memoized per binding epoch, and it
  cuts a cycle to `Unknown`. The decision log records that design.

Diagnostics split into two layers over one source of truth.

- `parse_stage_diagnostics(file)` holds the syntax errors, the
  typing-directive errors, and the `#:` block-form refusals. They are pure
  functions of the parse.
- `file_diagnostics(file)` holds the parse-stage set plus the naming findings,
  which are the unresolved and unused reports, and the type errors.
  `strict_diagnostics(file)` renders the strict-mode origins separately, so a
  host publishes them only under `[check] strict` or a per-file directive.

The host gates diagnostic classes by configuration, applies a per-file
`# typing:` override, escalates an unresolved finding to an error under
strict, appends the lints, and applies a `# ry: allow(...)` suppression
comment. The server and the CLI share that assembly in `crates/ry`.

## The language server

The server runs on two threads. The async-lsp frontend on the tokio thread
serializes every request and notification into a job for one dedicated worker
thread. That worker owns the salsa database and all documents, and the
database handle is deliberately confined to it.

- **The latest edit wins.** An input-mutating notification cancels the
  worker's salsa cancellation token before it enqueues the job, so an
  in-flight query unwinds cooperatively. Every later job starts on a fresh
  storage handle, which is a cheap clone, because a cancellation flip is
  consumed by whichever query it killed. A best-effort feature answers empty
  on cancellation. An authoritative answer never degrades. Pull diagnostics
  return a retryable `SERVER_CANCELLED` with `retriggerRequest: true`, and
  rename returns `CONTENT_MODIFIED`.
- **Push happens in two waves.** On every sync the server immediately
  publishes the parse-stage classes and the lints, with a version. At idle
  time it publishes the settled full set, without a version. The settled set
  is a faithful superset, so the second wave only adds findings. A
  pull-capable client suppresses push entirely and receives content-hash
  result ids with unchanged reports.
- **Idle work has an order.** The worker serves an owed settled publish for
  the most recently edited file first. With nothing owed, it warms one
  workspace file at a time, and never publishes that work. Cancelled idle work
  is requeued.
- **A coherence failure panics.** A document-sync failure is unrecoverable,
  and a panic escaping a worker job terminates the process deterministically
  rather than serving corrupted state. An IDE feature lookup, by contrast,
  must never panic.
- A position converts at the protocol edge only, against the target document's
  own text, in the negotiated encoding. The default is UTF-16, and the server
  uses UTF-8 when the client offers it. A signature-help label offset is
  always UTF-16, as the LSP specification requires.
- An `.Rtypes` stub buffer and a `NAMESPACE` file are served standalone,
  without ever entering the database. A stub buffer gets loader-problem
  diagnostics, semantic tokens, and goto for a type name. A `NAMESPACE` file
  gets import validation.

## Correctness and performance instruments

The fixture suites are the correctness contract. The
[testing page](/contributing/testing) describes them. The
cross-implementation parity program that once compared every finding against
the frozen legacy stack is complete and retired. The new stack's fixtures
stand on their own, and no change needs the old implementation to agree.

What remains in `legacy/differential` is the benchmark harness. Its
performance and memory witnesses, in `test_stats`, assert measured budgets
against a corpus of real files. The budgets cover wall time, resident set, and
resolve-step linearity, so a regression in any of the three fails a test
instead of being noticed later. The corpus is fetched on demand, so these
tests run locally rather than in CI.
