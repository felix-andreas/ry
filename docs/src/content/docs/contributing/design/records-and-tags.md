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
point <- (x = 1, y = 2)        # a record of type record{x: double, y: double}
pair <- (1L, "one")            # a tuple of type tuple{integer, character}
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
- **ry's own values, not R's.** Records, tuples and tags are a feature on top of R, like the
  built-in table type. They are not lists, carry no attributes, and have none of R's quirks: no
  partial matching, recycling or implicit coercion. Crossing into R is an explicit conversion, and
  any other attempt is an error with a clear message.
- **Cheap for the JIT.** A value has a fixed layout, a field resolves to a slot index when its type
  is known, a tag is a small integer, and scalar fields are stored unboxed.

## Records and tuples

A record or tuple literal is a parenthesized, comma-separated list:

| Literal | Type |
| --- | --- |
| `(x = 1, y = 2)` | `record{x: double, y: double}` |
| `(1L, "a")` | `tuple{integer, character}` |
| `(x = 1,)` | `record{x: double}` |
| `(1L,)` | `tuple{integer}` |
| `()` | `tuple{}` |

The comma is what makes it a literal. R reads `(x = 1)` as a parenthesized assignment and `(x)` as
a parenthesized expression, and both keep that meaning, so a one-field record and a one-item tuple
need a trailing comma, as a one-item tuple does in Python. R rejects a comma directly inside
parentheses, so every form in the table is new syntax with no prior meaning. A literal is all
fields or all items: `(1L, y = 2)` is a syntax error, and so is a repeated field name.

**Types.** `record{name: T}` and `tuple{T1, T2}` are new types, spelled like the existing
`list{...}` and `frame{...}`. They cannot reuse `list{...}`, because a record is not a list: passing
`(x = 1,)` where `list{x: double}` is expected is a type error, and so is the reverse. A record's
fields are all required, and its field order is part of neither its identity nor its type, so
`(x = 1, y = 2)` and `(y = 2, x = 1)` have the same type and are equal values.

**Access and update.** `point$x` reads a field by its exact name, and a name the record lacks is an
error, never a partial match or `NULL`. `pair[[1L]]` reads a tuple item, and an index past the end
is an error. `point$x <- 3` rebinds `point` to a copy with that field replaced, because records
have value semantics, and it cannot add a field or change its type. Nothing else from R applies:
`[`, `names()`, attributes and arithmetic are errors. Destructuring happens in `match` arms. A
destructuring assignment such as `(q, r) <- divmod(7L, 2L)` would also be unambiguous, because R
rejects `(q, r)`, and is left as follow-up work.

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

Records, tuples, tags and `match` are syntax, and GNU R has no such values, so a project that keeps
every file runnable by GNU R (`[check] inline-annotations = false`) cannot create them. It keeps
what it has today: `list{...}` types on `list(...)`, and unions of list records with a literal
discriminating field, narrowed by guards. The `record{...}`, `tuple{...}` and tag types are still
accepted in `#:` comments, for code that receives values from files that use the inline syntax.
It does not get exhaustiveness checking, because plain R has no construct whose arms the checker
could count without blessing particular call shapes, and blessing `switch()` would give one R
function a meaning R does not have.

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
| `(p1, p2)` | a tuple of that length whose items match |
| `(name = p, ..)` | a record with those fields, whose values match; without `..`, exactly those fields |
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

Records, tuples and tags share one heap object kind of ry's own. GNU R leaves type codes above 25
unused, so the object takes one of them, and every value has the same shape:

- a *layout*: the tag (or none, for a record or tuple), the field names in a canonical order (or
  none, for a tuple), and the storage of each slot. Layouts are interned in a runtime table that
  never shrinks, so the object stores a 32-bit layout id and two values have the same shape exactly
  when their ids are equal;
- the slots, eight bytes each. A slot holding a length-one `double`, `integer` or `logical` without
  attributes stores the number itself; any other value is stored as a handle, which the collector
  traces by reading the layout.

A tag is a small integer, its index in a runtime interner of tag names, so a tag test is one integer
comparison and the name exists only for printing. A record's fields are sorted by name in its
layout, which is what makes field order irrelevant, and a field whose type is known at compile time
resolves to its slot index then. Values are immutable: `$<-` builds a new object, or reuses the old
one when no other reference to it exists.

