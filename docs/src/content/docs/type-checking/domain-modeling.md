---
title: Domain modeling
description: Checked types for your own data with @type, and when S4, R6, or S7 is still the right tool
---

S4, R6, and S7 build classes at run time from values a static checker would have to execute to
understand, so ry cannot see inside them: an S4 slot or an R6 field is `Unknown`, and passing it to the
wrong function goes unreported. When what you want is for the checker to know what a value is, declare
it as a nominal type with `@type` instead (the [tour](/tour#structural-and-nominal-types) explains
nominal versus structural types).

## A record type with a constructor

```r
#: @type Money {list{amount: double, currency: character}}

#: fn(amount: double, currency: character) -> Money
money <- function(amount, currency) {
  if (amount < 0) stop("negative amount")
  #: @new Money
  structure(list(amount = amount, currency = currency), class = "Money")
}
```

Every `Money` in the program comes from an `@new`, and `@new` is only where you write it, so with one
constructor every value has passed its checks. The two halves divide the work: `@new` checks the
shape when ry analyzes the code, and `stop()` checks the values when the code runs.

A plain list with the right fields is still not a `Money`: the type is nominal, so matching the shape
is not enough, and the value has to come from the constructor. Reads and writes are checked against
the declared fields, so after `m <- money(1, "EUR")`, the write `m$amount <- "x"` is an error.

The class attribute is for R, not for ry: `structure()` keeps its argument's type, so the
constructor would check the same without it. R needs it to dispatch the operator method below.

## Nominal types over scalars

`@type UserId {character}` and `@type Email {character}` are both strings at run time, and the
checker keeps them apart. IDs, units, currencies, and validated-versus-raw input are the values that
get mixed up most, and nothing else in R catches the mix-up before the code runs. A `UserId` is still accepted where a
`character` is, so string functions keep working on it.

## Operators and generic types

Arithmetic and comparison on your type dispatch to the method R would call, so a method declared in
your code is checked like a stub. R finds the method through the value's class attribute, which is
why the constructor above sets one:

```r
#: fn(a: Money, b: Money) -> Money
`+.Money` <- function(a, b) {
  if (a$currency != b$currency) stop("currency mismatch")
  money(a$amount + b$amount, a$currency)
}
```

`money(1, "EUR") + money(2, "EUR")` is a `Money`. A type can take parameters, as
`@type Page<T> {list{items: list[T], total: integer}}`, and the parameter flows out again when you
read a field.

## `@alias`

`@alias Row {list{id: integer, label: character}}` names a structural type, so any list of that shape
is a `Row`. Use it to avoid repeating a long type, and `@type` when confusing a
value with its representation is the mistake you want caught.

## When to use R6 or S4 anyway

`@type` describes values. It does not give you:

- **Shared mutable state.** An R6 object that several callers modify in place has identity, and a
  value type cannot express that.
- **Inheritance.** There is no subtyping between nominal types.
- **Method dispatch.** `print()` and `summary()` per class are S3 or S4. `UseMethod` dispatches at
  run time, so those calls return `Any`. Operator methods, as above, are the exception.

Much R code uses R6 or S4 for things that are really values: a configuration, a result, a parsed
record. Those are worth converting. To keep an R6 or S4 class but still check who receives it, give
it a nominal type over `Any` and wrap its constructor. The class stays opaque, but passing something
else where an `Account` is expected becomes an error:

```r
#: @type Account {Any}

#: fn() -> Account
new_account <- function() {
  #: @new Account
  AccountClass$new()
}
```
