---
title: Checking code
description: What ry check reports in an existing R project without any configuration, and how to silence a finding
---

`ry check` finds the mistakes R would only show you when a line runs, without running anything and
without any configuration. Run it in the project directory:

```text
$ ry check
unresolved

  ! I could not resolve `ratee` in this package, its imports, or builtins. Did you mean `rate`?
   --[R/discount.R:2:11]
 1 | apply_discount <- function(price, rate) {
 2 |   price * ratee
   |           ^^^^^
 3 | }

1 problem in 1 file
```

It reads every `.R` file in the project, plus the R chunks of `.Rmd`, `.qmd`, and `.Rnw` documents.
Your editor shows the same findings as you type ([Editors](/guides/editors)). This page covers what
`ry check` finds with no configuration; type errors are opt-in and have
[their own section](/type-checking/overview).

## Names

A misspelled name is the most common finding, and R reports it only when execution reaches it. ry
resolves every name by R's scoping rules, so the typo is reported where you wrote it, as above.

Files in a package's `R/` share one namespace, a script is read from top to bottom, and `library()`
calls, `DESCRIPTION` dependencies, and `NAMESPACE` imports bring names into scope. The same analysis
reports an assignment that nothing reads, a top-level name defined twice in a package (R silently
keeps one of them), `pkg::name` where the package does not export `name`, and a `NAMESPACE` that
imports or exports a name that does not exist.

Every file outside `R/` is analyzed on its own, seeing its own definitions plus the package's.
A script also sees the top-level definitions of every file it `source()`s by a literal path, and of
the files those source in turn. The path is tried relative to the sourcing file's directory, then
relative to the project root, where R usually runs. A sourced file reports no unused assignments,
because the script that sources it is what reads them. A computed path such as
`source(file.path(dir, "utils.R"))` is not followed, so its names stay unresolved.

Files directly under `tests/testthat/` see the package's definitions and the `helper-*.R` and
`setup-*.R` files' definitions, but add nothing to the package, so a variable a test assigns never
changes what a name means under `R/`. Each test file's own bindings stay in that file, as they do
when testthat runs it.

A `.Rmd`, `.qmd`, or `.Rnw` document is read as one script made of its R chunks in order, so a chunk
sees what earlier chunks define, and an inline `` `r expr` `` sees everything defined above it.
Chunks in other languages are treated as prose.

One gap matters from the start: after `library()` of a package ry knows nothing about, any bare name
might be one of its exports, so ry stops reporting unresolved names in the whole project.
[Package stubs](/type-checking/stubs) shows how to turn the check back on.

Inside data-masking functions such as dplyr's `filter()` and `mutate()`, `data.table`'s `[`, and
base `with()`, `subset()`, and `transform()`, a bare name refers to a column, so this code reports
nothing:

```r
library(dplyr)

by_region <- function(sales) {
  sales |>
    filter(amount > 0) |>
    mutate(net = amount * (1 - fee))
}
```

## Syntax errors

R's parser stops at the first error and names the token it choked on, so for this code it says
`unexpected symbol` and points at `subtitle`. ry names what is missing and keeps analyzing the rest
of the file:

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

## Lints

Eleven lints are on by default. Three of them catch a habit that causes bugs, and here they are in a
script, with the `unused` finding for each assignment left out:

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

`T` is an ordinary variable that any code can overwrite. The trailing comma passes an empty
argument, which breaks `c(1, 2, )` when it runs but is harmless to a function that never evaluates
it, so it is a warning. The fourth, `object-system`, warns on each S4, R6 or
Reference Class definition, because [records and tags](/type-checking/records-and-tags) model data
with every field checked.

Five more catch code that reads right but does something else: `x == NA` is always `NA`, `&` in
an `if` condition evaluates both sides, `1:length(x)` counts down to zero when `x` is empty,
`class(x) == "data.frame"` misses a tibble, and `length(x == 0)` measures the comparison instead of
comparing the length. Each fix changes what the code does, which is the point,
so it is unsafe and runs only with `--unsafe-fixes`. The last two, `unreachable-code` and
`sprintf-arguments`, flag code after a `return()` and a `sprintf()` format that wants more or fewer
values than it gets. Style lints, such as `redundant-return`, `single-quotes` and `paste-sep`, are off until
you turn them on. [Diagnostic codes](/reference/diagnostic-codes) lists every code with its
reason, and [configuration](/reference/configuration) shows how to turn each one on or off.

## Fixing findings

A finding with an obvious rewrite says so under its snippet. `ry check --fix` applies the safe ones
to your files, which are the lints above, and then reports what is left:

```text
$ ry check --fix
unresolved

  ! I could not resolve `totl` in this package, its imports, or builtins. Did you mean `total`?
   --[R/rates.R:4:7]
 3 | x <- c(1, 2)
 4 | print(totl)
   |       ^^^^
  help: Change to `total` (`ry check --fix --unsafe-fixes` applies it)

applied 3 fixes
1 problem in 1 file
1 fixable with `ry check --fix --unsafe-fixes`
```

A spelling suggestion is a guess, so it needs `--unsafe-fixes` as well. Your editor offers the same
fixes as quick fixes, and [Fixes](/reference/diagnostic-codes#fixes) lists which findings have one.

## Silencing one finding

Every finding carries a code, and naming it in a comment silences it:

```r
y = 3L  # ry: allow(assignment-operator)
```

Type findings work the same way, as in `# ry: allow(type-mismatch)`.

A suppression covers its own line and the line below, so it can sit at the end of the line or above
it. To turn a check off everywhere, set it in [`ry.toml`](/reference/configuration).

lintr's comments work too, so code written for lintr keeps its suppressions. `# nolint` silences
every finding on its line except a syntax error, `# nolint start` and `# nolint end` do the same for
the lines between them, and either can name lintr linters, as in `# nolint: seq_linter.`. A named
linter silences the ry code that reports the same thing, such as `length-sequence` for
`seq_linter`, and a linter ry has no counterpart for silences nothing. A `.lintr` file's `linters` field carries
over too, as [configuration](/reference/configuration#a-lintr-lintr-file) describes.

## In CI

`ry check` exits with status 1 when it finds anything, and `ry fmt --check` does when a file is not
formatted, so a CI job needs only those two commands. Pin the ry version, because a newer release
can report findings an older one did not. `ry check --output json` gives machine-readable output, as
the [command reference](/reference/cli#json-output) describes.
