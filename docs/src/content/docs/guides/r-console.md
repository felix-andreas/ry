---
title: Working in the R console
description: An R session whose Tab completion comes from ry's type checker, and a script runner on the same session
---

`ry repl` is an R console, and `ry run` runs a script through the same session. They are the only ry
commands that need R.

ry does not reimplement R. It loads the R installed on your machine into its own process and hands
control to R's main loop, so evaluation, error messages, `browser()`, `readline()`, packages, and
your `.Rprofile` all behave as in stock R. What ry replaces is the line editor in front of it, and
with it, where completion comes from.

## Completion from analysis, not from the session

R's own console completes `account$` by looking at the object in memory. ry type-checks what you have
typed so far instead, which lets it show types and full signatures, and complete inside `#:`
annotations:

```
> account <- list(holder = "ada", balance = 120.5)
> account$
balance  double
holder   character
```

| You type | You get |
| --- | --- |
| a name | your session's bindings, base R functions with signatures, keywords |
| `stats::rnor` | that namespace's exports, attached or not |
| `account$` | the fields of the record ry inferred |
| `#: chara` | type names |

The trade-off is that completion never inspects the live session. It cannot see:

- data frame columns: `df$` completes nothing, because ry does not type columns;
- objects created by `source()` or by a script passed to `-f`, because ry never saw them typed;
- your project's files, `ry.toml`, or `stubs/`, because the console analyzes only the session;
- packages outside the [shipped declarations](/type-checking/stubs#what-ships). `library(dplyr)`
  makes dplyr's verbs complete, but `library(shiny)` adds nothing.

For typed work on project files, use the [language server](/tour#editors).

## Editing

Multi-line input is one editable buffer. ry decides that a line is incomplete from its syntax (an
open bracket, a trailing operator, an unclosed string), and errs toward sending it: if R needs more,
it asks with its own `+` prompt and nothing is lost.

| Key | Effect |
| --- | --- |
| Tab | completion menu; Tab again cycles |
| Ctrl-R | search history |
| Right arrow | accept the grey suggestion from history |
| Ctrl-C | clear the line, or interrupt evaluation |
| Ctrl-D | quit |

`ry repl --keybindings vi` switches to vi keys. History is ry's own file (`~/.local/share/ry/history.txt`
on Linux), separate from `.Rhistory`. Sessions start clean and save nothing: no `.RData` is restored or
written.

## `ry run`

`ry run script.R` feeds a file to the same console, line by line as if you had typed it, and exits at
its end. A top-level error stops the script with exit status 1, and `q(status = 7)` passes its status
through. It replays console input exactly, which is what it is for; for scripts in production,
`Rscript` is the better tool, because `ry run`:

- does no analysis (run `ry check` separately);
- takes no script arguments and no `-e`;
- reports `interactive()` as `TRUE`, so code that branches on it takes the interactive path;
- prints R's startup banner to stdout, so `ry run x.R > out` captures it;
- exits 0 after an error if the script sets its own `options(error = ...)`, which replaces the
  handler that exits 1.

`ry repl -f setup.R` feeds a script the same way and then keeps the prompt open.

## Which R it uses

ry uses `R_HOME` if it is set, and otherwise asks `R RHOME` on your `PATH`, so `R_HOME=/opt/R/4.5.1/lib/R ry repl`
pins a version. R must be 4.2 or newer and built as a shared library (`--enable-R-shlib`), which
every CRAN binary is. Only Windows checks the version up front; elsewhere an older R fails to load. If no suitable R is found, the command fails with exit status 2 and says what
it looked for.
