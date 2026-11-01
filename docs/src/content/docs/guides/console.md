---
title: Console
description: An R console that reloads your package as you edit it, with completion from the type checker
---

`ry repl` is the R console for working on a package. Start it anywhere inside the package, and each
time you edit a file, the next command sees the change without a `load_all()` or a restart:

```text
$ ry repl
Loaded tally: 1 function bound lazily, 0 top-level statements run, in 5 ms.
> count_words("the  quick fox")
[1] 4
> count_words("the  quick fox")     # after editing R/count.R
Reloaded tally: 1 function rebound, in 3 ms.
[1] 3
```

[Loading packages](/guides/loading-packages) covers this loop in full, including how to get the same
`load_all()` in RStudio. Outside a package, `ry repl` is a plain R console with the extras below.

## Completion that knows your values

Tab completion comes from the type checker, worked out from the code you typed rather than from
the live session, so it shows each field's type next to its name:

```text
> account <- list(owner = "Ada", balance = 10)
> account$<Tab>
balance  double
owner    character
```

## Running a script

`ry run script.R` runs a script through the same R and exits. Arguments after the script name reach
it through `commandArgs(trailingOnly = TRUE)`.

## Typed code at the console

You can type [inline annotations](/type-checking/inline-annotations) at the prompt and in scripts.
`ry repl` and `ry run` run each command with them erased, and print a warning first when an inline
annotation and a `#:` comment disagree. In [strict mode](/type-checking/strict-mode), a command with
a strict-mode error does not run.

## Which R it runs

The console runs the GNU R installed on your machine, which it finds on its own. Only the commands
that run code (`ry repl`, `ry run` and `ry load`) need R installed, and even ry's own runtime still
takes base R from it. Checking, formatting and the editor read the source and never run it.

`--runtime ry` runs the console or a script on ry's own interpreter instead, which is still
incomplete and is needed for records, tags and `DataFrame`. The console then loads no package. The
[command reference](/reference/cli#server-repl-run-and-load) lists every option.
