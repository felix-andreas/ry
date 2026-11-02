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
- **Every other top-level statement** runs at load time, in collation order. The code files are
  those of `R/` and then those of `R/unix/` (`R/windows/` on Windows), as R CMD INSTALL finds them:
  names that start with a letter or digit and end in `.R`, `.r`, `.S`, `.s` or `.q`. Collation
  follows `Collate.unix` (`Collate.windows`) when `DESCRIPTION` has it, else `Collate`, whose entries
  name subdirectory files as `unix/a.R`. Files the field leaves out follow, `R/` before `R/unix/` and
  each in C-locale order; R CMD INSTALL would refuse such a package, but `load_all()` appends them
  too.
- **`NAMESPACE`** works as in an installed package: imports, `S3method()` registrations, and exports.
  With `export_all = TRUE` every object in the namespace is attached as `package:<name>`, as with
  `load_all()`.
- **Datasets in `data/`** are read into the namespace's lazy data the way `data()` reads them:
  `.rda` and `.RData` files with `load()`, `.R` files with `sys.source(chdir = TRUE)`, and tables
  with `read.table(header = TRUE, as.is = FALSE)`, which splits `.csv` files on `;` and `.tab` and
  `.txt` files on whitespace. `pkg::name` finds a dataset there when no export has that name, and
  the attached `package:<name>` holds a copy of each, which wins over an export of the same name.
  Unlike an installed package, every dataset is read at load time whatever `LazyData` says, as with
  `load_all()`.
- `Depends` packages are attached, `R/sysdata.rda` is loaded, `src/` is compiled with
  [pkgbuild](https://pkgbuild.r-lib.org) and loaded, and `.onLoad` and `.onAttach` run.
- **A syntax error stops the load before anything runs**, and names the file and line. A statement
  that fails at load time is reported with its file and line too.
- **With [strict mode](/type-checking/strict-mode) on**, in `ry.toml` or in a file's
  `# typing: strict` comment, `ry load` checks the package's `R/` files. If any has a strict-mode
  error, it prints the errors and exits with status 1 before anything with an effect runs. On GNU
  R the check finishes before the load starts. On ry's runtime it runs while the runtime starts
  and the package's definitions bind, and the load waits for it before running any other
  top-level statement, compiled code, `.onLoad`, or your `-e` and `-f` code. The
  `R/` files are checked together, so a name one file defines resolves in the others, and test
  files are not part of the check.
  A later `load_all()` in the same session, and the load a console makes, do not check.
  A package that passes is remembered in ry's cache directory (`~/.cache/ry/strict` on Linux),
  so loading it again without changing its files, configuration, stubs, data files or the ry
  binary skips the check.

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
  a literal signature, or `setValidity()`. Any other one, such as `setRefClass()` or
  `setClassUnion()`, keeps them all in place.
- Every method is for a generic the package itself defines, or its signature names one of the
  package's own classes, as `setMethod("show", "Quantity", ...)` does. A method of the second kind
  must be for a standard generic that already exists, such as `show`, and the held-back definitions
  then also run the first time that generic dispatches or is looked up. A method for a primitive
  such as `length`, for a group generic such as `Arith`, or for a function that is not yet generic
  keeps them in place, because those do not dispatch through `standardGeneric()`, where ry runs
  held-back definitions. Every validity method
  must be for one of the package's own classes.
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

On GNU R, functions keep their source references, so `print()`, tracebacks, and the debugger point
into your files, as long as `options(keep.source = TRUE)` is set, which is the default in an
interactive session.

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

A console reloads before a command when the names, sizes or modification times of `DESCRIPTION`,
`NAMESPACE`, the code files in `R/` and `R/unix/`, `R/sysdata.rda` or the datasets in `data/`
changed since the last load, so it never reads a file to find out.
An edit under `src/` is not noticed, and neither is anything in a package whose first load failed:
call `load_all()` after either.

## Limits

- `system.file()` does not map a source package's `inst/` directory; `load_all()` does.
- Compressed dataset tables (`.csv.gz`, `.tab.bz2`, `.txt.xz` and so on) are not read. When one
  dataset name has files in several formats, such as `x.R` and `x.rda`, every file is read in name
  order, where `data()` reads only one.
- `if` around directives in `NAMESPACE` is not evaluated; ry warns and ignores the directive.
- testthat helpers are not sourced; `testthat::test_dir()` sources them itself.
- Rebuilding compiled code unloads the old library first, as `load_all()` does, so an object that
  still points into it must not be used afterwards.
- On ry's own runtime (`ry load`, the `ry run` console, and `load_all()` in a `ry run` script),
  functions carry no source references. Load in `ry R` when you need them.
- Compiled code is built with GNU R's toolchain on either runtime, because `R.home()` still names
  GNU R's installation. The library loads into ry because it links `libR.so` by its soname, which
  ry's own `libR.so` already provides.
- A top-level statement that reads a name before the statement defining it runs sees, on a reload,
  the value from the previous load. A fresh load would fail on such code anyway.
