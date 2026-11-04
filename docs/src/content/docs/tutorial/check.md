---
title: Check your code
description: Run ry check and ry fmt on an existing R project, with no configuration and no annotations
sidebar:
  label: 1. Check your code
---

**Goal:** find the mistakes in an R project that R reports only when a line runs, then format the
project. There is nothing to configure and nothing to annotate, and the code keeps running on GNU R.
[Install ry](/installation) first.

## Find mistakes

Take a package whose `R/discount.R` has a typo:

```r
apply_discount <- function(price, rate) {
  price * ratee
}
```

Run `ry check` anywhere in the project. It works the same in a folder of scripts with no
`DESCRIPTION`:

```text ry check
$ ry check
unresolved

  ! I could not resolve `ratee` in this package, its imports, or builtins. Did you mean `rate`?
   --[R/discount.R:2:11]
 1 | apply_discount <- function(price, rate) {
 2 |   price * ratee
   |           ^^^^^
 3 | }
  help: Change to `rate` (`ry check --fix --unsafe-fixes` applies it)

1 problem in 1 file
1 fixable with `ry check --fix --unsafe-fixes`
```

A finding starts with its code (`unresolved`), then `!` for a warning or `x` for an error. The exit
status is 1 when anything is found. [Fix and silence](#fix-and-silence) covers the `help:` line.

ry resolves every name by R's scoping rules, so it knows what each file can see:

| File | Sees |
| --- | --- |
| `R/*.R` | one package namespace, plus `library()` calls and the imports in `DESCRIPTION` and `NAMESPACE` |
| a script | its own definitions, the package's, and every file it `source()`s by a literal path |
| `tests/testthat/*.R` | the package, plus `helper-*.R` and `setup-*.R` |
| `.Rmd`, `.qmd`, `.Rnw` | its R chunks in order, as one script |

The same analysis reports an assignment nothing reads, a top-level name defined twice, `pkg::name`
where `pkg` does not export `name`, and a `NAMESPACE` entry that does not exist. Inside dplyr verbs,
data.table's `[` and `with()`, a bare name is a column, so it is not reported.

After `library(pkg)` of a package ry has no stub for, any bare name might be one of its exports,
so ry stops reporting unresolved names in the whole project. A [stub](/tutorial/types#unknown)
turns the check back on.

## Syntax errors

For this code, R says `unexpected symbol` and stops. ry names what is missing and keeps analyzing
the rest of the file:

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

Eleven lints are on by default. Three of them flag style that hides bugs, here in `R/rates.R`:

```r
total = 2L
flag <- T
x <- c(1, 2, )
print(totl)
```

```text ry check
assignment-operator

  ! Use <-, not =, for assignment

boolean-shorthand

  ! Use TRUE, not T, for Boolean values

trailing-comma

  ! Unexpected comma after last argument
```

The other eight flag code that reads right but does something else:

| Code | Flags | Why |
| --- | --- | --- |
| `equals-na` | `x == NA` | always `NA` |
| `vector-logic` | `&` or `\|` in an `if` | evaluates both sides |
| `length-sequence` | `1:length(x)` | counts down to 0 when `x` is empty |
| `class-equals` | `class(x) == "data.frame"` | misses a tibble |
| `length-test` | `length(x == 0)` | measures the comparison |
| `unreachable-code` | code after `return()` or `stop()` | never runs |
| `sprintf-arguments` | `sprintf("%s %s", a)` | wrong number of values |
| `object-system` | an S4, R6 or Reference Class definition | [records and tags](/tutorial/runtime#records-tags-and-match) are checked |

Opt-in style lints, such as `single-quotes` and `redundant-return`, are listed in
[Diagnostic codes](/reference/diagnostic-codes).

## Fix and silence

On `R/rates.R` above, `ry check --fix` applies the safe fixes (`=` to `<-`, `T` to `TRUE`, the
trailing comma) and reports what is left. A spelling suggestion is a guess, so it needs
`--unsafe-fixes` as well:

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

To accept one finding, name its code in a comment on the line or the line above:

```r
y = 3L  # ry: allow(assignment-operator)
```

lintr's `# nolint` comments work too. To turn a check off for the project, set it in
[`ry.toml`](/reference/configuration).

## Format

`ry fmt` formats every file in place:

```r
x<-c(1,2,3)
if(x>1){y<-2}
for (i in 1:3) print(i)
```

```r
x <- c(1, 2, 3)
if (x > 1) { y <- 2 }
for (i in 1:3) {
  print(i)
}
```

Every loop body gets braces. There is no column limit: a call you wrote on one line stays on one
line, and a call you broke across lines gets one argument per line. `ry fmt --range 2-3 FILE`
formats only those lines. [Formatting rules](/reference/formatting-rules) has every rule.

## In CI

```sh
ry check        # exit status 1 on any finding
ry fmt --check  # exit status 1 if a file is not formatted
```

Pin the ry version in CI, because a newer release can report findings an older one did not.
`ry check --output json` writes one JSON object per finding ([CLI](/reference/cli#json-output)).

## In the editor

The language server shows the same findings as you type, with hover, completion, go to definition,
references, rename and quick fixes for the same fixes. [Installation](/installation#editors) sets it
up.

**Next:** [type your code](/tutorial/types).
