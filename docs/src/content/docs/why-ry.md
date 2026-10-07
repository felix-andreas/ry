---
title: Why ry
description: What ry checks that other R tools cannot, and the trade-offs it makes
---

## Checking code before it runs

RStudio completes the columns of a data frame by looking at the object in your R session, so it
knows only about code that has already run. ry reads the source instead. It can check code that has
never run, it gives the same answers in your editor and in CI, and it needs no R installation.

What it checks is what you would otherwise learn only when the code runs: whether a name is defined,
whether a call matches the function it calls, and whether a value can be `NULL` where it is used.

## Speed

ry is written in Rust, and its analysis is incremental: after an edit, it re-checks only what the
edit could affect. The target is a re-check within 30 ms of a keystroke for half of all edits, and
within 100 ms for 95 percent of them, measured on the largest package in a 965,000-line corpus of
CRAN code. Every rule in the type system has to be cheap enough to run on every keystroke, which is
the reason for the trade-offs below.

## Types without annotations

A function that multiplies its argument needs a number, but R finds out only when the line runs.
Requiring an annotation on every function would rule out existing code, so ry infers types from how
values are used, with Hindley–Milner inference, the approach OCaml and Haskell take:

```r
discount <- function(price, rate) price * (1 - rate)
discount("a", 0.2)   # error: `*` needs a number
```

Nothing in this example is declared. The annotations you do write are `#:` comments, so the file
stays plain R, and type errors are reported only in projects or files that turn them on.

## What it leaves out

- **Overloading your own functions.** Several signatures per name would make every call a search over
  candidates. A union type or two functions cover the same cases.
- **Inheritance.** Subtyping between declared types makes inference slow and its results hard to
  predict. [Nominal types](/type-checking/tour#structural-and-nominal-types) keep values apart
  without it.
- **Guessing.** What ry cannot describe, such as S4 and R6 objects, data frame columns, and `eval()`,
  becomes `Unknown`, which is compatible with everything and so never causes an error by itself.
  [Strict mode](/reference/type-system#strict-mode) lists these places.

So a clean run says less about code built on data frames or R6 than about code built from functions
and lists. [Limitations](/type-checking/limitations) lists what is not checked.

## Status

ry is in beta. Diagnostic codes, `ry.toml` keys, and the JSON output are stable, so CI built on them
keeps working. The type system is still growing, so a new release can report findings an older one
did not, which is why CI should pin the version.
