---
title: News
description: Release announcements and project news
---

## Unreleased

- **ry needs no GNU R to run R.** `ry run`, `ry load`, `ry test`, `ry document` and `ry install`
  use a build of base R 4.6.2 that ships inside `ry`, so `R.version` reports R 4.6.2. Set
  `R_HOME` to a GNU R installation to emulate that release instead ([Installation](/installation#r)).
- **Typed code runs on native values.** A typed function in strict mode compiles before its first
  call to native code over numbers, strings and lists, so typed programs that build strings or
  call local functions run 10 to 120 times faster than on GNU R ([Benchmarks](/benchmarks)).
- **S4 definitions run natively.** `setClass()`, `setGeneric()` and `setMethod()` run in Rust for
  the shapes packages usually write, with results identical to the methods package's R code. On a
  generated package with 2,892 S4 definitions, the first `new()` after an uncached load takes about
  0.85 s instead of about 6 s.

### Known limitations

- **`ry R` still needs GNU R**, because it runs your code on GNU R itself.
- **Linear algebra on Windows needs GNU R.** Windows has no system LAPACK, so `solve()` and its
  relatives need `R_HOME` naming a GNU R installation, whose `Rblas.dll` and `Rlapack.dll` they call.

## ry 0.4.1-beta, on every platform

*Released in November 2026 as a pre-release.*

Version 0.4.1-beta ships ry with its runtime for Linux x86_64, macOS on Apple silicon and Windows
x86_64, loads large S4 packages much faster, and fixes the R incompatibilities that real package
test suites reported. The VS Code extension on the Marketplace is unchanged at 0.3.2, so set
`ry.path` to the new binary to use the runtime from VS Code ([Installation](/installation)).

### Highlights

- **macOS and Windows archives.** `ry run`, `ry load` and `ry install` work on all three platforms,
  and the release workflow smoke-tests each archive against GNU R and a CRAN package before it
  publishes.
- **Large S4 packages load in well under a second once cached.** On a generated package with
  2,877 S4 definitions, `ry load` followed by the first `new()` takes about 0.75 s on a second run,
  against about 6.6 s before. Class unions and `setAs` no longer stop ry from holding S4
  definitions back until first use, and the cached definitions are restored lazily without
  `methods::cacheMetaData`.
- **`sys.call()` and `sys.calls()` no longer copy the arguments.** Under `do.call()` with a large
  argument they took about 0.1 s each, which made data.table's `[` hundreds of times slower.

### All changes

- Regular expressions accept TRE's `{+}` quantifier.
- `names<-` dispatches `as.character`, so Date and POSIXct names match GNU R.
- `format(x, "%OS0")` prints whole seconds.
- An interrupt raised under `suspendInterrupts()` stays pending until the next check.
- `ry load` attaches testthat and sources the test helpers as pkgload does.
- `ry test` runs `test_*.R` files and filters on testthat's context name.
- `c()` drops `NULL` arguments before S4 dispatch, as GNU R does.
- `nrows()` and `ncols()` accept `NULL` and pairlists.
- `round()` and `signif()` port GNU R's algorithm, including the R 4.5.1 change, so they pick the
  same nearest double.
- On Windows, paths split at both separators, `system()` runs without a shell and honors its
  redirections, and `R CMD` puts Rtools first on `PATH`, so `R CMD build` and package installs work.

## ry 0.4.0-beta, the first release that runs R

*Released in November 2026 as a pre-release.*

Version 0.4.0-beta adds ry's own R runtime beside the checker, plus the language features that only
that runtime runs.

### Highlights

- **Your R code runs faster.** On the eleven programs on [Why ry](/getting-started), ry's runtime beats GNU
  R on ten: 1.2 times faster on vectorized arithmetic, 5 to 7 times on loops over vectors, and 130
  times on recursive `fib` (0.01 s against 1.31 s). `log_rank`, which spends its time in builtins,
  runs at GNU R's speed. [Benchmarks](/benchmarks) has the code and the method.
- **Types reach run time.** Inline annotations sit beside `#:` comments, and ry's runtime checks them
  at function boundaries. [Strict mode](/tutorial/fully-typed#strict-mode) rejects untyped code, and
  `ry check --annotate` writes the types it asks for.
- **Records, tuples, tagged unions and `match`** model data with exhaustiveness checks, and typed
  code that uses them compiles to machine code at its first call.
- **One binary.** A release archive holds `ry` alone. The runtime is embedded in it and unpacked into
  your cache directory the first time you run code.
- **Package development is built in.** `ry load` loads your package lazily, `ry test` runs its tests
  and `ry document` writes its documentation, each a drop-in for the devtools command.
- **`DataFrame`** is a typed table that runs on the Polars engine, and the
  [standard library](/reference/standard-library) (`Result`, `List`, `Str`, `Dict`, `Set` and more)
  now covers Roc's builtins.

### All changes

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
  fix-all action, refactorings (extract a variable or a function, inline a variable, and move types
  between a `#:` comment and inline syntax), a call hierarchy, workspace symbol search, an outline
  with sections, folding, selection ranges, semantic highlighting and parameter-name hints.
- **More lints, including lintr's.** `equals-na`, `vector-logic`, `length-sequence`, `class-equals`
  and `length-test` are on by default, each with an unsafe fix. Eleven more ported from lintr, such
  as `scalar-in`, `order-subset`, `matrix-apply` and `nzchar`, are available when you turn them on,
  like `redundant-return` and `commented-code` ([diagnostic codes](/reference/diagnostic-codes)). A project that is set up for lintr keeps its
  choices without a `ry.toml`, because ry reads the `linters` field of its
  [`.lintr` file](/reference/configuration#a-lintr-lintr-file).
- **`ry check --annotate` writes the types strict mode asks for.** It adds the inferred type of
  each unannotated top-level function and binding, then lists what is left to type by hand. The
  editor offers the same edit for one binding or a whole file
  ([Moving a file to strict mode](/tutorial/fully-typed#strict-mode)).
- **`ry fmt --range`** formats only the lines you name, the editor formats a selection or the line you
  just finished, and `ry fmt` runs in parallel ([Formatting](/tutorial/check#format)).
- **Faster recursion and more of base R.** A function that only does scalar arithmetic, such as
  recursive `fib`, compiles to frameless machine code (`fib(30)` takes 0.005 s against 0.9 s on GNU
  R), and sockets work, so `parallel`'s PSOCK clusters run.
- **A debugger and a console in VS Code.** `F5` runs the current script under a debugger with
  breakpoints, stepping and variable inspection, "Debug test" does the same for one `test_that()`
  block, and `Ctrl+Enter` sends the selection to a ry console. Other editors can use the same
  debugger through `ry debug-adapter`. See [Editors](/reference/cli#debug-adapter).
- **The editor attaches to a running console.** "ry: Attach to Console" debugs the commands you type
  in `ry run`, `browser()` stops at a `Browse[1]>` prompt that an editor can take over, "View Data"
  opens a paged, sortable and filterable table of a data frame, matrix or `DataFrame`, and hover and
  completion use the console's live values. R chunks in R Markdown and Quarto documents get the
  language-server features too (Unix only for the console connection).
- **Named arguments navigate.** Hover, go-to-definition, references and rename follow a named
  argument to the parameter it fills, so renaming a parameter also rewrites `rate = 0.05` at its
  call sites, and `pkg::name` goes to the package's stub or your own definition.
- **Faster loads.** `ry load` binds R6 class generators lazily and holds back S4 methods on other
  packages' generics, so a session that never touches them skips their setup
  ([package loading](/reference/package-loading)), and a strict `ry load` runs its check while the
  runtime starts and checks only the package's own files, not its tests and scripts.
- **`ry test` runs your package's tests as `devtools::test()` does.** It loads the package lazily
  with `ry load`, runs the `tests/testthat` files with testthat's own reporters, and exits with `1`
  when a test fails. With `--watch`, every save reloads only the changed definitions, rebuilds
  compiled code after a `src/` edit, and runs the test files the edit can reach first. It learns of
  saves from the operating system's file notifications, as the consoles now do. On a 308,000-line
  benchmark package with 502 test files, the first file's results arrive in about 2 s, against 62 s
  for `devtools::test()`
  ([CLI](/reference/cli#test), [Running tests](/reference/package-loading)).
- **`ry document` writes documentation as `devtools::document()` does.** It runs roxygen2 against
  the lazily loaded package, and its `man/*.Rd` files and `NAMESPACE` are byte for byte the same.
  It caches each file's parsed blocks and runs roxygen2's slowest steps in linear time, so on a
  benchmark package with 22,000 Rd files it takes about 2 minutes after an edit, against 21 for
  devtools ([Writing documentation](/tutorial/runtime#run-code)).
- **`ry load` follows `devtools::load_all()` more closely.** `system.file()` maps `inst/`, testthat
  helpers are sourced, lazily defined functions keep their source references, a package that
  imports a reloaded one sees its new bindings, `Encoding: latin1` is honored, and a declared
  `S3method()` with no function refuses the load with R's message. Every load of a strict package
  is checked, including a reload in the console, which re-checks only what the edit changed. On
  ry's runtime, `src/` compiles with ry's own toolchain, and `if` blocks in `NAMESPACE` are
  evaluated as GNU R evaluates them ([package loading](/reference/package-loading)).
- **Help works on ry's runtime.** `?mean`, `help("mean")`, `help(package = "stats")` and
  `help.start()` print or serve R's help pages, HTML help included, as GNU R does
  ([Console](/reference/cli#run-and-r)).
- **The editor follows `$` fields and installed packages.** Go-to-definition, hover, references and
  rename resolve `x$name` through the values that built `x`: lists, data frames, records and R6
  classes, including `self$` and `private$`. Rename refuses a field it cannot trace completely.
  For a package without a ry stub, definition and hover read the installed package's code and help
  without running R ([Editors](/installation#editors)). Renaming a package-level `the` also renames its
  uses after `the$x <- v` inside a function, where R works on a local copy.
- **Completion and signature help work on untyped code.** Completion ranks names by how often the
  project uses them, with locals first, and a package without a ry stub completes the exports in its
  installed `NAMESPACE`. Signature help covers untyped functions, installed packages and R6 methods
  called through `self$`, and an unresolved name that a package exports gets the quick fixes
  "Qualify as `pkg::name`" and, in a script, "Add `library(pkg)`". A function literal shows its
  inferred signature as an inlay hint, member names such as `x$field`, `x@slot` and R6 members are
  highlighted, and the outline lists S4 slots and a type's tags.
- **Fewer false reports on real packages.** Inferred signatures now carry facts about the callee's
  body, such as a return chosen by a flag default or a parameter read only through `substitute()`.
  Names injected into R6 and Reference Class methods (`self`, `private`, `.self`) resolve, a
  literal `source("file.R")` is followed, and `with`, `subset` and `transform` mask their column
  arguments. With typing on, the corpus packages' `R/` sources report 51 errors, down from 98, and
  about 26 of those are real package bugs. The generated benchmark packages report nothing.
- **Graphics draw as GNU R does.** A gallery of 149 figures (base graphics with `persp`, contours,
  dendrograms and plotmath, grid, lattice, ggplot2 and PerformanceAnalytics charts) renders on every
  device, and 146 of them match GNU R pixel for pixel on ragg. On `pdf()`, `svg()` and the bitmap
  devices, gradients, tiling patterns, clipping paths, masks, compositing groups and even-odd fills
  work, and `postscript()` refuses them as GNU R's device does. knitr and rmarkdown documents with
  plot chunks render on ry.
- **`DataFrame` checks column types and handles bigger data.** Arithmetic or comparison on a text
  column is a type error on the column reference. Reading a CSV no longer holds the parsed file
  next to its columns, so a 1e8-row group-by run fits in 15 GB. data.table's `dt[i]` runs on
  data.table's own code again, which uses less memory. [From data.table](/reference/from-data-table)
  maps `dt[i, j, by]`, `:=`, joins and `fread` to `DataFrame` verbs, and the opt-in
  `[lint] data-table` setting marks data.table calls and links to it.
- **Typed code compiles at its first call.** A closure whose parameters and return are annotated
  compiles from its signature instead of after warm-up. Records, tuples, tags and guarded `match`
  compile natively too, and a variable that is an integer on one path and a double on another stays
  unboxed. A fold over 200 tag-union records five times takes 0.05 to 0.1 s, against 0.4 to 0.7 s on GNU R for
  the same program written with `list(kind = )`. In compiled code, `List.walk`, `List.map`,
  `List.keep_if`, `List.drop_if`, `List.any`, `List.all` and `List.count_if` run as native loops
  with the callback inlined: walking a 100-element list 10,000 times takes 0.009 s, against 0.36 s
  with the JIT off and 0.94 s for `Reduce` on GNU R.
- **Faster everyday R on ry's runtime.** `switch` compiles to bytecode, a closure call makes one
  collection check for its whole frame, small objects are allocated by bumping a pointer, and
  `x[[i]] <- v` on a plain local vector inside a function writes in place, and S3 dispatch caches
  its method lookups. Base R's kernels, such as `match`, `order`, `split`, `strsplit`, `table`,
  `merge`, `aggregate` and `cumsum`, run at or below GNU R's time at 1e7 rows; `apply` over rows is
  still about 1.4 times slower.
- **The standard library is complete against Roc's builtins.** `Dict.with_capacity` and
  `Set.with_capacity` are new, and `reserve` now works on an empty `Dict` or `Set`
  ([standard library](/reference/standard-library)). `List.range` and `List.repeat` stop with an
  error past 2,147,483,647 elements.
- **Lower memory, faster cold checks.** Large vectors are collected sooner, strings are interned in
  a flat table, and `as.character()` of a long number vector is deferred as on GNU R, so peak memory
  on string-heavy code is within 1.1 times GNU R's. `1:n`, `seq_len(n)` and `seq_along(x)` with
  over a million elements are compact, so `seq_len(2^31 - 1)` no longer allocates 8 GB. `gc()`
  reports real cell counts, and `gc.time()` and `gcinfo()` work. The parser now builds ry's own flat
  syntax tree, and a cold check keeps one parse tree per worker, so `ry check --no-cache` on the
  300,000-line benchmark code bases takes about 1.2 s on 4 cores and peaks under 300 MiB.
- **More packages pass their own tests.** ry's runtime passes the tests of 155 of the 164 packages
  that GNU R passes (94.5%), among them rlang, purrr, vctrs, R6, Matrix, S7, pkgload, shiny, readr
  and git2r, and 35 of GNU R's own 66 `tests/*.R` scripts print exactly what they print on GNU R.
  The optimizers and integrators behind `optim` and `integrate` (L-BFGS-B, UNCMIN, QUADPACK and the interval
  search) are now written from scratch and match GNU R's results.
  A session uses one temporary directory, removed when it exits.
- **Linear algebra works on NixOS.** BLAS and LAPACK are also looked up in the directories GNU R's
  `etc/Makeconf` records, and packages that ry installs link the same libraries
  ([Installation](/installation)).
- **Seeded harnesses cover more of ry**: the help server, makefile expansion in `R CMD`, the JIT,
  reloads, the debugger's message framing, CSV reading and rendered graphics files. The bugs they
  found are fixed, and an unclosed quote in a CSV file is now an error.

### Installing

This release has one archive, for Linux x86_64: `ry-x86_64-unknown-linux-gnu.tar.gz`. It holds the
single `ry` binary, with ry's runtime built in, and needs glibc 2.35 or newer. macOS and Windows
archives, and a VS Code extension that bundles the new binary, follow in a later release.
[Installation](/installation) shows how to install the archive and what to use on other platforms.

### Known limitations

- **ry's runtime still needs GNU R**, version 4.3 or later built as a shared library, because it
  reads base R's sources and the default packages from that installation. `ry check`, `ry fmt` and
  `ry server` need no R.
- **Linux x86_64 only.** This release has no macOS or Windows archive. The VS Code extension on
  the Marketplace is still 0.3.2, which bundles 0.3.2-beta without ry's runtime and has none of
  the debugger and console features above. The Windows runtime has run only under
  Wine so far.
- **Peak memory is often higher than GNU R's.** On the benchmark programs, ry uses more memory on
  eight of eleven, up to 1.8 times as much (`drawdown`, 133 MiB against 73 MiB).
- **Not every package passes its tests yet**: 9 of the 164 packages that pass on GNU R still fail on
  ry's runtime, and `apply` over rows is about 1.4 times slower than on GNU R.

### Licence

ry 0.3 was UPL-1.0 throughout. Parts of ry's runtime translate GNU R's own C code, so the `ry`
binary and the crates behind the runtime are now GPL-2.0-or-later, like GNU R. The tooling crates
(the parser, the checker, the formatter and the editor features) stay UPL-1.0, the C headers that
packages compile against are LGPL-2.1-or-later as R's are, and the VS Code and Zed extensions are
MIT. [Licensing](/reference/licensing) has the details, including why the binary as a whole is
passed on under GPL-3.0-or-later.

## ry 0.3.0, a complete rewrite

*Released in July 2026 as a pre-release.*

Version 0.3.0 replaces the previous implementation end to end, with a new parser, a new analysis
engine, and a static type checker, the first one for R.

- **The project is renamed from Roughly to ry.** The binary is `ry`, the configuration file is
  `ry.toml`, and a suppression comment reads `# ry: allow(...)`. The crate is published as `ry-lang`.
- **R gets a static type checker**, new and experimental: Hindley-Milner inference with numeric
  constraints and generics, guided by [`#:` annotation comments](/tutorial/types#annotate). Type
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