**The R boundary is explicit.** `as.list()` converts a record to a named list, a tuple to an
unnamed list, and a tag to the list of its payload, and `as_record()` and `as_tuple()` convert a
list back, checking that its names make a valid record. Without a conversion, a native value that
reaches R code which inspects it is an error that names the conversion:

```text
Error in sum(point) : `point` is a ry record, not an R object; convert it with `as.list()`
```

A short list of builtins handles native values directly: `print`, `format` and `deparse` use ry's
own printer (`(x = 1, y = 2)`, `:Circle(radius = 2)`); `identical` compares structure; `$`, `[[`,
`$<-` and `length` work as above; and the builtins that only store or pass a value along (`list`,
`invisible`, `return`, `assign`, `get`, `identity`, and element access on lists) accept it as any
other value, so a list of records works. Every other builtin, `.Call`, `.External` and the rest of
the C API boundary refuse a native value with the error above, which also covers `serialize` and
`saveRDS`. Native code never sees one.

**Language objects.** R code inspects code (`quote`, `body`, `deparse`), so the new syntax lowers to
calls, as `if` and `for` do. A record or tuple literal lowers to a call of the `.ry_record` or
`.ry_tuple` primitive, a tag to a call of the `.ry_tag` primitive with the name as its first
argument, and a `match` to a call of the `.ry_match` special with the scrutinee followed by one
`` `=>` `` call per arm, whose patterns stay unevaluated. The primitive itself is the call's
function rather than a symbol, so rebinding a name cannot change what a literal builds, and
`deparse` prints all four back in ry syntax.

### What the JIT gets

- **Tags as small integers.** In code compiled for a closed union, a tag is also the index of its
  name in the union's sorted tag list, as Roc assigns discriminants, so a `match` compiles to a
  jump table.
- **Shapes as one comparison.** A guard that a value has a known shape is one comparison of its
  layout id, and a field read after it is an indexed load of a slot whose storage is known.
- **Unboxed fields.** Scalar slots already hold raw numbers, and a value that compiled code builds
  and consumes without letting it escape needs no heap object at all: its fields live in registers
  or stack slots (scalar replacement, which the [prior-art page](/contributing/design/prior-art/)
  already adopts for temporaries).
- **Payload-free tags as constants.** `:None` has no payload, so each site returns one shared,
  immutable object instead of allocating.

## Measured against Roc, Bend, goose and Vx

| Question | Roc | Bend | goose | Vx | ry |
| --- | --- | --- | --- | --- | --- |
| Tag declaration | none, structural | `type` declares constructors | not verified | not verified | none, structural (Roc) |
| Open unions | row variable `[A]r` | closed only | not verified | not verified | `..` without a row variable |
| Payload | positional | named fields | not verified | not verified | positional or named |
| Records | structural, row-polymorphic | constructor fields | value structs | not verified | closed, structural, fixed layout |
| Exhaustiveness | yes | yes | not verified | not verified | yes, Maranget |
| Runtime tag | sorted index | constructor number in a word | not verified | not verified | interned small integer, sorted index in compiled code |

Roc is the model for the surface: structural tags, closed and open unions, payloads, and a
`match` that must be exhaustive. ry departs in two places, both forced. Open unions lose Roc's row
variable, which would need row unification. Records stay closed, because Roc's row-polymorphic records
(`{ name : Str }*`) would need the same row machinery. Bend's lesson is the runtime one: a
constructor with a word-sized field packs into a word and never touches the heap, which is what
scalar replacement gives ry's compiled code, and its constructor numbering is the sorted-index
scheme above. goose builds values in their final destination, which applies to a record literal
built directly into the slot that receives it. Vx contributes nothing specific to data modeling
beyond its content-hashed identity, which tags do not need because tag names are already interned.
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

- Literals are parenthesized comma lists, typed `record{...}` and `tuple{...}`.
- The tag marker is a leading `:`, in values, patterns and types.
- A tag union is a union; `..` makes it open; there are no row variables.
- `match (x) { PATTERN => EXPR }`, with the `{` on the `)` line, guards written `if (cond)`.
- Records, tuples and tags are ry heap objects with interned layouts, not R lists; R sees them
  only through `as.list()`, and any other crossing is an error.
- Pattern bindings assign in the current environment.

## Open questions

- **Row variables.** Pass-through typing for open unions and row-polymorphic records need row
  unification. Both are Hindley-Milner-compatible (Rémy), but each is a new unification case, so
  they wait for a measured need.
- **Destructuring assignment** `(a, b) <- pair`.
- **Formatting of long arms** beyond one arm per line.
