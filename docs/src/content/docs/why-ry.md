---
title: Why ry
description: The case for a static, fast, type-checking toolchain for R, and the trade-offs it makes
---

## Answers without running code

R's existing editor support learns what your values are by asking a live R session. That is how
RStudio can complete the columns of a data frame you loaded a minute ago. It is also the limit: a
session only knows code that has already run, in whatever state it is in. It cannot tell you about
the branch you have not taken, the function nobody has called yet, or the file in someone else's pull
request.

ry works from the source alone. Its answers are the same in your editor, in CI, and on code that has
never run, and it needs no R installation to give them. The questions it answers are the ones a
session cannot: is this name defined anywhere, does this call match its function, can this value be
`NULL` here.

## Speed

A check slow enough to interrupt you stops being run. ry is written in Rust, and its analysis is
incremental: an edit re-checks only what it could have affected, so a keystroke in a large project
costs milliseconds rather than a full pass. It is tested against roughly 970,000 lines of real R from
69 CRAN packages and R's base library.

Speed also shapes the type system. Every rule has to be decidable quickly enough to run on every
keystroke, which is the reason for the trade-offs below.

## Types without annotations

Python, JavaScript, and Ruby all added optional type checkers once their codebases grew past what
running the program could verify. R code relies on types just as much (a function that multiplies its
argument needs a number), but nothing checks that until the line runs.

Requiring annotations everywhere would make that check useless for existing R. So ry infers types
from how values are used, with Hindley–Milner inference, the same family as OCaml and Haskell:

```r
scale <- function(x, factor) x * factor
scale("a", 2)       # error: `*` needs a number
```

Nothing was declared. The annotations you do write go in `#:` comments, so the file stays plain R for
every other tool, and type errors are opt-in per project or per file.

## What it deliberately leaves out

Three choices keep the checker fast and its answers trustworthy:

- **No overloading of your own functions.** A name with several signatures turns each call into a
  search over candidates. Use a union type, or two functions.
- **No class hierarchies.** Subtyping between classes makes inference expensive and its results hard
  to predict. Named types in `#:` comments give you distinct types without inheritance.
- **`Unknown` instead of guesses.** What cannot be described statically (S4 dispatch, R6 objects,
  data frame columns, `eval`) becomes `Unknown`, which is compatible with everything. A gap means a
  check was skipped, never a false error, and [strict mode](/reference/type-system#strict-mode) shows
  where the gaps are.

The consequence is that a clean run proves less on code heavy in data frames or R6 than on code built
from functions and lists. [Limitations](/type-checking/limitations) says exactly where.

## Status

ry is beta software. Diagnostic codes, `ry.toml` keys, and the JSON output are stable, so CI built on
them keeps working. The type system still grows, so a new release can report findings an older one
did not: pin the version in CI.

`ry check` also reads the R chunks of `.Rmd`, `.qmd`, and `.Rnw` documents. The language server does
not handle those yet.
