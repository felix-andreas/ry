---
title: Structure
description: What each source file holds in the syntax, semantics, ide, format, loader, oracle, ry, repl, heap, interp, jit, libr, sniff, table, and rcmd crates
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
- **`native_values.rs`**: the facts about ry's own records, tuples, tags, dictionaries and sets,
  and the R functions that accept them, which the checker and the runtime both read so the two
  cannot drift apart.
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
  computed by the `scc_exports` fixpoint, and the item-span queries.
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
- **`exhaustiveness.rs`**: whether the arms of a `match` cover the scrutinee's type, by Maranget's
  usefulness algorithm.
- **`top_level.rs`**: what a package file's top level runs when the package loads, and which
  functions are declared `@pure`, which strict mode's definitions-only rule and the lazy constants
  of `ry load` both read.
- **`sorted.rs`**: read-only sorted maps and sets for memoized results.
- **`lints.rs`**: the style lints and their configuration types.
- **`testing.rs`**: test support that other crates share. It holds the invariant battery for the
  semantic pipeline, which the fuzz harness and the coverage-guided targets run, the shared fixture
  database, and `ProbeDatabase`, which logs query executions for the incrementality tests.
- **`diagnostics.rs`**: where diagnostics leave the crate. It produces the parse-stage set and the
  full per-file set, renders strict output, and holds `TypeRenderer`, the one place types are turned
  into user-facing text.

## `ide`

The crate root is `src/ide.rs`, and `src/testing.rs` holds the editor features' fuzz contract,
which the in-tree harness and the coverage-guided target share. Every editor feature in `ide.rs` is a read of `semantics`
queries: hover, goto-definition, references, rename, call hierarchy, inlay hints (including
parameter names), signature help, completion, code actions with their fixes (`diagnostic_fixes`,
`fix_all`), the outline, workspace symbols, folding, selection ranges, semantic tokens, and
navigation to annotation types and S4 definitions.
Definition, references, and rename share one occurrence engine, and completion uses the shared
smart-case matcher.

## `format`

The whole crate is one file, `src/format.rs`: the formatter, which works on the syntax tree and
preserves your line breaks, together with its configuration types.

## `loader`

The crate root is `src/loader.rs`.

- **`loader.rs`**: the session that reads a package's sources into the analysis, and the diff that
  turns the load index into a backend-neutral `Plan` of what to bind, run, remove, and register.
- **`runtime.R`**: the R code both backends share, which applies a plan to a session and provides
  `load_all()`. **`r.rs`** turns a plan into an R list, encoding it as an RDS file for GNU R, and
  **`gnur.R`** and **`ry.R`** are the hooks of the two runtimes.
- **`ry_runtime.rs`** (feature `runtime`): the backend for ry's own runtime, where the plan never
  leaves the process and a lazy definition is a promise over the analysis's syntax tree.
- **`testing.rs`**: the fuzz contracts for reloads and the load index, shared by the in-tree
  harness and the coverage-guided targets.

## `oracle`

The library root is `src/oracle.rs`, and the binary root is `src/main.rs`. It runs one R file under
GNU R and under a second engine and compares stdout, messages, warnings, errors and the last value;
`driver.R` is the script that produces each engine's report. `fuzz.rs` generates the programs for
differential fuzzing, `packages.rs` runs the corpus packages' own tests on both runtimes,
`install.rs` installs them from source on ry, and `bench.rs` compares the generated benchmark
packages' tests test by test. [Corpus, oracle and benchmark generator](/contributing/corpus-and-oracle)
describes how to run it.

## `ry`

The library root is `src/ry.rs`, and the binary root is `src/main.rs`.

