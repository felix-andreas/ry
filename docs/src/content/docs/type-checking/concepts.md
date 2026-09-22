---
title: Concepts
description: The vocabulary you need to read what ry's type checker tells you
---

## Inference

**Inference** is the checker working out a type instead of being told one. What a value must be
follows from what you do with it.

```r
scale <- function(x, factor) {
  x * factor
}
```

`*` is arithmetic, so `x` and `factor` are numbers. That is inferred, not declared, and it is enough
to reject `scale("a", 2)`.

Annotate where you want to state something the code does not already state. That means a boundary,
a domain type, or a rule you want enforced.

## Scalars and vectors

In R, `1L` is an integer vector of length one. There is no separate scalar type, so the type system
has to choose a convention. ry's is:

| Notation | Means |
| --- | --- |
| `integer` | An integer vector of **length one** |
| `integer[]` | An integer vector of **unknown length** |
| `integer[named]` | An integer vector of unknown length, **keyed by names** |

Literals are scalars. `1L` is `integer`, not `integer[]`.

The relationship between them is **one-way**: a scalar is accepted where a vector is expected,
because a length-one vector is a vector. The reverse is not, because a vector of unknown length
might have length zero, or seven.

## Atomic types

The atomic types are `logical`, `integer`, `double`, `complex`, `character`, and `raw`.

Four of them form a ladder, and values widen up it implicitly:

```
logical  <  integer  <  double  <  complex
```

So an `integer` is accepted where a `double` is wanted. `character` and `raw` are deliberately
**not** on the ladder: R coerces a number to a string, but doing so silently is more often a mistake
than an intention, so ry requires you to write the conversion.

Widening happens only when checking whether a value fits somewhere. It does not happen when the
checker works out what two types have in common, because widening there would discard information.

## Lists

R gives you one `list()` and expects you to use it for everything. In practice it does at least four
different jobs, and the mistakes available in each are different:

| What you are building | Type | Example |
| --- | --- | --- |
| A record with named fields | `list{name: character, age: integer}` | `list(name = "Ada", age = 36L)` |
| A fixed-size tuple | `list{integer, character}` | `list(1L, "ok")` |
| A growable sequence, all one type | `list[integer]` | results you `append()` to in a loop |
| A lookup table keyed by name | `list[named: integer]` | counts by category |

The distinction that matters is **fixed shape** versus **unknown shape**. `list{...}` means the
checker knows exactly which elements exist, so it can check `$` and `[[`. `list[...]` means it knows
the element type but not how many there are, so it cannot.

Atomic vectors have the same two shapes: `integer` is one value, `integer[]` is many.

**None of this changes anything at runtime.** All four are an ordinary R `list`; `is.list()` is
`TRUE` for every one, `str()` prints what it always printed, and there is nothing to migrate. The
distinction exists so the checker can tell you which of the four you built.

That is what makes `$` checkable:

```r
person <- list(fullname = "Ada", age = 36L)
person$fullnme
```

The literal has a fixed shape, so the checker knows the field is not there, instead of the silent
`NULL` you get at runtime:

```text
type-mismatch

  x field `fullnme` does not exist in `list{fullname: character, age: integer}`. Did you mean `fullname`?
   --[R/a.R:2:8]
 1 | person <- list(fullname = "Ada", age = 36L)
 2 | person$fullnme
   |        ^^^^^^^
```

## Functions

A function type is written in the order you would say it:

```
fn(x: integer, y: character) -> logical
```

A parameter with a default is marked optional. Parameter **names** are part of the type, because R
calls functions by name as often as by position.

## Unions and `NULL`

A value that could be one of several things has a union type, written with `|`:

```
integer | character
```

You have already seen one: when a variable is assigned different types on different branches, its
type after the `if` is the union of both. That is what the type checker reports in the
[Features](/features) example.

`NULL` is its own type, and this is where unions matter most. A function that may return nothing has
type `character | NULL`. Using that result as a `character` without checking is an error, and it is
the missing `if (is.null(x))` that would have failed at runtime.

## `Any` and `Unknown`

Both accept anything and are accepted anywhere. They exist as two names because the *reason* they
arise differs, and the reason is what you need to know:

| | Means |
| --- | --- |
| `Unknown` | **The checker could not work it out.** A gap in its knowledge |
| `Any` | **You declared that it does not matter.** A deliberate opt-out you wrote |

Because `Unknown` is compatible with everything, one construct the checker cannot model does not
cascade into a screen of errors, and produces no findings at all. It also means a clean run does not
by itself tell you how much was checked.

