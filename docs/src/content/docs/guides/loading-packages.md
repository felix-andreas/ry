---
title: Loading packages
description: Edit your package, run the next command, and see the change, without calling load_all() or restarting R
---

When you work on an R package, most of your time goes into one loop: edit a function, run it, look at
the result, edit again. Start `ry run` anywhere inside the package and that loop needs no extra
step, because ry reloads what you changed before each command:

```text
$ ry run
Loaded tally: 1 definition bound lazily, 0 top-level statements run, in 5 ms.
ry console on ry's runtime. Type q() or Ctrl-D to quit.
> count_words("the  quick fox")
[1] 4
```

The double space produced an empty word, so you change `strsplit(text, " ")` in `R/count.R` to split
on `"[[:space:]]+"`, save, and run the same command again:

```text
> count_words("the  quick fox")
Reloaded tally: 1 definition rebound, in 3 ms.
[1] 3
```

There is no `load_all()` to remember and no session to restart. A reload replaces exactly the
definitions you changed and reruns only the top-level code that depends on them, so it stays fast on
a large package: about 80 ms after an edit on a 326,000-line package, where `load_all()`
takes 8.6 s (see [Speed](#speed)).
Under [strict mode](/type-checking/strict-mode), the package's pure constants are bound the same
lazy way as its functions.

## Running one command or a script

`ry load` loads the package, runs something, and exits, which suits a quick check from the shell or
a script in CI. Like `ry run`, it runs the package on ry's own runtime, which is still incomplete
([Which R runs your package](#which-r-runs-your-package)):

```text
$ ry load -e 'count_words("the quick fox")'
Loaded tally: 1 definition bound lazily, 0 top-level statements run, in 6 ms.
[1] 3
```

`ry load -f run.R` runs a script instead of an expression. To run a script against the package on
GNU R, start it with `load_all()` and run it with `ry R run.R`.

## From RStudio or plain R

If you work in RStudio or another R session, ry provides a `load_all()` you call as you do today:

```r
source(pipe("ry load --r-entry"))
load_all()
```

Put the first line in your `~/.Rprofile` so `load_all()` is there in every session. It takes `path`,
`export_all` (default `TRUE`), `attach`, `full`, and `quiet`. This `load_all()` reloads only when you
call it, as devtools' does.

## Controlling reloads

In the console, `load_all()` reloads on demand, `load_all(full = TRUE)` rebuilds the namespace from
scratch when you want a clean slate, and `--no-load` (`ry R --no-load`, `ry run --no-load`) starts
the console without loading the package at all.

## Differences from devtools

Two things `devtools::load_all()` does are missing: `system.file()` does not find files under the
package's `inst/`, and datasets in `data/` are not loaded.
[Package loading](/reference/package-loading#limits) lists every difference.

## Which R runs your package

`ry run` and `ry load` run your package on ry's own runtime. `ry R` and `load_all()` in an R session
run it on GNU R, and `load_all()` loads on whichever runtime calls it. The consoles of `ry run` and
`ry R` load and reload a package the same way, so when ry's runtime lacks something your package
needs, `ry R` gives you the same loop on GNU R.

A package written in plain R, with or without [inline annotations](/type-checking/inline-annotations),
loads on both. A package that uses ry's own syntax (records, tags, `match`, `DataFrame`) loads only
on ry's runtime, because GNU R's parser rejects it.

## Speed

A load parses a function only when something first calls it, and a reload touches only the
definitions you changed. On a generated package of 500 files, against `devtools::load_all()` and
[loadfast](https://github.com/finccam/loadfast):

| 326,000 lines | `load_all()` | `load_fast()` | `ry R` | `ry load` |
| --- | --- | --- | --- | --- |
| cold load | 9.6 s | 4.0 s | 1.3 s | 1.7 s |
| reload after a body edit | 8.6 s | 125 ms | 79 ms | 83 ms |

In the `ry run` console, a command after a body edit takes 57 to 86 ms including the reload. On
this package, `ry run` starts its console with the package loaded in 1.0 s, against 1.2 s for
`ry R` (medians of seven runs, timed in turn on the same machine).

These are medians of five runs of `tools/loader-bench` (`load_all()` is `pkgload::load_all()`, which
`devtools::load_all()` calls). The machine was shared and ry was built without link-time
optimization, so trust the ratios more than the milliseconds. On the 14,000-line package the bench
also generates, a cold load takes 1.2 s with `load_all()`, 0.37 s with `load_fast()`, 0.15 s with
`ry R` and 0.54 s with `ry load`.

[Package loading](/reference/package-loading) lists exactly what a load and a reload do, and what
devtools' `load_all()` does that ry does not yet.