- **`main.rs`**: the dispatch of every subcommand and the [exit-code contract](/reference/cli#exit-codes).
  **`command_line.rs`** is the clap model, and resolves `ry run`, `ry R` and the `ry repl` alias to
  one session shape.
- **`preflight.rs`**: the check that runs before ry executes R code. It prints the inline-annotation
  warnings and enforces strict mode, and leaves everything else to `ry check`.
- **`cli.rs`**: the implementations of `check`, `fmt`, and `ry debug ast`. `check` assembles the
  project, renders the output, and reports `NAMESPACE` problems and stub overrides.
- **`server.rs`**: the LSP server, all of it: the frontend and worker threads, document sync, push
  and pull diagnostics, every feature endpoint, semantic tokens, and the stub and `NAMESPACE`
  buffers. **`server/simulation.rs`** runs that same server under a seeded scheduler
  ([Robustness](/contributing/robustness#the-language-server-simulator)).
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
  the `ry R` console, the plan emitter behind the hidden `ry load --emit-plan`, and the R code that installs the
  runtime, byte-compiled once and cached.

## `repl`

The crate root is `src/repl.rs`. This crate backs `ry run` and `ry R`, and it finds and loads the
system's R, or ry's runtime library, at runtime. That is why the rest of the workspace builds, and analyzes R, on a machine
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
- **`space.rs`**: the small-object space: one reserved address range cut into 64 KiB pages of one
  size class each, with the mark, allocated and finalize bits in per-page bitmaps outside the
  objects, and the platform's page reservation (`mmap` or `VirtualAlloc`).
- **`records.rs`**: the tag-name and layout interners behind records, tuples, tags, dictionaries
  and sets.
- **`strings.rs`**: the weak `CHARSXP` intern table and the encodings.
- **`env.rs`**: environment frames, which map symbols to binding cells.
- **`arena.rs`**: the `R_alloc` arena and its `vmaxget` watermarks.

## `interp`

The crate root is `src/interp.rs`. It is the R interpreter: it evaluates R code on `heap`'s
objects and implements base R's primitives and internals, and the default packages' native
routines, in Rust. It knows nothing about C; `libr` installs it and supplies the native-code
builtins. The root holds the interpreter state (`Interp`, the frame stack, the host `Hooks`).

- **`eval.rs`**: the AST evaluator: variable lookup, promises, calls and argument matching,
  closures and `on.exit`, the control-flow specials, `substitute` and complex assignment.
- **`bytecode.rs`**: the tier-0 compiler and register machine, its guards, inline caches and type
  feedback.
- **`unwind.rs`**: `Unwind` and `Jump`, the non-local exits as Rust errors.
- **`conditions.rs`**: condition signalling, handler and restart stacks, the default error and
  warning handlers, and the traceback.
- **`dispatch.rs`**: S3 dispatch (`UseMethod`, `NextMethod`, group and internal generics) and the
  hooks S4 dispatch uses. **`methods.rs`**: the methods package's native routines and S4 dispatch.
- **`envir.rs`**: environments, the `sys.*` functions, options, namespaces and the apply family.
- **`builtins.rs`**: the builtin registry (`BuiltinDef`, `Call`) and argument matching for
  builtins. **`values.rs`**: object constructors and accessors the rest of the crate shares, and
  `Globals`.
- **`lower.rs`**: R source to R language objects, through `syntax`'s parser.
- **`deparse.rs`**, **`format.rs`**, **`print.rs`**: deparsing, number formatting and printing.
- **`arith.rs`**, **`coerce.rs`**, **`subset.rs`**, **`strings.rs`**, **`sort.rs`**: base's
  vector internals: arithmetic and math, coercion, subsetting and assignment, strings and regular
  expressions, sorting and matching.
- **`linalg.rs`**: BLAS and LAPACK (opened at runtime) and base's LINPACK routines.
  **`distributions.rs`**: the probability distributions. **`random.rs`**: the random number
  generators.
- **`connections.rs`**, **`serialize.rs`**, **`system.rs`**: the connection table (the `Rconn`
  struct native code sees, the built-in connection kinds, custom connections, sinks and `cat`),
  serialization and the lazy-load database, and the operating system (files, time, the
  environment).
- **`native.rs`**: the foreign-function builtins for a session without libR, and base's routines.
  **`builtin_libraries.rs`**: the default packages' routine tables, which `dyn.load` hands out
  instead of loading their shared libraries; **`utils.rs`**, **`tools.rs`** and **`graphics.rs`**
  implement them.
- **`platform.rs`**: every operating-system call the interpreter makes, with a Unix and a Windows
  implementation of the same functions, so a libc use outside it is a Windows build error.
  **`gnu_r_home.rs`**: where a GNU R installation lives when `R_HOME` and `R RHOME` name none.
- **`graphics_engine.rs`**: R's graphics engine, with the device table, the graphics systems
  registered with every device, and the `DevDesc` layout that device packages fill in.
- **`session.rs`**: start-up (base R from the GNU R installation, the profiles, the default
  packages) and running a script.
- **`stats.rs`** and its `stats_*.rs` siblings (`stats_models`, `stats_optimize`, `stats_nlm`,
  `stats_lbfgsb`, `stats_port`, `stats_arima`, `stats_cluster`, `stats_deriv`, `stats_tests`): the
  stats package's native routines. **`parallel.rs`** holds the parallel package's, **`rd.rs`** the
  Rd parser behind `tools::parse_Rd`, and **`graphics_render.rs`** what a graphics device draws
  and the image files it writes.
- **`annotations.rs`**: the run-time checks that [inline annotations](/type-checking/inline-annotations#running-annotated-code)
  become at a function's boundary.
- **`records.rs`**: records, tuples, tags and `match` at run time. **`arrays.rs`**: `Array<T>`, an R
  vector with the heap's Array bit set, and the strict rules that go with it.
- **`dataframe.rs`**: the Polars-style `DataFrame` verbs and column expressions. **`frame.rs`**: the
  `DataFrame` value, its readers, writers and conversions. **`data_table.rs`**: data.table queries
  answered on ry's engine when the result is guaranteed identical, and declined to data.table
  otherwise.
- **`standard_library.rs`**: `Result`, `List`, `Str`, `Num`, `Dict` and `Set`, typed by
  `types/ry.Rtypes`.
- **`testing.rs`**: the fuzz contracts of the interpreter, shared by the in-tree harness and the
  coverage-guided targets.

## `jit`

The crate root is `src/jit.rs`. It is the optimizing tier: it compiles hot tier-0 bytecode to
machine code with Cranelift, and hands the activation back to tier 0 when a speculation fails.

- **`analysis.rs`**: the speculation plan, from an abstract interpretation of a body's bytecode
  over R value types.
- **`codegen.rs`**: machine code for a plan.
- **`runtime.rs`**: the activation record compiled code runs on, and the helpers it calls for
  everything that allocates, runs R code or can fail.

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
- **`eval.rs`**: installs the interpreter and its `Evaluator` hook, the evaluation entry points of
  the C API (with a small built-in evaluator for a host without the interpreter), the
  foreign-call builtins and `dyn.load` family; `.Call`,
  `.External`, `.C` and `.Fortran`; conditions; and the deparser for messages.
- **`dynload.rs`**: loading package libraries, routine registration, `NativeSymbolInfo` lookup and
  C callables.
  **`macho.rs`**: on macOS, rewrites a GNU R package binary's load command for GNU R's `libR` to
  name ry's runtime.
- **`extptr.rs`**: external pointers, weak references and finalizers.
- **`altrep.rs`**: the ALTREP class registry, dispatch for every accessor, and base R's own
  classes (compact sequences, deferred strings, wrappers). **`frame_altrep.rs`**: a table column
  as an ALTREP vector.
- **`serialize.rs`**: R's serialization format in every variant, the stream API, RDS files and
  lazy-load databases.
- **`connections.rs`**: the `R_ext/Connections.h` entry points over the interpreter's
  connection table, and the trampolines that call a package's own connection methods.
- **`classes.rs`**: the implicit class and the S4 slot and class functions.
- **`hashtab.rs`**: R's hash tables, kept in heap objects.
- **`legacy.rs`**: names R 4.6 no longer declares that binaries built against older R import
  (`TRUELENGTH`, `SETLENGTH`, `FRAME`, `R_PPStack` and the like).
- **`graphics.rs`**: the C entry points of R's graphics engine over `interp`'s `graphics_engine.rs`,
  so grid and device packages such as ragg run unchanged.
- **`arith.rs`**: NA tests, sorting, and the random number generator, which shares its state with the
  interpreter's through `.Random.seed`.
- **`rmath.rs`**: `Rmath.h`: the densities, distribution functions, quantiles and random variates
  of R's distributions, and the special functions under them.
- **`applic.rs`**: `R_ext/Applic.h`: the `optim()` and `nlm()` optimizers, QUADPACK integration,
  `findInterval`, `R_max_col` and `R_pretty`.
- **`linpack.rs`**: the LINPACK routines R uses, including `dqrdc2` behind `qr()` and `lm.fit()`,
  and the PORT machine constants.
- **`utils.rs`**: `R_ext/Utils.h`: file and temporary names, string predicates, and the printing
  and signalling helpers Fortran code calls.
- **`globals.rs`**: GNU R's data symbols.
- **`embed.rs`**: ry's own `ry_` entry points for an embedding host, declared in
  `include/ry_embed.h`, and the embedding functions of `Rembedded.h`.
- **`include/`**: the header set, written independently and source-compatible with R 4.6.
  `BLAS.h` and `Lapack.h` declare the system's BLAS and LAPACK, which packages link themselves.

## `table`

The crate root is `src/table.rs`. It is ry's built-in typed table, an engine over Polars.

- **`table.rs`**: `Table`, a Polars `DataFrame` whose columns are restricted to the types that have
  an R vector type, so every column crosses into R and back without loss.
- **`convert.rs`**: conversion between Arrow columns and R vectors, including `NA`.
- **`query.rs`**: the query IR, and its lowering of `tbl[i, j, by]` to a Polars lazy plan with
  data.table's semantics. **`lazy.rs`**: the Polars-style plan (`Plan`, its verbs, the optimizer and
  `explain`). **`group.rs`**, **`join.rs`** and **`kernel.rs`**: kernels that beat Polars' general
  plans for grouped aggregation, joins and row-wise expressions.
- **`io.rs`**: `fread` and `fwrite`, with the layout read by `sniff`. **`ops.rs`**: the data.table
  helpers that real code calls beside `[` (`setkey`, `rbindlist`, `melt`, `dcast`, `shift`, `fifelse` and
  the like).
- **`ffi.rs`**: the zero-copy interchange with the Arrow C data interface. A column reaches R and
  native code as an ALTREP vector, which `libr`'s `frame_altrep.rs` provides.

## `sniff`

The crate root is `src/sniff.rs`. It reads a delimited text file's separator, header and column
types by data.table's `fread` rules. The table engine's reader and the checker share it, so a
literal `fread("prices.csv")` is typed as exactly the columns that read produces.

## `rcmd`

The library root is `src/rcmd.rs`, and the binary `ry-rcmd` (`src/main.rs`) is what `bin/R`,
`bin/Rscript` and `bin/Rcmd` in ry's R home run. `assemble` creates the R home (headers,
`libR.so`, `etc/Makeconf`, `share/make/shlib.mk`, links to GNU R's packages, `share/`, `etc/`
and `R CMD` scripts, and the three front-end scripts); `ry-rcmd assemble` is its command line.
`install`, `shlib` and `config` are `R CMD INSTALL`, `R CMD SHLIB` and `R CMD config`.
`r_command_line` and `rscript_command_line` parse GNU R's front-end arguments into a `Session`
that `main.rs` runs through `ry_run` in the home's `libR.so`. `src/install.R` is the R-level
half of `R CMD INSTALL`, embedded and run on ry after the compile.
