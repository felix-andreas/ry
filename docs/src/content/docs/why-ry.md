---
title: Why ry
description: What ry checks that other R tools cannot, and the trade-offs it makes
---

## Checking code before it runs

R reports an undefined name, a call that does not match its function, or a `NULL` where a value is
needed only when that line runs, possibly hours into a job. ry reads the source instead, so it
reports these as you type. It gives the same answers in your editor and in CI, and it needs no R
installation.

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
discount("a", 0.2)   # expected a numeric value (`integer` or `double`), found `character`
```

The annotations you do write are `#:` comments, so the file stays plain R, and type errors are
reported only in projects or files that turn them on.

## What it leaves out

- **Overloading your own functions.** Several signatures per name would make every call a search
  over candidates; the [tour](/type-checking/tour#functions) shows what to use instead.
- **Inheritance.** Subtyping between declared types makes inference slow and its results hard to
  predict. [Nominal types](/type-checking/tour#structural-and-nominal-types) keep values apart
  without it.
- **Unmodeled constructs.** What ry cannot describe, such as S4 and R6 objects, data frame columns,
  and `eval()`, becomes `Unknown`, which is compatible with everything and so never causes an error
  by itself. [Strict mode](/type-checking/tour#unknown-and-strict-mode) lists most of these places,
  though not R6 objects.

So a clean run says less about code built on data frames or R6 than about code built from functions
and lists. [Limitations](/type-checking/limitations) lists what is not checked.

## Status

ry is in beta. Diagnostic codes, `ry.toml` keys, and the JSON output are stable, so CI built on them
keeps working. The type system is still growing, so a new release can report findings an older one
did not, which is why CI should pin the version.
