---
title: Checking code
description: What ry check reports without any configuration, and how to silence a finding
---

`ry check` reads every `.R` file in the project, plus the R chunks of `.Rmd`, `.qmd`, and `.Rnw`
documents, and reports problems without running anything. This page covers what it finds with no
configuration. Type errors are opt-in and have [their own section](/type-checking/overview).

## Names

Every name is resolved by R's scoping rules, so a typo is reported where you wrote it rather than
when execution reaches it:

```text
unresolved

  ! I could not resolve `ratee` in this package, its imports, or builtins. Did you mean `rate`?
```

Files in a package's `R/` share one namespace, a script is read from top to bottom, and `library()`
calls, `DESCRIPTION` dependencies, and `NAMESPACE` imports bring names into scope. The same analysis reports an assignment that
nothing reads, a top-level name defined twice in a package (R silently keeps one of them),
`pkg::name` where the package does not export `name`, and a `NAMESPACE` that imports or exports a
name that does not exist.

Inside data-masking functions such as dplyr's `filter()` and `mutate()`, `data.table`'s `[`, and base
`with()`, `subset()`, and `transform()`, a bare name refers to a column, so it is not reported as
unresolved:

```r
library(dplyr)

by_region <- function(sales) {
  sales |>
    filter(amount > 0) |>
    mutate(net = amount * (1 - fee))
}
```

## Syntax errors

R's parser stops at the first error and names the token it choked on. ry names what is missing and
keeps analyzing the rest of the file:

```r
config <- list(
  title = "Revenue"
  subtitle = "by quarter"
)
```

```text ry check
syntax-error

  x missing `,` between these arguments
   --[R/config.R:2:20]
 1 | config <- list(
 2 |   title = "Revenue"
   |                    ^
```

R would stop with `unexpected symbol` and point at `subtitle`.

## Lints

Three lints are on by default. Here they are in a script, with the `unused` finding for each
assignment left out:

```r
total = 2L
flag <- T
x <- c(1, 2, )
```

```text ry check
assignment-operator

  ! Use <-, not =, for assignment

boolean-shorthand

  ! Use TRUE, not T, for Boolean values

trailing-comma

  ! Unexpected comma after last argument
```

The trailing comma is a warning: R passes it as an empty argument, which breaks `c(1, 2, )` when it
runs but is harmless to a function that never evaluates it.
[Diagnostic codes](/reference/diagnostic-codes) lists every code with its reason, and
[configuration](/reference/configuration) shows how to turn each one on or off.

## Silencing one finding

Every finding carries a code, and naming it in a comment silences it:

```r
y = 3L  # ry: allow(assignment-operator)
```

A suppression covers its own line and the line below, so it can sit at the end of the line or above
it. To turn a check off everywhere, set it in [`ry.toml`](/reference/configuration).

## In CI

`ry check` exits with status 1 when it finds anything, and `ry fmt --check` does when a file is not
formatted, so a CI job needs only those two commands. Pin the ry version, because a newer release
can report findings an older one did not. `ry check --output json` gives machine-readable output, as
the [command reference](/reference/cli#json-output) describes.
