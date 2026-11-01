---
title: Loading packages
description: Load the R package you are working on from source, lazily, and reload only what you changed
---

ry loads the package you are developing straight from its sources, as `devtools::load_all()` does,
but it parses a function only when you first call it, and after an edit it replaces exactly the
definitions you changed. Under [strict mode](/type-checking/strict-mode), constants are bound the
same lazy way. On a 14,000-line package a cold load is almost 10 times faster than
`load_all()` and a reload after an edit about 60 times faster (see [Speed](#speed)).

```sh
ry R               # inside a package: load it, and reload edits before each command
ry load -f run.R   # load it, run a script, and exit
```

```text
$ ry load -e 'count_words("the quick fox")'
Loaded tally: 1 definition bound lazily, 0 top-level statements run, in 3 ms.
[1] 3
```

## In the console

Started anywhere inside a package, `ry R` loads that package before the first prompt. When you
then edit a file and run the next command, ry reloads first, so the command sees the code on disk:

```text
> greet()
[1] "first version"
> greet()        # after editing R/greet.R
Reloaded replpkg: 1 definition rebound, in 2 ms.
[1] "second, edited version"
```

`load_all()` reloads on demand, `load_all(full = TRUE)` rebuilds the namespace from scratch, and
`ry R --no-load` starts without loading anything. The console of `ry run`, on ry's runtime, does not
load the package yet.

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
are what the interpreter evaluates, so a lazy function is lowered from that tree on first use.
`load_all()` loads on the runtime it is called from: ry's in a `ry run` script, and GNU R in
`ry R` and in plain R.

A package that uses ry's own syntax (records, tags, `match`, `DataFrame`) loads and runs only on ry's
runtime, because GNU R's parser rejects it (`unexpected ':'` for a tag). Inline annotations alone
are fine on both.

`tools/loader-bench` generates packages of 80 files (14,000 lines) and 500 files (326,000 lines) and
times `devtools::load_all()`, `loadfast::load_fast()` and ry on both runtimes. On the larger package
ry loads cold in about a second (`load_fast()` about 3 s, `load_all()` about 7 s) and reloads after
a body edit in about 50 ms (81 ms and 6.6 s).

Keeping source references, as an interactive session does by default, adds 10 to 20% to a cold
load.

[Package loading](/reference/package-loading) lists exactly what a load and a reload do, and what
`load_all()` does that ry does not yet.
