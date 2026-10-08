---
title: Limitations
description: What ry does not check, and where it reports correct code
---

When ry cannot determine a type, the value becomes `Unknown`, which is compatible with everything, so
most gaps on this page are checks that are silently skipped, and a clean run proves less than it
seems. [Strict mode](/type-checking/tour#unknown-and-strict-mode) reports where that happens. The
last section lists the places where ry reports correct code instead.

## Data frames

Columns have no types, so everything read out of a data frame is `Unknown`:

```r
#: fn(df: data.frame) -> integer
count_rows <- function(df) df$whatever
```

This passes, although the column may not exist. It is the gap that matters most for analysis code,
because analysis code lives in data frames: `df: data.frame` checks that a data frame arrives, not
which columns it has. `data.frame()` itself returns `Unknown`, so it is usually the first place
strict mode reports in data code.

Matrices have the same gap one level down: matrix arithmetic is checked, but dimensions are not, so a
non-conformable product goes unreported.

## Object systems

S4 and R6 objects are `Unknown`, and calls to an S3 generic that dispatches through `UseMethod()`
return `Any`, because their behavior is decided at run time from class attributes. Strict mode
reports S4 objects, but not R6 objects or S3 calls. Operator methods such as `+.Date` and methods
called directly are checked, and `structure(x, class = "dog")` keeps `x`'s type, because a class
attribute is data. The [tour](/type-checking/tour#structural-and-nominal-types) shows how to
give your own classes checked types.

## Packages

Names from a package with no [stub](/type-checking/stubs) are `Unknown`, and attaching such a package
switches off unresolved-name reports for the whole project.

A project's own `%op%` operators are left untyped, because one may quote its right operand instead of
evaluating it, as magrittr's `%>%` does, and checking it as a call would reject correct code.

## What is analyzed together

`source()` is not followed, so a function from a sourced file is reported as unresolved where you
call it, and as unused where you define it. Moving shared helpers into `R/`, with a `ry.toml` at the
project root, makes them visible to every script. Files under `R/` share one namespace, and every other file is analyzed
on its own, seeing its own definitions plus the package's. Files directly under `tests/testthat/` are
analyzed as one environment with the helpers first, which is more permissive than testthat: a name
one test file defines and another uses resolves in ry and fails when the tests run.

`ry check` reads the R chunks of `.Rmd`, `.qmd`, and `.Rnw` documents, but the language server does
not analyze them and `ry fmt` skips them.

## Where correct code is reported

These are known bugs, not design decisions:

- **Mixed numbers into an unannotated function.** `f <- function(price, rate) price * rate` infers
  one type for both parameters, so `f(1L, 0.5)` is reported. Separately, an unannotated parameter used
  in arithmetic rejects a `logical`, so `g <- function(x) x * 2; g(TRUE)` is reported. Annotating the
  parameters as `double` fixes both.
- **A vector meeting an unannotated parameter.** In `g <- function(x) x + c(1L, 2L)`, `x` is assumed
  to have length one, so `g(c(1, 2))` is reported. Annotate `x` as `double[]`.
- **A list filled by position.** `res <- list(); res[[i]] <- x` in a loop infers a dict-like list, and
  a later `res[[1]]` is reported. Annotate the empty list as `#: list[T]`.
- **The loop variable after a loop.** At the top level of a file, reading `i` after
  `for (i in xs) {}` is reported as unresolved, although R keeps its last value.
- **A variable named like a function.** After `mean <- 3`, the call `mean(x)` is reported as calling a
  number, although R skips non-function bindings when it looks up a function.
- **Column names in tidyselect and masking functions of untyped packages.** Only packages with typed
  declarations (dplyr, data.table, ggplot2, testthat, and base R) mark which arguments are column
  names, so after `library(tidyr)`, `pivot_longer(d, cols = c(a, b))` reports `a` and `b`. A top-level
  `utils::globalVariables(c("a", "b"))` silences them, as it does for `R CMD check`.
- **A trailing comma for rlang's dynamic dots.** Functions built on rlang, such as dplyr's verbs,
  accept `mutate(d, y = 1, )`, but ry reports the trailing comma as an error, as it would for
  `c(1, 2, )`.
- **Package-qualified calls outside a package.** `pkg::fun()` reports an unknown namespace unless
  some file in the project calls `library(pkg)`, and for a package ry has no stub or export list for,
  even then. In a package, the same report for a package missing from `DESCRIPTION` is correct,
  because R CMD check warns about it too.
- **Strict mode.** `if (x < 0) stop("negative")` reports the `stop()` call as an undetermined type.
  No annotation on a binding clears a strict finding yet; `# ry: allow(strict)` does.
