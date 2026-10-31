---
title: Strict mode
description: Reject code whose types ry cannot determine
---

By default, ry checks what it can and lets the rest through as `Unknown`. Strict mode turns that
around: every value ry cannot type, and every parameter nothing constrains, is an error.

```toml
[check]
typing = true
strict = true
```

A `# typing: strict` comment turns it on for a single file instead.

## What it reports

A parameter with no type, whose use in the body does not determine one, accepts any value unchecked:

```r
describe <- function(x) cat("a value\n")
```

```text
strict

  x strict mode: parameter `x` has no type. How the function uses it does not determine one, so any value is accepted unchecked. Annotate it, as in `x: TYPE`
```

A function such as `function(x) x` is not reported, because it is generic: it works for any `x` and
returns the same type.

An expression whose type ry could not determine is reported where it appears:

```r
total <- function(df: data.frame) -> double sum(df$amount)
```

```text
strict

  x strict mode: this expression has an undetermined type (`Unknown`)
   --[R/a.R:3:49]
 3 | total <- function(df: data.frame) -> double sum(df$amount)
   |                                                 ^^^^^^^^^
```

An unresolved name, normally a warning, becomes an error.

## Clearing a finding

An annotation clears the finding, because the value now has a type. A value declared `Any`, or a
call that returns `Any` (`readRDS()`, `eval()`, and S3 generics that dispatch through `UseMethod()`),
is a deliberate opt-out, so strict mode does not report it.

Strict mode does not yet report R6 objects or bare names from packages ry knows only by name, and it
wrongly reports a `stop()` guard such as `if (x < 0) stop("negative")`. `# ry: allow(strict)` on that
line silences it.
