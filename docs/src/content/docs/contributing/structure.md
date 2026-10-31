---
title: Structure
description: What each source file holds in the syntax, semantics, ide, format, loader, ry, repl, heap, and libr crates
---

This page lists what each source file in ry's crates is responsible for. How the crates depend on
each other, and where one phase hands over to the next, is in [Architecture](/contributing/architecture).

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
  the `RY_BLESS` and `FIXTURE_FILTER` variables, the rejection of duplicate case ids, and the
  `#~~~~` separator that splits one case into several files. It also reads the program corpus in
  `tests/programs/`, about 2,000 R edge cases that run through the invariant batteries.

## `semantics`

The crate root is `src/semantics.rs`.

- **`semantics.rs`**: the salsa database. It declares three of the database inputs, `SourceFile`,
  `ProjectFiles` and `AnalysisSettings` (the others, `PackageMetadata`, `StubSources` and
  `PackageSources`, live with the modules that own them). It also holds the item tree and its
  insertion-stable identities, the anchoring of each item to its syntax, the package interface
  computed by the `scc_schemes` fixpoint, and the item-span queries.
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
  resolves imports and dependencies, and it reads the full `NAMESPACE` directive set a loader
  honors.
- **`load_index.rs`**: the package-load index behind `ry load`, with its own input,
  `PackageSources`. Per item it derives a content hash, whether the item is a function definition,
  and the registrations it makes; per file, the text R reads (inline annotations erased) and every
  item's position; per package, which names are bound lazily.
- **`lints.rs`**: the style lints and their configuration types.
- **`testing.rs`**: test support that other crates share. It holds the invariant battery for the
  semantic pipeline, which the fuzz harness and the coverage-guided targets run, the shared fixture
  database, and `ProbeDatabase`, which logs query executions for the incrementality tests.
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

## `loader`

The crate root is `src/loader.rs`.

- **`loader.rs`**: the session that reads a package's sources into the analysis, and the diff that
  turns the load index into a backend-neutral `Plan` of what to bind, run, remove, and register.
- **`gnur.rs`**: the GNU R backend. It encodes a plan as an RDS file.
- **`runtime.R`**: the R side of the GNU R backend, which applies a plan to a session and provides
  `load_all()`.

## `ry`

The library root is `src/ry.rs`, and the binary root is `src/main.rs`.

- **`main.rs`**: the CLI surface and the [exit-code contract](/reference/cli#exit-codes).
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
- **`load.rs`**: package loading in R sessions: the `.Call` routines and file watcher behind
  `ry repl`, the plan emitter behind `ry load --emit-plan`, and the R code that installs the
  runtime, byte-compiled once and cached.

## `repl`

The crate root is `src/repl.rs`. This crate backs `ry repl` and `ry run`, and it finds and loads the
system's R at runtime. That is why the rest of the workspace builds, and analyzes R, on a machine
with no R installed.

- **`repl.rs`**: the session, with evaluation, the read-eval-print loop, and the exit codes.
- **`libr.rs`**: the binding to R's shared library, resolved at runtime with no build-time link,
  including the registration of `.Call` routines that live in the ry process.
- **`console.rs`**: the terminal front end.

## `heap`

The crate root is `src/heap.rs`. It holds R's object model and the garbage collector, and knows
nothing about C.

- **`heap.rs`**: `Heap`, with allocation, the barriered stores, environment bindings, the roots
  (the `PROTECT` stack, the precious multiset, the handle stack, globals and `RootSource`), and the
  collector itself: marking, ephemeron processing for weak references, and the young and full sweeps.
- **`object.rs`**: `Sexp`, the type codes, the header and payload layouts, and the raw accessors.
- **`strings.rs`**: the weak `CHARSXP` intern table and the encodings.
- **`env.rs`**: environment frames, which map symbols to binding cells.
- **`arena.rs`**: the `R_alloc` arena and its `vmaxget` watermarks.

## `libr`

The crate root is `src/libr.rs`, and the crate builds `libR.so` as well as an rlib. It holds the
runtime state and R's C API. Each exported function that can fail runs its body through
`boundary::enter`.

- **`libr.rs`**: the process-wide `Runtime` and its start-up.
- **`boundary.rs`**: the native boundary: `Unwind`, `enter` and `raise`, the trampolines that call
  native code, the handle scope, finalizer runs, and the Rust half of `native.c`.
- **`native.c`**: the C half: the context stack, the trampolines, `ry_jump`, the variadic functions,
  and the non-local exits native code sets up (`R_UnwindProtect`, `R_ToplevelExec`,
  `R_ExecWithCleanup`, `R_tryCatch`). `build.rs` exports what it marks `RY_EXPORT`.
- **`memory.rs`**: allocation, duplication, `PROTECT`, the precious list and multisets, `R_alloc`
  and the `R_Calloc` family.
- **`vectors.rs`**: vector access, the type predicates, coercion and number formatting, `identical`
  and `match`.
- **`lists.rs`**: pairlists, calls, closures and promises.
- **`attributes.rs`**: attributes, classes and the header bits.
- **`strings.rs`**: `CHARSXP`s, encodings, symbols and `Riconv`.
- **`environments.rs`**: environments, variable lookup, bindings, namespaces and options.
- **`eval.rs`**: `Rf_eval` over its small primitive table, conditions and the deparser for messages.
- **`dynload.rs`**: loading package libraries, routine registration and C callables.
- **`extptr.rs`**: external pointers, weak references and finalizers.
- **`altrep.rs`**: ALTREP classes and dispatch.
- **`arith.rs`**: NA tests, sorting, and the random number generator.
- **`globals.rs`**: GNU R's data symbols.
- **`embed.rs`**: ry's own `ry_` entry points for an embedding host, declared in
  `include/ry_embed.h`.
- **`include/`**: the header set, written independently and source-compatible with R 4.6 for the
  names `libR` implements.
