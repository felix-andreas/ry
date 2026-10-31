---
title: Unknown and strict mode
description: What ry does with code it cannot type, and how to see where that happens
---

R has constructs no static checker can follow: data frame columns, S4 and R6 objects, `eval()`.
Their values are `Unknown`, which is compatible with everything. ry would rather skip a check than
report something false, so one unmodeled construct never causes a cascade of errors.

The cost is that a clean run does not say how much was checked. Strict mode reports the places where
a value became `Unknown`:

```toml
[check]
typing = true
strict = true
```

```r
#: fn(df: data.frame) -> double
total <- function(df) sum(df$amount)
```

```text
strict

  x strict mode: this expression has an undetermined type (`Unknown`)
    --[R/a.R:11:27]
 11 | total <- function(df) sum(df$amount)
    |                           ^^^^^^^^^
```

A `# typing: strict` comment turns strict mode on for one file.

## Giving a value a type

A plain `#:` annotation gives an `Unknown` value a type, so every later use is checked against it,
and the strict finding goes away. [Annotations](/type-checking/annotations) shows `@if-unknown`
and `@trust` for the cases where a checked annotation does not fit.

`Any` is also compatible with everything, but a value declared `Any` is a deliberate choice, so
strict mode does not report it. Calls to S3 generics that dispatch through `UseMethod()` return `Any`
for the same reason.

## Data frames

Columns are not typed, so `df$amount` is `Unknown`, and `df: data.frame` checks that a data frame
arrives but not which columns it has. For analysis code this is the gap that matters most, and
`data.frame()` is usually the first place strict mode reports. Bare column names inside dplyr verbs
and the other data-masking functions are recognized, as [checking code](/guides/checking#names)
shows.

## What strict mode misses

Strict mode does not yet report R6 objects or bare names from packages ry knows only by name, and it
wrongly reports a `stop()` guard such as `if (x < 0) stop("negative")`. `# ry: allow(strict)` on
that line silences it.
