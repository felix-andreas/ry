---
title: Console
description: An R console that reloads your package as you edit it, with completion from the type checker
---

`ry R` is the R console for working on a package. Start it anywhere inside the package, and each
time you edit a file, the next command sees the change without a `load_all()` or a restart:

```text
$ ry R
Loaded tally: 1 function bound lazily, 0 top-level statements run, in 6 ms.
> count_words("the  quick fox")
[1] 4
> count_words("the  quick fox")     # after editing R/count.R
Reloaded tally: 1 function rebound, in 3 ms.
[1] 3
```

[Loading packages](/guides/loading-packages) covers this loop in full, including how to get the same
`load_all()` in RStudio. Outside a package, `ry R` is a plain R console with the extras below.

## Completion that knows your values

Tab completion comes from the type checker, worked out from the code you typed rather than from
the live session, so it shows each field's type next to its name:

```text
> account <- list(owner = "Ada", balance = 10)
> account$<Tab>
balance  double
owner    character
```

## Two runtimes

`ry R` runs the GNU R installed on your machine, which ry finds on its own. `ry run` opens the same
console on ry's own runtime, which runs [records, tags](/type-checking/records-and-tags) and
[`DataFrame`](/guides/dataframes) and turns inline annotations into checks when a function is
called. That runtime is still incomplete, so code that needs something it lacks stops with an error
that names it, and its console does not load your package yet.

Both run a script when you give them one, and the arguments after the script name reach it through
`commandArgs(trailingOnly = TRUE)`:

```sh
ry R script.R a b    # on GNU R
ry run script.R a b  # on ry's runtime
```

`-i script.R` runs the script and then opens the console instead of exiting.

## Typed code at the console

You can type [inline annotations](/type-checking/inline-annotations) at the prompt and in scripts.
ry prints a warning first when an inline annotation and a `#:` comment disagree, and in
[strict mode](/type-checking/strict-mode) a command with a strict-mode error does not run. GNU R runs
each command with the annotations erased.

Only the commands that run code (`ry R`, `ry run` and `ry load`) need R installed, and even ry's own
runtime still takes base R from it. Checking, formatting and the editor read the source and never
run it. The [command reference](/reference/cli#run-and-r) lists every option.
