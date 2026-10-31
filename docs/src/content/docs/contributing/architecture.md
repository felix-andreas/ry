---
title: Architecture
description: How ry builds its analysis stack, from the hand-written parser to the salsa semantics database and the language server
---

The crate boundaries, the analysis database, and the language server's scheduling described here
are the contract for the implementation. What the type checker *means* is specified separately, by
the [typing reference](/reference/type-system).

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
  so type syntax is grammar rather than comment text scraped after the fact. Inline annotations run
  the same type grammar on R's token stream, in `TYPE_ASCRIPTION`, `RETURN_TYPE`, and a function's
  `TYPE_BINDER_LIST` nodes, and `erase_inline_annotations` gives the plain R program an annotated one
  means. The crate depends on nothing semantic.
- **`semantics`** holds the salsa database and every analysis query: the per-item item tree and HIR,
  naming, interned types, Hindley-Milner inference, the stub corpus, the lints, and the point where
  diagnostics leave the analysis. It depends on `syntax` and salsa.
- **`ide`** implements the editor features (hover, navigation, rename, completion, inlay hints,
  signature help, code actions, and symbols) as pure reads of `semantics` queries at byte offsets.
  Anything to do with the editor protocol lives in the server, never here.
- **`format`** is the formatter. It depends on `syntax` alone, and because the compiler enforces
  that, it can never come to depend on an analysis result.
- **`loader`** is the package-development loader. It reads a package into the analysis, diffs the
  load index against what a session has loaded, and produces a backend-neutral plan. Its GNU R
  backend hands the plan to R code running in the session; ry's own runtime will take the same plan.
- **`repl`** is the R console behind `ry repl` and `ry run`. It loads R at run time, so no crate,
  `repl` included, needs R to build.
- **`heap`** is the R runtime's object model and garbage collector: the `SEXP` object layout,
  allocation, the non-moving generational collector and its roots, `CHARSXP` interning, symbols,
  environment frames and the `R_alloc` arena. It depends on nothing else in the workspace.
- **`libr`** is R's C API, built as the shared library `libR.so` that compiled packages link
  against, with the C half of the native boundary and an R 4.6-compatible header set. It depends on
  `heap`. Its design is the draft at
  [`contributing/design/r-runtime.md`](/contributing/design/r-runtime/), and nothing else in the
  workspace uses it yet.
- **`ry`** is the product: the LSP server and the CLI, with the `check`, `fmt`, `server`, `repl`,
  `run`, `load`, and `debug` commands. It owns configuration, conversion between position encodings,
  assembling and publishing diagnostics, and suppression comments.

## The semantics database

Analysis runs on salsa and is incremental at the granularity of an *item*: one top-level statement
of a file, such as a function definition, a value binding, or a bare call.

### Inputs

The analysis of a project has five inputs:

- `SourceFile` carries one file's text and document kind.
- `ProjectFiles` is a singleton listing the project's files: package documents first, in the
  `DESCRIPTION` `Collate` order when one is declared and by path otherwise, then the rest by path.
  The last-writer-wins symbol index and the CLI both rely on that order.
- `PackageMetadata` is a singleton carrying the imports from `NAMESPACE` and the dependency universe
  from `DESCRIPTION`.
- `StubSources` is a set-once singleton carrying the `.Rtypes` corpus.
- `AnalysisSettings` is a singleton carrying the configuration that changes what analysis means
  rather than which findings a host publishes, today only `[check] inline-annotations`. It is read
  only where an inline annotation exists, so toggling it re-checks only the items that have one.

The package loader adds a sixth, `PackageSources`, described [below](#the-package-load-index).

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

An item's green subtree lowers to HIR, with spans relative to the item. Its annotations lower
separately: a `#:` block and an inline annotation go through the same type lowering into the same
`Annotation`, and a statement annotated both ways gets one merged annotation
(`annotations::merge_annotations`), so the checker never knows which spelling it was given. Naming
then runs the variable model, in which a variable is a mutable slot with reaching-write flow.
Finally, the inference walk, `item_check`, produces the expression types, the type errors, the
strict-mode origins, and the scheme the item exports.

**An inference variable must never leave the item that created it.** A variable is an index into one
item's inference table, so nothing that outlives that table may hold one. An exported scheme is
closed at the item boundary by `close_scheme`, and a value cached across a rollback of the table has
its variables erased first. A leaked id is not merely a crash risk: in a reader's table it either
points past the end, which panics, or it silently names some unrelated variable of the reader's own,
which is worse.

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

A check never reads a project-wide map either. Each lookup it makes across items goes through a
firewall keyed by one name or one item: `package_name` gives a name's winner and its conditional
writers, `interface_group` gives an item's cyclic group, and `frame_binder` gives the file-local
binder that an immediate or script read sees. A structural edit re-runs each firewall that a check
read, and an unchanged answer is backdated, so a new definition re-checks only the items that read
its name. `crates/semantics/tests/test_incremental.rs` pins these contracts.

Types are interned, so type equality is id equality and nothing is ever deep-cloned. Deep resolution
over the interned type graph is memoized per binding epoch and cuts any cycle to `Unknown`; the
decision log records that design.

### The package-load index

`ry load` reads a package through its own input, `PackageSources`: the `R/` files in collation
order plus the `NAMESPACE` and `DESCRIPTION` text, as separate fields so a `NAMESPACE` edit never
re-derives an item. The index has three layers, and each depends only on the one below:

- `item_load` gives one item's content hash (of the text R evaluates, with inline annotations
  erased), whether it is a function definition, and the S4, R6 and S3 registrations it makes. It
  reads only `item_syntax`, so it is the per-item firewall. `item_immediate_reads` gives the names a
  statement reads while it runs, which excludes reads inside the functions it defines.
- `file_items` joins one file's items with their positions in the erased text, which keeps every
  line break, so line numbers match the file on disk.
- `package_index` joins the files and decides which names are bound lazily: a name whose one writer
  is a function definition.

A reload depends on all of a package's files, so salsa would revisit every file's memos after any
edit. The loader avoids that with durability: a file starts at high durability and drops to low on
its first edit, and salsa skips verifying memos that depend only on inputs at a durability nothing
changed at. The diff then looks inside only the files whose hash changed, plus the eager code of
every file, which is all an edit can reach. `test_incremental.rs` pins the per-item contract.

### Diagnostics

Diagnostics come in two layers over one source of truth:

- `parse_stage_diagnostics(file)` holds the syntax errors, the typing-directive errors, the
  refusals of malformed `#:` blocks and inline annotations, and, while inline annotations are turned
  off, a warning at each one. All of them are pure functions of the parse and `AnalysisSettings`.
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

The fixture suites are the correctness contract. `crates/semantics/tests/test_incremental.rs` pins
the incrementality contracts by counting query executions after an edit, and
`crates/semantics/tests/test_perf.rs` asserts wall-time, memory, and keystroke-latency budgets
against a corpus of real files. [Testing](/contributing/testing) documents all of them.
