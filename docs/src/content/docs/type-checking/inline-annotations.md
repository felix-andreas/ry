---
title: Inline annotations
description: Types written in the code itself, and how they live next to #: comments
---

You can write a type right after a name, and a function's return type after its parameters:

```r
apply_discount <- function(price: double, rate: double = 0.2) -> double {
  price * (1 - rate)
}

apply_discount("100")
```

```text
type-mismatch

  x expected `double`, found `character`
   --[R/discount.R:5:16]
 5 | apply_discount("100")
   |                ^^^^^
```

A variable takes its type the same way:

```r
person: list{name: character, age: integer} <- readRDS("person.rds")
```

Inline annotations and `#:` comments share one type language and mean the same thing. Deleting each
inline annotation turns a file into the plain R program it means.

## When the two disagree

A statement may carry both spellings. If they give a parameter, the return, or the value different
types, ry uses the inline type and warns:

```r
#: fn(price: double, rate: double) -> double
apply_rate <- function(price: integer, rate) price * rate
```

```text
annotation-mismatch

  ! the inline annotation gives parameter `price` the type `integer`, but the `#:` comment gives it `double`. ry uses the inline type; make the two agree, or delete one
   --[R/both.R:2:31]
 2 | apply_rate <- function(price: integer, rate) price * rate
   |                               ^^^^^^^
```

## Keeping files runnable by R

R itself cannot run code with inline annotations. Inline annotations are on by default, so if your
project must also run under R, turn them off:

```toml
[check]
inline-annotations = false
```

ry then warns at every inline annotation it finds, and keeps warning while the setting is off:

```text
inline-annotation

  ! inline type annotations are turned off (`[check] inline-annotations = false`), so this one is not checked. R itself cannot run this code: delete the annotation, or turn inline annotations back on
```

Use `#:` comments in such a project. The [type system reference](/reference/type-system#inline-annotation-syntax)
lists every place an inline annotation may go.
