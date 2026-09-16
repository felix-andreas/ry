---
title: Architecture
description: Implementation architecture of ry's analysis stack — the hand-written parser, the salsa semantics database, and the language server
---

This document is the authoritative implementation architecture for ry. The
[Typing Reference](/reference/type-system) is the authoritative user-facing typing
contract; this page defines the implementation boundaries that realize it.

## Crate graph

The crate graph enforces the layering — crate boundaries are the
compiler-checked analog of "make illegal states unrepresentable":

- **`syntax`** — a hand-written lexer and recursive-descent/Pratt parser
  emitting [rowan](https://github.com/rust-analyzer/rowan) green/red trees:
  lossless (every byte, including trivia, reprints exactly), error-resilient
  (every parse yields a tree; errors are local to the break), and
  position-independent (width-only green nodes make an untouched statement's
  subtree structurally equal across edits elsewhere in the file). `#:`
  annotations are lexed as structured trivia and parsed as **first-class
  nodes** with real spans — type syntax is grammar, not comment-scraping.
  Depends on nothing semantic.
- **`semantics`** — the salsa database and every analysis query: the per-item
  item tree and HIR, naming, interned types, Hindley–Milner inference, the
  stub corpus, lints, and the diagnostics edge. Depends on `syntax` and salsa.
- **`ide`** — editor features (hover, navigation, rename, completion, inlay
  hints, signature help, code actions, symbols) as pure reads of `semantics`
  queries at byte offsets. Editor-protocol concerns live in the server, never
  here.
- **`format`** — the preserving formatter, on `syntax` only
  (compiler-enforced: it can never depend on analysis results).
- **`ry`** — the product surface: the LSP server and the CLI
  (`check`, `fmt`, `server`, `debug`). Owns configuration, position-encoding
  conversion, diagnostics assembly and publication, and suppression comments.

The `*-legacy` crates (`ry-legacy`, `analysis-legacy`, `engine-legacy`)
are the previous stack, frozen in-tree as the baseline the performance
benchmarks measure against; no code is shared between the two stacks by
design.

## The semantics database

Analysis is incremental at **item** granularity on salsa:

- `SourceFile` is a salsa input (text + document kind); `ProjectFiles` is a
  singleton input listing the project's files, package documents first in
  workspace-relative path order (the order the last-writer-wins symbol index
  and the CLI agree on). `StubSources` is a set-once singleton carrying the
  `.Rtypes` corpus.
- `parse(file)` is a per-file query accelerated by a **splice cache**: the
  file's previous text and tree are kept beside the database (shared across
  storage-handle clones), the current edit is derived as the longest
  common-prefix/suffix delta, and `syntax::reparse` reparses only the touched
  statement region, sharing the untouched green subtrees by pointer. The
  spliced result is byte- and error-identical to a from-scratch parse (the
  edit-stream fuzzer pins the equivalence; the splice refuses and falls back
  whenever that is not provable), so the cache is invisible to every
  downstream query — sub-file parse *invalidation* stays out of salsa by
  design. Semantic incrementality happens one level down: the **item tree**
  assigns insertion-stable identities (kind + name + parent + disambiguator,
  interned) so an edit in one item leaves sibling items' derived values equal,
  and salsa's early cutoff prunes all downstream work.
- Per item: green subtree → HIR (with item-relative spans) → naming (the
  mutable-slot variable model with reaching-write flow) → the inference walk
  (`item_check`) producing expression types, type errors, strict origins, and
  the exported scheme.
- **An inference variable is an index into one item's table**, so nothing that
  outlives that table may carry one: an exported scheme closes at the item
  boundary (`close_scheme`), and a value cached across a table rollback is
  variable-erased first. A leaked id is not merely a crash — in a reader's
  table the index either dangles or names an unrelated variable of the
  reader's own, and the second failure is silent. Every transformation that
  rewrites a type (resolution, substitution, generalization, erasure) goes
  through the one structural map, `map_child_types`, so a member such as a
  parameter's *default* cannot be honored by one walk and skipped by another;
  its reading counterparts are `for_each_child_type` and `Parameter::types`.
- The **package interface** resolves cyclic definition groups through one
  canonical fixpoint, independent of which member is queried first:
  `interface_sccs` condenses the static item-to-winner reference graph
  (iterative Tarjan, canonical project order), `scc_schemes` runs Jacobi
  rounds from all-`Unknown` until the scheme table converges (the round cap
  pins **all** members), and `item_check` adopts the canonical scheme as the
  single exported truth. Member checks inside the fixpoint never re-enter
  `item_check`, so no salsa cycle forms; salsa's own cycle recovery remains
  only as a backstop for reference edges the static graph cannot see.
