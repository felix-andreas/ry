---
title: Architecture
description: How ry builds its analysis stack, from the hand-written parser to the salsa semantics database and the language server
---

This page describes how ry is built, and it is the contract for the implementation: the crate
boundaries, the analysis database, and the language server's scheduling. What the type checker
*means* is specified separately, by the [typing reference](/reference/type-system); this page covers
the machinery that delivers it.

## Crate graph

Layering is enforced by the crate graph. A crate boundary is the compiler-checked way of making an
illegal dependency unrepresentable.

- **`syntax`** is a hand-written lexer and a recursive-descent parser with Pratt parsing for
  expressions, emitting [rowan](https://github.com/rust-analyzer/rowan) green and red trees. Three
  properties matter downstream:
  - The trees are *lossless*: every byte, trivia included, reprints exactly.
  - Parsing is *error-resilient*: every parse yields a tree, and an error stays local to the place
    where the code broke.
  - Parsing is *position-independent*: a green node stores its width but no absolute offset, so an
    untouched statement's subtree stays structurally equal when something elsewhere in the file
    changes.

  A `#:` annotation is lexed as structured trivia and parsed into first-class nodes with real spans,
  so type syntax is grammar rather than comment text scraped after the fact. The crate depends on
  nothing semantic.
- **`semantics`** holds the salsa database and every analysis query: the per-item item tree and HIR,
  naming, interned types, Hindley-Milner inference, the stub corpus, the lints, and the point where
  diagnostics leave the analysis. It depends on `syntax` and salsa.
- **`ide`** implements the editor features (hover, navigation, rename, completion, inlay hints,
  signature help, code actions, and symbols) as pure reads of `semantics` queries at byte offsets.
  Anything to do with the editor protocol lives in the server, never here.
- **`format`** is the formatter. It depends on `syntax` alone, and because the compiler enforces
  that, it can never come to depend on an analysis result.
- **`ry`** is the product: the LSP server and the CLI, with the `check`, `fmt`, `server`, and `debug`
  commands. It owns configuration, conversion between position encodings, assembling and publishing
  diagnostics, and suppression comments.

The `*-legacy` crates (`roughly-legacy`, `analysis-legacy`, and `engine-legacy`) are the previous
stack. They stay frozen in the tree as the baseline the performance benchmarks measure against, and
by design the two stacks share no code.

## The semantics database

Analysis runs on salsa and is incremental at the granularity of an *item*: one top-level statement
of a file, such as a function definition, a value binding, or a bare call.

### Inputs

The database has four inputs:

- `SourceFile` carries one file's text and document kind.
- `ProjectFiles` is a singleton listing the project's files: package documents first, then in
  workspace-relative path order. The last-writer-wins symbol index and the CLI both rely on that
  order.
- `PackageMetadata` is a singleton carrying the imports from `NAMESPACE` and the dependency universe
  from `DESCRIPTION`.
- `StubSources` is a set-once singleton carrying the `.Rtypes` corpus.

### Parsing, and why it is not where incrementality happens

`parse(file)` is a per-file query with a splice cache beside it. The cache remembers the file's
previous text and tree (shared across clones of the storage handle), works out the current edit as
the longest common prefix and suffix, and lets `syntax::reparse` reparse only the statements the
edit touched, sharing every untouched green subtree by pointer.

The spliced tree is byte-identical and error-identical to a parse from scratch. The edit-stream
fuzzer pins that equivalence, and the splice refuses and falls back to a full parse whenever the
equivalence is not provable, so the cache is invisible to every query downstream. Invalidation below
the level of a whole file stays out of salsa on purpose.

The real incrementality happens one level down, in the item tree. An item's identity interns its
kind, its name, its parent, and a disambiguator, which makes it stable when other items are
inserted around it. An edit inside one item therefore leaves the derived values of its siblings
equal, and salsa's early cutoff prunes all the work downstream of them.

### From item to scheme

Each item goes through four stages. Its green subtree lowers to HIR, with spans relative to the item.
Naming then runs the variable model, in which a variable is a mutable slot with reaching-write flow.
Finally, the inference walk, `item_check`, produces the expression types, the type errors, the
strict-mode origins, and the scheme the item exports.

**An inference variable must never leave the item that created it.** A variable is an index into one
item's inference table, so nothing that outlives that table may hold one. An exported scheme is
closed at the item boundary by `close_scheme`, and a value cached across a rollback of the table has
its variables erased first. A leaked id is worse than a crash. In a reader's table it either points
past the end, or it silently names some unrelated variable of the reader's own.

To keep that from happening by accident, every transformation that rebuilds a type (resolution,
substitution, generalization, erasure) goes through one structural map, `map_child_types`. That way
no walk can handle a member, such as a parameter's default, that another walk forgets. Reads go
through the matching `for_each_child_type` and `Parameter::types`.

### Cyclic definitions

When definitions refer to each other in a cycle, the package interface resolves the whole group
through one canonical fixpoint, so the result never depends on which member happened to be queried
first:

1. `interface_sccs` condenses the static graph of references from each item to the definition it
   resolves to, using an iterative Tarjan walk in canonical project order.
2. `scc_schemes` starts every member at `Unknown` and runs Jacobi rounds until the table of schemes
   stops changing. If the round cap is reached, every member is pinned.
3. `item_check` adopts the canonical scheme as the one exported truth.

A member check inside the fixpoint never re-enters `item_check`, so no salsa cycle forms. Salsa's own
cycle recovery remains only as a backstop, for a reference the static graph cannot see.

That backstop has a trap of its own: **a cycle recovery must not pin a value that depends on the
round that produced it.** Salsa stops iterating when a recovery returns the same value as last time,
so the value pinned at the round cap has to come back unchanged. A pin computed from the freshly
recomputed value does not: the parts it leaves alone keep moving with the cycle, every round
differs, and salsa runs into its own `MAX_ITERATIONS` and panics. Pin a constant, or re-pin what was
already returned, and when the pinned type is a composite, cut every surface it exports, not just
the obvious one.

### Keeping project-wide work cheap

A walk over the whole project reads small per-item projections instead of full naming results.
`item_interface_reads` is the set of names an item reads, which feeds `interface_sccs`, and
`item_top_level_names` is the set of names it binds, which feeds the resolution of conditional
slots. Both are tiny tracked queries whose values survive an edit that only shifts ranges. The
typical keystroke therefore leaves them unchanged, and the project-wide graph walks stay green
instead of running again.

Types are interned, so type equality is id equality and nothing is ever deep-cloned. Deep resolution
over the interned type graph is memoized per binding epoch and cuts any cycle to `Unknown`; the
decision log records that design.

### Diagnostics

Diagnostics come in two layers over one source of truth:

- `parse_stage_diagnostics(file)` holds the syntax errors, the typing-directive errors, and the
  refusals of malformed `#:` blocks. All of them are pure functions of the parse.
- `file_diagnostics(file)` adds the naming findings (unresolved and unused names) and the type
  errors. The strict-mode origins are rendered separately by `strict_diagnostics(file)`, so a host
  publishes them only under `[check] strict` or a per-file directive.

The host, meaning the server or the CLI, then gates diagnostic classes by configuration, applies
per-file `# typing:` overrides, escalates unresolved names to errors under strict mode, appends the
lints, and honors `# ry: allow(...)` suppression comments. The server and the CLI share this
assembly in `crates/ry`, so they cannot disagree.

## The language server

The server runs on two threads. The async-lsp frontend, on the tokio thread, turns every request and
notification into a job for a single dedicated worker thread. The worker owns the salsa database
and all the documents, and the database handle deliberately never leaves it.

- **The latest edit wins.** A notification that changes an input first cancels the worker's salsa
  cancellation token and then enqueues its job, so a query in flight unwinds cooperatively. Every
  later job starts on a fresh, cheaply cloned storage handle, because a cancellation is consumed by
  whichever query it killed. A best-effort feature answers with nothing when cancelled, but an
  authoritative answer never degrades: pull diagnostics return a retryable `SERVER_CANCELLED` with
  `retriggerRequest: true`, and rename returns `CONTENT_MODIFIED`.
- **Diagnostics are pushed in two waves.** On every sync the server immediately publishes the
  parse-stage classes and the lints, tagged with the document version. At idle time it publishes the
  settled full set, without a version. The settled set is a faithful superset of the first, so the
  second wave only ever adds findings. A client that supports pull diagnostics gets no pushes at
  all, and instead receives result ids derived from a content hash, with unchanged reports.
- **Idle work has an order.** The worker first serves any settled publish it owes for the most
  recently edited file. With nothing owed, it warms up the rest of the workspace one file at a time
  and never publishes that work. Idle work that gets cancelled is requeued.
- **A coherence failure panics.** A failed document sync is unrecoverable, and a panic that escapes a
  worker job terminates the process deterministically rather than going on to serve corrupted state.
  An IDE feature lookup, by contrast, must never panic.
- **Positions convert only at the protocol edge**, against the target document's own text, in the
  negotiated encoding. That is UTF-16 by default, and UTF-8 when the client offers it. Signature-help
  label offsets are always UTF-16, as the LSP specification requires.
- **Stub buffers and `NAMESPACE` files are served standalone** and never enter the database. An
  `.Rtypes` buffer gets loader-problem diagnostics, semantic tokens, and goto-definition for type
  names, and a `NAMESPACE` file gets import validation.

## Correctness and performance instruments

The fixture suites, described on the [testing page](/contributing/testing), are the correctness
contract. The parity program that once compared every finding against the frozen legacy stack is
complete and retired, so the new stack's fixtures stand on their own and no change needs the old
implementation to agree.

What remains in `legacy/differential` is the benchmark harness. Its performance and memory
witnesses, in `test_stats`, assert measured budgets for wall time, resident memory, and the
linearity of resolve steps against a corpus of real files, so a regression in any of the three
fails a test instead of being noticed months later. The corpus is fetched on demand, so these tests
run locally rather than in CI.
