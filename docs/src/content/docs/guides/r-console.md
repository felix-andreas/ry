---
title: Working in the R console
description: Run a real R session inside ry, with Tab completion backed by the type checker
---

`ry repl` gives you an R prompt whose Tab completion comes from ry's type checker, and
`ry run` executes a script through the same session.

## What it is

ry does not reimplement R. It finds the R already installed on your machine, loads it into its own
process, and hands control to R's real main loop. Everything R does, R still does: parsing, evaluation,
autoprinting, error messages, `browser()`, `readline()`, S4 dispatch. Your `.Rprofile` is sourced.
Packages install and attach normally.

What ry replaces is the *line editor* in front of R. That is where the two differ:

```
$ ry repl
... R's own startup banner ...
ry R console, R at /usr/lib/R. Type q() or Ctrl-D to quit.
> account <- list(holder = "ada", balance = 120.5)
> account$
balance  double
holder   character
```

R's own console can complete `account$balance` because by the time you press Tab the object exists
in memory. ry reaches the same answer by type-checking what you have typed so far, which also lets
it show that `balance` is a `double`, complete full signatures, and complete names inside `#:`
annotations. The trade-off is that **completion never inspects the live R session.**


## Starting it, and the R it finds

```sh
ry repl                        # start a session
ry repl --keybindings vi       # vi editing instead of emacs
ry repl -f setup.R             # run a script first, then hand you the prompt
```

ry locates R in two steps:

1. **`R_HOME`**, if it is set and non-empty.
2. Otherwise it runs **`R RHOME`** using your `PATH` and takes the answer.

Whatever it settles on is exported back as `R_HOME` before R starts, so `R.home()` inside the session
agrees with the banner line, which is how you check which installation you got. On Unix,
`R_SHARE_DIR`, `R_INCLUDE_DIR` and `R_DOC_DIR` are recovered from R's own `bin/R` script too, so
`R.home("share")` stays correct on distributions that relocate those directories.

To pin a specific R, set `R_HOME` for the one command:

```sh
R_HOME=/opt/R/4.5.1/lib/R ry repl
```

Startup problems fail immediately and name the cause, with exit status 2:

```
$ ry run analysis.R
error: no R found: R_HOME is not set and running `R RHOME` failed (No such file or directory (os error 2)). Install R or set R_HOME to an R installation.

$ R_HOME=/nonexistent ry run analysis.R
error: R_HOME is set to /nonexistent, but that directory does not exist
```

There is one requirement beyond "R is installed": it must have been built as a shared library
(`--enable-R-shlib`). Every CRAN binary distribution is. If yours is not, ry reports that and names
the directory it looked in.

ry's console is developed and tested against R 4.2 or newer, and R 4.2 is the minimum on Windows.
On Windows an older R is rejected with a message saying so. On Unix nothing checks the version, so
an older R fails at load time with a missing-symbol error instead.

Sessions start clean and leave nothing behind: no `.RData` is restored on the way in, and nothing is
saved on the way out. `ry repl` also needs a terminal. Piping into it does nothing useful, because
the editor cannot run and the session immediately sees end of input. Use `ry run` for anything
non-interactive.

## Where completions come from

Press Tab and ry type-checks the session so far, meaning every line you have accepted plus the line
you are editing, then offers what fits at the cursor. The right-hand column is the type it inferred.

```
> nchar
nchar                               fn(x: Any, [type]: character, [allowNA]: logical, [keepNA]: logical) -> integer
nzchar                              fn(x: Any, [keepNA]: logical) -> logical
getDLLRegisteredRoutines.character  From the `base` package.
```

The first two have typed [stubs](/type-checking/stubs) shipped with ry, so the full signature and
the optional arguments (`[type]`) are shown. The third is a real `base` export with no stub yet, so
only the name and its package are shown. Tab again to cycle, Enter to accept.

Six kinds of completion are available:

| You type | You get |
| --- | --- |
| a bare name | Session bindings you defined, standard-library functions, R reserved words |
| `stats::rnor` | That namespace's exports, with signatures where a stub exists |
| `account$` | Fields of the record type ry inferred, each with its own type |
| `obj@` | S4 slot names already spelled after `@` earlier in the session, not read from `setClass`. The object's own name is offered alongside them |
| `#: chara` | Type names. Inside an annotation comment the completer switches from values to types |
| `x[["` | Record fields, when the string subscripts a record |

Anything you have defined is available on the next line, and anything reachable through `::` is
available whether or not the package is attached:

```
> monthly_rate <- function(annual) annual / 12
> monthly
monthly_rate

> stats::rnor
rnorm             fn(n: Any, [mean]: Any, [sd]: Any) -> double[]
rlnorm            fn(n: Any, [meanlog]: Any, [sdlog]: Any) -> double[]
order.dendrogram
```

Matching is fuzzy and smart-case: exact beats prefix, prefix beats substring, substring beats
subsequence (which needs three characters). Names you defined outrank standard-library names.

