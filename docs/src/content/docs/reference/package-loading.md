---
title: Package loading
description: What ry binds, runs and rebinds when it loads a package from source, and what it does not do yet
---

[`ry load`](/guides/loading-packages), `ry repl` and `load_all()` all load a package the same way.
This page is the exact behavior.

## A load

- **A function definition**, `name <- function(...) ...`, becomes a promise that parses and
  evaluates only that definition the first time anything uses `name`. When another top-level
  statement writes the same name, as `f <- memoise(f)` does, every writer of it runs in order
  instead.
- **Every other top-level statement** runs at load time, in `Collate` order, with the files that
  `Collate` leaves out following in C-locale order.
- **`NAMESPACE`** works as in an installed package: imports, `S3method()` registrations, and exports.
  With `export_all = TRUE` every object in the namespace is attached as `package:<name>`, as with
  `load_all()`.
- `Depends` packages are attached, `R/sysdata.rda` is loaded, `src/` is compiled with
  [pkgbuild](https://pkgbuild.r-lib.org) and loaded, and `.onLoad` and `.onAttach` run.
- **A syntax error stops the load before anything runs**, and names the file and line. A statement
  that fails at load time is reported with its file and line too.

On GNU R, functions keep their source references, so `print()`, tracebacks, and the debugger point
into your files, as long as `options(keep.source = TRUE)` is set, which is the default in an
interactive session.

## A reload

A reload works out which top-level items changed and touches only those:

- A changed function is rebound, a new one is bound, and a deleted one is removed. A function that
  only moved, because you edited something above it, is rebound to keep its source reference right.
- A top-level statement runs again when you changed it, or when it read something that changed while
  it ran, including anything the functions it called use. `cached <- helper()` reruns after you edit
  `helper`, or a function `helper` calls, while a function that calls `helper()` does not need to,
  because it looks `helper` up when it runs.
- When two statements write the same name, the last one in collation order wins again, and a
  statement like `f <- memoise(f)` sees `f` as it was at load time rather than wrapping it twice.
- A `NAMESPACE` change rebuilds the imports or resyncs the exports, `.onLoad` runs again when you
  changed it, and a deleted S4 method or class is removed.

## Limits

- `system.file()` does not map a source package's `inst/` directory, and datasets in `data/` are not
  loaded; `load_all()` does both.
- `if` around directives in `NAMESPACE` is not evaluated; ry warns and ignores the directive.
- testthat helpers are not sourced; `testthat::test_dir()` sources them itself.
- Rebuilding compiled code unloads the old library first, as `load_all()` does, so an object that
  still points into it must not be used afterwards.
- On ry's own runtime (the default for `ry load`), functions carry no source references, and a
  package with `src/` does not load yet, because pkgbuild cannot find its build tools there. Use
  `ry load --runtime gnu-r` for either.
- A top-level statement that reads a name before the statement defining it runs sees, on a reload,
  the value from the previous load. A fresh load would fail on such code anyway.