- **A cycle recovery's refusal must not depend on the round that produced it.**
  Salsa stops iterating when a recovery returns a value equal to the last one,
  so the value a recovery pins at its round cap has to be reachable again
  unchanged. A pin computed from the freshly recomputed value is not: the parts
  it leaves alone keep moving with the cycle, every round differs, and salsa
  runs to its own `MAX_ITERATIONS` and panics. Pin a constant, or re-pin what
  was already returned — and where the pinned type is a composite, cut *every*
  exported surface, not just the obvious one.
- Whole-project walks read **per-item projections**, not full naming:
  `item_interface_reads` (the read-name set feeding `interface_sccs`) and
  `item_top_level_names` (the binding names feeding conditional-slot
  resolution) are small tracked queries whose values survive body edits that
  only shift ranges — the common keystroke backdates the projection and the
  project-wide graph walks stay green instead of re-executing.
- Types are **interned** (id equality, no deep clones). Deep resolution over
  the interned type DAG is memoized per binding epoch with
  cycle-cut-to-`Unknown` semantics; the decision log records the design.

Diagnostics split into two layers with one source of truth:

- `parse_stage_diagnostics(file)` — syntax errors, typing-directive errors,
  and `#:` block-form refusals: pure functions of the parse.
- `file_diagnostics(file)` — the parse-stage set plus naming findings
  (unresolved, unused) and type errors; `strict_diagnostics(file)` renders
  strict-mode origins separately so hosts publish them only under
  `[check] strict` or a per-file directive.

The host (server and CLI share the assembly in `crates/ry`) gates classes
by configuration, applies per-file `# typing:` overrides, escalates unresolved
findings to errors under strict, appends lints, and applies
`# ry: allow(...)` suppression comments.

## The language server

Two threads. The async-lsp frontend on the tokio thread serializes every
request and notification into jobs for one dedicated worker thread that owns
the salsa database and all documents (the database handle is deliberately
confined to that thread).

- **Latest-edit-wins cancellation:** an input-mutating notification cancels
  the worker's salsa cancellation token *before* enqueueing, so an in-flight
  query unwinds cooperatively; every subsequent job starts on a fresh storage
  handle (a cheap clone), since a flip is consumed by whichever query it
  killed. Best-effort features answer empty on cancellation; authoritative
  answers do not degrade — pull diagnostics return a retryable
  `SERVER_CANCELLED` (`retriggerRequest: true`) and rename returns
  `CONTENT_MODIFIED`.
- **Two-wave push:** on every sync the server publishes the parse-stage
  classes plus lints immediately (versioned), then the settled full set
  version-less at idle time — a faithful superset, so the second wave only
  adds findings. Pull-capable clients suppress push entirely and get
  content-hash result ids with unchanged reports.
- **Idle work:** owed settled publishes are served most-recently-edited
  first; with nothing owed, the worker warms one workspace file at a time
  (never published). Cancelled idle work is requeued.
- **Coherence failures panic.** Document-sync failures are unrecoverable; a
  panic escaping a worker job terminates the process deterministically rather
  than serving corrupted state. IDE feature lookups, by contrast, must never
  panic.
- Positions convert at the protocol edge only, against the target document's
  own text, in the negotiated encoding (UTF-16 by default, UTF-8 when
  offered). Signature-help label offsets are always UTF-16 per the LSP spec.
- `.Rtypes` stub buffers and `NAMESPACE` files are served standalone —
  loader-problem diagnostics, semantic tokens, and type-name goto for stubs;
  import validation for NAMESPACE — without ever entering the database.

## Correctness and performance instruments

The fixture suites are the correctness contract; see the [testing
page](/contributing/testing). The cross-implementation parity program that once compared
every finding against the frozen legacy stack is complete and retired — the
new stack's fixtures stand on their own, and no change needs the old
implementation's agreement.

What remains in `legacy/differential` is the benchmark harness. Its perf and
memory witnesses (`test_stats`) assert measured budgets — wall time, resident
set, and resolve-step linearity — against a real-file corpus, so a regression
in any of the three fails a test rather than being noticed later. The corpus
is fetched on demand, so these run locally rather than in CI.
