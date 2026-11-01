---
title: Limitations
description: What ry does not check, and where it reports correct code
---

When ry cannot determine a type, the value becomes `Unknown` and its checks are skipped, so most
gaps on this page are silent. [Strict mode](/type-checking/strict-mode) shows where they occur. The last
section lists the places where ry reports correct code instead.

## Not checked

- **Data frame columns.** Everything read out of a base data frame is `Unknown`, so `df$whatever`
  passes even if the column does not exist. A [`DataFrame`](/type-checking/dataframes) types its
  columns.
- **Matrix dimensions.** Matrix arithmetic is checked, but a non-conformable product is not.
  Indexing with several indices, as in `m[i, j]`, or with `drop = FALSE` gives `Unknown`.
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

Files directly under `tests/testthat/` see the package's definitions and the `helper-*.R` and
`setup-*.R` files' definitions, but add nothing to the package, so a variable a test assigns never
changes what a name means under `R/`. Each test file's own bindings stay in that file, as they do
when testthat runs it.

`ry check` reads the R chunks of `.Rmd`, `.qmd`, and `.Rnw` documents, but the language server does
not analyze them and `ry fmt` skips them.

## Where correct code is reported

These are known bugs, not design decisions:

- **Column names in packages without typed declarations.** After `library(tidyr)`,
  `pivot_longer(d, cols = c(a, b))` reports `a` and `b`. A top-level
  `utils::globalVariables(c("a", "b"))` silences them, as it does for `R CMD check`.
- **A trailing comma for rlang's dynamic dots.** dplyr's verbs accept `mutate(d, y = 1, )`, but ry
  reports the trailing comma as it would for `c(1, 2, )`.
- **Package-qualified calls to a package ry does not know.** `pkg::fun()` reports an unknown
  namespace when ry has no stub or export list for `pkg`, even if a file calls `library(pkg)`.
- **A `stop()` guard in strict mode.** `if (x < 0) stop("negative")` reports the `stop()` call as an
  undetermined type.
- **A generic function passed inline as an argument.** `lapply(xs, function<T>(v: T) -> T v)` is
  reported, because type parameters on a function that is not bound to a name are not instantiated.
  Leave the function's types off, or bind it to a name first.
