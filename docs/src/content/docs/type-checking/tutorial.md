---
title: Tutorial
description: Learn ry's type checker by putting it on real R, one step at a time
---

This tutorial covers the type checker: writing an annotation, what inference gives you without one,
unions and `NULL`, declaring your own domain types, generics, and strict mode. It assumes you can
already run `ry check` on a project.

Every transcript below comes from a package, with a `DESCRIPTION` file and the code under `R/`. In a
loose script a top-level function is not package-visible, so `ry` also reports it as unused. That
warning is not part of the lesson.

:::note
Type errors are opt-in. Add this to `ry.toml`, or put `# typing: on` at the top of a file:

```toml
[check]
typing = true
```
:::

## 1. Your first annotation

Here is a function with a type written above it:

```r
#: fn(price: double, rate: double) -> double
apply_discount <- function(price, rate) {
  price * rate
}
```

That first line is the annotation. It reads left to right: `fn(` the parameters and their types `)`,
then `->` and the return type. It lives in a `#:` comment, so R and every other R tool see a comment
and carry on. Annotated code is still ordinary R.

Now call it wrongly:

```r
apply_discount(100, "0.2")
```

```text
type-mismatch

  x expected `double`, found `character`
   --[t.R:6:21]
 5 | 
 6 | apply_discount(100, "0.2")
   |                     ^^^^^
```

ry found that without running the code, and the caret is on the argument rather than on the line
that would have failed.

The annotation is checked in both directions. Claim the wrong return type and the body is reported:

```r
#: fn(price: double, rate: double) -> character
apply_discount <- function(price, rate) {
  price * rate
}
```

```text
type-mismatch

  x expected `character`, found `double`
   --[a.R:3:3]
 2 | apply_discount <- function(price, rate) {
 3 |   price * rate
   |   ^^^^^^^^^^^^
 4 | }
```

## 2. Inference

Delete the annotation entirely and run it again:

```r
apply_discount <- function(price, rate) {
  price * rate
}

apply_discount(100, "0.2")
```

```text
type-mismatch

  x expected `double`, found `character`
   --[t.R:5:21]
 4 | 
 5 | apply_discount(100, "0.2")
   |                     ^^^^^
```

The same error. `*` is arithmetic, so `price` and `rate` are numbers, and the checker worked that
out from the body. This is **inference**, and it is why most R needs no annotations at all.

Two properties of it are worth knowing.

- The type ry gives a value is the most general one consistent with how the value is used. It does
  not depend on the order you read the file in or on the machine you run it on, so you and a
  colleague get the same answer. Calls into the standard library are the one place a choice is
  made, because a few of R's builtins are declared with several signatures, and the
  [rules for that](/reference/type-system#overload-sets) are fixed too.
- ry does not accept a contradiction silently. Where the types cannot line up, it reports that.

The second one carries the limit. R has constructs no type system can follow, including `UseMethod`
dispatch, data-frame columns, and S4. There ry yields `Unknown` rather than guessing, and `Unknown`
is compatible with everything, so one gap does not produce a run of follow-on errors.
[Strict mode](#7-strict-mode) is how you find those gaps.

## 3. When to annotate

Inference covers the code inside a function. Annotate where the code meets something else:

- **Exported functions**, and anything else another file or another person calls. The annotation is
  documentation the checker enforces, and it stops a change to the body quietly changing the
  contract.
- **Where you want a promise held.** If a function must return a `double`, say so, and the day
  someone makes it return a list you find out immediately.
- **Where inference cannot see.** A value that arrives from a data frame, a database, or `readRDS()`
  is `Unknown`. If you know what it is, say so.

Not every local variable. Annotating `n <- 1L` adds nothing the checker did not already know.

## 4. `NULL` and narrowing

This is the error most likely to be a real bug in code you have already shipped:

```r
#: fn(config: list{retries: integer} | NULL) -> integer
retry_count <- function(config) {
  config$retries
}
```

```text
type-mismatch

  x expected a list, found `list{retries: integer} | NULL`
   --[a.R:3:3]
 2 | retry_count <- function(config) {
 3 |   config$retries
   |   ^^^^^^^^^^^^^^
 4 | }
```

The `|` makes a union: `config` is *either* that list *or* `NULL`. You cannot reach into it until
you have ruled out `NULL`. Add the guard and the error goes away:

```r
#: fn(config: list{retries: integer} | NULL) -> integer
retry_count <- function(config) {
  if (is.null(config)) return(3L)
  config$retries
}
```

```text
1 file checked, no problems
```

The checker follows the `if`: after the early return, `config` cannot be `NULL` any more, so the
field access is fine. That is called narrowing.

## 5. Domain types

Two `double`s that must never be mixed up:

```r
#: @type Celsius {double}

#: @type Fahrenheit {double}

#: fn(t: Celsius) -> Fahrenheit
to_fahrenheit <- function(t) t
```

```text
type-mismatch

  x expected `Fahrenheit`, found `Celsius`
   --[a.R:6:30]
 5 | #: fn(t: Celsius) -> Fahrenheit
 6 | to_fahrenheit <- function(t) t
   |                              ^
```

`@type` makes a name that is its own type, distinct from everything else even when the
representation is identical. This is the part inference cannot do for you: only you know that these
two numbers mean different things.

See [domain modeling](/type-checking/domain-modeling) for constructors and validation.

## 6. Generics

An unannotated function that constrains nothing is already generic:

```r
identity2 <- function(x) x
```

Hover it and you get `<T> fn(x: T) -> T`, meaning it takes a value of any type `T` and returns the same type. Add
arithmetic and the type narrows on its own to `<T: numeric> fn(x: T) -> T`. Neither has to be asked
for, and neither has to be maintained.

One caveat: a single `T` used twice means *the same* type twice. Against
`<T: numeric> fn(value: T, factor: T) -> T`, the call `scale_by(2L, 0.5)` is reported, because
`integer` and `double` are different `T`s. Widen one side yourself when you want to mix.

## 7. Strict mode

A clean run means the checker found no contradictions. It does not mean it checked everything.
Strict mode reports every place a value became `Unknown`:

```toml
# ry.toml
[check]
typing = true
strict = true
```

```r
summarise <- function(frame) {
  frame$amount
}
```

```text
strict

  x strict mode: this expression has an undetermined type (`Unknown`)
   --[a.R:2:3]
 1 | summarise <- function(frame) {
 2 |   frame$amount
   |   ^^^^^^^^^^^^
 3 | }
```

Data-frame columns have no types yet, so that expression was never really checked, and without
strict mode it looked like a pass. Turn this on for the modules you rely on, not across a legacy
codebase on day one.

## 8. Adopting it a file at a time

You do not have to convert a project at once. A directive at the top of a file beats the project
setting, in either direction:

```r
# typing: on       # check this file even if the project has typing off
# typing: off      # skip this one while you work through the rest
# typing: strict   # hold this module to the stronger standard
```

[Adopting an existing codebase](/guides/adopting) has the order that works on a project with
findings already in it.

## Where to go next

- [Concepts](/type-checking/concepts) covers the full vocabulary: vectors, records, and `Any` against `Unknown`
- [Domain modeling](/type-checking/domain-modeling) covers nominal types instead of S4, R6, or S7
- [Limitations](/type-checking/limitations) covers what the checker cannot do
- [Type system reference](/reference/type-system) has the precise rules
