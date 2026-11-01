---
title: Loading packages
description: Load the R package you are working on from source, lazily, and reload only what you changed
---

ry loads the package you are developing straight from its sources, as `devtools::load_all()` does,
but it parses a function only when you first call it, and after an edit it replaces exactly the
definitions you changed. On a 14,000-line package a cold load is almost 10 times faster than
`load_all()` and a reload after an edit about 60 times faster (see [Speed](#speed)).

```sh
ry repl            # inside a package: load it, and reload edits before each command
ry load -f run.R   # load it, run a script, and exit
```

```text
$ ry load -e 'count_words("the quick fox")'
Loaded tally: 1 function bound lazily, 0 top-level statements run, in 3 ms.
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

Put the first line in your `~/.Rprofile` to keep calling `load_all()` as you do today. It takes
`path`, `export_all` (default `TRUE`), `attach`, `full`, and `quiet`.

## Speed

`ry load` runs on ry's own interpreter, and parses each file once: the analysis's syntax trees
are what the interpreter evaluates, so a lazy function is lowered from that tree on first use. Pass
`--runtime gnu-r` (or set `RY_RUNTIME=gnu-r`) to load on GNU R instead. `ry repl` always loads on GNU R, `ry run`
does unless you pass `--runtime ry`, and `load_all()` in plain R runs on GNU R by definition.

Median of five runs of the load call itself, in a fresh session without source references, on
generated packages (`tools/loader-bench` regenerates them). The GNU R numbers are on R 4.3.

| Package | Loader | Cold load | Reload after a body edit |
| --- | --- | --- | --- |
| 80 files, 14,000 lines | `devtools::load_all()` | 868 ms | 435 ms |
| | `loadfast::load_fast()` | 282 ms | 10 ms |
| | ry on GNU R | 82 ms | 6 ms |
| | ry on its own runtime | 104 ms | 5 ms |
| 500 files, 326,000 lines | `devtools::load_all()` | 7,385 ms | 6,581 ms |
| | `loadfast::load_fast()` | 3,064 ms | 81 ms |
| | ry on GNU R | 1,036 ms | 53 ms |
| | ry on its own runtime | 932 ms | 58 ms |

The two ry rows of each package were measured together on one machine, and the other loaders on a
different one, where ry on GNU R took 90 ms and 867 ms cold. Compare the ry rows with each other,
and the other loaders with those 90 and 867 ms.

Keeping source references, as an interactive session does by default, adds 10 to 20% to a cold
load.

[Package loading](/reference/package-loading) lists exactly what a load and a reload do, and what
`load_all()` does that ry does not yet.