### Three things it will not complete

Completion is static analysis over the session transcript, and only that transcript. The
consequences are:

- **Objects R knows about but ry never saw you type.** Anything created by `source()` or defined in
  a file you passed to `-f` is invisible to Tab. `ry repl -f setup.R` feeds the script straight to
  R, so `compound()` from that file runs but does not complete. Paste the definition into the
  prompt to have it completed.
- **Your project's files and `ry.toml`.** The console analyzes one document, which is the session. It does
  not read your working directory, your project sources, or your `stubs/*.Rtypes` overrides. Use the
  [language server](/features) for typed work inside project files.
- **Third-party packages.** Only the standard library is available. `library(dplyr)` attaches dplyr
  in R as usual, but `dplyr::muta` + Tab offers nothing typed: activating those stubs needs project
  metadata a console session does not have.

## Editing

The editor is ry's, and its keys follow the conventions of a modern shell.

| Key | Effect |
| --- | --- |
| Tab | Open the completion menu; Tab again cycles candidates |
| Ctrl-R | Reverse search through history |
| Up / Down | Walk history |
| Ctrl-C | At the prompt, clear the line. During evaluation, interrupt it |
| Ctrl-D | End the session (same as `q()`) |

History is persistent and shared across sessions, keeping the last 1000 entries, in
`~/.local/share/ry/history.txt` on Linux, `~/Library/Application Support/ry/` on macOS. It is
ry's own file, separate from R's `.Rhistory`. As you type, the greyed-out text ahead of the cursor
is a suggestion from that history; Right arrow accepts it. Input is syntax-highlighted by the same lexer
`ry check` uses: keywords blue, constants like `TRUE` purple, strings green, numbers cyan,
comments grey.

Multi-line input is one editable buffer, not a sequence of lines you can no longer reach:

```
> compound <- function(principal, rate) {
+ principal * (1 + rate)
+ }
> compound(100, 0.05)
[1] 105
```

ry decides a line is incomplete from the syntax alone, such as an open bracket, a trailing operator, or a
dangling comma, an unclosed string. It is deliberately conservative: if it treats a line as complete
and R disagrees, R asks for the rest with its own `+` prompt and nothing is lost.

Every prompt R produces passes through unchanged, because ry renders no prompts of its own, so
debugging behaves as it does in stock R:

```
> f <- function(x) { browser(); x * 2 }
> f(21)
Called from: f(21)
Browse[1]> x
[1] 21
Browse[1]> c
[1] 42
```

Interrupts are cooperative, the same as in stock R: Ctrl-C sets a pending flag that R honors at its next
check point. A long R loop stops promptly and drops you back at the prompt:

```
> for (i in 1:100000000) x <- sqrt(i)
^C
> 1 + 1
[1] 2
```

Compiled code that never checks for interrupts will not respond, exactly as in stock R.

## ry run

`ry run FILE` is the same embedded session with the editor removed: the file is fed to R, R
evaluates it, and the process exits.

```
$ cat report.R
rates <- c(0.02, 0.035, 0.05)
cat("mean rate:", mean(rates), "\n")

$ ry run report.R
... R startup banner ...
mean rate: 0.035
$ echo $?
0
```

An error at top level halts the script and fails the run, the same halt-on-error contract `Rscript`
has:

```
$ ry run report.R     # cat("before\n"); stop("rate table is empty"); cat("after\n")
before
Error: rate table is empty
$ echo $?
1
```

`after` never printed. Parse errors behave the same way: output before the bad line is kept, the run
stops there, and the status is 1. An explicit `q(status = 7)` in your script passes straight through.
For the full status table, see the [CLI reference](/reference/cli).

Four things to know before using it as an `Rscript` replacement:

- **`ry run` does no analysis.** No type checking, no findings, and no diagnostic codes. The bytes go
  to R. Run [`ry check`](/reference/cli) if you want the script checked.
- **`interactive()` returns `TRUE`.** The embedded session is always an interactive one, which is a real
  divergence from `Rscript`. Code that branches on `interactive()` takes the other path.
- **R's startup banner is printed**, and your `.Rprofile` is sourced. There is no quiet or vanilla mode.
- **If your script sets its own `options(error = ...)`, it replaces the handler that produces the
  failing exit status**, and a failing script may then exit 0.

`ry repl -f FILE` uses the same feeding mechanism but keeps you at the prompt afterwards, which
loads a scratch setup before an interactive session.

## What needs R

Only the console does. Everything else in ry is self-contained: type information for the standard
library is compiled into the binary, not read from an R installation.

| Command | Needs R installed? |
| --- | --- |
| `ry check` | No |
| `ry fmt` | No |
| `ry server` (the language server) | No |
| `ry repl` | Yes, at runtime |
| `ry run` | Yes, at runtime |

This is why continuous integration for a ry project needs no R at all; see
[Continuous integration](/guides/continuous-integration).
