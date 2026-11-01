---
title: Why ry
description: What ry gives you that other R tools do not, and the trade-offs behind it
---

## How fast it runs

ry's own runtime (`ry run --runtime ry`) interprets R and compiles hot loops to machine code. Each
program below exists twice in `tools/bench/why/`: as plain R, and with inline annotations checked
in [strict mode](/type-checking/strict-mode). The times are the best of three runs after one warm-up
run, measured inside the script, so they leave out startup and, for the typed column, the static
check. All three columns compute the same result.

| program | GNU R, untyped | ry, untyped | ry, typed and strict | ry vs GNU R |
| --- | ---: | ---: | ---: | ---: |
| closures | 0.782 s | 0.381 s | 0.405 s | 2.1x |
| drawdown | 0.309 s | 0.021 s | 0.021 s | 14.9x |
| fib | 0.081 s | 0.088 s | 0.090 s | 0.9x |
| log_rank | 0.099 s | 0.063 s | 0.062 s | 1.6x |
| loop_sum | 0.247 s | 0.047 s | 0.047 s | 5.2x |
| vasicek | 0.036 s | 0.041 s | 0.041 s | 0.9x |
| vector_arith | 0.177 s | 0.122 s | 0.121 s | 1.5x |

Measured on an Intel Xeon at 2.10GHz with 4 cores, Linux 6.18, against GNU R 4.3.3 with its
bytecode compiler on. `just bench-why` builds ry and regenerates the table on your machine.

Loops and vector code run several times faster than on GNU R. Code dominated by small function
calls (`fib`) or by the random number generator (`vasicek`) is still about 10% slower. Types do not
make code faster yet: a static `double` still admits an integer or a logical through promotion, so
the compiled code keeps its runtime checks, and checking annotations at each call costs a little,
as `closures` shows.

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

You write annotations inline, such as `function(x: integer) -> integer`, or as `#:` comments, and
most typed examples in the docs offer both. Inline annotations are on by default, at the price that plain
`Rscript` cannot run the file unless it goes through `ry run`. Comments keep the file plain R that
every other tool can read.

## Data that the checker understands

R has no good way to say "a record with these fields" or "either a success or an error". ry adds both,
and the checker follows them exactly:

```r
alias Shape = :Circle(radius: double) | :Rect(width: double, height: double) | :Empty

area <- function(s: Shape) -> double {
  match (s) {
    :Circle(radius = r) => pi * r^2
    :Rect(width = w, height = h) => w * h
  }
}
```

```text
non-exhaustive-match

  x this `match` does not handle `:Empty`: add an arm for it, or a catch-all `_ => ...` arm
```

[Records, tuples and tags](/type-checking/records-and-tags) replace the S4, R6 and Reference Class
code that nothing can check. A [standard library](/reference/standard-library) modeled on Roc's
(`Result`, `List`, `Str`, `Num`, `Dict`, `Set`) returns these values instead of `NA` or a warning,
and [frames](/type-checking/frames) are a built-in table whose columns the checker knows, so a
misspelled column in a query is an error before the query runs. All of this runs on
ry's own runtime (`ry run --runtime ry`) and not on GNU R.

## Fast enough for every keystroke

ry is written in Rust, and after an edit it re-checks only what the edit could affect, so findings
keep up with your typing. Every rule in the type system has to be cheap enough to run on each
keystroke, which explains the trade-offs below.

## What it leaves out

- **Several signatures for one of your functions.** That would turn every call into a search. A
  parameter can accept either of two types instead, written `integer | character`.
- **Inheritance.** One type extending another makes inference slow and its results hard to predict.
  [Custom types](/type-checking/custom-types) keep values apart without it.
- **What it cannot model.** S4 and R6 objects and data frame columns get a type that is compatible
  with everything, so they never cause a false error, but they are not checked either.
  [Strict mode](/type-checking/strict-mode) shows where that happens.

A clean run therefore says less about code built on data frames or R6 than about code built from
functions and lists. [Limitations](/type-checking/limitations) lists what is not checked.

## Where ry is going

ry is growing into a backwards-compatible implementation of R itself, so that existing R code and
packages run unchanged with static typing on top. Today it is a type-checked R toolchain: a checker,
a language server, a formatter, a loader that runs a package from its sources, and a console that
runs your installed R. Beside these, the first parts of a runtime exist. There is an ABI-compatible C API for compiled
packages, and an interpreter (`ry run --runtime ry`) that already checks
[inline annotations](/type-checking/inline-annotations) while it runs. The runtime is incomplete: it still reads base R from your GNU R installation, and a
script that uses something it lacks stops with an error that names it. For real work, run on GNU R.

## Status

ry is in beta. Diagnostic codes, `ry.toml` keys, and the JSON output are stable, so CI built on them
keeps working. The type system is still growing, so a new release can report findings an older one
did not, which is why CI should pin the version.
