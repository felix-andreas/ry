---
title: Loading packages
description: Edit your package, run the next command, and see the change, without calling load_all() or restarting R
---

When you work on an R package, most of your time goes into one loop: edit a function, run it, look at
the result, edit again. Start `ry repl` anywhere inside the package and that loop needs no extra
step, because ry reloads what you changed before each command:

```text
$ ry repl
Loaded tally: 1 function bound lazily, 0 top-level statements run, in 5 ms.
> count_words("the  quick fox")
[1] 4
```

The double space produced an empty word, so you change `strsplit(text, " ")` in `R/count.R` to split
on `"[[:space:]]+"`, save, and run the same command again:

```text
> count_words("the  quick fox")
Reloaded tally: 1 function rebound, in 3 ms.
[1] 3
```

There is no `load_all()` to remember and no session to restart. A reload replaces exactly the
definitions you changed and reruns only the top-level code that depends on them, so it stays fast on
a large package: about 50 ms after an edit on a 326,000-line package, where `devtools::load_all()`
takes 6.6 s (see [Speed](#speed)).

## Running one command or a script

`ry load` loads the package, runs something, and exits, which suits a quick check from the shell or
a script in CI:

```text
$ ry load -e 'count_words("the quick fox")'
Loaded tally: 1 function bound lazily, 0 top-level statements run, in 6 ms.
[1] 3
```

`ry load -f run.R` runs a script instead of an expression.

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

In `ry repl`, `load_all()` reloads on demand, `load_all(full = TRUE)` rebuilds the namespace from
scratch when you want a clean slate, and `ry repl --no-load` starts the console without loading the
package at all.

Two things `devtools::load_all()` does are missing: `system.file()` does not find files under the
package's `inst/`, and datasets in `data/` are not loaded.
[Package loading](/reference/package-loading#limits) lists every difference.

## Which R runs your package

`ry repl` and `load_all()` in an R session run your package on GNU R. `ry load` runs it on ry's own
runtime by default; pass `--runtime gnu-r`, or set `RY_RUNTIME=gnu-r`, to run it on GNU R instead.

A package written in plain R, with or without [inline annotations](/type-checking/inline-annotations),
loads on both. A package that uses ry's own syntax (records, tags, `match`, `DataFrame`) loads only
on ry's runtime, because GNU R's parser rejects it.

## Speed

A load parses a function only when something first calls it, and a reload touches only the
definitions you changed. On a generated package of 500 files, against `devtools::load_all()` and
[loadfast](https://github.com/finccam/loadfast):

| 326,000 lines | `load_all()` | `load_fast()` | ry |
| --- | --- | --- | --- |
| cold load | about 7 s | about 3 s | about 1 s |
| reload after a body edit | 6.6 s | 81 ms | about 50 ms |

[Package loading](/reference/package-loading) lists exactly what a load and a reload do, and what
devtools' `load_all()` does that ry does not yet.