[Strict mode](#typing-modes) addresses that: it reports every place a value became `Unknown`, so the
gaps are visible instead of looking like approval.

## Naming your own types

### `@type`

Start with the common case: a thing in your domain with named fields. In R you would reach for S4,
R6 or S7. ry offers a third option that the checker can see into:

```r
#: @type Person {list{name: character, age: integer}}

#: fn(name: character, age: integer) -> Person
new_person <- function(name, age) {
  #: @new Person
  list(name = name, age = age)
}

#: fn(p: Person) -> character
greet <- function(p) paste0("hi ", p$name)
```

A `Person` is an ordinary named list at runtime, with no class system, no dispatch, and no dependency. To
the checker it is a **distinct type**, so `greet` accepts a `Person` and nothing else:

```r
greet(list(name = "Bob", age = 40L))
```

```text
type-mismatch

  x expected `Person`, found `list{name: character, age: integer}`
    --[R/a.R:12:7]
 11 | 
 12 | greet(list(name = "Bob", age = 40L))
    |       ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

The list has exactly the right fields with exactly the right types, and it is still rejected.
Matching the shape is not sufficient; the value has to have been introduced as a `Person`:

```r
#: @new Person
ada <- list(name = "Ada", age = 36L)
```

`@new` is the only way in. It checks the value against the declared representation and returns the
nominal type. Put it inside a constructor, as `new_person` does, and every `Person` in your program
came from there.

This is **nominal** typing, as opposed to **structural**. Structural typing asks whether a value has
the right shape. Nominal typing asks whether it is the thing itself. S4 and R6 answer that question
at run time and tell the checker nothing. `@type` answers it at analysis time and adds nothing at run time.

### Two names for the same shape

Nominal types matter most when two values have the same shape but must not be substituted for each
other:

```r
#: @type Celsius {double}

#: @type Fahrenheit {double}

#: fn(temp: Celsius) -> Fahrenheit
to_fahrenheit <- function(temp) {
  #: @new Fahrenheit
  temp * 9 / 5 + 32
}

#: @new Celsius
freezing <- 0

warm <- to_fahrenheit(freezing)
to_fahrenheit(warm)
```

```text
type-mismatch

  x expected `Celsius`, found `Fahrenheit`
    --[R/a.R:15:15]
 14 | warm <- to_fahrenheit(freezing)
 15 | to_fahrenheit(warm)
    |               ^^^^
```

The last line converts an already-converted temperature. Both types are `double` underneath, and
arithmetic on them still works, because the representation is visible *outward*. What cannot happen
is a bare `double`, or the other unit, arriving where one of them is expected.

### `@alias`

Sometimes you want the opposite: a shorthand that stays interchangeable with what it abbreviates.

```r
#: @alias Row {list{id: integer, label: character}}
```

`Row` expands to its body everywhere, so a `Row` and that list are the same type. Use it to avoid
repeating a long type; use `@type` when being confused with the underlying value is what you are
trying to prevent.

:::note
A `#:` block does one thing: it declares types (`@type`, `@alias`) **or** it annotates the item on
the next line. Running them together reports:

```text
annotation

  x `@type` and `@alias` declarations need their own `#:` block. Separate them from other annotations with a blank line.
   --[R/a.R:1:1]
 1 | --> #: @type Row {list{id: integer}}
 2 | `-> #: fn(x: integer) -> integer
 3 |     f <- function(x) x
```

Separate them with a blank line, as above.
:::

See [domain modeling](/type-checking/domain-modeling) for constructors, validation, and when R6 is
still the right answer.

## Narrowing

Inside an `if`, the checker learns from the condition:

```r
greet <- function(name) {
  if (is.null(name)) {
    return("hello, stranger")
  }
  paste0("hello, ", name)
}
```

After the guard, `name` is no longer `NULL` and `paste0` accepts it.

Narrowing is deliberately limited. It happens on the condition of an `if`, and only for a guard
applied directly to a plain variable. `if (is.null(x))` narrows `x`; `if (is.null(obj$field))` does
not, and neither does a guard behind an `&&`. If a narrowing you expected did not happen, this is
almost always why. Lift the value into a local variable first.

## Generics

You rarely write these, and you get them anyway. An unannotated function whose parameters nothing
constrains is generic automatically:

```r
identity2 <- function(x) x
```

is `<T> fn(x: T) -> T`, meaning it returns exactly what it was given, whatever that was. Add one arithmetic
operation and the checker narrows the type on its own:

```r
increment <- function(x) x + 1L
```

is `<T: numeric> fn(x: T) -> T`: some number in, the same kind of number out. Neither has to be
asked for, and neither has to be maintained.

When you do write one, the binder goes at the front:

```r
#: <T> fn(items: list[T], index: integer) -> T
```

## Typing modes

There are three, and they are per file as well as per project:

| Mode | Reports |
| --- | --- |
| `off` | No type findings. Everything else still runs |
| `on` | Contradictions, meaning places where the types genuinely disagree |
| `strict` | Contradictions, plus every place a value became `Unknown` |

Set the project default in [`ry.toml`](/reference/configuration), and override it in any single file
with a comment at the top:

```r
# typing: strict
```

The file always wins, which is what lets you adopt this one module at a time.

## Next

- [Tutorial](/type-checking/tutorial) applies the same ideas to real code
- [Domain modeling](/type-checking/domain-modeling) covers nominal types instead of S4, R6, or S7
- [Limitations](/type-checking/limitations) covers what the checker cannot do
- [Type system reference](/reference/type-system) has the exact contract
