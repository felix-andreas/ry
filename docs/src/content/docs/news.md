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
- **More lints, including lintr's.** `equals-na`, `vector-logic`, `length-sequence`, `class-equals`
  and `length-test` are on by default, each with an unsafe fix. Eleven more ported from lintr, such
  as `scalar-in`, `order-subset`, `matrix-apply` and `nzchar`, are available when you turn them on,
  like `redundant-return` and `commented-code` ([diagnostic codes](/reference/diagnostic-codes)). A project that is set up for lintr keeps its
  choices without a `ry.toml`, because ry reads the `linters` field of its
  [`.lintr` file](/reference/configuration#a-lintr-lintr-file).
- **`ry check --annotate` writes the types strict mode asks for.** It adds the inferred type of
  each unannotated top-level function and binding, then lists what is left to type by hand. The
  editor offers the same edit for one binding or a whole file
  ([Moving a file to strict mode](/type-checking/strict-mode#moving-a-file-to-strict-mode)).
- **`ry fmt --range`** formats only the lines you name, the editor formats a selection or the line you
  just finished, and `ry fmt` runs in parallel ([Formatting](/guides/formatting#formatting-part-of-a-file)).
- **Faster recursion and more of base R.** A function that only does scalar arithmetic, such as
  recursive `fib`, compiles to frameless machine code (`fib(30)` takes 0.005 s against 0.9 s on GNU
  R), and sockets work, so `parallel`'s PSOCK clusters run.
- **A debugger and a console in VS Code.** `F5` runs the current script under a debugger with
  breakpoints, stepping and variable inspection, "Debug test" does the same for one `test_that()`
  block, and `Ctrl+Enter` sends the selection to a ry console. Other editors can use the same
  debugger through `ry debug-adapter`. See [Editors](/guides/editors#running-and-debugging).
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
  runtime starts.
- **`ry test` runs your package's tests as `devtools::test()` does.** It loads the package lazily
  with `ry load`, runs the `tests/testthat` files with testthat's own reporters, and exits with `1`
  when a test fails. With `--watch`, every save reloads only the changed definitions and runs the
  test files the edit can reach first. On a 308,000-line benchmark package with 502 test files, the
  first file's results arrive in about 2 s, against 62 s for `devtools::test()`
  ([CLI](/reference/cli#test), [Running tests](/guides/loading-packages)).
- **`ry load` follows `devtools::load_all()` more closely.** `system.file()` maps `inst/`, testthat
  helpers are sourced, lazily defined functions keep their source references, a package that
  imports a reloaded one sees its new bindings, `Encoding: latin1` is honored, and a declared
  `S3method()` with no function refuses the load with R's message. Every load of a strict package
  is checked, including a reload in the console ([package loading](/reference/package-loading)).
- **Help works on ry's runtime.** `?mean`, `help("mean")`, `help(package = "stats")` and
  `help.start()` print or serve R's help pages, HTML help included, as GNU R does
  ([Console](/guides/console#help)).
- **The editor follows `$` fields and installed packages.** Go-to-definition, hover, references and
  rename resolve `x$name` through the values that built `x`: lists, data frames, records and R6
  classes, including `self$` and `private$`. Rename refuses a field it cannot trace completely.
  For a package without a ry stub, definition and hover read the installed package's code and help
  without running R ([Editors](/guides/editors)).
- **Fewer false reports on real packages.** Inferred signatures now carry facts about the callee's
  body, such as a return chosen by a flag default or a parameter read only through `substitute()`.
  With typing on, the corpus packages' `R/` sources report 63 errors, down from 98, and about 22 of
  those are real package bugs. The generated benchmark packages report nothing.
- **Graphics gain patterns and masks.** On `pdf()`, `svg()` and the bitmap devices, gradients, tiling
  patterns, clipping paths, masks, compositing groups and even-odd fills work, and `postscript()`
  refuses them as GNU R's device does. knitr and rmarkdown documents with plot chunks render on ry.
- **`DataFrame` checks column types and handles bigger data.** Arithmetic or comparison on a text
  column is a type error on the column reference. Reading a CSV no longer holds the parsed file
  next to its columns, so a 1e8-row group-by run fits in 15 GB. data.table's `dt[i]` runs on
  data.table's own code again, which uses less memory. [From data.table](/guides/from-data-table)
  maps `dt[i, j, by]`, `:=`, joins and `fread` to `DataFrame` verbs, and the opt-in
  `[lint] data-table` setting marks data.table calls and links to it.
- **Typed code compiles at its first call.** A closure whose parameters and return are annotated
  compiles from its signature instead of after warm-up. Records, tuples, tags and guarded `match`
  compile natively too, and a variable that is an integer on one path and a double on another stays
  unboxed. A fold over 200 tag-union records five times takes 0.05 to 0.1 s, against 0.4 to 0.7 s on GNU R for
  the same program written with `list(kind = )`.
- **The standard library is complete against Roc's builtins.** `Dict.with_capacity` and
  `Set.with_capacity` are new, and `reserve` now works on an empty `Dict` or `Set`
  ([standard library](/reference/standard-library)). `List.range` and `List.repeat` stop with an
  error past 2,147,483,647 elements.
- **Lower memory, faster cold checks.** Large vectors are collected sooner, so peak memory at 1e6
  rows is closer to GNU R's. `ry check --no-cache` uses a cheaper one-shot path, which takes the
  finance benchmark from 1.60 s to 1.48 s on 4 cores.
- **More packages pass their own tests.** ry's runtime passes the tests of 135 of the 164 packages
  that GNU R passes (82.3%). The optimizers and integrators behind `optim` and `integrate` (L-BFGS-B,
  UNCMIN, QUADPACK and the interval search) are now written from scratch and match GNU R's results.
  A session uses one temporary directory, removed when it exits.

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
