---
title: Running and reloading
description: An R console that reloads your package as you edit it, plus commands that run scripts, tests and roxygen2 against the loaded package
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
a large package: the command after an edit runs within 10 to 30 ms on a 326,000-line package, where
`load_all()` takes 8.6 s ([Speed](#speed)). Outside a package, `ry run` is a plain R console with the
same extras.

## Commands

| Command | What it does |
| --- | --- |
| `ry run` | The console on ry's runtime, with the package loaded and reloaded before each command |
| `ry R` | The same console, with the same loading, on the GNU R installed on your machine |
| `ry run script.R a b` | Runs a script on ry's runtime; `ry R script.R` runs it on GNU R. Arguments reach `commandArgs(trailingOnly = TRUE)`, and `-i` opens the console afterwards |
| `ry load -e 'expr'` | Loads the package, evaluates `expr` and exits (`-f run.R` runs a script) |
| `ry test` | Runs the testthat tests against the loaded package, as `devtools::test()` does |
| `ry document` | Runs roxygen2 against the loaded package, as `devtools::document()` does |

`ry test` prints testthat's own report and exits with `1` when a test fails, so CI that reads it
keeps working. `--filter allocation` runs only `test-allocation*.R`, and `--watch` reruns after every
save: it reloads only the definitions you changed and runs first the test files that can see the
change (the files you edited, and the files that call a changed function directly or through other
functions), then the rest of the suite. `ry document` writes the same `man/*.Rd` files and
`NAMESPACE` as roxygen2, byte for byte, and after an edit it parses only the files you changed.
`--roclet namespace` runs one roclet.

In the console, `load_all()` reloads on demand, `load_all(full = TRUE)` rebuilds the namespace from
scratch, and `--no-load` starts the console without loading the package. A load does what
`devtools::load_all()` does, including mapping `system.file()` to `inst/` and sourcing testthat
helpers, apart from a few edge cases that [Package loading](/reference/package-loading#limits)
lists. Under [strict mode](/type-checking/strict-mode), the package's pure constants are bound the
same lazy way as its functions.

## From RStudio or plain R

If you work in RStudio or another R session, ry provides a `load_all()` you call as you do today:

```r
source(pipe("ry load --r-entry"))
load_all()
```

Put the first line in your `~/.Rprofile` so `load_all()` is there in every session. It takes `path`,
`export_all` (default `TRUE`), `attach`, `full`, and `quiet`. This `load_all()` reloads only when you
call it, as devtools' does.

## Which R runs your code

`ry run`, `ry load`, `ry test` and `ry document` run on ry's own runtime, which runs
[records, tags](/type-checking/records-and-tags) and [`DataFrame`](/guides/dataframes) and turns
inline annotations into [checks](/type-checking/inline-annotations#running-annotated-code). That
runtime is still incomplete, so code that needs something it lacks stops with an error that names
it. `ry R` gives you the same loop on GNU R, and `load_all()` loads on whichever runtime calls it.

A package written in plain R, with or without [inline annotations](/type-checking/inline-annotations),
loads on both: GNU R runs it with the annotations erased. A package that uses ry's own syntax
(records, tags, `match`, `DataFrame`) loads only on ry's runtime, because GNU R's parser rejects it.
Both runtimes print a warning first when an inline annotation and a `#:` comment disagree, and in
[strict mode](/type-checking/strict-mode) a command with a strict-mode error does not run.

Only the commands that run code need R installed, and even ry's own runtime still takes base R from
it. Checking, formatting and the editor read the source and never run it. The
[command reference](/reference/cli#run-and-r) lists every option.

## The console

You can type ahead. Lines you type while the console starts or while a command runs are kept, and
they run in order at the next prompt. A command that reads standard input, such as
`readLines(file("stdin"), n = 1)`, reads the lines typed after it, as in a terminal. Ctrl-C
interrupts the running command and discards the lines queued behind it.

Tab completion comes from the type checker, worked out from the code you typed rather than from
the live session, so it shows each field's type next to its name:

```text
> account <- list(owner = "Ada", balance = 10)
> account$<Tab>
balance  double
owner    character
```

`browser()` stops the console at a `Browse[1]>` prompt, as in R. A line you type there runs in the
stopped function, `n` runs the next statement, `s` steps into a call, `f` finishes the current
function, `c` continues, `Q` abandons the command, and `where` shows the call stack:

```
> f <- function(x) {
+   y <- x + 1
+   browser()
+   y * 2
+ }
> f(1)
debug at #3: browser()
Browse[1]> y
[1] 2
Browse[1]> c
[1] 4
```

An editor connected to the console can take over a stopped program, stop in `debug(f)` and
`trace()`, open `View(df)` in its data viewer, and show plots in a panel
([Editors](/guides/editors#running-and-debugging)). Without an editor, `debug(f)` does nothing and
`View()` stops with an error that says how to connect one. In a script that `ry run file.R` runs,
`browser()` does nothing, as in a non-interactive R session.

`?mean`, `help("mean")` and `help(package = "stats")` print R's help pages as in GNU R. With
`help_type = "html"` or `help.start()`, ry's runtime starts R's help server and opens the page in
your browser.

## Speed

A load parses a function only when something first calls it, and a reload touches only the
definitions you changed. On a generated package of 500 files, against `devtools::load_all()` and
[loadfast](https://github.com/finccam/loadfast):

| 326,000 lines | `load_all()` | `load_fast()` | `ry R` | `ry load` |
| --- | --- | --- | --- | --- |
| cold load | 9.6 s | 4.0 s | 1.3 s | 1.7 s |
| reload after a body edit | 8.6 s | 125 ms | 79 ms | 83 ms |

In the `ry run` console, a command after a body edit prints its output 20 to 30 ms after you press
Enter, reload included, the first time you edit a file, and about 10 ms on later edits of the same
file. On this package, `ry run` starts its console with the package loaded in 0.35 s, against
0.84 s for `ry R`, because it analyzes the package while base R loads and reuses the analysis of an
unchanged package from its [cache](/reference/package-loading#the-load-index-cache).

On finbench, a generated package of 308,000 lines and 502 test files, one test file's results
arrive about 2 s after you start `ry test --filter`, where `devtools::test(filter =)` takes 62 s,
nearly all of it spent loading the package. After a save, `--watch` shows the affected file's
results 0.7 to 1.5 s later. Documented with roxygen2 (22,000 Rd files), the same package takes 24
minutes with `devtools::document()` and 6.7 minutes with `ry document`, and about 2 minutes after a
one-file edit, where devtools takes 21.

These are medians of several runs of `tools/loader-bench` on a shared machine, with ry built without
link-time optimization, so trust the ratios more than the milliseconds.
[Package loading](/reference/package-loading) lists exactly what a load and a reload do.
