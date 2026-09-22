---
title: Domain modeling
description: Give your domain its own checked types with nominal declarations, instead of reaching for S4, R6, or S7
---

R's object systems were designed for dispatch, not for static reasoning. This page shows what to use
instead when what you want is for the checker to know what a value is.

## What the object systems give a checker

Very little. Here is an S4 class and an R6 class, and a function that only accepts strings:

```r
setClass("Point", representation(x = "numeric"))
pt <- new("Point", x = 1)

#: fn(s: character) -> character
want_chr <- function(s) s

want_chr(pt@x)
```

That is accepted. `pt@x` is declared `"numeric"` in the class definition, it is being passed where a
`character` is required, and no finding is reported, because the checker cannot see inside S4. Strict
mode reports it as an undetermined type:

```text
strict

  x strict mode: this expression has an undetermined type (`Unknown`)
   --[R/a.R:7:10]
 6 | 
 7 | want_chr(pt@x)
   |          ^^^^
```

R6 behaves the same way. This gap will not close soon: both systems build their objects at runtime
out of values the checker would have to execute to understand.

## Declaring a type

```r
#: @type Person {list{name: character, age: integer}}

#: @new Person
ada <- list(name = "Ada", age = 36L)
```

`@type` declares a nominal type. The name is its own type, distinct from everything else, including
things with an identical representation. `@new` is the one way a plain list becomes one, and it
checks the list against the declared shape as it goes.

From there the checker knows what `ada` is: `ada$name` is a `character`, `ada$nam` is a reported
mistake, and passing `ada` where a `Person` is wanted works while passing any other list does not.

## Constructors

In practice you want one function that builds the value, so validation lives in one place:

```r
#: @type Person {list{name: character, age: integer}}

#: fn(name: character, age: integer) -> Person
new_person <- function(name, age) {
  if (age < 0L) stop("age must not be negative")
  #: @new Person
  list(name = name, age = age)
}
```

```r
new_person("Ada", "36")
```

```text
type-mismatch

  x expected `integer`, found `character`
    --[R/a.R:10:19]
  9 | 
 10 | new_person("Ada", "36")
    |                   ^^^^
```

The `@new` is **inside** the constructor, so the constructor is the only way in: everything
downstream receives a `Person`, and the checker enforces that statically.

The two halves do different jobs. `@new` is an **analysis-time** check. It is not `setValidity()`,
it is not an R6 `initialize()`, and it emits no runtime assertion. It cannot know that `age` is
non-negative, and it cannot check a field whose type it could not determine. The `stop()` on the
line above is what enforces the invariant when the code runs. The `stop()` guards values at run
time, the `@new` guards shape at analysis time, and both sit in the one function every caller goes
through.

## Distinct types that look identical

This is what a structural checker cannot do for you:

```r
#: @type Celsius {double}

#: @type Fahrenheit {double}

#: fn(t: Celsius) -> Fahrenheit
to_fahrenheit <- function(t) t
```

```text
type-mismatch

  x expected `Fahrenheit`, found `Celsius`
   --[R/a.R:6:30]
 5 | #: fn(t: Celsius) -> Fahrenheit
 6 | to_fahrenheit <- function(t) t
   |                              ^
```

Both are `double` underneath, and neither is interchangeable with the other. Use this anywhere your domain has values that are
the same shape but must not be mixed: two `character` ids, two currencies, or a validated email
beside an unvalidated string.

The representation still leaks **outward**: a `Celsius` is accepted anywhere a plain `double` is
expected, so `t / 2` and `mean(temps)` keep working. The reverse does not hold, and a bare `double`
never becomes a `Celsius` on its own. Arithmetic therefore stays convenient, while the only way into
the type is a `@new` you wrote.

For a name that is pure shorthand and stays interchangeable with its body, use
[`@alias`](/type-checking/concepts#naming-your-own-types) rather than `@type`.

## Types with a parameter

```r
#: @type Box<T> {list{value: T}}
```

A `Box<integer>` and a `Box<character>` are different types, and the element type flows out again
when you read it.

## When to use R6

Nominal types describe values. They do not give you:

- **Mutable state with reference semantics.** An R6 object shared between callers and mutated in
  place is a thing this cannot express.
- **Inheritance hierarchies.** There is no subtyping between nominal types.
- **Dispatch.** If you need `print()` and `summary()` to do the right thing per class, that is S3.
  Those go through `UseMethod`, which dispatches at run time, so the checker types such calls as
  `Unknown` and cannot follow them. (S3 *operator* dispatch, `+.Date` and friends, is resolved
  statically; method dispatch is not.)

Use a nominal type when you have a **value with a shape and an invariant**, which is most of what an
analysis codebase passes around. Use R6 when you have an **object with identity and mutable state**,
and accept that its interior is opaque to the checker.

Plenty of R code uses R6 for things that are really just values. Those are the ones worth
converting.

## Next

- [Concepts](/type-checking/concepts) covers nominal against structural, and the rest of the vocabulary
- [Limitations](/type-checking/limitations) covers what is and is not supported in R's object systems
- [Type system reference](/reference/type-system) has the exact rules for `@type`, `@new`, and `@alias`
