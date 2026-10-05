---
title: Limitations
description: What ry does not check, and how much that weakens a clean run
---

When ry cannot determine a type, the value becomes `Unknown`, which is compatible with everything. So
most gaps on this page mean checks are silently skipped, and a clean run proves less than it seems.
The last section lists the few places where ry reports correct code instead. [Strict mode](/reference/type-system#strict-mode) reports the
places where that happens, which is how to find out how much of a file is really checked.

## Data frames

Columns have no types, so everything read out of a data frame is `Unknown`:

```r
#: fn(df: data.frame) -> integer
count_rows <- function(df) df$whatever
```

This passes, although the column may not exist. It is the gap that matters most for analysis code,
because analysis code lives in data frames: annotations on such code look protective and are not.
Typed columns need a row-type design that does not exist.

Matrices have the same gap one level down: matrix arithmetic is checked, but dimensions are not, so a
non-conformable product goes unreported.

## Object systems

S4 and R6 objects are `Unknown`, and calls to an S3 generic that dispatches through `UseMethod()`
return `Any`, because their behavior is decided at run time from class attributes. Strict mode reports
the first but not the second. What is checked: operator methods such as `+.Date`,
directly called methods, and `structure(x, class = "dog")`, which keeps `x`'s type because a class
attribute is data. [Domain modeling](/type-checking/domain-modeling) shows how to give your own
classes checked types.

## Packages without stubs

Names from a package with no [stub](/type-checking/stubs) are `Unknown`, and attaching such a package with `library()`
stops unresolved-name reports across the whole project, because any name might be one of its
exports. A two-line stub file restores both.

Column names passed to a data-masking or tidyselect function are reported as unresolved unless the
package has typed declarations (dplyr, data.table, ggplot2, testthat, and base R). An export list is
not enough: after `library(tidyr)`, `pivot_longer(d, cols = c(a, b))` reports `a` and `b`. A top-level `utils::globalVariables(c("a", "b"))` silences them, as
it does for `R CMD check`.

A project's own `%op%` operators are left untyped, because one may quote its right operand instead of
evaluating it, as magrittr's `%>%` does, and checking it as a call would reject correct code.

## One signature per function

Your own functions have exactly one signature (the [tour](/tour#functions) explains why). The shipped
stubs do overload some functions, so `sum(1L, 2L)` is `integer` and `sum(1.5, 2.5)` is `double`.

## What is analyzed together

`source()` is not followed. Files under `R/` share one namespace, and every other file is analyzed
on its own, seeing its own definitions plus the package's. Files directly under `tests/testthat/` are analyzed as one environment with the helpers
first, which is more permissive than testthat: a name one test file defines and another uses resolves
in ry and fails when the tests run.

## Where correct code is reported

These are known bugs, not design decisions:

- **Mixed numbers into an unannotated function.** `f <- function(price, rate) price * rate` infers
  one type for both parameters, so `f(1L, 0.5)` and `f(TRUE, 0.5)` are reported. Annotating the
  parameters as `double` fixes it.
- **A list filled by position.** `res <- list(); res[[i]] <- x` in a loop infers a dict-like list, and
  a later `res[[1]]` is reported. Annotate the empty list as `#: list[T]`.
- **A variable named like a function.** After `mean <- 3`, the call `mean(x)` is reported as calling a
  number, although R skips non-function bindings when it looks up a function.
- **Strict mode and annotated bindings.** `#: @trust double` or `#: @if-unknown double` above
  `amount <- df$amount` still leaves a strict finding on `df$amount`, so in strict mode an annotation
  cannot acknowledge an `Unknown`.
- **Package-qualified calls.** `pkg::fun()` with no `library(pkg)` or `DESCRIPTION` reports an
  unknown namespace for packages outside base R, as described under [stubs](/type-checking/stubs#what-ships).
