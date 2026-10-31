---
title: Loading packages
description: Load the R package you are working on from source, lazily, and reload only what you changed
---

ry loads the package you are developing straight from its sources, as `devtools::load_all()` does,
but it parses a function only when you first call it, and after an edit it replaces exactly the
definitions you changed. On a 14,000-line package that makes a cold load almost 10 times faster
than `load_all()` and a reload after an edit about 60 times faster (see [Speed](#speed)).

```sh
ry repl            # inside a package: load it, and reload edits before each command
ry load -f run.R   # load it, run a script, and exit
```

```text
$ ry load -e 'count_words("the quick fox")'
Loaded tally: 1 function bound lazily, 1 top-level statement run, in 5 ms.
[1] 3
```

## In the console

Started anywhere inside a package, `ry repl` loads that package before the first prompt. When you
then edit a file and run the next command, ry reloads first, so the command sees the code on disk:

```text
> greet()
[1] "first version"
> greet()        # after editing R/greet.R
Reloaded replpkg: 1 function rebound, in 2 ms.
[1] "second, edited version"
```

`load_all()` reloads on demand, `load_all(full = TRUE)` rebuilds the namespace from scratch, and
`ry repl --no-load` starts without loading anything.

## From plain R

In RStudio, `Rscript`, or any other R session, ry provides the same `load_all()`:

```r
source(pipe("ry load --r-entry"))
load_all()
```

The first line attaches `tools:ry` with `load_all()` on the search path, so put it in your
`~/.Rprofile` to keep calling `load_all()` as you do today. Each call runs `ry` to work out what
changed. `load_all()` takes `path`, `export_all` (default `TRUE`), `attach`, `full`, and `quiet`.

## What a load does

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

Functions keep their source references, so `print()`, tracebacks, and the debugger point into your
files, as long as `options(keep.source = TRUE)` is set, which is the default in an interactive
session.

## What a reload does

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

## Speed

Median times of five runs of the `load_all()` call itself on GNU R 4.3, each in a fresh session
without source references (as `Rscript` runs), on generated packages of helper functions, S3
classes, and some R6 and S4 classes. `tools/loader-bench` regenerates them.

| Package | Loader | Cold load | Reload after a body edit | Reload after adding a function | Reload with no change |
| --- | --- | --- | --- | --- | --- |
| 80 files, 14,000 lines | `devtools::load_all()` | 868 ms | 435 ms | 312 ms | 263 ms |
| | `loadfast::load_fast()` | 282 ms | 10 ms | 8 ms | 3 ms |
| | ry | 90 ms | 7 ms | 5 ms | 2 ms |
| 500 files, 326,000 lines | `devtools::load_all()` | 7,385 ms | 6,581 ms | 5,308 ms | 5,111 ms |
| | `loadfast::load_fast()` | 3,064 ms | 81 ms | 82 ms | 25 ms |
| | ry | 867 ms | 59 ms | 39 ms | 6 ms |

Keeping source references, as an interactive session does by default, adds 10 to 20% to a cold
load.

## Limits

- `system.file()` does not map a source package's `inst/` directory, and datasets in `data/` are not
  loaded; `load_all()` does both.
- `if` around directives in `NAMESPACE` is not evaluated; ry warns and ignores the directive.
- testthat helpers are not sourced; `testthat::test_dir()` sources them itself.
- Rebuilding compiled code unloads the old library first, as `load_all()` does, so an object that
  still points into it must not be used afterwards.
- A top-level statement that reads a name before the statement defining it runs sees, on a reload,
  the value from the previous load. A fresh load would fail on such code anyway.
