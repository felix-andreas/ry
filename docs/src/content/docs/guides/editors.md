---
title: Editors
description: What the language server gives you in your editor, and how to set it up
---

In your editor, ry shows every finding of `ry check` as you type, and it tells you the type of
anything you hover, so you see a mistake the moment you make it instead of when the code runs. In
VS Code, install the
[ry extension](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry) and open an R
project: there is nothing else to set up. [Installation](/installation) covers Zed, and any other
editor that speaks LSP can run `ry server` as its language server for R.

You get:

- diagnostics as you type, the same ones `ry check` reports
- hover with the inferred type of any expression, parameter or loop variable
- completion, including the fields of a list such as `person$`, the members of `pkg::`, and the
  argument names a call still accepts (`second = `), also for R6 methods and untyped package
  functions; a package without a stub completes the exports listed in its installed `NAMESPACE`;
  among equally good matches, a call's argument names come first, then the enclosing function's
  own variables (unless a `(` already follows the cursor), then names you use often in the project;
  before a `(`, the functions the project calls most come first
- go-to-definition, find references, rename, and a call hierarchy that lists the callers and
  callees of a project function, across files; `pkg::name` goes to the package's stub or, for your
  own package, to the definition
- go-to-definition and hover on `$` fields: `account$balance` goes to where `balance` was put
  into the value, whether that is `list(balance = 0)`, a record, an `account$balance <- x`
  write, or a member of an R6 class read through `self$` or `private$`
- navigation into installed packages that have no stub: go-to-definition opens the package's R
  code, read from your R library, and hover shows its help page
- signature help with each parameter's type and default, also for an untyped function from an
  installed package and for a method of an R6 class called through `self$` or an instance
- inlay hints for assignments, loop variables, the inferred types of a function's parameters and
  result (`function<T>(x: T) -> T`), and parameter names in calls with two or more positional
  arguments (`grow(principal = 100, years = 2)`)
