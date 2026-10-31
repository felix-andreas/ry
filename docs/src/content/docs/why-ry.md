---
title: Why ry
description: What ry gives you that other R tools do not, and the trade-offs behind it
---

## Mistakes show up before the code runs

R reports an undefined name, a call with the wrong arguments, or a `NULL` where a value is needed
only when that line runs, which can be hours into a job. ry reads the source instead, so it reports
these as you type. It never runs your code and, apart from the console, needs no R installation, and your editor and CI read
the same `ry.toml`, so they check by the same rules.

## Types without rewriting your code

Requiring an annotation on every function would rule out the R code that already exists, so ry
infers types from how values are used:

```r
discount <- function(price, rate) price * (1 - rate)
discount("a", 0.2)   # expected a numeric value (`integer` or `double`), found `character`
```

The annotations you do write are `#:` comments, so the file stays plain R that every other tool can
read.

## Fast enough for every keystroke

ry is written in Rust, and after an edit it re-checks only what the edit could affect. The target is
a re-check within 30 ms of a keystroke for half of all edits and within 100 ms for 95 percent of
them, on a 300,000-line project. Every rule in the type system has to be that cheap, which explains
the trade-offs below.

## What it leaves out

- **Several signatures for one of your functions.** That would turn every call into a search. A
  parameter can accept either of two types instead, written `integer | character`.
- **Inheritance.** One type extending another makes inference slow and its results hard to predict.
  [Custom types](/type-checking/custom-types) keep values apart without it.
- **What it cannot model.** S4 and R6 objects and data frame columns get a type that is compatible
  with everything, so they never cause a false error, but they are not checked either.
  [Strict mode](/type-checking/unknown) shows where that happens.

A clean run therefore says less about code built on data frames or R6 than about code built from
functions and lists. [Limitations](/type-checking/limitations) lists what is not checked.

## Where ry is going

ry is growing into a backwards-compatible implementation of R itself, so that existing R code and
packages run unchanged with static typing on top. Today it is the tooling: a checker, a language
server, a formatter, and a console that runs your installed R.

## Status

ry is in beta. Diagnostic codes, `ry.toml` keys, and the JSON output are stable, so CI built on them
keeps working. The type system is still growing, so a new release can report findings an older one
did not, which is why CI should pin the version.
