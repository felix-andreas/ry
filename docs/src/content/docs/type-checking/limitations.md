---
title: Limitations
description: What ry does not check, and how much that weakens a clean run
---

When ry cannot determine a type, the value becomes `Unknown`, which is compatible with everything. So
nothing on this page causes a false error. Each gap instead means checks are silently skipped, and a
clean run proves less than it seems. [Strict mode](/reference/type-system#strict-mode) reports the
places where that happens, which is how to find out how much of a file is really checked.

## Data frames

Columns have no types, so everything read out of a data frame is `Unknown`:

```r
#: fn(df: data.frame) -> integer
count_rows <- function(df) df$whatever
```

This passes, although the column may not exist. It is the gap that matters most for analysis code,
because analysis code lives in data frames: annotations on such code look protective and are not.
Typed columns need a row-type design that does not exist yet.

Matrices have the same gap one level down: matrix arithmetic is checked, but dimensions are not, so a
non-conformable product goes unreported.

## Object systems

S4 and R6 objects, and calls dispatched by `UseMethod`, are `Unknown`, because their behavior is
decided at run time from class attributes. What is checked: operator methods such as `+.Date`,
directly called methods, and `structure(x, class = "dog")`, which keeps `x`'s type because a class
attribute is data. [Domain modeling](/type-checking/domain-modeling) shows how to give your own
classes checked types.

## Packages without stubs

Names from a package with no [stub](/type-checking/stubs) are `Unknown`, and attaching such a package with `library()`
stops unresolved-name reports across the whole project, because any name might be one of its
exports. A two-line stub file restores both.

A data-masking function from a package ry does not know, called as `pkg::fun()`, reports its column
names as unresolved. A top-level `utils::globalVariables(c("a", "b"))` silences them, as
it does for `R CMD check`.

A project's own `%op%` operators are left untyped, because one may quote its right operand instead of
evaluating it, as magrittr's `%>%` does, and checking it as a call would reject correct code.

## One signature per function

Your own functions have exactly one signature. Several signatures per name would turn every call into
a search over candidates. A union parameter, as in `fn(x: integer | character) -> character`, or two
functions with different names, covers most cases. The shipped stubs do overload some functions, so
`sum(1L, 2L)` is `integer` and `sum(1.5, 2.5)` is `double`.

## What is analyzed together

`source()` is not followed. Files under `R/` share one namespace, and every other file is analyzed
on its own, seeing its own definitions plus the package's. Files directly under `tests/testthat/` are analyzed as one environment with the helpers
first, which is more permissive than testthat: a name one test file defines and another uses resolves
in ry and fails when the tests run.