- quick fixes for misspelled names, arguments, fields and columns, and for the lints that have one
  obvious rewrite (`T` to `TRUE`, `=` to `<-`, a trailing comma), and an "Apply all safe fixes"
  action your editor can run on save; [Fixes](/reference/diagnostic-codes#fixes) lists them all
- for a name no attached package defines but a package exports (one ry has types for, or an
  installed one the project already uses), a fix that qualifies it as `pkg::name` and, outside a package's own code, one that adds `library(pkg)` after
  the `library` calls above the name
- refactorings: extract the selection into a variable or into a function (its parameters are the
  variables it reads, and a variable it assigns that later code reads becomes the result), inline
  a variable, and move a function's or a binding's types between a `#:` comment and inline syntax
- an outline that nests functions under RStudio-style sections (`# Accounts ----`) and helper
  functions and sections under the function that defines them, lists R6 members, S4 slots and a
  type definition's tags under their class or type, plus workspace symbol search
- folding for blocks, comment runs, sections and `# region` markers, and selection ranges that grow
  from a name to its argument, call, statement and block
- semantic highlighting of parameters, called functions, package namespaces and member names
  (`x$field`, `x@slot`, `x$method()`, record and pattern fields, R6 members, S4 slots, and the
  columns a `DataFrame` query such as `sales[amount > 0, .(total = sum(amount))]` reads), which an
  R grammar cannot tell apart, with functions from packages and base R marked as library functions
- formatting with the same rules as `ry fmt`, for the whole document, a selection, or as you type
  (a line break lays out the line you finished, and `}` the block it closes, when your editor
  formats on type)
- code lenses: "Run file" above a script, "Run test" and "Debug test" above each testthat
  `test_that()` block, and a reference count above each function

Hover, completion and inlay hints use inferred types even when type errors are switched off, so you
get them on any R project.

Rename edits the binding you picked and nothing else: a local `total`, a global `total`, and the
word "total" in a string are three different things. A parameter and the named arguments that fill
it are one binding, so renaming `rate` in `grow <- function(principal, rate)` also rewrites
`grow(100, rate = 0.05)` in every file, and hover, go-to-definition and references work from either
end. An abbreviated argument that R completes by partial matching (`grow(100, ra = 0.05)`) is not
followed, so spell arguments out before you rename their parameter.

A field ry cannot trace to the value it was read from, such as a field of a function parameter,
goes to every place in the project that puts a field of that name into a value, and rename refuses
it, because those places are a guess. For the same reason, renaming a field you can trace is
refused while any other read of that name in the project cannot be traced, since that read might
be the same field. A rename covers every `if` branch that builds the value, and it does not touch
string indexing such as `cfg[["port"]]`. A field of a value that a package function returns, such as
`optim(start, cost)$par`, goes to that function, whose help page describes what it returns. A
named argument passed to a function that collects its `...` into a list, as in
`new_rate <- function(...) list(...)` and then `new_rate(pause = 1)`, counts as a place that puts
`pause` into a value. In `account$deposit(amount = 5)`, the argument name `amount` goes to the
parameter of the function stored in `deposit`, and renaming that parameter renames the argument.

The code of an installed package comes from its lazy-load database, so it is the code R runs. ry
writes it to a file under your cache directory (`~/.cache/ry/packages` on Linux), named by the
package's version, and reads a package only when you first navigate into it. Edits to that file
change nothing, because ry regenerates it. Packages are looked up in this order: the project's
renv library, the libraries in `R_LIBS`, `R_LIBS_USER` and `R_LIBS_SITE`, your user library, and
R's own library. A package
installed with `options(keep.source.pkgs = TRUE)` shows its original source with comments,
otherwise the code is deparsed. Base R's primitive functions, such as `sum` and `c`, have no R
code, so they show only their arguments.

A refactoring is offered only where it keeps what the code does. Extracting a variable evaluates
the expression once, before its statement, so it is not offered where the statement would evaluate
it repeatedly, conditionally or not at all: in the body of a loop or a function written inside
that statement, in an `if` branch, on the right of `&&` or `||`, or in a formula or `quote()`. Extracting a function is not
offered for code that uses `return()`, `<<-`, `...`, or a function that inspects its caller's
frame, such as `missing()` or `parent.frame()`. Inlining is refused when a name the value reads is
reassigned before the last use, and when a value with a call would run more than once.

ry's own syntax, from [records and tags](/type-checking/records-and-tags#in-the-editor) to
[DataFrame](/type-checking/dataframes) columns, gets the same features as plain R.

## R Markdown and Quarto

In `.Rmd`, `.qmd`, and `.Rnw` documents, the R chunks get diagnostics, hover, completion,
go-to-definition, references, rename, and formatting. The chunks are analyzed together as one
script, so a name defined in a setup chunk resolves in every later chunk and in inline
`` `r expr` `` code. Formatting rewrites only the R chunks and never touches the prose, and
completion stays quiet in the prose. VS Code sends these documents to ry whichever extension
(vscode-R, Quarto, or none) gives them their language.

## Running and debugging

The VS Code extension runs your code on ry's runtime.

- **Run file** runs the current script with `ry run`, or a testthat file with `testthat::test_file()`
  after loading your package.
- **Send to console**: `Ctrl+Enter` (`Cmd+Enter` on macOS) sends the selection, or the current line
  and moves to the next one, to a "ry console" terminal running `ry run`. `Ctrl+Alt+F` sends the
  function around the cursor, and "ry: Source File in Console" sends the whole file.
- **Debugging**: press `F5` in an R file, or click "Debug test" above a test. You get breakpoints,
  stepping in, over and out, the call stack, each frame's variables, and evaluation in the selected
  frame. `browser()` stops in the debugger, and so does an uncaught error, where it was raised.
  Breakpoints in your package's `R/` files work while you debug one of its tests, because the
  package is loaded with source references for the session.

- **Attaching to the console**: "ry: Attach to Console" attaches the debugger to the `ry run`
  console running in your workspace, starting one if none runs. Breakpoints in your files then stop
  the commands you type, and `debug(f)` or `debugonce(f)` stops when `f` is next called, even for a
  function you typed at the prompt. Attaching while the console waits at a `browser()` stop's
  `Browse[1]>` prompt takes that stop over. Stopping the debug session leaves the console running.
- **Viewing data**: right-click a data frame, matrix, vector or `DataFrame` in the debugger's
  variables view and choose "View Data", or call `View(x)` in the console. The viewer fetches
  100 rows at a time, so a large table opens instantly. Click a column header to sort by it
  (again for descending, a third time for the original order), and type in "Filter rows" to keep
  the rows where some cell contains the text, ignoring case. Both apply to the whole table in the
  console, not only to the rows on screen; missing values sort first and match no filter, and each
  row keeps its original row name or number.

While a console runs in the workspace, hover and completion also use its live values: hovering
`df` shows its class and size, and `df$` completes the columns of the data frame you actually
loaded, which the type checker cannot know when the data comes from a file.

On Windows, the console has no editor connection yet, so attaching, `View()` and live values
are not available there.

Other editors can launch the same debugger as `ry debug-adapter`, a
[Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/) server on standard
input and output. Its launch arguments are `program` (the script), `args`, `stopOnEntry`, `prelude`
(R code run first, such as `load_all("."); library(testthat)`) and `testName` (run only the
`test_that()` block with that description). To attach to a console instead, connect to the Unix
socket named in `$TMPDIR/ry-<uid>/console-<pid>.json` and send `initialize` and then `attach`, as
to any debug adapter. The data viewer's `ryView` and `ryTable` requests go to the debugger
connection while the program is stopped, and to a second connection while the console waits for
input.

## Settings

The server reads the same `ry.toml` as the command line, so the editor and CI report the same
findings. Every editor setting is listed under
[editor settings](/reference/configuration#editor-settings).
