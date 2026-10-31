---
title: Records, tuples and tags
description: "Design draft for ry's own data modeling: record and tuple literals, tag constructors, tag union types, and exhaustive pattern matching"
---

This draft designs the data-modeling features ry recommends over S4, R6 and Reference Classes:
records, tuples and tagged unions in the style of Roc, with an exhaustive `match`. It is a draft,
not a contract. Where it disagrees with the [type system reference](/reference/type-system/), the
reference is right.

## The shape in one example

```r
#: @alias Shape {:Circle(radius: double) | :Rect(width: double, height: double) | :Empty}

area <- function(shape: Shape) -> double {
  match (shape) {
    :Circle(radius = r) => pi * r^2
    :Rect(width = w, height = h) => w * h
    :Empty => 0
  }
}

area(:Circle(radius = 2))
point <- (x = 1, y = 2)        # a record, which is `list(x = 1, y = 2)`
pair <- (1L, "one")            # a tuple, which is `list(1L, "one")`
```

Deleting the `:Empty` arm is an error that names the case it misses:

```text
non-exhaustive-match

  x this `match` does not handle `:Empty`
  = help: add an arm for `:Empty`, or a catch-all `_ => ...` arm
```

## Goals and constraints

- **Plain R stays plain R.** Every new form is text that R's own parser rejects, so no working R
  program changes meaning. The constructs are part of ry's inline syntax, next to inline
  annotations.
- **Hindley-Milner only** (`decisions.md`, "The type system is Hindley-Milner"). Tags are
  structural, and a tag union is an ordinary union, so nothing here adds subtyping, row
  unification, or a new inference algorithm.
- **Values are ordinary R objects.** Every value the new syntax creates is a list that `str`,
  `print`, `identical`, `serialize`, `saveRDS` and C code already handle, because any object native
  code can reach must be a real `SEXP` (see [the runtime draft](/contributing/design/r-runtime/)).
- **Cheap for the JIT.** A record's field and a tag's identity can be read with a pointer comparison
  and an indexed load, and a value that never escapes compiled code need not exist at all.

## Records and tuples

A record or tuple literal is a parenthesized, comma-separated list:

| Literal | Type | The same value in plain R |
| --- | --- | --- |
| `(x = 1, y = 2)` | `list{x: double, y: double}` | `list(x = 1, y = 2)` |
| `(1L, "a")` | `list{integer, character}` | `list(1L, "a")` |
| `(x = 1,)` | `list{x: double}` | `list(x = 1)` |
| `(1L,)` | `list{integer}` | `list(1L)` |
| `()` | `list{}` | `list()` |

The comma is what makes it a literal. R reads `(x = 1)` as a parenthesized assignment and `(x)` as
a parenthesized expression, and both keep that meaning, so a one-field record and a one-item tuple
need a trailing comma, as a one-item tuple does in Python. R rejects a comma directly inside
parentheses, so every form in the table is new syntax with no prior meaning. A literal is all
fields or all items: `(1L, y = 2)` is a syntax error, and so is a repeated field name.

**Types.** Records and tuples keep the existing types, `list{name: T}` and `list{T1, T2}`. The
literal is not a new kind of value; it is a shorter `list(...)` that always has a fixed shape,
because it cannot mix names, splice `...`, or drop names. A second type spelling such as
`(name: character)` was rejected, because it would give one type two names.

**Access and update** stay R's: `point$x`, `pair[[1L]]`, and `point$x <- 3`, which copies on
modify as it always has. Destructuring happens in `match` arms. A destructuring assignment such as
`(q, r) <- divmod(7L, 2L)` would also be unambiguous, because R rejects `(q, r)`, and is left as
follow-up work.

## Tags

A *tag* is a name that marks which case a value is in, optionally carrying a payload. A tag value
is written with a leading colon:

```r
:None
:Some(5L)
:Rect(width = 2, height = 3)
```

