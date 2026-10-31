---
title: Limitations
description: What ry does not check, and where it reports correct code
---

When ry cannot determine a type, the value becomes `Unknown` and its checks are skipped, so most
gaps on this page are silent. [Strict mode](/type-checking/unknown) shows where they occur. The last
section lists the places where ry reports correct code instead.

## Not checked

- **Data frame columns.** Everything read out of a data frame is `Unknown`, so `df$whatever` passes
  even if the column does not exist.
- **Matrix dimensions.** Matrix arithmetic is checked, but a non-conformable product is not.
- **S4 and R6 objects** are `Unknown`, and calls to an S3 generic that dispatches through
  `UseMethod()` return `Any`. Operator methods such as `+.Date` and methods called directly are
  checked. [Custom types](/type-checking/custom-types#r6-and-s4-objects) shows how to check who
  receives an object.
- **Packages without a [stub](/type-checking/stubs).** Their names are `Unknown`, and attaching one
  switches off unresolved-name reports for the whole project.
- **A project's own `%op%` operators**, because one may quote its right operand instead of
  evaluating it, as magrittr's `%>%` does.

## What is analyzed together

Files under `R/` share one namespace, and every other file is analyzed on its own, seeing its own
definitions plus the package's. `source()` is not followed, so a function from a sourced file is
reported as unresolved where you call it, and as unused where you define it. Moving shared helpers
into `R/` makes them visible to every script.

Files directly under `tests/testthat/` are analyzed as one environment with the helpers first. That
is more permissive than testthat: a name one test file defines and another uses resolves in ry but
fails when the tests run.

`ry check` reads the R chunks of `.Rmd`, `.qmd`, and `.Rnw` documents, but the language server does
not analyze them and `ry fmt` skips them.

## Where correct code is reported

These are known bugs, not design decisions:

- **Mixed numbers into an unannotated function.** `f <- function(price, rate) price * rate` infers
  one type for both parameters, so `f(1L, 0.5)` is reported. An unannotated parameter used in
  arithmetic also rejects a `logical`, so `g <- function(x) x * 2; g(TRUE)` is reported. Annotating
  the parameters as `double` fixes both.
- **A vector meeting an unannotated parameter.** In `g <- function(x) x + c(1L, 2L)`, `x` is assumed
  to have length one, so `g(c(1, 2))` is reported. Annotate `x` as `double[]`.
- **A list filled by position.** `res <- list(); res[[i]] <- x` in a loop infers a dict-like list,
  and a later `res[[1]]` is reported. Annotate the empty list with its element type, such as `#: list[integer]`.
- **The loop variable after a loop.** At the top level of a file, reading `i` after
  `for (i in xs) {}` is reported as unresolved, although R keeps its last value.
- **A variable named like a function.** After `mean <- 3`, the call `mean(x)` is reported as calling
  a number, although R skips non-function bindings when it looks up a function.
- **Column names in packages without typed declarations.** After `library(tidyr)`,
  `pivot_longer(d, cols = c(a, b))` reports `a` and `b`. A top-level
  `utils::globalVariables(c("a", "b"))` silences them, as it does for `R CMD check`.
- **A trailing comma for rlang's dynamic dots.** dplyr's verbs accept `mutate(d, y = 1, )`, but ry
  reports the trailing comma as it would for `c(1, 2, )`.
- **Package-qualified calls outside a package.** `pkg::fun()` reports an unknown namespace unless
  some file in the project calls `library(pkg)`, and for a package ry has no stub or export list for,
  even then.
- **A `stop()` guard in strict mode.** `if (x < 0) stop("negative")` reports the `stop()` call as an
  undetermined type.
