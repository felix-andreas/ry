---
title: Limitations
description: What ry cannot check yet, and which gaps matter before you adopt it
---

ry is a type checker for a language that was not designed to have one. Some of R resists static
analysis, some of it is simply not built yet, and the difference matters when you decide how much to
trust a clean run.

One rule makes the rest of this page readable: **when the checker cannot determine a type, the value
becomes `Unknown`, and `Unknown` is compatible with everything.** One unmodelled construct therefore does not cascade into a
screen of errors. Every gap below works the same way: a check is skipped, and no wrong answer is
produced.

Strict mode makes most of those skips visible:

```toml
# ry.toml
[check]
typing = true
strict = true
```

It reports every place a value became `Unknown`, and every call whose result the shipped
declarations could not describe, such as `min()` on a classed value. It is how you find out how much
of a file is actually checked. That includes the attached-package tolerance described
[below](#non-standard-evaluation), which it reports as an undetermined read rather than leaving
silent.

## Data frames

**Everything you take out of a `data.frame` is `Unknown`.** Columns have no types, so this is
accepted:

```r
#: fn(df: data.frame) -> integer
count_rows <- function(df) df$whatever
```

`df$whatever` is `Unknown`, `Unknown` satisfies `integer`, and the annotation reports nothing. The
column may not exist, and would not be an integer if it did.

This is the gap that most affects analysis code, because the data frame is where analysis code
lives. Typed column vocabularies need a row-type design that does not exist yet. Until it does,
annotations on data-frame-heavy code look protective and are not, and `strict = true` is the only
way to see it.

Matrices have the same gap one level down. `%*%`, `%o%` and `%x%` produce a `matrix`, and matrix
arithmetic and comparison are checked, but **dimensions are not tracked**. A non-conformable product
or a transposed dimension is invisible.

## R's object systems

**S3 is largely covered; S4 and R6 are opaque.** Operator dispatch (`+.Date`, `Ops.Class`) resolves
statically, a directly called method is just a function, and `structure(x, class = "dog")` keeps
`x`'s type because a class attribute is data rather than a type. `UseMethod` dispatch, and
everything in S4 and R6, is `Unknown`.

The full table is in the reference under
[object systems](/reference/type-system#object-systems-s3-s4-r6), which also explains why the line
falls there. What does work today is declaring the class yourself. See
[domain modeling](/type-checking/domain-modeling).

## Non-standard evaluation

`dplyr`, `data.table`, `ggplot2` and `testthat` have shipped stubs, and their data-masked verbs are
understood: bare column names inside `mutate()` or a `data.table` bracket are column references, not
unresolved variables, and classes flow through a pipeline. A full
`read.csv |> mutate |> filter` chain checks clean.

Outside those, a data-masking function ry does not know about reports its column names as
unresolved. A top-level `utils::globalVariables(c("a", "b"))` silences them for the whole package.

**Attaching a package ry does not know weakens the `unresolved` check.** A `library(pkg)` whose
exports ry cannot enumerate means any bare name *could* be one of them, so otherwise-unresolved bare
names are tolerated rather than reported, project-wide rather than only in that file. Where this
applies, a clean run says less than it appears to.

Three things narrow it:

1. **Most packages are already known.** ry ships the export lists of the packages R code attaches
   most. See [what ships](/type-checking/stubs#what-ships). Attaching any of them keeps the check
   fully on, so a real export resolves and a typo beside it is still reported with a suggestion.
2. **A near miss of a name in scope is still reported.** `library(shiny)` cannot explain `repositry`
   sitting next to a `repository` parameter, so that stays a finding. In a package this covers your
   top-level definitions too; in a loose script it covers locals and parameters.
3. **`library(yourpkg)` costs nothing.** A `library()` naming your own package weakens nothing, so
   the one in `tests/testthat.R` is harmless.

The way to close it is a two-line [`stubs/<pkg>.Rtypes`](/type-checking/stubs), which also silences
the unknown-namespace warning. **Strict mode surfaces it.** A tolerated read is genuinely
undetermined, so under `[check] strict` each one is reported where it is read, naming the package
declaration as the fix. Ordinary runs stay silent, because otherwise every export of an unstubbed
`library()` would be a false `unresolved`.

A project's own `%op%` is left opaque. It may be a wrapper whose right operand is quoted rather than
evaluated, as `%>%` from magrittr is, and checking that as an ordinary call would reject correct
code.

## One signature per function

A `#:` annotation declares exactly one signature, and there is no way to give your own function
several. The standard-library declarations do have them, so `sum(1L, 2L)` is
`integer` and `sum(1.5, 2.5)` is `double`. That asymmetry is deliberate rather than pending.

A name with several signatures has no single most general type, so a call to it must be resolved by
trying candidates rather than inferred outright, which gives up the principal-type guarantee. Where
you would reach for an overload, a [union](/reference/type-system#union-types) parameter usually
says the same thing, as `fn(x: integer | character) -> character` does, and two functions with
distinct names always do.

## Scope of analysis

`source()` calls are not followed. Package files under `R/` share one namespace; every other file is
analyzed on its own. See [project discovery](/reference/configuration#project-discovery) for the
rules.

Files directly under `tests/testthat/` are an **approximation**: ry analyzes them as one shared
environment, helpers first. Real testthat only shares `helper*.R` and `setup*.R` that way, and each
`test-*.R` runs in its own child environment. So a name one test file defines and another uses will
resolve here and fail when you actually run the tests. The approximation is deliberate, because it
keeps helper-defined names resolving, but it is more permissive than testthat is.

---

[Adopting an existing codebase](/guides/adopting) covers how to turn ry on a piece at a time, and
[project status](/why-ry#project-status) covers how far along the project is.