The colon is the marker because R's `:` is binary only, so a `:` where an expression starts is a
syntax error in R. Tag names follow Roc's convention of a capital letter, though any R name is
accepted. A payload is positional (`:Some(5L)`) or named (`:Rect(width = 2, height = 3)`), never
both, mirroring tuples and records.

Tags are structural and need no declaration: `:Some(5L)` means the same tag wherever it appears, as
in Roc and as OCaml's polymorphic variants do.

### Tag types

A tag's type is written the way its value is, with types in place of values:

| Value | Type |
| --- | --- |
| `:None` | `:None` |
| `:Some(5L)` | `:Some(integer)` |
| `:Rect(width = 2, height = 3)` | `:Rect(width: double, height: double)` |

A *tag union* is an ordinary [union](/reference/type-system/#union-types) of tag types, so the
existing union rules apply unchanged: `:Some(integer) | :None` is the option type, and a value fits
it when it fits one member. That gives Roc's open producer for free. `:Some(5L)` has the type
`:Some(integer)`, which fits every union that contains a compatible `:Some`, so a constructor never
needs to know the union it will flow into. An `if` whose branches build different tags joins them
into their union.

Union normalization gains one rule: two members with the same tag name and payload shape merge into
one member with the payloads joined position by position, so joining `:Some(1L)` with `:Some("a")`
gives `:Some(integer | character)`. With several payload positions this loses which combinations
occur together, which is the price of keeping one member per tag.

**Closed and open unions.** A union of tags is *closed*: a `match` over it must cover every member,
and a tag outside it is rejected. A union that ends in `..` is *open*: `:Ok(integer) | ..` holds
`:Ok(integer)` or any other tag, so a value with a tag the union does not list fits it, and a
`match` over it needs a catch-all arm. `:Ok(character)` still does not fit, because the union does
list `:Ok`. In Roc, an open union carries a type variable for the remaining tags (`[Ok a]r`), which
lets a function pass unknown tags through with their types. ry does not have row variables, because
a union never unifies with a variable (`decisions.md`, "A multi-member union is for joins and
annotations"), and adding row unification is a new algorithm. `..` is the part of open unions that
needs no new algorithm, and pass-through typing is recorded as an open question.

**Generic tag unions** come from aliases: `#: @alias Option<T> {:Some(T) | :None}` and
`#: @alias Result<T, E> {:Ok(T) | :Err(E)}`. Neither is built in, because each is one line, and a
built-in name would claim a word users may already use.

### Where a `#:`-only user stands

Tag values and `match` are syntax, so they exist only in ry's inline syntax. A project that keeps
every file runnable by GNU R (`[check] inline-annotations = false`) still gets:

- record and tuple types on `list(...)`, exactly as today;
- tag union types in `#:` comments, for values that come from ry code or from a plain R
  constructor the user trusts, such as `#: @trust fn(value: T) -> :Some(T)` over a function that
  builds the representation below;
- unions of records with a literal discriminating field, narrowed by guards as today.

It does not get exhaustiveness checking, because plain R has no construct whose arms the checker
could count without blessing particular call shapes, and blessing `switch()` on a tag's name would
give one R function a meaning R does not have.

## Pattern matching

```r
match (value) {
  :Some(x) if (x > 0L) => x
  :Some(_) => 0L
  :None => NA_integer_
}
```

`match (EXPR) { ARMS }` is a `match` expression when its `{` is on the same line as the closing
`)`. R reads `match(value)` as a call to `base::match`, and a call directly followed by `{` is a
syntax error in R, so the form is free. `match(x, table)` and every other use of `base::match` keep
their meaning, and `match(value)` followed by a newline and a braced block is still two R
statements.

An arm is `PATTERN => EXPR` or `PATTERN if (CONDITION) => EXPR`, and arms are separated by newlines
(or `;` or `,`). R has no `=>` token outside its disabled pipe-bind experiment, so `=>` cannot occur
in working R code. `->` was rejected for arms because it is R's right assignment.

**Patterns:**

| Pattern | Matches |
| --- | --- |
| `_` | anything, binding nothing |
| `name` | anything, binding it to `name` |
| `1L`, `"a"`, `TRUE`, `NA`, `NULL`, `-1` | a length-one value equal to the literal (`NA` matches only `NA`, `NULL` only `NULL`) |
| `:Tag`, `:Tag(p1, p2)`, `:Tag(field = p)` | a tag value with that name, whose payload matches |
| `(p1, p2)` | an unnamed list of that length whose items match |
| `(name = p, ..)` | a list with those names whose fields match; without `..`, exactly those names |
| `p1 \| p2` | either pattern; both sides must bind the same names |

The value of a `match` is the value of the first arm whose pattern matches and whose guard is
`TRUE`, and its type is the join of the arm types, as for `if`. When no arm matches, evaluation
stops with an error, which an exhaustive `match` never reaches.

**Bindings are assignments.** A pattern's names are bound in the current environment when its arm
is chosen, the same way a `for` loop binds its variable, so assignments in an arm body reach the
enclosing function as in R. A fresh environment per arm would have made `<-` in an arm body
invisible after the `match`.

### Exhaustiveness

The checker decides coverage with Maranget's usefulness algorithm, over constructors read off the
scrutinee's type:

- a tag union contributes its tags, and an open union an unlisted "other tags" case that only a
  catch-all covers;
- a tuple and a record are one constructor each, with their items or fields as columns;
- `logical` has three constructors, `TRUE`, `FALSE` and `NA`, because R's logical has three values;
- `NULL` is one constructor, and a union adds one case per member;
- every other type (numbers, strings, `Unknown`) has unboundedly many values, so only a binding or
  `_` covers it.

An arm with a guard counts for reachability but not for coverage. Two findings come out:

- `non-exhaustive-match` (error) names up to three missing cases, as patterns: `:Err(_)`,
  `(_, FALSE)`, or `_` for an unbounded type.
- `unreachable-pattern` (warning) marks an arm whose pattern the arms above already cover.

A pattern that cannot match the scrutinee's type is a `type-mismatch` at the pattern, such as a tag
the closed union does not list, a payload of the wrong length, or a field the record lacks. A
scrutinee whose type is still an unbound inference variable takes its shape from the patterns, as
`is.null` shapes an unannotated parameter (`decisions.md`): the arms' tags form a closed union, or an
open one when a catch-all arm exists. So an unannotated `function(r) match (r) { :Ok(v) => v; :Err(e)
=> 0 }` infers `fn(r: :Ok(T) | :Err(U)) -> T | double`.

## Runtime representation

A record is a named list and a tuple an unnamed list, with no class, which is why
`identical((x = 1,), list(x = 1))` is `TRUE` and every R function treats them as lists.

A tag value is a list of its payload with two attributes:

```r
structure(list(radius = 2), ry_tag = quote(Circle), class = "ry_tag")
```

- `ry_tag` holds the tag name as a symbol. Symbols are interned, so a tag test is one pointer
  comparison, and a symbol serializes by name, so `saveRDS` and `readRDS` round-trip tags across
  sessions and into GNU R.
- `class = "ry_tag"` lets R code recognize a tag with `inherits(x, "ry_tag")` and lets `print` and
  `format` show `:Circle(radius = 2)`. The tag name is not the class, because tags are structural
  and global: a tag named `Date` must not dispatch to `print.Date`.
- `identical` compares the payload and both attributes, so two tags are identical exactly when
  their names and payloads are. `str` shows a list with its attributes, and `unclass` gives the
  payload list back.

Constructing from C is ordinary API use (`allocVector(VECSXP, n)`, `setAttrib`), and `libR` adds
three helpers for native code that wants to speak tags directly: `Ry_IsTag(x)`, `Ry_TagName(x)`
and `Ry_MakeTag(name, payload)`, declared in `Ry.h` outside R's own headers.

**Language objects.** R code inspects code (`quote`, `body`, `deparse`), so the new syntax lowers to
calls, as `if` and `for` do. A record or tuple literal lowers to a call whose function is the `list`
primitive itself rather than the symbol `list`, so rebinding `list` cannot change what a literal
builds. A tag lowers to a call of the `.ry_tag` primitive with the name as its first argument, and a
`match` to a call of the `.ry_match` special with the scrutinee followed by one `` `=>` `` call per
arm, whose patterns stay unevaluated. `deparse` prints all three back in ry syntax.

### What the JIT gets

- **Tags as small integers.** In code compiled for a closed union, a tag is the index of its name in
  the union's sorted tag list, as Roc assigns discriminants. Reading it from a boxed value is an
  attribute load and a pointer comparison per arm, and a `match` compiles to a jump table once the
  value is unboxed.
- **Record shapes as one comparison.** A record literal's names vector is a constant of its call
  site, shared by every value the site builds, so a guard that a value has a known shape is one
  pointer comparison on its `names` attribute, followed by an indexed load. Values built elsewhere
  fail the guard and take the generic path.
- **Unboxed fields.** A record, tuple or tag that compiled code builds and consumes without letting
  it escape into an environment, an attribute or a native call needs no heap object: its fields
  live in registers or stack slots (scalar replacement, which the
  [prior-art page](/contributing/design/prior-art/) already adopts for temporaries).
- **Payload-free tags as constants.** `:None` has no payload, so each site can return one shared,
  immutable object instead of allocating.

## Measured against Roc, Bend, goose and Vx

| Question | Roc | Bend | goose | Vx | ry |
| --- | --- | --- | --- | --- | --- |
| Tag declaration | none, structural | `type` declares constructors | not verified | not verified | none, structural (Roc) |
| Open unions | row variable `[A]r` | closed only | not verified | not verified | `..` without a row variable |
| Payload | positional | named fields | not verified | not verified | positional or named |
| Records | structural, row-polymorphic | constructor fields | value structs | not verified | closed structural lists |
| Exhaustiveness | yes | yes | not verified | not verified | yes, Maranget |
| Runtime tag | sorted index | constructor number in a word | not verified | not verified | interned symbol, sorted index in compiled code |

Roc is the model for the surface: structural tags, closed and open unions, payloads, and a
`match` that must be exhaustive. ry departs in two places, both forced. Open unions lose Roc's row
variable, which would need row unification. Records stay closed and keep R's list representation,
because a record is a `list` that R code already handles, and Roc's row-polymorphic records
(`{ name : Str }*`) would need the same row machinery. Bend's lesson is the runtime one: a
constructor with a word-sized field packs into a word and never touches the heap, which is what
scalar replacement gives ry's compiled code, and its constructor numbering is the sorted-index
scheme above. goose builds values in their final destination, which applies to a record literal
built directly into the slot that receives it. Vx contributes nothing specific to data modeling
beyond its content-hashed identity, which tags do not need because a symbol is already interned.
The goose and Vx columns were not checked against their sources and are marked so.

## S4, R6 and Reference Classes

They keep working, because existing code must. ry discourages them in two ways:

- The `object-system` lint warns where code defines a class or method: `setClass`, `setRefClass`,
  `setGeneric`, `setMethod`, `setValidity` and `R6Class`, qualified or not. Using objects from a
  package, such as a DBI connection (an S4 object) or `x@slot` on one, is not reported, because
  that code has no alternative.
- `[check] object-systems = false` turns them off for a project: every such definition is then an
  `object-system` error.

## Settled choices

- Literals are parenthesized comma lists; types stay `list{...}`.
- The tag marker is a leading `:`, in values, patterns and types.
- A tag union is a union; `..` makes it open; there are no row variables.
- `match (x) { PATTERN => EXPR }`, with the `{` on the `)` line, guards written `if (cond)`.
- A tag value is a payload list with an `ry_tag` symbol attribute and class `ry_tag`.
- Pattern bindings assign in the current environment.

## Open questions

- **Row variables.** Pass-through typing for open unions and row-polymorphic records need row
  unification. Both are Hindley-Milner-compatible (Rémy), but each is a new unification case, so
  they wait for a measured need.
- **Destructuring assignment** `(a, b) <- pair`.
- **Formatting of long arms** beyond one arm per line.
