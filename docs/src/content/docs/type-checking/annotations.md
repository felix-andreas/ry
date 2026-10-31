---
title: Annotations
description: The #: comments that declare types, and when to write each kind
---

An annotation is a `#:` comment directly above the assignment it describes. It is checked against
the value and against every use:

```r
#: fn(price: double, rate: double) -> double
apply_rate <- function(price, rate) price * rate

apply_rate(10, "x")
```

```text
type-mismatch

  x expected `double`, found `character`
   --[R/annot.R:4:16]
 4 | apply_rate(10, "x")
   |                ^^^
```

The body is checked too, so a function declared `-> double` that returns a string is an error at the
return value.

## Values with no type

A value read from a file has no type until you give it one, and then every use is checked:

```r
#: list{name: character, age: integer}
person <- readRDS("person.rds")
person$age + "x"   # expected a numeric value (`integer` or `double`), found `character`
```

## Overriding inference

Two forms give a type without checking it against the value:

- `#: @if-unknown TYPE` types a value ry cannot infer, and becomes an error once ry can, so it
  never goes stale:

  ```text
  x `@if-unknown` applies only where the type is unknown, and this is already `double`. Drop the
    annotation, or use `#:` to check the type or `@trust` to override it
  ```

- `#: @trust TYPE` overrides a type ry did infer, for the cases where you know better.

## Function annotations

The compact form above fits most functions. A long signature can be written one parameter per line
instead, in the style of roxygen:

```r
#: @param count {integer}
#: @param [label] {character | NULL}
#: @return {integer}
```

[Functions](/type-checking/functions) covers optional parameters, `...`, and generics. The
[type system reference](/reference/type-system#typing-comment-syntax) has the full grammar,
including where a block may attach.
