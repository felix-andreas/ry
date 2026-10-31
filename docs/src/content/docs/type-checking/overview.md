---
title: Overview
description: How ry checks types in R code that has no type annotations
---

Type checking finds the mistakes R reports only at run time: a string passed where a number is
needed, a misspelled list field, a `NULL` that nobody handled. You switch it on in `ry.toml`, because
an existing codebase can have many such findings and you should choose when to face them:

```toml
[check]
typing = true
```

## Nothing to annotate

ry infers types from how values are used, so unannotated code is checked as it is:

```r
apply_discount <- function(price, rate) price * (1 - rate)
apply_discount(100, "0.2")
```

```text
type-mismatch

  x expected a numeric value (`integer` or `double`), found `character`
   --[R/discount.R:2:21]
 1 | apply_discount <- function(price, rate) price * (1 - rate)
 2 | apply_discount(100, "0.2")
   |                     ^^^^^
```

`-` and `*` are arithmetic, so both parameters must be numbers. Inference runs even with type errors
switched off, because hover, completion, and inlay hints in your editor are built on it.

## Annotations where they help

You write a type after a name, and a function's return type after its parameters:

```r
apply_discount <- function(price: double, rate: double) -> double price * (1 - rate)
```

R itself cannot run that, so the same types can also go in `#:` comments, which keep the file plain
R that every other tool can read:

```r
#: fn(price: double, rate: double) -> double
apply_discount <- function(price, rate) price * (1 - rate)
```

Inference already covers the inside of a function, so annotate where code meets other code:
exported functions, values read from files, and anything whose contract you want enforced.
[Annotations](/type-checking/annotations) shows every form, and
[inline annotations](/type-checking/inline-annotations) covers using both spellings.

## Adopting it one file at a time

A comment in a file, usually at the top, overrides the project setting for that file:

```r
# typing: on
```

The values are `on`, `off`, and `strict`, so you can switch type checking on file by file, or keep
it on for the project and switch it off where a file is not ready.

## What is not checked

What ry cannot model, such as data frame columns and R6 objects, becomes `Unknown`, which is
compatible with everything. That way an unmodeled construct never causes a false error, but a clean
run also proves less than it seems. [Strict mode](/type-checking/strict-mode) finds these places, and
[limitations](/type-checking/limitations) lists them.
