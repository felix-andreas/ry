---
title: Records, tuples and tags
description: "Design draft for ry's own data modeling: record and tuple literals, tag constructors, tag union types, and exhaustive pattern matching"
---

This draft designs the data-modeling features ry recommends over S4, R6 and Reference Classes:
records, tuples and tagged unions in the style of Roc, with an exhaustive `match`. The settled
typing rules now live in the [type system reference](/reference/type-system/#records-tuples-and-tags),
which is the contract and wins wherever the two disagree. This page keeps the rationale, the runtime
representation and the open questions.

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
fields or all items: `(1L, y = 2)` is a syntax error, and so is a repeated field name or a spliced
`...`.

**Types.** `record{name: T}` and `tuple{T1, T2}` are new types, spelled like the existing
`list{...}` and `frame{...}`. They cannot reuse `list{...}`, because a record is not a list: passing
`(x = 1,)` where `list{x: double}` is expected is a type error, and so is the reverse. A record's
fields are all required, and field order is part of neither its identity nor its type, so
`(x = 1, y = 2)` and `(y = 2, x = 1)` have the same type and are equal values. A record stores and
prints its fields sorted by name in byte order, independent of the locale, so the second literal
prints as `(x = 1, y = 2)`.

**Access and update.** `point$x` reads a field by its exact name, and a name the record lacks is an
error, never a partial match or `NULL`. `point[["x"]]` is the same read with a string. `pair[[1L]]`
reads a tuple item, an index past the end is an error, and `length()` counts fields or items. `point$x <- 3` rebinds `point` to a copy with that field replaced, because records
have value semantics, and it cannot add a field or change its type. Nothing else from R applies:
`[`, `names()`, attributes, arithmetic and `==` are errors, and `identical()` is equality. A tag
has no accessors at all: its payload is read with `match`. Destructuring happens in `match` arms. A
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
both, mirroring tuples and records, and the `(` must follow the name with no space. `:Some()` is
an error, because a tag without a payload is written `:Some`.

Two places need care, because the leading colon meets R's binary `:` and `::`. A parameter or
binding annotated with a tag type needs a space after its own colon (`x: :None`), because `x::None`
is namespace access; ry reports `x::None` in a parameter list with that fix. And inside `( )` or
`[ ]`, where R ignores newlines, a tag at the start of a continuation line continues the previous
line instead, so `f(y\n  :None)` is `f(y:None)`, exactly as R reads it.

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

A tag name has one payload shape within a union. Union normalization (`union_of`, which builds
every union, written or inferred) merges two members with the same tag name and shape into one
member with the payloads joined position by position, so joining `:Some(1L)` with `:Some("a")`
gives `:Some(integer | character)`. With several payload positions this loses which combinations
occur together, which is the price of keeping one member per tag. Two members with the same name
and a different shape (`:Some(1L)` and `:Some(1L, 2L)`, or `:Rect(w = 1)` and `:Rect(width = 1)`)
are a `type-mismatch` where they meet, because a `:Some(x)` pattern could not tell them apart.

**Closed and open unions.** A union of tags is *closed*: a `match` over it must cover every member,
and a tag outside it is rejected. A union that ends in `..` is *open*: `:Ok(integer) | ..` holds
`:Ok(integer)` or any other tag, so a value with a tag the union does not list fits it, and a
`match` over it needs a catch-all arm. `:Ok(character)` still does not fit, because the union does
list `:Ok`. In Roc, an open union carries a type variable for the remaining tags (`[Ok a]r`), which
lets a function pass unknown tags through with their types. ry does not have row variables, because
a union never unifies with a variable (`decisions.md`, "A multi-member union is for joins and
annotations"), and adding row unification is a new algorithm. `..` is the part of open unions that
needs no new algorithm, and pass-through typing is recorded as an open question.

`..` is a union member of its own, the *other tags* member, so an open union is a union whose
members include it, and only its compatibility rules are new:

- a tag value fits an open union when the union lists its name with a compatible payload, or does
  not list its name at all;
- a closed union fits an open one when each of its members does;
- an open union fits a closed union never, and fits another open union only when every tag the
  target lists is also listed by the source with a compatible payload, since a tag hidden in the
  source's `..` could otherwise break the target's payload type.

Inside patterns and types, `..` is reserved and never the R symbol of that name.

**Generic tag unions** come from aliases: `#: @alias Option<T> {:Some(T) | :None}` and
`#: @alias Result<T, E> {:Ok(T) | :Err(E)}`. Neither is built in, because each is one line, and a
built-in name would claim a word users may already use.

### Where a `#:`-only user stands

Records, tuples, tags and `match` are syntax, and GNU R has no such values, so a project that keeps
every file runnable by GNU R (`[check] inline-annotations = false`) cannot create them. It keeps
what it has today: `list{...}` types on `list(...)`, and unions of list records with a literal
discriminating field, narrowed by guards. The `record{...}`, `tuple{...}` and tag types are still
accepted in `#:` comments, for code that receives values from files that use the inline syntax.
With the setting off, each record, tuple, tag or `match` is still checked but is reported as a
`ry-syntax` warning, as each inline annotation is reported as `inline-annotation`. It does not get
exhaustiveness checking, because plain R has no construct whose arms the checker
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
| `name` | anything, binding it to `name`; `T` and `F` are errors that suggest `TRUE` and `FALSE` |
| `1L`, `"a"`, `TRUE`, `NA`, `NULL`, `-1` | a length-one value equal to the literal (`NA` matches only `NA`, `NULL` only `NULL`) |
| `:Tag`, `:Tag(p1, p2)`, `:Tag(field = p)` | a tag value with that name, whose payload matches |
| `(p1, p2)`, `(p,)` | a tuple of that length whose items match |
| `(name = p, ..)`, `(name = p,)` | a record with those fields, whose values match; without `..`, exactly those fields |
| `p1 \| p2` | either pattern; both sides must bind the same names |

Patterns follow the literals' trailing-comma rule: `(p)` only groups, and a one-item tuple or
one-field record pattern is `(p,)` or `(name = p,)`. A literal pattern matches a length-one value
of the same kind (numbers with numbers, so `1` matches `1L`) that is equal to it; `NA` matches a
missing value of any type, and `NaN` matches `NaN`.

The value of a `match` is the value of the first arm whose pattern matches and whose guard is
`TRUE`, and its type is the join of the arm types, as for `if`. When no arm matches, evaluation
stops with an error. An exhaustive `match` reaches it only when a value's type was not what the
checker was told, through `Any`, an untyped value or a `@trust` signature.

**Bindings are assignments.** A pattern's names are bound in the current environment when its arm
is chosen, the same way a `for` loop binds its variable, so assignments in an arm body reach the
enclosing function as in R. A fresh environment per arm would have made `<-` in an arm body
invisible after the `match`. A guard runs before the arm is chosen, so it sees the bindings in a
temporary child environment, and a guard that fails leaves the current environment untouched.
After the `match`, a name that only some arms bind is typed like a variable assigned in only some
branches of an `if`.

### Exhaustiveness

The checker decides coverage with Maranget's usefulness algorithm, over constructors read off the
scrutinee's type:

- a tag union contributes its tags, and an open union an unlisted "other tags" case that only a
  catch-all covers;
- a tuple and a record are one constructor each, with their items or fields as columns;
- a scalar `logical` has three constructors, `TRUE`, `FALSE` and `NA`, because R's logical has
  three values (a `logical[]` vector is unbounded, like the types below);
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

Records, tuples and tags share one heap object kind of ry's own, with type code 27, which GNU R
leaves unused (ry already uses 26 for binding cells). Every value has the same shape:

- a *layout* id: the layout is the tag (or none, for a record or tuple), the number of slots, and
  the field names sorted in byte order (or none, for a tuple). Layouts are interned in a runtime
  table that never shrinks, so two values have the same shape exactly when their 32-bit ids are
  equal, whatever their slots hold;
- an *unboxed mask*, one bit per slot, saying which slots hold a raw number;
- the slots, eight bytes each. A length-one `double`, `integer` or `logical` without attributes is
  stored as the number itself with its mask bit set; any other value is stored as a handle, which
  the collector traces by reading the mask. Slots past the 64th are always handles.

A raw `double` is stored as its bits, and a raw `integer` or `logical` as its 32 bits under one of
two reserved quiet-NaN prefixes, so one mask bit per slot is enough and a slot read can tell the
three apart. A `double` whose upper word happens to equal a prefix is stored boxed instead, which
keeps every NaN payload, `NA_real_` among them, exact. The object also carries its slot count, in
what would otherwise be header padding, so the collector sizes and traces it without consulting
the layout table.

A tag is a small integer, its index in a runtime interner of tag names, so a tag test is one integer
comparison and the name exists only for printing. A field whose type is known at compile time
resolves to its slot index then. Values are immutable: `$<-` builds a new object with the same
layout, or reuses the old one when no other reference to it exists.

**The R boundary is explicit.** `as.list()` converts a record to a named list, a tuple to an
unnamed list, and a tag to `list(tag = "Circle", payload = list(radius = 2))`. `as.record()`,
`as.tuple()` and `as.tag(name, payload)` convert back, checking that the names make a valid
record. They live in base, so a user's own function of the same name shadows them, as with any base
function; a built-in `Option` type is declined for a different reason, which is that type names
have no such shadowing.

Each kind has an implicit class, `ry_record`, `ry_tuple` or `ry_tag`, which is what `class()` and
`inherits()` report and what S3 dispatch uses, so `as.list()` is an ordinary S3 method and a user
can write `format.ry_record`. Builtins split into three groups:

- **Builtins that understand native values:** `print`, `format` and `deparse` use ry's own printer
  (`(x = 1, y = 2)`, `:Circle(radius = 2)`); `identical` compares structure; `$`, `[[`, `$<-` and
  `length` work as above; `all.equal` compares structure, so testthat's expectations work.
- **Builtins that never look inside a value:** the type and class predicates (`is.null`, `is.list`
  and the other `is.*` functions, `typeof`, which reports `"record"`, `"tuple"` or `"tag"`, `class`,
  `inherits`), `missing`, `force`, `invisible`, `return`, `assign`, `get`, `identity`, and the
  builtins that store a value, such as `list` and element access on lists, so a list of records
  works.
- **Every other builtin** refuses a native value with an error that names the conversion:

```text
Error in sum(point) : `point` is a ry record, not an R object; convert it with `as.list()`
```

The subject is the argument's source text when it is short. When it is longer than 60 characters,
or when a `...` before it makes its source unknown, the message says `argument 2` instead, and a
`for` loop, an `if` condition or `serialize` without source text says `the value`. Each builtin
carries a flag saying whether it accepts native values, set at startup from one list
(`ACCEPTS_NATIVE` in `crates/interp/src/records.rs`), so the check is one flag test plus a type
test per top-level argument, and nothing looks inside lists.

Field access names what went wrong and never falls back to a partial match:

```text
the record has no field `z`; its fields are `x`, `y`
a tuple has no field `x`; read its items by position, as in `x[[1L]]`
a tag has no fields; read its payload with `match`
index 3L is out of range for a tuple of length 2
no arm of this `match` matches :Triangle(sides = 3L)
```

`[[` on a record also takes a position, which counts fields in their sorted order.

**C code sees an opaque object.** A list of records can still reach `.Call`, through purrr's `map`
or a data.table column of records, and checking every argument deeply would cost time in
proportion to the data. So the boundary does not scan: `TYPEOF` returns 27, `VECTOR_ELT` and
`SET_VECTOR_ELT` on a list move the handle like any other element, and every other C API accessor
applied to a native value raises the conversion error above. `serialize` and `saveRDS` raise it as
well, when they reach the value, so a native value never leaves the session.

**Language objects.** R code inspects code (`quote`, `body`, `deparse`), so the new syntax lowers to
calls, as `if` and `for` do. A record or tuple literal lowers to a call of `.ry_record` or
`.ry_tuple`, a tag to a call of `.ry_tag` with the name as a string first, and a `match` to a call
of the `.ry_match` special with the scrutinee followed by one `` `=>` `` call per arm, whose
patterns stay unevaluated. The function of each call is a symbol, as for every other call, because
code that inspects calls (`as.character(call[[1]])` in rlang-style evaluation) expects one, and
`deparse` prints all four back in ry syntax.

An arm is `` `=>`(pattern, body) ``, or `` `=>`(pattern, guard, body) `` with a guard. A pattern is
encoded like the value it matches: `_`, `..` and a binding are symbols, a literal is its constant
(`-1L` folded to one), tag, tuple and record patterns are `.ry_tag`, `.ry_tuple` and `.ry_record`
calls, and alternatives and grouping are `|` and `(` calls. So `quote(match (x) { :Some(..) => 1
})[[3]][[2]]` is the call `.ry_tag("Some", ..)`, which prints as `:Some(..)`. A parenthesized
scrutinee lowers to its inner expression, as the condition of `if` does.

### What the JIT gets

- **Tags as small integers.** In code compiled for a closed union, a tag is also the index of its
  name in the union's sorted tag list, as Roc assigns discriminants, so a `match` compiles to a
  jump table.
- **Shapes as one comparison.** A guard that a value has a known shape is one comparison of its
  layout id, and a field read after it is an indexed load, plus a mask test when the slot's
  storage is not known statically.
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
