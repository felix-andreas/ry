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
| `account$`, `x[["` | the fields of the record ry inferred |
| `#: chara` | type names |

The trade-off is that completion never inspects the live session. It cannot see:

- objects created by `source()` or by a script passed to `-f`, because ry never saw them typed;
- your project's files, `ry.toml`, or `stubs/`, because the console analyzes only the session;
- third-party packages: `library(dplyr)` attaches dplyr in R, but its declarations are not loaded.

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

`ry repl --keybindings vi` switches to vi keys. History is ry's own file, separate from
`.Rhistory`. Sessions start clean and save nothing: no `.RData` is restored or written.

## `ry run`

`ry run script.R` feeds a file to R and exits. A top-level error stops the script with exit status 1,
like `Rscript`, and `q(status = 7)` passes its status through. Three differences matter if you use it
in place of `Rscript`:

- **It does no analysis.** Run `ry check` separately.
- **`interactive()` is `TRUE`,** because the embedded session is always interactive. Code that
  branches on it takes the interactive path.
- **Setting `options(error = ...)` in the script replaces the handler that makes errors exit 1**, so
  a failing script may then exit 0.

R's startup banner prints, and `.Rprofile` is sourced. `ry repl -f setup.R` runs a script the same
way and then keeps the prompt open.

## Which R it uses

ry uses `R_HOME` if it is set, and otherwise asks `R RHOME` on your `PATH`, so `R_HOME=/opt/R/4.5.1/lib/R ry repl`
pins a version. R must be 4.2 or newer and built as a shared library (`--enable-R-shlib`), which
every CRAN binary is. If no suitable R is found, the command fails with exit status 2 and says what
it looked for.
