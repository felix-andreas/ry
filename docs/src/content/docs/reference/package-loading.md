---
title: Package loading
description: What ry binds, runs and rebinds when it loads a package from source, and what it does not do yet
---

[`ry load`](/guides/loading-packages), the `ry run` and `ry R` consoles and `load_all()` all load a package the same way.
This page is the exact behavior.

## A load

- **A function definition**, `name <- function(...) ...`, becomes a promise that parses and
  evaluates only that definition the first time anything uses `name`. When another top-level
  statement writes the same name, as `f <- memoise(f)` does, every writer of it runs in order
  instead.
- **A constant of a strict file**, `name <- value` in a file under [strict mode](/type-checking/strict-mode)
  whose value calls only pure functions, becomes a promise the same way, unless another statement
  writes the same name. Strict mode guarantees the value has no effect, so evaluating it on first use
  gives what a load would have computed
  ([lazy constants](/reference/type-system#lazy-constants)).
- **An R6 class generator**, `Name <- R6Class(...)` or `Name <- R6::R6Class(...)` in any file,
  becomes a promise too when its arguments are built only from literals, names, functions, and
  `list()` or `c()` of them, and every package name it reads outside its methods (an `inherit =`
  class, say) is written once, earlier in collation order. Such a generator depends on nothing but
  its arguments, and its methods read the namespace only when they run, so building it on first
  use gives the same class. An argument that calls anything else, as in
  `public = list(at = Sys.time())`, keeps the statement eager.
- **Every other top-level statement** runs at load time, in collation order. The code files are
  those of `R/` and then those of `R/unix/` (`R/windows/` on Windows), as R CMD INSTALL finds them:
  names that start with a letter or digit and end in `.R`, `.r`, `.S`, `.s` or `.q`. Collation
  follows `Collate.unix` (`Collate.windows`) when `DESCRIPTION` has it, else `Collate`, whose entries
  name subdirectory files as `unix/a.R`. Files the field leaves out follow, `R/` before `R/unix/` and
  each in C-locale order; R CMD INSTALL would refuse such a package, but `load_all()` appends them
  too.
- **`NAMESPACE`** works as in an installed package: imports, `S3method()` registrations, and exports.
  A declared method whose function the package does not define refuses the load, as R does.
  With `export_all = TRUE` every object in the namespace is attached as `package:<name>`, as with
  `load_all()`.
- **Datasets in `data/`** are read into the namespace's lazy data the way `data()` reads them:
  `.rda` and `.RData` files with `load()`, `.R` files with `sys.source(chdir = TRUE)`, and tables
  with `read.table(header = TRUE, as.is = FALSE)`, which splits `.csv` files on `;` and `.tab` and
  `.txt` files on whitespace. `pkg::name` finds a dataset there when no export has that name, and
  the attached `package:<name>` holds a copy of each, which wins over an export of the same name.
  Unlike an installed package, every dataset is read at load time whatever `LazyData` says, as with
  `load_all()`.
- **`if` in `NAMESPACE`** works as in GNU R: at every load, the condition is evaluated in the
  global environment, and the directives on the branch it takes apply, so
  `if (.Platform$OS.type == "windows") export(f)` exports `f` only on Windows. This holds inside
  a top-level `{ }` block too. A condition that fails stops the load with its error. `ry check` cannot evaluate R, so it reads the directives
  of both branches: an import on either one resolves the names it brings in.
- `Depends` packages are attached, `R/sysdata.rda` is loaded, `src/` is compiled and loaded,
  and `.onLoad` and `.onAttach` run. On ry's runtime, `src/` is compiled by ry itself, against
  ry's own headers and `libR`, with the `ry-rcmd` that sits beside ry's runtime; GNU R and
  pkgbuild are not involved. The R home this needs is created once per build of ry in ry's cache
  directory (`~/.cache/ry/home` on Linux). On GNU R, `src/` is compiled with
  [pkgbuild](https://pkgbuild.r-lib.org), as `load_all()` does.
- **`system.file()`** finds a source package's files the way `load_all()` maps them: a path under
  `inst/` is found as if the package were installed, so `system.file("extdata", "a.txt", package =
  "mypkg")` returns `inst/extdata/a.txt`, and other paths are looked up in the package root. Code
  of a package loaded from source, `.onLoad` included, and code run from the console or a script
  see this mapping. Code inside an installed package calls base R's `system.file()`, which does
  not know the source package.
- **testthat helpers**, the `tests/testthat/helper*.R` files, are sourced into the namespace after
  the code when `export_all = TRUE`, the default, and attached with the package's objects, so tests
  and the console can call them. A reload sources them again when it changed any code, or when a
  helper was added, changed or deleted, so a helper's value computed from the package's functions
  stays current. A helper that fails fails the load, and the next load sources it again.
- **A file in latin1** loads when `DESCRIPTION` says `Encoding: latin1` (or `ISO-8859-1`): its text
  is read as latin1, and its strings arrive in R as UTF-8. Without that field, a file that is not
  valid UTF-8 refuses the load with a message naming the field.
- **A syntax error stops the load before anything runs**, and names the file and line. A statement
  that fails at load time is reported with its file and line too.
- **With [strict mode](/type-checking/strict-mode) on**, in `ry.toml` or in a file's
  `# typing: strict` comment, `ry load` checks the package's `R/` files. If any has a strict-mode
  error, it prints the errors and exits with status 1 before anything with an effect runs. On GNU
  R the check finishes before the load starts. On ry's runtime it runs while the runtime starts
  and the package's definitions bind. Imported packages may load in that time, but the load waits
  for the check before it runs a top-level statement whose arguments could have an effect,
  `data/*.R`, compiled code, `.onLoad`, or your `-e` and `-f` code. The
  `R/` files are checked together, so a name one file defines resolves in the others, and test
  files are not part of the check.
  Every later load checks too: `load_all()`, a reload, and the load a console makes at start, on
  either runtime. When the check refuses one of them, the load raises
  `<package> was not loaded, because strict mode refused it` before anything runs, so the session
  keeps the code it had. A load that changes nothing skips the check.
  The first check in a process shares [`ry check`'s record](/reference/cli#the-check-cache) in
  `.ry_cache/`, so a load after `ry check`, the editor or an earlier load reuses the findings of
  every file that has not changed and checks only the rest. The first reload builds an analysis
  that the process keeps, and every reload after it redoes only what the edit affects.

### S4 definitions

On ry's runtime, a fresh load holds back a package's S4 definitions and runs them the first time
anything needs one, so a session that never touches its classes never pays for registering them. The
package behaves as if they had run in place: each name they bind (a class generator, a generic, the
`.__C__` and `.__T__` metadata) is a promise that runs them, and so is looking up one of their classes
or generics by name, as `new()`, `is()`, `getClass()` and method dispatch do. When a statement needs
one during the load itself, the held-back definitions placed before that statement run then, and the
rest run in their own places.

A load holds them back only when all of these hold, and otherwise runs every one of them in place:

- Every S4 registration in the package is `setClass()` (alone or as `Gen <- setClass(...)`),
  `setGeneric()` with a function literal as its definition, `setMethod()` or `setReplaceMethod()` with
  a literal signature, `setValidity()`, or `setClassUnion()` with literal members. Any other one, such
  as `setRefClass()`, keeps them all in place.
- Every method is for a generic the package itself defines, or its signature names one of the
  package's own classes, as `setMethod("show", "Quantity", ...)` does. A method of the second kind
  may be for a generic function such as `show` or `initialize`, for a primitive such as `length` or
  `[`, or for a function that is not yet generic, such as `as.list`, which the method turns into a
  generic in the namespace. The held-back definitions then also run the first time that generic is looked up or
  dispatches, the first time the primitive dispatches on an S4 object, or the first time code reads
  the function's name in the namespace. A method for a group generic such as `Arith` keeps them in
  place, because its members dispatch to it without a lookup ry can see. Every validity method must
  be for one of the package's own classes.
- Every class union unites only the package's own held-back classes, or only other packages'
  classes. A union of the second kind, such as `setClassUnion("NumOrNull", c("numeric", "NULL"))`,
  is not held back but runs in place, because it changes what `numeric` and `NULL` extend for every
  later lookup of them. A union of both kinds keeps every definition in place.
- Every name the definitions read is written once, by a statement placed before them, so running them
  later reads the same values.
- Every name the definitions bind is written by them alone.
- No generic they define already exists as a function the namespace can see.

`options(ry.load.lazy_s4 = FALSE)` turns this off. The load's summary counts the definitions it held
back, as in `Loaded shapes: 1 definition bound lazily, 1 top-level statement run, 6 S4 definitions held
back until first use, in 5 ms.` A reload that changes one of them, or a name they bind, runs them all
first.

When the held-back definitions run after the load, ry saves what they bound (the classes, generics
and method tables) to `$XDG_CACHE_HOME/ry/s4` (`~/.cache/ry/s4` by default), and a later process
restores them instead of running them, much as GNU R restores an installed package's S4 metadata. On
the 326,000-line bench package, this cuts the first S4 use from about 330 ms to about 100 ms. A saved
entry is used only when all of these match the process that saved it:

- the text of every held-back definition, the package's name and version, and the build of ry;
- the R version, and the version of every package the namespace imports.

The definitions are saved only when they read nothing but the names they bind themselves and the
`methods` and base functions that build S4 objects, such as `setClass()`, `representation()`, `c()`
and `function`, so no other code in the package can change what they produce. A definition that
reads a package constant, as in `prototype(side = ZERO)`, still runs in every process. Nothing is
saved when running them signalled a message or a warning, so a later process shows it again. The
directory is safe to delete.

### Source references

Functions keep their source references on both runtimes, so `print()` shows them as written and
`getSrcLocation()`, tracebacks and the debugger point into your files, as long as
`options(keep.source = TRUE)` is set, which is the default in an interactive session. On ry's runtime
a lazily bound definition gets its source reference when it is first used, and a file's `srcfile` is
built only then. Columns count characters and byte offsets count bytes on both runtimes, so in
`m <- function() "é"; n <- function() 3` the definition of `n` starts at column 27 and byte 28.

## The load-index cache

A load reads its analysis of the package from a cache when an earlier process analyzed exactly the
same sources, so a cold start parses nothing until code runs. The cache lives in
`$XDG_CACHE_HOME/ry/load-index` (`~/.cache/ry/load-index` by default), holds one entry per package
root, and is safe to delete. An entry is used only for the same `DESCRIPTION`, `NAMESPACE` and `R/`
texts, the same strict setting, and the same build of ry.

## A reload

A reload works out which top-level items changed and touches only those:

- A changed function is rebound, a new one is bound, and a deleted one is removed. A function that
  only moved, because you edited something above it, is rebound to keep its source reference right.
- A top-level statement runs again when you changed it, or when it read something that changed while
  it ran, including anything the functions it called use. `cached <- helper()` reruns after you edit
  `helper`, or a function `helper` calls, while a function that calls `helper()` does not need to,
  because it looks `helper` up when it runs.
- A lazy constant is bound again when you changed it, or when anything its value reaches changed,
  such as the body of a pure function it calls, so its next use computes the new value.
- When two statements write the same name, the last one in collation order wins again, and a
  statement like `f <- memoise(f)` sees `f` as it was at load time rather than wrapping it twice.
- A `NAMESPACE` change rebuilds the imports or resyncs the exports, `.onLoad` runs again when you
  changed it, and a deleted S4 method or class is removed.
- When a dataset file was added, changed or deleted, judged by the names, sizes and modification
  times of the files in `data/`, every dataset is read again, and a deleted one is gone.
  `R/sysdata.rda` is loaded again when its content changed.

A console reloads before a command when `DESCRIPTION`, `NAMESPACE`, or anything under `R/`,
`data/` or `src/` changed since the last load, and an edit under `src/` rebuilds the compiled code
first. It learns of edits from the operating system's file notifications, so a command costs
nothing when nothing changed. Where notifications are unavailable, it compares the names, sizes
and modification times of the code files, `R/sysdata.rda` and the datasets instead, which misses
`src/`. A package whose load failed, or was refused by strict mode, is watched by those stamps, so
fixing the file loads it before the next command.

When a package another development package imports reloads, the importer's imports are rebuilt
too, so it calls the new definitions without reloading itself. If the importer's `NAMESPACE` no
longer fits, because it imports a name the reload removed, the reload still succeeds, and the
importer keeps its old imports with a warning until you reload it.

## Limits

- Compressed dataset tables (`.csv.gz`, `.tab.bz2`, `.txt.xz` and so on) are not read. When one
  dataset name has files in several formats, such as `x.R` and `x.rda`, every file is read in name
  order, where `data()` reads only one.
- A reload that leaves a declared S3 method without its function is refused before it changes
  anything, unless it reruns a top-level statement that may bind a name it computes, such as
  `assign(paste0("print.", class), ...)`, which could define the function. Then the refusal comes
  after the statements ran, and the namespace holds the new code. In a file that is not strict,
  every statement other than a function definition counts as such a statement.
- Rebuilding compiled code unloads the old library first, as `load_all()` does, so an object that
  still points into it must not be used afterwards. On ry's runtime the old library stays mapped, and
  the rebuilt one loads from a fresh copy of the file, because the system loader would otherwise
  hand back the mapping it already has.
- A registered routine whose R name (after `.fixes`) is already defined in the namespace is not
  bound, and the load warns, as GNU R's `loadNamespace` does.
- A top-level statement that reads a name before the statement defining it runs sees, on a reload,
  the value from the previous load. A fresh load would fail on such code anyway.
