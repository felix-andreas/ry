---
title: Type system
description: The precise static-typing semantics contract for ry's R type checker
---

This page is the specification of ry's type system: the contract the type checker implements, rule
by rule. It is written to be looked things up in rather than read front to back. If you are new to
the checker, start with the [tutorial](/type-checking/tutorial) and [concepts](/type-checking/concepts),
which introduce the same ideas through examples, and come back here when you need the exact rule.

The page moves from the notation outward. It starts with how annotations are written and what the
types are, then covers functions and annotations, then how each operator, call, and control-flow
construct is typed, and finally how names resolve, how data frames and R's object systems are
handled, and what strict mode adds.

Two ideas run through everything. First, the checker prefers skipping a check to giving a wrong
answer: a construct it cannot describe becomes `Unknown`, which is compatible with everything, so a
gap never produces a false error. Second, [strict mode](#strict-mode) is how you see those gaps.

## Typing comment syntax

Annotations live in `#:` comments, directly above the binding or expression they describe.
Consecutive `#:` lines with no blank line between them form one *annotation block*.

There are four annotation forms:

| Form | Meaning |
| --- | --- |
| `#: TYPE` | a [checked annotation](#checked-annotations) |
| `#: @trust TYPE` | a [trusted coercion](#trusted-coercions) |
| `#: @if-unknown TYPE` | an [unknown-only coercion](#unknown-only-coercions) |
| `#: @new NOMINAL_TYPE` | a [nominal introduction](#nominal-introduction) |

A block holds exactly one of these: a single line in one of those forms, one
[expanded function annotation](#expanded-function-annotations) written as `@param` and `@return`
lines, or one or more `@type` and `@alias` lines. The three kinds cannot be mixed in one block.

```r
#: integer
value <- 1L
```

```r
#: fn(count: integer) -> integer
double_count <- function(count) count + count
```

A block can attach at any statement depth, not only at the top level. Inside a function body it
annotates the assignment or the block-final expression that follows it, which is how a function
that builds a value of a named type is written:

```r
#: @type Person {list{name: character}}

#: fn(name: character) -> Person
make_person <- function(name) {
  #: @new Person
  list(name = name)
}
```

### Attachment

A block attaches to the expression that starts on the line directly after it. When a block needs a
target and has none, it is an error and the annotation does not apply. That happens when:

- a blank line separates the block from the expression;
- a plain `#` comment separates them, or no expression follows at all;
- the block has no content beyond the `#:` marker;
- the block sits inside a call's argument list, for example beside a lambda passed to `lapply`,
  because an argument is not a statement. Give the value its own binding and annotate that instead.

A braceless function body, a braceless `if` branch, and a parenthesised expression are valid
targets.

A *definition block*, one that contains only `@type` and `@alias` lines, attaches to nothing, and
neither does a `@strict` toggle, so the adjacency rules do not apply to them.

### Refused blocks

A block is refused as a whole when it mixes forms, orders its directives wrongly, declares a
duplicate or unknown type parameter, or gives `@new` a payload that is not nominal. A refused block
reports its error and carries no typing payload, so a broken annotation never causes follow-on
findings. A block the annotation grammar could not even read is refused silently, because the parse
error has already said what was wrong.

## Types

### Atomic names

Types use R's own names: `logical`, `integer`, `double`, `complex`, `character`, `raw`, and `NULL`.
The names from other languages, such as `bool`, `int`, `float`, and `string`, are not accepted.

### Reserved constants

R's reserved constants have fixed scalar types:

- `TRUE` and `FALSE` are `logical`.
- `NA` is `logical`, while `NA_integer_`, `NA_real_`, `NA_complex_`, and `NA_character_` are
  `integer`, `double`, `complex`, and `character`.
- `Inf` and `NaN` are `double`.
- An imaginary literal such as `1i` is `complex`.
- `NULL` is `NULL`.

### Vector shapes

Every atomic vector type comes in three shapes, written with a suffix on the atomic name:

| Shape | Written | Means | Examples |
| --- | --- | --- | --- |
| scalar-like | `T` | a vector of length one | `character`, `integer`, `double` |
| array-like | `T[]` | a vector of unknown length | `character[]`, `integer[]`, `double[]` |
| map-like | `T[named]` | a vector of unknown length, keyed by names | `character[named]`, `integer[named]` |

#### Vector coercions

- A scalar-like `T` coerces to an array-like `T[]`.
- A map-like `T[named]` coerces to an array-like `T[]`.
- An atomic type coerces up R's promotion order, `logical` < `integer` < `double` < `complex`, in
  every shape. That covers `logical` to `integer`, `integer[]` to `double[]`, `double[named]` to
  `complex[named]`, and compositions such as a scalar `integer` to `double[]`. The reverse never
  holds. `character` and `raw` are not part of the order, because R reaches `character` only through
  an explicit conversion.
- No other coercion between vector types is allowed unless a rule elsewhere on this page states it
  explicitly.

Whether a coercion changes the type of a result depends on the construct that uses it.

### List shapes

R uses `list(...)` for several quite different kinds of collection, and the type system tells them
apart. There are four list shapes:

| Shape | Written | Fixed size | Homogeneous | Meaningful in the type |
| --- | --- | --- | --- | --- |
| tuple-like | `list{T1, T2, ...}` | yes | no | positions |
| record-like | `list{name: T, ...}` | yes | no | names |
| array-like | `list[T]` | no | yes | nothing |
| map-like | `list[named: T]` | no | yes | nothing |

The tuple-like and record-like shapes have a *fixed shape*: their positions or field names are part
of the type. The array-like and map-like shapes are homogeneous collections: every element has the
same type, and no particular position or name is part of the type.

A `list(...)` expression infers a fixed shape whenever its elements carry enough information:

- When all elements are unnamed, it is tuple-like.
- When all elements are named, it is record-like.
- When named and unnamed elements are mixed, the names are dropped and it is array-like, so
  `list(1L, bar = "foo")` is `list[integer | character]`.

A fixed shape wins wherever the elements allow one. `list(1L, 2L, 3L)` is
`list{integer, integer, integer}`, not `list[integer]`, and `list(foo = 1L, bar = 2L)` is
`list{foo: integer, bar: integer}`, not `list[named: integer]`. Array-like and map-like types mostly
come from annotations, and from coercing a fixed-shape list.

#### List coercions

- Any list coerces to an array-like `list[T]` when every element is compatible with `T`.
- A record-like or map-like list coerces to a map-like `list[named: T]` when every value is
  compatible with `T`.
- The reverse is never allowed: a `list[T]` does not coerce back into a tuple-like, record-like, or
  map-like list, and a `list[named: T]` does not coerce back into a record-like one.

#### Tuple-like lists

A `list(...)` expression whose elements are all unnamed is tuple-like, even when every element has
the same type:

- `list()` is `list{}`.
- `list(1L, 2L, 3L)` is `list{integer, integer, integer}`.
- `list(1L, "foo")` is `list{integer, character}`.

#### Record-like lists

A `list(...)` expression whose elements are all named is record-like when the names are known
statically, so `list(foo = 1L, bar = "foo")` is `list{foo: integer, bar: character}`.

Two records are compatible when they declare the same field names and each field's type is
compatible with the field of the same name on the other side. Fields pair by name, so their order
does not matter: `list(label = "a", id = 1L)` satisfies `list{id: integer, label: character}`.

R lets a list name be any string. A field name that is not a syntactic R name is written, and
rendered, in backticks, so `list(\`max size\` = 10L)` has the type `` list{`max size`: integer} ``.

##### Reporting a record that does not fit

When a record is rejected, the finding names the one field that failed rather than printing both
whole types. That field is one of three things: a field both sides declare whose types do not fit, a
field the expected type declares that the value lacks, or a field the value has that the expected
type does not declare. For a nested record, the finding names the path, outermost field first, as
in `retry.count`.

#### Array-like lists

An array-like list `list[T]` is a list whose elements all share the type `T`, with no fixed
positions and no requirement that element names be known. Such types usually come from an
annotation, or from coercing a fixed-shape list whose values are all compatible with `T`.

A fixed-shape list coerces into `list[T]` with `T` being the union of its element types, so
`lapply(list(1L, "a"), f)` passes `integer | character`. Coercion into `list[named: T]` works the
same way. Where `T` is already concrete, every element must fit it.

#### Map-like lists

A map-like list `list[named: T]` is a name-keyed collection whose values all share the type `T`,
without requiring the set of names to be known. Such types usually come from an annotation, or from
coercing a list whose element names are not statically available.

#### The empty list

`list()` is the empty tuple `list{}`. Since it has no element whose type or name could conflict, it
is compatible with every element-typed list shape, `list[T]` and `list[named: T]` alike. That makes
`function(options = list())` a usable default for a `list[named: T]` parameter. A record type with
required fields still rejects it, because those fields really are missing.

### `NULL`

The literal `NULL` has the type `NULL`, which is the unit type of this type system and is
incompatible with every other type. An empty block is also `NULL`.

### `Any` and `Unknown`

`Any` and `Unknown` are both compatible with every type, in both directions. They differ in where
they come from, and therefore in what they mean.

#### `Any`

`Any` is an explicit opt-out from type checking. It comes either from an annotation you wrote or
from a standard-library declaration. The shipped declarations use it where any precise type would
reject calls R accepts, so seeing `Any` in a hover is not by itself a sign of a problem.

#### `Unknown`

`Unknown` means the checker could not work out a more specific type, because of an unsupported or
partly supported construct, an unresolved name, or simply too little information. It is not an
opt-out: it records a gap in the checker's knowledge.

Because `Unknown` is compatible with everything, a gap means a check is skipped rather than answered
wrongly, and a value the checker could not type flows into a `double` parameter without complaint.
The difference in intent from `Any` changes behavior in exactly one place:
[`@if-unknown`](#unknown-only-coercions) supplies a type where one is missing, so it applies to an
`Unknown` and is refused on an `Any`, which already says not to check the value.

[Strict mode](#strict-mode) reports the *sites* where a type could not be determined, not every type
that happens to be `Unknown`. A declaration whose return type is `Unknown` produces no finding at its
call sites, and an `Unknown` nested inside a larger type is not reported either.

### Type parameters, aliases, and nominal types

A type expression can bind type parameters with a leading binder, written `<T> TYPE` or
`<T, U> TYPE`:

- `<T> list[T]`
- `<T> list{ value: T }`
- `<T, U> fn(T) -> U`
- `<T> fn(T) -> T | NULL`

A binder name may carry a constraint, written `NAME: CONSTRAINT`, as in
`<T: numeric> fn(values: T) -> T`. There are two constraints:

- `numeric` admits `integer` and `double`, scalar or vector, and any class that declares an
  arithmetic [operator method](#operator-methods-on-a-class).
- `atomic` admits one of the six atomic scalar types. Using a parameter as a vector element, `T[]`,
  imposes the same bound.

Any other constraint name is an annotation error, and the error lists the two that exist. An
argument whose type violates a constraint is a type error at the call.

A constraint limits what a caller may instantiate `T` with, and the annotated body may rely on it.
Under `<T: numeric> fn(x: T) -> T` the body may use `x` as a number, because every admissible
instantiation is numeric. A body under a plain `<T>` that does arithmetic is a type error, and
`atomic` does not imply `numeric`.

Binders are rank-1: a binder is allowed only at the outermost level of a type expression, never
nested inside another type. A directive's `{...}` payload does not count as the outermost level, so
the expanded form declares its type parameters with `@forall`, and a named type declares them on its
name, as in `@type Pair<T>`. All three of these are refused:

- `fn(f: <T> fn(T) -> T) -> integer`
- `list{ value: <T> list[T] }`
- `@param f {<T> fn(T) -> T}`

A refused binder is reported once. The type is then read as though the binder had not been written,
and the block carries no typing payload.

Named types are declared with `@type` and `@alias`, and either may take parameters, as in
`NAME<T, U>`:

- `#: @type NAME {TYPE}` declares a *nominal type* whose representation is `TYPE`.
- `#: @alias NAME {TYPE}` declares a *structural alias* for `TYPE`.

The two forms share one namespace, so reusing a name under either form is an error. A definition
block is allowed only at the top level of a file. Definitions are project-global, so a reference
need not come after the declaration, whether in the same block or the same file; [type
names](#type-names) has the scoping rules.

#### Type parameters and generic application

A generic application must match its declaration's arity, checked against the project's vocabulary:

- Applying the wrong number of type arguments is an error at the applied name, as `Box<integer, double>`
  is for a one-parameter `Box<T>`.
- Applying type arguments to a declaration that is not generic, such as `Meters<integer>`, is an
  error.
- A bare reference to a generic, such as `Box` without arguments, is an error everywhere except after
  `@new`, where an unapplied generic infers its arguments through the representation check.
- A name applied wrongly afterwards compares like `Unknown`, so the one arity error never cascades
  into mismatches between values.

Named generics, whether aliases or nominal types, are applied with angle brackets, as in
`Box<integer>` and `Pair<integer, character>`. For `Pair<T, U>`, `Pair<integer, character>` is
valid, while `Pair<integer>` and `Pair<integer, character, double>` are errors. `@new` uses the same
syntax to introduce a value of a generic nominal type: `#: @new Person<integer>` is valid when
`Person<T>` is declared with `@type`. `#: @new Person` is valid too, because the arguments come from
the representation check; this is the one place an unapplied generic is allowed.

In `@type NAME<T, U, ...> {TYPE}` and `@alias NAME<T, U, ...> {TYPE}`, the declared type parameters
are in scope only within `TYPE`, where they shadow any project-global type of the same name. A type
parameter is not itself a generic, so applying type arguments to one, as in `Wrap<integer>` where
`Wrap` is a parameter, is an annotation error rather than a reference to the shadowed global.

A type parameter may appear inside a structural type, a function type, and the vector suffix forms:
`list[T]`, `list{ value: T }`, `fn(T) -> T`, `T | NULL`, `T[]`, and `T[named]`.

Using a type parameter as a vector element restricts it. A `T` in `T[]` carries the atomic-element
bound, so it can only be instantiated with one of the six atomic types: `logical`, `integer`,
`double`, `complex`, `character`, and `raw`. This is what makes element-preserving signatures
possible. With `sort : <T> fn(x: T[]) -> T[]`, `sort(c("b", "a"))` is `character[]` and
`sort(c(1L))` is `integer[]`, while a list argument cannot bind `T` at all, because a list is not an
atomic element type. Several rules follow from the bound:

- A scalar argument coerces into a generic vector parameter and binds the element, so
  `<T> fn(x: T[])` called with `2.5` binds `T := double`.
- `[[` on a generic vector `T[]` extracts `T`.
- An arithmetic operator on a `T[]` operand also requires the element to be numeric. The variable
  then holds both bounds, so it is a scalar `integer` or `double`. The result keeps the element, so
  `sort(x) + 1L` is still `T[]`, unless a `double` operand promotes the result to `double[]`.
- A comparison on a `T[]` operand yields `logical[]`, and a numeric partner requires the element to
  be numeric.

A bound that can no longer be satisfied is a type error at the expression that imposed it, whether
that means binding an element variable to a type that is not atomic or requiring a `character`
element to be numeric.

Writing `X[]` is an error when `X` is neither an atomic type nor a type parameter, because a vector
holds only atomic elements. Records, functions, and nominal types are all such an `X`, and `list[X]`
is how to write a list of them. An alias expands first, so `Id[]` with `@alias Id {integer}` is fine,
while an alias of a record is refused.

#### Type aliases

An alias is purely structural: writing its name is the same as writing the type it stands for. It
creates no new identity and is compatible with whatever its underlying type is. An alias may appear
anywhere a type may, including inside a larger type, and a cycle of alias definitions is an error.

```r
#: @alias PersonShape {list{ name: character, age: double }}

#: PersonShape
value <- list(name = "bob", age = 20)
```

A generic alias may use its parameters anywhere inside the underlying type:

```r
#: @alias Box<T> {list{ value: T }}

#: Box<integer>
value <- list(value = 1L)
```

#### Nominal types

A nominal type creates a fresh type identity, even when another nominal type has exactly the same
representation. A nominal type name may appear anywhere a type expression may, and a generic nominal
may use its type parameters anywhere inside its representation.

- A nominal type is compatible with itself.
- Two different nominal types are not compatible with each other, even when their representations
  are identical.
- A structural value is not compatible with a nominal type unless `@new` introduces it.
- A value of a nominal type *is* compatible with its representation type.
- When an operator, an indexing form, or a loop needs a structural shape, a nominal value projects to
  its representation type. The result of the projection is structural, not nominal.

`Person` and `Pet` below are distinct and incompatible, even though their representations are
identical. A nominal value satisfies its representation type, so the annotation on `shape` is valid.
A structural value does not satisfy the nominal type, so the final call is an error:

```r
#: @type Person {list{ name: character, age: double }}
#: @type Pet {list{ name: character, age: double }}

#: @new Person
person <- list(name = "bob", age = 20)

#: list{ name: character, age: double }
shape <- person

#: fn(value: Person) -> character
get_name <- function(value) value$name

get_name(list(name = "bob", age = 20))
```

Projection is what makes field access and arithmetic work on a nominal value, and its result loses
the nominal identity:

```r
#: @type Person {list{name: character}}

#: @new Person
person <- list(name = "bob")

person$name
```

`person$name` is `character`, because `$` sees the representation of `Person`.

```r
#: @type Meters {double}

#: @new Meters
height <- 1.8

height + height
```

`height + height` is `double`: arithmetic projects `Meters` to `double`, and the result does not keep
the nominal identity.

A generic nominal takes its parameters on the declared name:

```r
#: @type Person<T> {list{ value: T }}

#: @new Person<integer>
person <- list(value = 1L)

#: list{ value: integer }
shape <- person
```

##### Opaque nominal types

An *opaque* nominal type has no representation to project. The standard-library stubs declare
`data.frame`, `factor`, `connection`, `Date`, and other types the grammar cannot describe
structurally as a bare `@type NAME` (see [stubs](/type-checking/stubs)). Four rules apply to them:

- `$`, `[`, and `[[` are accepted, and give `Unknown` rather than an error. The R objects behind
  these classes commonly support access that depends on values, such as `df$amount` and
  `df[rows, ]`, and refusing it would reject ordinary R.
- The access is not checked any further: there is no field-existence check, no index-count check,
  and no index-type check, so `df[i, j]` and `df[rows, ]` both pass.
- Every such access is an unsupported construct under [strict mode](#strict-mode).
- Arithmetic, loops, and every other structural requirement on an opaque nominal remain type errors,
  unless the class declares the matching [operator method](#operator-methods-on-a-class). The
  nominal identity itself checks like any other nominal type.

#### Type-argument variance

Two applications of the same generic nominal type, such as `Box<integer>` and `Box<integer | NULL>`,
are compared one type argument at a time. The direction in which each argument is checked depends on
where its type parameter occurs in the representation, so each parameter's *variance* follows from
its occurrences:

- **Covariant** positions keep the checking direction. They are a function's return, a direct
  occurrence, and an element of a container or structural type (a `list` item, a `list{...}` field, a
  tuple item, a vector element, or a union member). So `Box<integer>` is accepted where
  `Box<integer | NULL>` is expected, because a narrower argument satisfies a wider one.
- A function **parameter** position is contravariant and flips the direction. Given
  `@type Handler<T> {fn(value: T) -> NULL}`, a `Handler<integer | NULL>` is accepted where a
  `Handler<integer>` is expected, but not the other way around, because then a `NULL` could reach a
  function that only accepts `integer`.
- A parameter that occurs in both kinds of position is **invariant**, and its arguments must match
  exactly. Given `@type Cell<T> {list{ get: T, set: fn(value: T) -> NULL }}`, `Cell<integer>` and
  `Cell<integer | NULL>` are incompatible in both directions.
- A parameter that does not occur at all constrains nothing, and accepts any argument.

A type parameter inside a nested generic application, such as the `T` of `Sink<T>` within
`@type Outer<T> {Sink<T>}`, is invariant: the inner type's own variance does not compose with the
outer direction. When a generic nominal has no visible definition, every argument is checked
invariantly, which errs toward rejecting (requiring an exact match) rather than toward accepting an
unsound widening.

R's lists and vectors are mutable, yet their element positions are still treated as covariant. A
sound mutable container would have to be invariant, which would break structural coercions such as
scalar to vector and `T` into `T | NULL`.

Where a single representative type is needed, every nominal argument must match exactly, whatever
the parameter's variance.

### Union types

A union type `A | B | ...` describes a value that has one of the member types. It may have any
number of members, and any type may be a member; `T | NULL` is the nullable form of `T`. A union can
be written anywhere a type can (a variable annotation, a parameter, a return, a nested function
type, a list annotation), and it describes the shapes a value can take without merging or coercing
its members:

- `integer | character`
- `character[] | NULL`
- `fn(count: integer | NULL) -> character | logical | NULL`
- `(fn() -> integer) | NULL`, a function returning `integer`, or `NULL`

That last example shows why a type may be parenthesized for grouping, where `(TYPE)` means exactly
`TYPE`. In `fn() -> integer | NULL`, the `->` extends over the whole union, so an optional callback
has to be written `(fn() -> integer) | NULL`, which is also how it renders. A `<T>` binder may not
appear inside parentheses.

#### Union normalization

Unions are kept in one normal form, so equivalent spellings mean the same type and render the same
way:

- A member that is itself a union is flattened, so `(A | B) | C` becomes `A | B | C`.
- Repeated members collapse, keeping the first, so `integer | character | integer` becomes
  `integer | character`.
- Member order does not affect meaning. Rendering keeps first-occurrence order, except that `NULL`
  always renders last.
- A union whose members collapse to one type is that type, so `integer | integer` is `integer` and
  `NULL | NULL` is `NULL`.
- A union with an `Any` member is `Any`, because every value already satisfies `Any`.
- Otherwise, a union with an `Unknown` member is `Unknown`.

The checker's own unions are normalized too, so a rendered union is always flat, free of duplicates,
and at least two members long.

### Union compatibility

A value fits an expected union when it fits any one member, with the usual coercions applied per
member. So `integer` fits `integer | character | NULL`, and `NULL` fits any union that contains
`NULL`.

A union *value* has to be acceptable in every shape it can take, so it fits an expected type only
when every one of its members does. That means a union fits any wider union (`integer | NULL` fits
`integer | character | NULL`), but not a single member type: `integer | character` does not fit
`integer`, and `T | NULL` does not fit plain `T`.

Members are tried in order, and a failed attempt leaks no bindings into the next one.

An unannotated value checked against an expected union binds to the whole union at its first use.
Uses commit in program order, so a later use that needs a different type is reported at that later
site. When two union-typed contracts share only some members, such as `integer | character` at one
call and `logical | character` at the next, the checker does not compute their intersection; annotate
the value with the member type you intend.

## Function types

A function can be annotated in one of two styles, but not both at once:

- the **expanded** style, with an optional `@forall`, then `@param` lines, then `@return` or
  `@returns`;
- the **compact** style, a single `fn(...)` type with an optional `-> RETURN_TYPE`.

Either way, consecutive `#:` lines form one annotation block for the function, not a series of
independent annotations.

### Expanded function annotations

The expanded form puts one directive on each line: `@forall` to declare type parameters,
`@param name {TYPE}` for each parameter (or `@param [name] {TYPE}` for an optional one), and
`@return {TYPE}` or `@returns {TYPE}` for the result. A constrained binder is written
`@forall T: numeric`, with the same names and meaning as the compact `<T: numeric>`.

```r
#: @param count {integer}
#: @param [label] {character | NULL}
#: @return {integer}
double_count <- function(count, label = NULL) { count + count }
```

```r
#: @forall T
#: @param condition {logical}
#: @param value {T}
#: @return {T | NULL}
then_some <- function(condition, value) {
  if (condition) value
}
```

- Directives come in order: every `@forall` before any `@param`, and every `@param` before `@return`
  or `@returns`.
- Repeated `@forall` lines accumulate in source order, and a duplicate type parameter name is an
  error.
- At most one `@return` or `@returns` may appear. Without one, the return type is
  [elided](#elided-return-types).

### Compact function annotations

A compact annotation is a single function type, in one of these forms:

- `fn(name: TYPE) -> RETURN_TYPE`
- `fn(TYPE) -> RETURN_TYPE`
- `fn(name: TYPE, [optional_name]: TYPE) -> RETURN_TYPE`
- `<T> fn(name: TYPE) -> RETURN_TYPE`
- `<T, U, ...> fn(TYPE) -> RETURN_TYPE`

A leading `<...>` binder introduces rank-1 type parameters for the whole function type. The binder
always comes first: `fn<T>(...)` is not the syntax. An optional parameter must be named, as in
`[name]: TYPE`; a bare optional positional form such as `fn(integer, [character])` is not supported.
Omitting the return type [elides](#elided-return-types) it.

A function may declare one rest parameter to accept a variable number of arguments. It is written
`...: TYPE`, and `fn(...)` is shorthand for `...: Any`. Naming it, as in `...items: TYPE`, is an
annotation error, because rest arguments are matched by position. It can come after the fixed
parameters, as in `fn(prefix: TYPE, ...: TYPE) -> RETURN_TYPE`, or before named ones, as in
`fn(...: TYPE, [option]: TYPE) -> RETURN_TYPE`:

```r
#: fn(...: character) -> character
join <- function(...) paste0(...)

#: fn(x: character, ...: character) -> character
wrap <- function(x, ...) paste0(x, ": ", paste(...))
```

The position of the rest parameter is part of the signature. It must match the position of `...` in
the function's formals, counted as the number of parameters declared before it, and it decides which
parameters a positional argument can fill (see [function calls](#function-calls) and
[function type compatibility](#function-type-compatibility)).

### Annotations and the formal list

An annotation declares the *types* of a definition's parameters; it does not declare the parameter
*list*. R matches a call's arguments against the formals in the `function(...)` header, so those
formals are the call interface: their names, their order, their defaults, and where `...` sits. An
annotation cannot add, remove, or reorder them. A parameter the annotation does not mention keeps
its inferred type, which makes annotating just one of several parameters a supported partial form.

When the declared shape disagrees with the definition, the definition wins at every call site, and
the disagreement is reported once, at the definition. A call is never blamed for a mistake in an
annotation. This is the same rule that [refused blocks](#refused-blocks) follow, extended to an
annotation that parses cleanly but has the wrong shape. The possible disagreements are:

- a declared parameter name that is not one of the formals, meaning the annotation describes a
  parameter the function does not have;
- more declared parameter types than there are formals left to receive them;
- a declared optional `[name]` over a formal with no default. Declaring a parameter optional tells
  callers they may omit it, so the formal must have a default. The reverse is fine: a formal with a
  default that the annotation declares as required is not a disagreement;
- a rest parameter at a different position in the annotation than in the formals. The rest parameter
  must also exist on both sides or on neither, so a fixed annotation on a variadic function and a
  variadic annotation on a fixed function are both rejected.

The first case names the parameter, and the other three are reported as a mismatch of the whole
signature. In every case, the body is still checked under whatever parameter types the annotation
does pin down, so hover and navigation keep their facts.

### Elided return types

Both styles allow the return type to be left out: an expanded block without `@return` or
`@returns`, or a compact `fn(...)` without `-> RETURN_TYPE`. An elided return is not the same as a
written `NULL`, and what it means depends on whether there is a function body to infer from.

**When there is a body, the return type is inferred from it**, exactly as if there were no
annotation. That is the case for a checked annotation on a function definition, meaning an
annotation attached to a `function(...)` literal whose body is checked against it. Annotating only
the parameters is the usual partial form, and it must not silently pin the return type, so
`@param u {integer}` on `add_one <- function(u) u + 1L` infers `fn(u: integer) -> integer`. Writing
the return as `Unknown` has the same effect, because `Unknown` says nothing is known and so never
overrides what the body shows. (`Any` is the annotation that turns checking off for a value.)

**When there is no body, an elided return means `NULL`**, which matches R functions called for their
side effects. There are three such positions:

- a nested function type, such as a callback parameter written `@param cb {fn(integer)}`;
- a [trusted coercion](#trusted-coercions) or an [`@if-unknown` coercion](#unknown-only-coercions),
  both of which adopt exactly the written type without looking at the body;
- an annotation on a value that is not a function literal, such as `#: fn(integer)` on `g <- f`.

A function that really does return `NULL` can always say so, with `@returns {NULL}` or `-> NULL`.
The explicit form is enforced, so a body that returns anything other than `NULL` is then a type
error.

```r
#: fn(count: integer) -> integer
double_count <- function(count) count + count
```

```r
#: fn(count: integer, [label]: character | NULL) -> integer
double_count <- function(count, label = NULL) count + count
```

```r
#: fn(count: integer)
log_count <- function(count) { }
```

```r
#: <T> fn(value: T) -> T
identity <- function(value) value
```

```r
#: <T> fn(condition: logical, value: T) -> T | NULL
then_some <- function(condition, value) {
  if (condition) value
}
```

### Inferred function types

An unannotated `function(...)` expression gets its type from the definition itself:

- Every parameter appears as a named parameter under its name in the definition, because R matches
  parameters both by name and by position.
- A parameter with a default is optional at call sites, and so is one the body tests with
  [`missing()`](#missing-on-a-defaultless-formal).
- A `...` formal becomes a rest parameter with element type `Any`, at the position it holds among
  the formals, so `function(x, ...) …` is `fn(x: T, ...: Any) -> …`. The values that reach `...` are
  not tracked into the body, so forwarding `...` to another call gives `Unknown`.
- Parameter and return types are inferred, and a parameter nothing constrains generalizes when the
  function is bound to a name, so `function(x) x` is `<T> fn(x: T) -> T`.
- A requirement inference could not discharge survives into the exported type: a parameter used as a
  number exports as `<T: numeric>`, so calls from other files keep being checked.

`function(count, label = NULL) count` may therefore be called as `f(1L)`, `f(count = 1L)`, or
`f(1L, "x")`.

#### Defaults

Defaults are type-checked, and they interact with the parameter's type in four ways:

- An error inside a default is reported, and the default of an annotated parameter must be
  compatible with the declared type.
- A `NULL` default is checked like any other. `function(title = NULL)` is R's usual way to write an
  optional argument, but it does not make the parameter optional *inside the body*: when the caller
  omits it, `title` is `NULL` there, so declaring it `character` is a promise the function does not
  keep. Declare `character | NULL` and narrow with `if (is.null(title))`. Marking the parameter
  `[title]` relaxes only the call.
- An unannotated parameter takes its type from its uses, not from its default, so
  `function(x = 1) x` is `<T> fn([x]: T) -> T`, and passing a character is not a finding.
- A call that omits the argument gets the default's type, because that is the value R puts in the
  frame: with `f <- function(x = 1) x`, `f()` is a `double` and `f("a")` is a `character`. A default
  is therefore checked against an *instantiation* of the declared type rather than against the
  binder, so `#: <T> fn([x]: T) -> T` over `function(x = 1) x` is accepted. A concrete declared type
  is unaffected: `fn(title: character)` still refuses a `NULL` default.

### Named and positional parameters

Parameter names in a function type are part of the call interface. A named parameter may be passed
as a named argument, while an unnamed parameter is positional only. So `fn(count: integer) -> integer`
allows a call with `count = 1L`, while `fn(integer) -> integer` makes a named argument a type error,
but only when no definition stands behind the type. When the annotation sits on a `function(count)`
definition, the formals supply the name, and `f(count = 1L)` stays legal.

An optional parameter follows the same rule, and must be named, as in
`fn(count: integer, [label]: character) -> integer`.

Parameter and record field names may contain an interior `.`, matching R's convention for arguments
such as `na.rm` and `length.out`: `fn(x: double, na.rm: logical) -> double` and
`list{na.rm: logical}` are both valid. The first character must still be a letter or `_`, and the dot
can only be interior. Type names and type parameter names cannot contain a `.` at all.

### Function type compatibility

A function value is compatible with an expected function type when its parameters accept every call
the expected type may make, and its return type satisfies the expected return type.

Parameter names are part of the interface, because R matches arguments against the definition's
formal names:

- A named parameter pairs by name, so `fn(a: integer, b: character)` accepts a function defined as
  `function(b, a)`.
- An unnamed parameter type pairs with the remaining parameters from left to right, so
  `fn(count: integer) -> NULL` and `fn(integer) -> NULL` are compatible with each other.
- An annotation may not rename a parameter. `fn(count: integer) -> integer` over `function(n) n` is
  an error, because it would promise callers a name the function does not accept.

Arity is a range, not a fixed count. The function may declare more parameters than the interface
passes, as long as the extra ones have defaults. It may not require more than the interface supplies,
and it may not refuse an argument the interface may send. An optional parameter in the expected type
promises callers they can omit it, so the function's formal must have a default.

Parameters are contravariant and the return type is covariant. Each expected parameter type must be
compatible with the function's parameter type, so the function accepts every argument the interface
may pass, and the function's return type must be compatible with the expected one:

- `fn(integer | NULL) -> integer` is accepted where `fn(integer) -> integer` is expected.
- `fn(integer) -> integer` is rejected where `fn(integer | NULL) -> integer` is expected, because the
  interface may pass `NULL`.
- `fn(a: integer, [b]: integer) -> integer` is accepted where `fn(integer) -> integer` is expected,
  because `b` has a default. This is what lets a standard-library function serve as a callback:
  `lapply(list(mean, sd), function(g) g(1:3))` is `list[double]`, even though `mean` and `sd` declare
  optional formals the callback never passes.
- `fn(a: integer, b: integer) -> integer` is rejected there, because the interface never supplies `b`,
  and `fn() -> integer` is rejected because it cannot receive the argument the interface sends.
- `fn(count: integer, [label]: character) -> integer` does not accept `function(count, label) count`,
  because `label` has no default.

Variadic compatibility is conservative. A variadic function type is compatible only with another
variadic function type, never with a fixed-arity one in either direction. Their rest element types
are contravariant like any parameter, and both sides must declare the same number of parameters
before `...`, because that position decides which parameters callers can fill by position. This
rejects some safe pairings, but never admits an unsound one.

#### Callback forwarding at variadic call sites

R's apply family calls its callback as `FUN(element, ...)`, forwarding the caller's extra arguments,
so a callback with more formals than the interface declares is still correct when the call forwards
the difference. At a call to a variadic function, a function-typed argument that fails the plain
interface check is therefore checked again, as that forwarded call:

- Forwarded named arguments fill the callback's formals of the same name first, each checked against
  its formal's type.
- The interface's own parameter types then fill the remaining formals in order, followed by the
  forwarded positional arguments.
- Any formal left unfilled must have a default.
- The callback's return type must satisfy the interface's return type.
- If the second check fails, it binds nothing, and the reported error is the plain interface
  mismatch.

`lapply(words, gsub, pattern = "a", replacement = "o")` is therefore checked as
`gsub(word, pattern = "a", replacement = "o")` and is `list[character]`, and `lapply(words, nchar)`
accepts `nchar`'s optional formals. A forwarded argument of the wrong type fails the check, and the
call is an error.

#### Reporting a function that does not fit

When a function value is rejected at a parameter, the finding names the one position that failed
rather than printing both whole signatures: either the parameter to which the interface passes a
value the function will not take, or the function's return, which the interface will not take.

### Higher-order function types

A function type may appear inside another function type. Polymorphism is rank-1 only, so a binder
cannot appear inside a nested type:

- `fn(transform: fn(integer) -> character) -> character` is valid.
- `fn(fn(integer) -> character, integer) -> character` is valid.
- `fn(transform: <T> fn(T) -> T, integer) -> integer` is not.
- `fn(fn(value: <T> list[T]) -> integer) -> integer` is not.

The expanded form can use a function type directly:

```r
#: @param render_count {fn(integer) -> character}
#: @param count {integer}
#: @return {character}
apply_renderer <- function(render_count, count) { render_count(count) }
```

## Type annotations and assertions

### Checked annotations

`#: TYPE` is a checked annotation: the annotated value must be *compatible* with `TYPE`. Checking is
based on compatibility rather than exact equality, so it allows widening wherever the rules on this
page define it. When the check succeeds, the value is accepted (through a coercion if one is needed),
and the annotated binding or expression has the type `TYPE` from then on.

```r
#: list[integer]
value <- list(1L, 2L, 3L)
```

This is valid, because `list{integer, integer, integer}` is compatible with `list[integer]`.

### Unknown-only coercions

`#: @if-unknown TYPE` fills a gap in inference. It is allowed only when the inferred type is
`Unknown`, and it then gives the annotated binding or expression the type `TYPE`. When the checker
already knows the type, `@if-unknown` is an error, even if the requested type matches the known one,
so it can never override information the checker has.

```r
#: @if-unknown integer
value <- unsupported_value
```

This is valid only if `unsupported_value` is `Unknown`.

```r
#: @if-unknown integer
value <- 1L
```

This is an error, because the checker already knows the type.

### Trusted coercions

`#: @trust TYPE` is the unchecked override: it tells the checker to treat the annotated value as
`TYPE` without requiring the usual compatibility at that site. It has the same effect as coercing the
value to `Any` and then to `TYPE`, and exists as a shorter way to write that.

```r
#: @trust integer
value <- external_input
```

```r
#: @trust fn(count: integer) -> character
render_count <- callback
```

A trusted coercion silences a real error as readily as a false one, so use it only when you know
more than the checker does.

### Nominal introduction

`#: @new NOMINAL_TYPE` is the one way to create a value of a nominal type. The annotated value must
be compatible with the nominal type's representation, and when it is, the annotated binding or
expression has the nominal type from then on.

- `NOMINAL_TYPE` must refer to a nominal type declared with `@type`, either by its bare name, such as
  `Person`, or as a generic application, such as `Person<integer>`. An alias, a structural type, a
  union, a function type, or any other type form is not allowed after `@new`.
- A generic nominal may be written unapplied. `@new Person` on a `Person<T>` infers the type
  arguments from the representation check, so a value of `list{value: 1L}` becomes a
  `Person<integer>`.
- When the value already has the nominal type, `@new` is allowed and does nothing further.
- `@new` is an annotation form, not a type expression, so it cannot appear inside a compact type or
  an expanded function annotation.
- A checked annotation such as `#: Person` on a structural value is a type error, even when the value
  matches the representation. The checked form asserts that the value *already* has the nominal type;
  it does not create one.

```r
#: @type Person {list{ name: character, age: double }}

#: @new Person
value <- list(name = "bob", age = 20)
```

```r
#: @type Person<T> {list{ value: T }}

#: @new Person<integer>
value <- list(value = 1L)
```

```r
#: @type Person {list{ name: character, age: double }}

#: Person
value <- list(name = "bob", age = 20)
```

The third example is an error: an ordinary checked annotation with a nominal type requires the value
to be a `Person` already.

## Operators

### Operators over union operands

Control-flow joins and mixed containers produce union-typed operands, so every operator below accepts
a union member by member. A union operand is accepted when every member is accepted; one unacceptable
member rejects the whole operand, and the diagnostic shows the full union. The result is the join of
the per-member results, which for a binary operator means the join over every pair of left and right
members.

- `(integer | double) + integer` is `integer | double`, because `integer + integer` is `integer` and
  `double + integer` is `double`.
- `(integer | double) > 0L` is `logical`.
- `(integer | character) + 1L` is a type error, because the `character` member is not numeric.
- `(integer | NULL) + 1L` is a type error, because the `NULL` member is not numeric.
- `rec$a` on `list{a: integer} | list{a: character}` is `integer | character`. A member that lacks
  the field contributes `NULL` instead, as [indexing](#indexing) describes.
- A `for` loop over `integer[] | character[]` binds its variable as `integer | character`.

#### Conditions

`if`, `while`, and the operands of `&&` and `||` all take a *scalar condition*, and R decides what a
scalar condition admits:

- `logical` is the ordinary case.
- `integer` and `double` are accepted, and they coerce exactly as R coerces them: zero is false, and
  anything else is true. That makes `if (length(x))`, `if (nrow(df))`, and `while (n)` ordinary R
  rather than mistakes.
- `character`, `complex`, and `raw` are type errors. R refuses `complex` and `raw` outright. For a
  `character` condition, R accepts only spellings of `TRUE` and `FALSE` such as `"T"` and `"true"`,
  and fails at run time on every other string, so `if ("yes")` is reported.
- A vector is a type error, because a condition whose length is not one is an error in R too.
- A condition whose type is still undetermined becomes `logical`. That is the useful default for an
  unannotated predicate, so `function(flag) if (flag) 1L` infers `flag: logical`.

`!` coerces its operand the same way, so `!0` is `TRUE` and `!5` is `FALSE`; see [unary `!`](#unary-)
for the result.

### Indexing

`[[` extracts a single element, and `[` is R's general subsetting operator, defined here for the
vector and list shapes. Failures that only happen at run time are not modelled anywhere in this
section: an out-of-range position or a missing name gives `NA` at run time, which is a property of
the value, not the type.

`$name` behaves like `[["name"]]` on lists, on records, and on the opaque nominals where access is
tolerated, and a backtick-quoted name follows the same rule. It does not work on atomic vectors,
because R rejects `$` on every atomic vector, named ones included: `c(foo = 1L)$foo` is a type error,
while `c(foo = 1L)[["foo"]]` is `integer | NULL`.

On a union, a field may be missing from some of the members. R answers `NULL` for a name a list does
not have, so a field present in only some of the subject's shapes reads as that field's type unioned
with `NULL`. Code that builds a list field by field therefore checks:

```r
args <- list()
if (escape) args$escape <- TRUE
args$escape        # logical | NULL
```

A field that *no* member has is still an error, because that is a typo rather than an absence the
program is prepared for. The "did you mean" suggestion draws on every field that any member has.

#### `[[` on vectors

- A scalar-like `T` returns `T`.
- An array-like `T[]` returns `T`.
- A map-like `T[named]` returns `T | NULL` for a name index.

#### `[[` on lists

- An array-like `list[T]` returns `T`.
- A map-like `list[named: T]` returns `T | NULL` for a name index, and `T` for a positional or
  computed one.
- A tuple-like list returns the element at a literal position. A position that does not exist is an
  error, and a computed position returns the union of the item types.
- A record-like list returns the field at a literal name or literal position. A name or position that
  does not exist is an error, and a computed index returns the union of the field types, which is how
  a call to a function looked up in a list gets its type.

#### `[` on vectors

The result of `[` on a vector depends on the shape of the subject and the shape of the index. The
index shapes are:

- A scalar-like `integer`, `double`, or `character` index selects one position and gives the scalar
  element type. That is not always exact, since a scalar negative index such as `x[-1]` drops one
  element and returns the rest, but a scalar coerces into every vector position, so the claim can
  never cause a false error later.
- An array-like or map-like numeric or character index, such as `x[c(1L, 3L)]`, selects many
  positions and keeps the subject's shape.
- A `logical` index of any shape is a mask, as in `x[x > 0]`, and keeps the subject's shape. A scalar
  `TRUE` or `FALSE` is recycled over the whole vector.
- `NULL` selects nothing and gives the array-like vector of the element type.
- An index whose shape is undetermined, such as an unannotated parameter or an `Unknown`, counts as
  scalar-like and is left unconstrained.
- A `complex` or `raw` index is a type error, as is a list, a function, or any other index that is not
  a vector.

With `E` as the element type, the results are:

| Subject | Scalar numeric or character index | Any other index |
| --- | --- | --- |
| scalar-like `E` | `E` | `E[]` |
| array-like `E[]` | `E` | `E[]` |
| map-like `E[named]` | `E` | `E[named]`, because `[` keeps names |

A character index is allowed on every vector shape, not only a map-like one. R returns `NA` rather
than failing when the subject has no names, and most operations erase names, so requiring a map-like
subject would flag legal programs.

- `c(1L, 2L, 3L)[2L]` is `integer`.
- `c(1L, 2L, 3L)[c(1L, 3L)]` is `integer[]`.
- `x[x > 0]` on `x: double[]` is `double[]`.
- `c(a = 1L, b = 2L)[c("a", "b")]` is `integer[named]`.
- `x[list(1)]` is a type error.

#### `[` on lists

`[` slices a list, so a fixed shape does not survive into the result:

- An array-like `list[T]` returns `list[T]`.
- A map-like `list[named: T]` returns `list[named: T]`.
- A tuple-like list returns `list[T]`, where `T` is the union of the item types, so
  `list(1L, "foo")[1L]` is `list[integer | character]`.
- A record-like list returns `list[named: T]`, where `T` is the union of the field types.
- Slicing the empty list gives `list[NULL]`.

#### Indexing opaque nominal types

`$`, `[`, and `[[` on an opaque nominal type such as `data.frame` or `factor` give `Unknown` without
further checking; [opaque nominal types](#opaque-nominal-types) explains why.

#### Indexing a value whose shape is unknown

`$`, `[[`, and `[` on a value whose shape the checker has not determined give `Unknown`, and leave the
value unconstrained. An unannotated parameter is such a value, as in `function(node) node$value`,
`function(x) x[[1L]]`, and `function(x) x[1L]`, and none of these reports a "not a list" or
"unsupported `[`" error.

Ordinary R reads fields, elements, and slices off values whose shape nobody wrote down. A tree fold
does it, and so does a generic accessor, so refusing here would report correct code. The access is
therefore left undescribed rather than refused, and reported as an unsupported construct under
[strict mode](#strict-mode), exactly as for an opaque nominal.

That covers indexing with several indices too: `function(m, i, j) m[i, j]` is silent, because such a
function is written for a caller that knows the shape even though the callee does not. A subject
whose shape *was* written down still refuses a shape no rule covers, so `c(1L, 2L)[1L, 2L]` is an
error.

### Unannotated values in arithmetic

An unannotated value used as an arithmetic operand is *required to be numeric* rather than rejected:
it must end up as `integer` or `double`, in any vector shape, or as a class that declares an
arithmetic [operator method](#operator-methods-on-a-class).

- `function(x) x + 1L` is `<T: numeric> fn(x: T) -> T`, and calling it with `"oops"` is a type error.
- `function(x) x / 2` is `<T: numeric> fn(x: T) -> double`.
- `function(a, b) a + b` is `<T: numeric> fn(a: T, b: T) -> T`.

A value that is still unconstrained when it reaches a binding defaults to `double`, which matches how
R treats bare numbers.

### Arithmetic operators

The arithmetic operators are defined for numeric operands:

- `integer` and `double`;
- `logical`, which R promotes to `integer` before arithmetic, so `TRUE + TRUE` is `2L`. A logical
  operand therefore computes as `integer`, and the result rules below need no logical case;
- an unannotated value, which is then [required to be numeric](#unannotated-values-in-arithmetic);
- a class that declares an [operator method](#operator-methods-on-a-class).

A map-like vector takes part through its compatibility with an array-like vector, so arithmetic does
not preserve map-likeness.

An operand whose shape is still unknown, such as an unannotated parameter, counts as scalar-like, both
here and in the comparison rules, by the same scalar claim that [`[` on vectors](#-on-vectors)
makes. The cost is that a function taking a vector and returning a vector does not track that shape.
A generic vector written `T[]` is the exception, and its operator results really are vectors.

#### Result shapes

Every arithmetic operator uses the same shape rule: the result is scalar-like when both operands are
scalar-like, and array-like otherwise. A map-like operand therefore gives an array-like result.
Unary `-` keeps a scalar-like or array-like operand's shape. Only the atomic type of the result
differs between operators:

| Operator | Atomic result |
| --- | --- |
| `+`, `-`, `*`, `%%`, `%/%` | `integer` when both operands are `integer`, otherwise `double` |
| `/`, `^`, `**` | always `double` (`**` is R's parser alias for `^`) |
| unary `-` | the operand's own atomic type |

- `integer + integer` is `integer`.
- `integer - double` is `double`.
- `double * integer[]` is `double[]`.
- `integer[named] + integer` is `integer[]`.
- `integer / integer` is `double`.
- `2L ^ 3L` is `double`.
- `-c(foo = 1L, bar = 2L)` is `integer[]`.

Every `%op%` operator other than `%%` and `%/%` is covered under
[operator methods on a class](#operator-methods-on-a-class).

### Operator methods on a class

When an operand of an operator is a nominal type, the operator first dispatches to the class's
declared operator method, before any numeric rule applies. This is how R dispatches `d + 30L` on a
`Date` through `+.Date`.

The lookup follows R's own order: first the operator-specific method, such as `+.Date`; then the
operator's S3 group generic, which is `Arith.Date` for arithmetic and `Compare.Date` for comparison;
and finally `Ops.Date`. Either operand's class may supply the method, the left one first, so
`30L + d` behaves like `d + 30L`.

A method is declared under the name R gives it, in a stub or an annotation, so the result stays
precise for each pairing of operands: subtracting two `Date` values gives a `difftime`, and offsetting
one by a count gives a `Date`. A class that declares an operator, but has no candidate that accepts
the operands at hand, is a `type-mismatch` error rather than a fall-through to the numeric rules, just
as in R. A class that declares nothing does fall through to the numeric rules, so arithmetic on an
opaque nominal is still a type error.

Your own classes count. A method declared anywhere the global scope reaches, which includes a
package's `R/` sources and a script's own top level, makes its class support that operator exactly as
a shipped stub does. That is also how the class satisfies the numeric requirement: passing a `Money`
to `function(x) x + 1L` is accepted when the project defines `+.Money`, and refused when it defines
no arithmetic method.

`c()` dispatches the same way. A class that declares a `c.Class` method keeps its class through
concatenation, so `c(d1, d2)` on two `Date` values is a `Date`. A nominal with no such method gives
`Unknown`, because R's default `c()` strips attributes.

The method name's suffix is the nominal type's name, not R's full class vector, so a class declared
as `@type ggplot` takes `+.ggplot`, even though R registers the method as `+.gg`.

`a %op% b` is the call `` `%op%`(a, b) ``. A `%…%` operator that the standard-library stubs declare
is checked and typed as that call, so `"a" %in% valid` is `logical` and `m %*% m` is a `matrix`.
Every other `%…%` operator, including a project's own, gives `Unknown`, because a user operator may
quote its right operand instead of evaluating it, as magrittr's `%>%` does. Using any `%…%` operator
still counts as a read of its name, so a project's own operator is never reported as unused.

### Comparison operators

`<`, `<=`, `>`, `>=`, `==`, and `!=` compare two operands of the same *comparison family*. There are
two families:

- the **numeric** family, which holds `logical`, `integer`, and `double`, freely mixed. R promotes a
  logical operand to `integer` before comparing, exactly as it does for arithmetic, so `flags > 0` and
  `flag == TRUE` are ordinary numeric comparisons;
- the **character** family, which holds `character`.

Both operands must belong to the same family, and comparing across families is a type error.
`complex` and `raw` operands are not supported. A map-like vector takes part through its
compatibility with an array-like vector.

An unannotated operand is required to be numeric when the other operand is concretely numeric, and
otherwise left unconstrained. When neither operand is concrete, both stay unconstrained, so
`function(a, b) a < b` is `<T, U> fn(a: T, b: U) -> logical`, and calling it across families is
accepted. There is deliberately no "comparable" constraint: R's comparison coerces across atomic
families at run time, so `1 < "2"` is legal R, and tying undetermined operands to each other or to a
family would reject legal programs. The same-family rule applies only where both families are
concretely known.

The result is always `logical`: scalar-like when both operands are scalar-like, and array-like
otherwise.

- `1L < 2L` is `logical`.
- `1L == 1.5` is `logical`.
- `"a" < "b"` is `logical`.
- `c(1L, 2L) > 1L` is `logical[]`.
- `c(TRUE, FALSE) > 0` is `logical[]`.
- `1L < "a"` is a type error.

### Unary `!`

Logical negation coerces its operand exactly as a [scalar condition](#conditions) does, and its
result is always logical:

- `!logical` is `logical`.
- `!integer` and `!double` are `logical`, because R treats zero as false and every other number as
  true.
- `!logical[]`, `!integer[]`, and `!double[]` are `logical[]`.
- A map-like operand gives `logical[]`, because negation does not preserve map-likeness.
- An `Any` or `Unknown` operand gives `Unknown`.
- An operand whose type is still undetermined is constrained to `logical`, and the result is
  `logical`.
- Any other operand is a type error.

### Range operator `:`

`from:to` builds a numeric sequence. Both operands must be a scalar-like `integer` or `double`; an
array-like or non-numeric operand is a type error. The result is `integer[]` when both operands are
`integer`, and `double[]` when either is `double`. A whole-number `double` literal such as `1` or `10`
counts as `integer` here, matching what R does at run time.

An unannotated operand, as in `1:n`, is required to be a scalar `integer` or `double`. Passing a
numeric vector through the enclosing function is therefore a type error at the call, because R's
warning about truncating the endpoint marks a bug. The result is `double[]`, because the endpoint may
turn out to be a `double`.

- `1L:10L` is `integer[]`.
- `1:10` is `integer[]`, even though the literals are `double`, because both are whole numbers.
- `1.5:3L` is `double[]`.
- `x:10L` is `double[]` when `x` is a `double`.

### Combine `c(...)`

`c(...)` builds an atomic vector from atomic arguments of any shape:

- With no arguments, `c()` is `NULL`, as in R.
- `NULL` arguments are dropped, as in R, so `c(x, NULL)` is `c(x)` and `c(NULL)` is `NULL`.
- A union-typed argument takes part member by member. Its `NULL` members are dropped first, because
  at run time the value is either `NULL`, which `c` drops, or one of the other members. Every
  remaining member must be an atomic vector type, and joins the coercion like a separate argument. An
  accumulator that starts out as `NULL` therefore combines cleanly: with `acc` of type
  `double[] | NULL`, `c(acc, 1.0)` is `double[]`.
- When any argument is a list, `c` concatenates into a list instead of an atomic vector, since
  `c(list_a, list_b)` is R's standard way to append to a list. The result is an array-like `list[T]`
  whose element type is the join of every argument's elements, with an atomic argument contributing
  its own type, so `c(list(1L), "a")` is `list[integer | character]`. The atomic coercion rules below
  apply only when no argument is a list.
- An argument whose element type is not known statically, such as `Any`, `Unknown`, or an unannotated
  parameter (as in `function(x) c(x, 1L)`), is tolerated rather than rejected. The combined element
  type is then indeterminate, so the whole result is `Unknown`, and it is a strict-mode origin when
  the argument is an unresolved variable. This keeps `c` from reporting a false "expected `integer`,
  found `T`" in a generic wrapper, and from cascading on a value that is already `Unknown`. Claiming a
  concrete element type would be unsound, because a later argument could widen it.
- Mixed atomic arguments coerce to the widest type in R's order,
  `logical < integer < double < complex < character`. `raw` is not part of the order and combines
  only with `raw`.
- When every argument is named, the result is a map-like `T[named]`; otherwise it is an array-like
  `T[]`.

- `c(1L, 2L)` is `integer[]`.
- `c(1L, 2.5)` is `double[]`.
- `c(TRUE, 1L)` is `integer[]`.
- `c(1L, NA)` is `integer[]`.
- `c(1L, "a")` is `character[]`.
- `c(foo = 1L, bar = 2L)` is `integer[named]`.
- `c(list(1L), list(2L))` is `list[integer]`.
- `function(x) c(x, 1L)` is `<T> fn(x: T) -> Unknown`, because the unannotated `x` leaves the element
  type indeterminate.

### Assignment operator `<-`

`name <- expr` writes the type of `expr` into the variable slot for `name` in the current scope,
creating the slot on the first write (see [value names](#value-names) for the slot model). The
assignment expression itself has the type of `expr`, so `y <- (x <- 1L)` gives both `x` and `y` the
type `integer`. When the assignment carries a typing annotation, `expr` is checked by the annotation
rules on this page.

- A string in place of the name binds that name: `"x" <- 1` is `x <- 1`, and so is `` `x` <- 1 ``.
  All three forms create the same slot, and a later `x` resolves to it. For a string target, the
  range of any finding is the literal, quotes included, because that is what was written.
- A target that is not a name, such as a computed value or a number, is reported as a
  `syntax-error`. R parses these and refuses them at run time; see
  [diagnostic codes](/reference/diagnostic-codes#syntax) for the exact shapes and the one exemption.
- A later assignment in the same scope writes the same variable. On a straight-line path the new
  write replaces the old type, and writes that arrive from different control-flow paths are joined
  (see [control-flow joins](#control-flow-joins)).

So after `x <- 1L`, `x` is `integer`; after `x <- 1L; x <- "foo"`, a later `x` is `character`; and
after `x <- 1L; if (flag) x <- "foo"`, a later `x` is `integer | character`.

#### Recursion

A function can call itself, because its own name is visible inside its body: the target is bound to a
fresh type variable before the body is inferred, and that variable is then unified with the inferred
function type. `fact <- function(k) if (k <= 1L) 1L else k * fact(k - 1L)` is therefore
`fn(integer) -> integer`, and a call that violates the recursively inferred signature is an error.
The recursive uses share one instantiation, so there is no polymorphic recursion.

Two local functions that call each other are beyond this, because names are bound one at a time. The
forward reference still resolves, since a closure sees later writes to its frame, but it stays
tolerant `Unknown` rather than precisely typed.

At a package's top level, a self-recursive definition and a group of mutually recursive definitions
both resolve through the interface fixpoint. Every member starts as `Unknown` and is re-derived each
round until the schemes stop changing. Simple recursion converges to its precise type: a top-level
`fact <- function(n) if (n <= 1L) 1L else n * fact(n - 1L)` exports `fn(n: integer) -> integer`, and
the pair `is_even` and `is_odd` exports `<T: numeric> fn(n: T) -> logical`.

A self-reference whose type would grow without bound cannot converge in a type system without
recursive types. A tree fold is one: its parameter would need the recursive type
`T = double | list[T]`. Such a group settles at `Unknown`. A cycle can also converge with an `Unknown`
inside it, and a pure self-call such as `f <- function() f()` settles at `fn() -> Unknown`. Either
way the `Unknown` is the usual tolerance: an unannotated consumer flows through it, and strict mode
reports it (see [what strict mode flags](#what-strict-mode-flags)). An explicit annotation on the
binding closes the cycle exactly.

### Boolean operators `&&` and `||`

Both operands are [scalar conditions](#conditions), so each must be a `logical` or a number that
coerces, and the result is a scalar `logical`. An array-like or map-like logical vector is not
accepted.

- `TRUE && FALSE` is `logical`.
- `flag || other_flag` is `logical`.
- `c(TRUE, FALSE) && TRUE` is a type error.
- `TRUE || c(FALSE, TRUE)` is a type error.

## Calls

### Function calls

A call evaluates to its callee's return type. When the callee expression or its return type is
`Unknown`, the call is `Unknown`. When the callee is a union whose members are all function types,
the call must be valid against every member, since the value could be any of them, and it evaluates
to the union of the members' return types. Each member is checked in an isolated probe, so no
member's argument bindings leak into another's. Calling a function looked up in a list, as in
`handlers[[name]](...)`, has this shape.

A call is a type error when a required argument is missing, when it passes too many arguments to a
callee without a rest parameter, or when an argument is incompatible with its parameter's type. The
[named, positional, and optional parameter rules](#named-and-positional-parameters) apply throughout.

#### Matching arguments to parameters

Arguments are matched in R's two passes, and the order is observable:

1. Every argument given by name claims the parameter of that name, before any positional argument is
   placed.
2. The positional arguments then fill what is left: first the fixed positional parameters, then the
   unclaimed named parameters declared before the rest parameter, in declaration order. When the
   function is not variadic, they fill all the unclaimed named parameters.
3. The rest parameter absorbs whatever remains, positional or named.

So in `vapply(xs, character(1), FUN = f)`, the named `FUN` is claimed first, and `character(1)`
reaches `FUN.VALUE`, exactly as R matches it. A positional argument never collides with a parameter
that a later named argument has already claimed.

A rest parameter follows R's rules for the formals around the dots:

- It adds no required arguments, so a variadic function may be called with none; `paste()` is legal.
- Positional arguments fill the parameters declared before it first, so `wrap("a", "b")` on
  `fn(x: character, ...: character)` gives `x = "a"` and sends `"b"` to the rest.
- It then absorbs any number of remaining positional arguments, each checked against its element
  type.
- A parameter declared after it can only be matched by name, so `sum(1, 2, na.rm = TRUE)` on
  `fn(...: integer[] | logical[], [na.rm]: logical)` sends `1` and `2` to the rest and `na.rm` by
  name.
- It also absorbs a named argument that matches no declared parameter, which is how a wrapper passes
  an option through, as in `read.csv(file, colClasses = "character")`.
- A named argument that duplicates a parameter already given is still an error, because R rejects a
  formal matched twice.

When an argument is the enclosing function's bare `...`, it forwards an unknown number of arguments,
possibly none, as in a wrapper `function(x, ...) helper(x, ...)`. Such a call skips both arity
checks, because neither "missing required argument" nor "too many arguments" can be decided
statically, and the `...` argument itself matches no parameter. The call's other arguments are still
checked against their parameters as usual.

Whether a parameter is optional comes from the formals, not from the annotation. A formal with a
default is optional in R, and no annotation can change that, so the exported signature takes each
parameter's optionality from the function, and an annotation that disagrees is reported once, at the
definition. It is never reported as a missing argument at the call sites, because those calls are
correct. Both directions are reported: a required declaration over a formal with a default, and an
`[optional]` declaration over a formal without one.

#### Argument compatibility

Arguments are checked for compatibility, not exact equality:

- The ordinary coercions apply at parameter positions, including a scalar-like `T` into an
  array-like `T[]`, and `T` or `NULL` into `T | NULL`.
- The promotion order from [vector coercions](#vector-coercions) applies, so `mean(1L)`,
  `sd(c(1L, 2L))`, and `sum(x > threshold)` are not errors. (Unification itself never widens.)
- A whole-number `double` literal counts as `integer` at a parameter, so `seq_len(10)` and
  `substr(x, 1, 3)` are as valid as `seq_len(10L)` and `substr(x, 1L, 3L)`. This generalizes the rule
  that the `:` operator applies to its endpoints. A fractional literal such as `2.5` is still
  rejected at an `integer` parameter, and so is a `double` variable that happens to hold a whole
  number.
- An argument whose type is `Unknown` is accepted at any parameter. Whatever made the value `Unknown`
  was already diagnosed where it happened, and repeating it at every later use would add nothing.

#### The native pipe

R's parser rewrites `x |> f(y)` into `f(x, y)` before anything runs, so the pipe types exactly like
that call. The piped value becomes the first positional argument, and every call rule applies to it:
arity, argument compatibility, and overload selection. A chain composes from left to right, and a
type error in the piped value is reported on the left-hand expression.

The `_` placeholder follows R's rule: it is legal only as the entire value of exactly one named
argument, which then receives the piped value instead of the first positional slot. So
`x |> lm(y ~ z, data = _)` is `lm(y ~ z, data = x)`, and `2 |> f(tag = _)` supplies only `tag`, which
leaves any other required parameter missing.

A pipe that R itself would reject is not guessed at. That covers a right-hand side that is not a
call, a positional or repeated `_`, and a `_` nested inside a subexpression. Such a pipe stays an
opaque operator: it types as a silent `Unknown`, and its reads stay quiet.

### Overload sets

A standard-library stub may declare several signatures for one name, forming an ordered overload
set ([authoring stubs](/contributing/authoring-stubs#overloads-and-generics) describes how). A call
to such a name is resolved separately at each call site:

- Candidates are tried in declaration order, and the call commits to the first one whose parameters
  accept the arguments. That candidate's return type is the call's type, so `sum(1L, 2L)` is `integer`
  and `sum(1.5, 2.5)` is `double`.
- Each failed candidate is probed in isolation, so nothing it bound leaks into the next candidate or
  into the committed result.
- A candidate that leaves the caller's undetermined types as they were beats one that constrains
  them, whatever the declaration order. A wrapper like `function(x) sum(x)` therefore keeps its
  parameter open, because the `Any` fallback accepts it without constraining anything. When only one
  candidate fits, it is selected and whatever it determines stands, so `f(function(v) v, 1L)` selects
  the candidate whose second parameter is `integer`.
- The [whole-number literal rule](#argument-compatibility) does not influence which candidate is
  selected. Candidates are first tried against the arguments' true types, so `sum(1, 2)` selects the
  `double` candidate, matching what R computes. Only if no candidate accepts them is the set tried
  again with literals allowed as integers, so a name whose only fitting candidate wants `integer`
  still accepts `foo(1)`.
- When no candidate accepts the arguments, the call is a type error. Normally the error names the
  overloaded callee and how many signatures were tried, and gives the first candidate's failure as a
  concrete hint, because when the candidates disagree about what is wrong, no single complaint is the
  answer. In two cases one candidate's own finding is reported instead, at its own argument range:
  when every candidate rejects the call for the same reason, and when one candidate got strictly
  further into the argument list than all the others, which makes it the signature the call meant.
- Passing an overloaded name as a value, or hovering over it, sees the *last* declaration. By
  convention the last declaration is the most general, so a value never carries a narrower contract
  than the calls it might make. Go-to-definition jumps to the *first* declaration, where the set
  begins.

Only a declaration file can overload a name. A name with several signatures has no single most
general type, so a call must be resolved by search rather than inferred, which gives up the
principal-type guarantee. A `#:` annotation on your own function declares exactly one signature; to
make one name accept several shapes, give the parameter a [union type](#union-types), or split the
shapes into separate functions.

A local or package binding that shadows a stub name switches its overload set off, and the binding is
used everywhere, calls included. A project [override stub](/type-checking/stubs#overriding-a-shipped-declaration)
may declare overload sets, because a `.Rtypes` file is a declaration file for foreign code wherever
it lives.

## Control flow

### `if` expressions

#### `if` without `else`

An `if` without `else` takes a [scalar condition](#conditions). If the branch has type `T`, the whole
expression is `T | NULL`, because the missing branch contributes `NULL`. The usual union
normalization applies, so a `NULL` body stays `NULL`, a body that is already nullable stays a single
`T | NULL`, and an `Unknown` body stays `Unknown`.

- `if (flag) 1L` is `integer | NULL`.
- `if (flag) { }` is `NULL`.

#### `if ... else`

An `if ... else` takes a [scalar condition](#conditions) and joins the types of its two branches into
the result:

- Branches that unify share that type. `if (flag) 1L else 2L` is `integer`, and `if (cond) a else b`
  over two unconstrained values keeps them unified as one polymorphic type.
- A `NULL` branch joins by union without constraining the other branch, so one branch of type `T` and
  one of `NULL` give `T | NULL`.
- Branches with genuinely different types give their union: `if (flag) 1L else "foo"` is
  `integer | character`. Different branch types are not a type error.
- A branch whose type is still undetermined is never pinned by the other branch.
  `function(flag, x) if (flag) x else "s"` is `<T> fn(flag: logical, x: T) -> T | character`, not
  `fn(flag: logical, x: character)`. Unifying there would make the caller wrong because of a line
  that is not wrong. Guards need the same thing: `if (is.character(x)) x else "other"` exists
  precisely for callers that may pass something else.
- A branch whose variable the body has already constrained may unify with the other branch, because
  that adds nothing the program did not already require. That is how
  `function(n) if (n <= 1L) 1L else n * fact(n - 1L)` converges to `fn(n: integer) -> integer`.
- Two branches that are both still open are tied to each other, because neither pins the other.
  `function(value, fallback) if (is.null(value)) fallback else value` is
  `<T> fn(value: T | NULL, fallback: T) -> T`.
- An `Unknown` branch makes the whole conditional `Unknown`, rather than claiming the other branch's
  type.

Beyond unification, the join does not merge or coerce branch types; it only records the alternatives.

- `if (flag) 1L else 2L` is `integer`.
- `if (flag) 1L else NULL` is `integer | NULL`.
- `if (flag) NULL else 2L` is `integer | NULL`.
- `if (flag) 1L else "foo"` is `integer | character`.
- `if (flag) { } else { }` is `NULL`.
- `if (c(TRUE, FALSE)) 1L else 2L` is invalid, because a condition must be a scalar, not a vector.

#### Diverging branches

A branch *diverges* when it never falls through to the code after the `if`: `return(...)`,
`stop(...)`, `break`, or `next`, a block ending in one of those, or an `if ... else` whose branches
both diverge. A diverging branch contributes neither its value nor the state of its variables:

- `x <- if (c) return(NULL) else 5` gives `x` the type `double`, not `NULL | double`.
- A variable written inside a diverging branch does not join into the state after the `if`; only the
  surviving branch's state flows on.

A call that never returns has no bottom type of its own. `function() stop("no")` is
`fn() -> Unknown`, and divergence is carried by the control-flow rule above, not by the call's type.
`stop(...)` is recognized by its bare name, as `local` and `return` are, so rebinding `stop` is not
modelled.

### Guard narrowing

A condition that applies a type-guard predicate to a plain local variable refines that variable's
type along each edge of the `if`. Inside each branch, the variable keeps the refined type until a
write in the branch replaces it, and the refinements merge back at the join exactly as branch writes
do. The recognized guards are these, where `x` is a local variable (a parameter counts):

| Condition | True edge | False edge |
|---|---|---|
| `is.null(x)` | `x : NULL` | the `NULL` member is removed from the union of `x` |
| `is.character(x)` | members outside the `character` family are removed | `character`-family members are removed |
| `is.logical(x)`, `is.integer(x)`, `is.double(x)`, `is.function(x)`, `is.list(x)` | as above, for that family | as above |
| `is.numeric(x)` | as above, where the family is `integer` or `double` | as above |
| `!cond` | the two edges swap | |

Combined with a [diverging branch](#diverging-branches), the surviving edge's refinement persists
after the `if`, which is the idiomatic early-exit guard:

```r
#: fn(x: integer | NULL) -> integer
f <- function(x) {
  if (is.null(x)) {
    return(0L)
  }
  x + 1L   # x : integer here
}
```

The details:

- A family test covers both the scalar and the vector of that atomic type, so `is.character` is true
  for `character` and `character[]`. `is.list` covers every list shape (`list[T]`, `list[named: T]`,
  and the fixed shapes), and `is.function` covers function types.
- Narrowing filters union members. A member whose family cannot be decided statically, such as an
  unannotated value or an opaque nominal, is kept on both edges.
- `is.null(x)` on an `Any` or `Unknown` variable refines the true edge to `NULL`, because the runtime
  guarantees it. A family guard does not refine `Any` or `Unknown`, because inventing a concrete shape
  there would cause reports on standard-library functions that declare a scalar result for a value of
  any length.
- `is.null(x)` on a value that has no constraints yet, such as an unannotated parameter nothing has
  used, *shapes* it. The test asserts that `NULL` is a possible value, so the variable becomes
  `T | NULL` for a fresh `T`, and the edges narrow as for an ordinary union: the true edge keeps
  `NULL` and the undecidable `T`, and the false edge is `T`. That is how a function that returns a
  fallback for a `NULL` argument types without any annotation:
  `function(value, fallback) if (is.null(value)) fallback else value` generalizes to
  `<T> fn(value: T | NULL, fallback: T) -> T`, exactly what its annotated form would declare. Two
  consequences follow. Testing a parameter for `NULL` and then using it unguarded is a genuine
  finding, since the test itself declared `NULL` possible. And the shaping never fires on a variable
  that already carries a constraint (a numeric variable cannot hold `NULL`) or on a declared rigid
  type parameter, because an annotation's contract is not reshaped.
- When a guard cannot fire, such as `is.null(x)` on a union with no `NULL` member, nothing is
  refined. Dead branches get no special typing.
- Only a read of a variable narrows. That covers parameters, function locals, and a top-level
  variable assigned by an earlier statement, but not arbitrary expressions: `is.null(f(x))` and
  `is.null(x$field)` do not narrow.
- A refinement does not outlive the statement it was made in. Checking runs one top-level statement
  at a time, so a guard narrows within its own `if`, meaning the whole `if` and `else` and everything
  nested in them, and the next top-level statement sees the variable's unrefined type again. This is
  what separates the two ways to write an early exit. Inside a function body, or inside one braced
  top-level block, `if (is.null(x)) stop(...)` narrows `x` for the rest of the body, because the
  guard and the later reads are one statement. Written as bare top-level statements, the guard and
  the read after it are two statements, and the read is not narrowed.
- A read from inside a closure narrows when the guard is in the same statement, just as a local
  does. The guard's own subject must not itself be a deferred read: that body runs later, so the test
  proves nothing about the value it will see then.
- A condition combined with `&&` or `||` does not narrow.
- `is.na(x)` is not a type guard, because being `NA` is a property of a value, not of a type.

#### `missing()` on a defaultless formal

A formal that the body tests with `missing(name)` is optional at call sites, which is how R writes an
optional argument without a default: `function(name, punct) if (missing(punct)) … else …punct…` may
be called without `punct`.

The test also narrows whether the formal was supplied, along the branch edges, like a type guard:

- On the true edge, reading `name` is an error, because R fails that read with "argument is missing,
  with no default". Writing it is legal and supplies it, as in `if (missing(punct)) punct <- "!"`.
- On the false edge the formal is supplied, and reads are ordinary.
- A diverging true edge, such as `if (missing(x)) stop(...)`, leaves the rest of the body on the
  supplied edge, and `!missing(name)` swaps the edges.
- After the branches rejoin, the formal counts as unsupplied only when it is unsupplied on both
  edges, so only definite failures are reported.
- A formal with a default is never narrowed, because reading it while unsupplied evaluates the
  default.
- As in R, `missing()` applies only to the immediate function's own formals, so an enclosing
  function's formal is not narrowed inside a nested function.

### Blocks

A block evaluates to the type of its last expression. An empty block, and a block whose last
expression ends with `;`, evaluate to `NULL`, and a block whose last expression is `Unknown` is
`Unknown`.

### `return`

`return(x)` exits the enclosing function with `x`, and `return()` exits with `NULL`. It is a
control-flow construct rather than a call: the syntactic call to the bare name `return` is recognized
during lowering, as `local` is.

- A function's return type is the union of every `return` value in its body with the body's trailing
  value, so `function() { if (c) return("foo"); 5 }` is `fn() -> character | double`.
- The `return` expression itself yields no value where it stands, so it types as `NULL` locally and is
  not a strict-mode origin, just like `break` and `next`.
- The returned expression is checked like any other expression, and its errors surface as usual.
- A `return` inside a loop exits the whole function, so for control flow it abandons the loop
  iteration the way `break` does.
- A `return` at the top level is an R runtime error. Its value is still checked, and joins no
  function's return type.

### `switch`

`switch(subject, a = ..., b = ..., default)` selects one branch by the subject's value at run time.
That selection cannot be modelled statically, but the call is still checked in full:

- The subject and every branch are type-checked, and an error inside any branch surfaces as it would
  anywhere else.
- The call's type is the union of the branch value types. `NULL` joins the union unless there is a
  default branch, because an unmatched `switch` returns an invisible `NULL`. A default branch is any
  branch without a name, including the first.
- A named branch with no value falls through to the next branch, as in R, and contributes no type of
  its own.
- The branches are alternatives, not a sequence. Exactly one runs, so assignments inside them fork and
  join exactly like the arms of an [`if`](#if-expressions): a name written in several branches holds
  the join of what they write, and no branch's write shadows another's. A branch that cannot fall
  through, such as one ending in `stop()`, contributes no state to what follows. A name introduced
  only inside the branches is not defined on the path where nothing matches, which is what R reports
  as `object 'r' not found`.
- `switch` is recognized syntactically by its bare name, as the quoting and masking families are, so a
  local binding named `switch` makes the call an ordinary one again.

### `for`

`for (name in value) body` needs an iterable source, and binds `name` to its element type:

- A vector of any shape iterates with its scalar element type.
- An array-like `list[T]` and a map-like `list[named: T]` iterate with `T`.
- A tuple-like or record-like list iterates with the union of its item types, which collapses to a
  single type for a homogeneous list. A heterogeneous fixed-shape list is therefore iterable:
  `for (item in list(a = 1L, b = "two")) ...` binds `item` as `integer | character`.
- The empty list `list()` iterates with `NULL`, the union of zero item types.
- A union of iterables iterates member by member, so `integer[] | character[]` binds
  `integer | character`.
- `NULL` is iterable and runs zero times, which is legal R. It binds the variable as `NULL`.
- `Any` iterates with `Any` items, and `Unknown` with `Unknown` items, so a source that already failed
  does not cause a second error on the loop.
- An opaque nominal iterates with `Any` items, because its element shape is not visible.
- Iterating does not constrain a value whose shape is undetermined, such as an unannotated parameter.
  R iterates both vectors and lists, and neither may be committed on the caller's behalf, so the loop
  variable becomes `Unknown`.
- Any other source, such as a function, is an error, reported on the source expression.

The source is evaluated once, before the first iteration, and the loop does not change its type
outside the loop. Inside the body, the loop variable has the element type, and is re-initialized from
the source on every iteration, so an assignment to it in the body does not carry over into the next
iteration. The loop variable is not visible after the loop.

### `while`

A `while` loop takes a [scalar condition](#conditions), which is re-evaluated before every iteration,
so reads in the condition see the loop's joined state. The loop as a whole evaluates to `NULL`.

### `repeat`

A `repeat` loop has no condition and runs its body at least once, so a variable written in the body is
definitely assigned after the loop. It evaluates to `NULL`.

## Naming and scoping

### Project file order

Project files are ordered the way R collates a package: by the `Collate` field of `DESCRIPTION` if it
has one, and otherwise by the default `C`-locale collation of the source file names. Wherever this
page talks about an earlier or later file, it means earlier or later in this order.

### Value names

A top-level value name in a package is global across its files:

- Any file may refer to a top-level binding in another.
- When several files define the same name, the later file wins, and both definitions are reported.
- A bare top-level `{ }` block always runs, so the assignments directly inside it are package globals
  too.
- An assignment inside an `if`, `for`, or `while` body runs only conditionally, so it is not a
  package global, and a reference to it from another file is unresolved.

A reference from another file sees the binding's generalized exported type. Type information does not
flow back into the exporting file, so a call in one file never changes the inferred type of a
function defined in another. Within one file, a top-level name also resolves to its final exported
type, so a use placed before the definition still sees it.

Inside executable code, naming is lexical, over *mutable variable slots*, matching R's environments:
a scope holds one variable per name, and assignment mutates it.

- A function body, a `local(expr)` call, and a script's top level each form one scope, called a
  *frame*.
- A parameter introduces a slot in the function's frame, and assigning to the parameter's name writes
  that slot.
- The first `<-` or `=` assignment to a name in a frame creates its slot. Every later assignment
  writes the same slot rather than creating a new, shadowing binding.
- An assignment inside a branch or a loop body writes the enclosing frame's slot, because braces and
  control flow do not create scopes.
- A slot shadows an outer or package-global binding of the same name. But a slot that no write reaches
  at a given read does not shadow, and the read resolves outward, just as R's lookup would at run
  time.
- `for` introduces a loop-local slot, re-initialized from the source on every iteration.
- `local(expr)` evaluates `expr` in a fresh child scope and takes its type. Assignments inside are
  local, while references still see the enclosing names.

Two families of calls evaluate an argument non-standardly. Each is recognized by its bare name, and
rebinding the name to a user function does not change that:

- `library(pkg)`, `require(pkg)`, and `help(topic)` read a bare first argument as a package or topic
  name, so `library(stats)` means `library("stats")`. The name is never resolved as a variable and
  never warned about. A string argument, a named first argument, or a qualified callee makes it an
  ordinary call.
- `quote(expr)`, `substitute(expr)`, `bquote(expr)`, and `expression(expr)` build an expression
  instead of running it, so an assignment written inside one binds nothing: `quote(x <- 1)` leaves `x`
  undefined. Nothing inside is checked for arity or argument types, and a name it mentions need not
  exist. Those names still count as reads, though, because `eval` may run the expression later.

At a package document's top level, a conditionally executed assignment is not visible to the rest of
the package, but within its own document it behaves like a slot. A later top-level read resolves to
it, and reports the maybe-undefined warning when a path without the assignment also reaches it. A
read from another item sees the join of every conditional writer's type, so
`for (i in 1:3) total <- i` followed by `report <- function() total` types `report` as
`fn() -> integer`. A name with many conditional writers has no useful joined type, so past eight
writers the slot is `Unknown`.

### Name references

A name reference evaluates to the type currently bound to the name. When the name is not bound, the
checker reports it as unresolved, and treats the reference as `Unknown` from then on, so checking
continues without a cascade of follow-on errors.

### Namespace access

`pkg::name` and `pkg:::name` read one name directly from a package's namespace, bypassing lexical
scoping.

- A namespace is *known* when stubs declare it. The shipped standard-library packages are known, and
  so is any namespace a project stub file declares: `stubs/dplyr.Rtypes` declares the namespace
  `dplyr` (see [stubs](/type-checking/stubs)).
- The project's own package is always known, whatever the stubs say. Qualifying a name with the
  package you are editing, as `withr::defer()` does inside the package `withr`, reads the definition
  the checker already holds, so the read has that definition's type rather than `Unknown`. This wins
  over a stub namespace of the same name, just as a package binding shadows a stub name. The name
  itself is not validated, because a package exports names its sources never bind: re-exports, S4
  generics from `setGeneric`, datasets under `data/`, and bindings installed by `.onLoad`. A name the
  definitions do not cover is therefore left alone rather than reported.
- When the stubs declare `name` in `pkg`, the qualified read has the stub's type, exactly like the
  bare name. A name that only the namespace's [export manifest](#standard-library-exports) lists
  validates the same way, and types as `Unknown`.
- An unknown namespace warns, and so does a known namespace that neither declares nor lists the name
  ("not exported"). The same mistake in a `NAMESPACE` `importFrom` is an error rather than a warning,
  because a bad import stops the package from loading at all, while a bad qualified read fails only
  if that line runs.
- Exports are tracked per declaration. A project stub that overrides a shipped name's type does not
  remove the name from its shipped namespace, so `stats::sd` stays valid under an `sd` override.
- A qualified read that could not be validated types as `Unknown`, and is a strict-mode origin.
- `::` and `:::` are not distinguished: the split between exported and internal names is not
  modelled.

### Package imports (`NAMESPACE` and `DESCRIPTION`)

The hosts read the package's `NAMESPACE` and `DESCRIPTION` files at the package root, and what those
files say extends name resolution across the whole package. Without them (a single file, or a project
with neither file), analysis behaves as though both were empty.

- `importFrom(pkg, name)` makes `name` a known bare read that is never reported as unresolved. The
  read has the stub corpus's type for the name when there is one, and `Unknown` otherwise. A typo in
  the import is caught once, at the import: an `importFrom` naming something a stub-described
  namespace does not export is an error there, because R refuses to load such a package, and it never
  shows up at the use sites.
- `import(pkg)` of a namespace the stubs describe makes exactly `pkg`'s exports known bare reads.
  When no stubs describe `pkg`, its export set cannot be known, so every otherwise-unresolved bare
  read in the package is tolerated instead of guessed at, and an unknown package never causes a false
  report. Unresolved-name detection resumes once stubs for `pkg` exist.
- A `library(pkg)` or `require(pkg)` call anywhere in the project is the script-world equivalent of
  `import(pkg)`, and follows the same rule. It attaches every export of `pkg` to the search path, so
  when nothing describes `pkg`, every otherwise-unresolved bare read is tolerated, and because R's
  search path is project-wide, so is the tolerance. It lifts as soon as the package's exports are
  known, which they now are for a long list of common packages. An
  [export manifest](/contributing/authoring-stubs#export-manifests) is enough; types are not
  required. The tidyverse, `knitr`, `rlang`, `glue`, `jsonlite`, `R6`, and the rest of the shipped
  manifests therefore keep unresolved-name detection on. A package nothing describes still switches
  it off, and a two-line `stubs/<pkg>.Rtypes` of your own switches it back on.
- Attaching a meta-package activates the packages it attaches, not the names it exports.
  `library(tidyverse)` makes `mutate`, `read_csv`, and `str_to_upper` reachable because it attaches
  `dplyr`, `readr`, and `stringr`, and those namespaces activate with it, types included where they
  have them.
- Attaching or importing the project's *own* package, the one named in the `Package` field of
  `DESCRIPTION`, does not switch the tolerance on, even though no stubs describe it. Its export set is
  not unknowable: those exports are the project's own definitions, which the checker already sees.
  Without this rule, the `library(yourpkg)` that `usethis` writes into `tests/testthat.R` would turn
  off unresolved-name detection for the whole package.
- A `pkg::name` read of a namespace the stubs do not know warns about an unknown namespace, unless
  `pkg` belongs to the package's *declared universe*: a `DESCRIPTION` dependency field (`Depends`,
  `Imports`, `Suggests`, or `Enhances`), or the source namespace of any `NAMESPACE` import. A
  namespace that is declared but not described stays quiet, and its reads type as `Unknown`.

### Standard-library exports

The shipped stubs pair each namespace with a vendored *export manifest*: the complete list of names
the namespace really exports, generated from a live R session, as
[authoring stubs](/contributing/authoring-stubs#export-manifests) describes. Every manifest name is a
known global:

- A bare read of a manifest name always resolves and never warns as unresolved. It has the stub's type
  when one exists, and `Unknown` otherwise.
- A qualified `pkg::name` read of a manifest name validates the same way, with no not-exported
  warning, and has the same type.
- Typo suggestions for names that really are unresolved draw on manifest names as well as on typed
  declarations.

Manifests follow the way R itself exposes each namespace:

- The default-attached packages, `base`, `stats`, `utils`, `graphics`, `grDevices`, `methods`, and
  `datasets`, are visible without qualification in every session, so their names resolve bare and
  qualified, unconditionally.
- The packages R ships but does not attach, `tools`, `parallel`, `compiler`, `grid`, `splines`,
  `stats4`, and `tcltk`, are reachable through `::` in every session, so their manifests always
  validate a qualified read. A bare read resolves only once the project attaches the package with a
  `library()`-family call or declares it as a dependency, exactly as in R.
- A conditional namespace's manifest activates together with its stubs (see
  [conditional stub namespaces](#conditional-stub-namespaces-datatable-dplyr-ggplot2-and-testthat)).
  While the namespace is inactive, its names stay unknown, bare and qualified alike.
- A read satisfied only by an import still counts as a use for the unused check, and strict mode
  reports its `Unknown` exactly like any other undetermined reference.

### Replacement-form assignment

A replacement-form assignment (`x$field <- v`, `x[["name"]] <- v`, `x[[key]] <- v`, or
`x@slot <- v`) reads the base variable, applies the write, and writes the result back to the base's
slot, so the slot's type reflects the update:

- A write to a known field, `x$field <- v` or `x[["literal"]] <- v`, on a record-like `x` sets that
  field's type to the type of `v`, adding the field if it is absent, and a later `x$field` reads the
  new type. The same write on an empty `list()` starts a record `list{field: V}`.
- The same write on a *nominal* `x` is checked rather than applied. A nominal type's representation is
  fixed, which is what makes `@type` an invariant rather than a label, so the write must satisfy it:
  `v` is checked against the field's declared type, a field the representation does not declare is an
  error, and `x` keeps its nominal type either way. This is the one write that reports instead of
  retyping, because the alternative is a value that still claims a nominal type while no longer
  matching its representation.
- A write with a computed key, `x[[key]] <- v` where `key` is not a literal, cannot name a field
  statically, so it refines the container's element type instead:
  - an empty `list()` becomes a map-like `list[named: V]`;
  - a map-like `list[named: T]` becomes `list[named: T | V]`;
  - an array-like `list[T]` becomes `list[T | V]`, and stays array-like, because its reads are not
    nullable;
  - a record-like or tuple-like container is left unchanged. A dynamic write does not statically
    change a shape whose fields are individually known, and widening such a shape would throw away
    precision the code has not given up.
- The accessor chain and the index or key expressions are ordinary reads, so their own errors surface.
  A replacement whose accessor chain has no variable at its root, such as `f(x)$a <- v`, is refused as
  an unsupported construct: it types as `Unknown` and is a strict-mode origin.

Reading a name from a map-like list gives `T | NULL`, because the key may be absent (see
[`[[` on lists](#-on-lists)). Building a map with computed-key writes and then reading a key back
therefore gives `V | NULL`, so guard the read with `is.null` before a use that needs a `V`.

### Control-flow joins

A read of a variable sees every write that can reach it, so wherever control flow merges, the
variable's possible states are joined:

- After an `if` without `else`, a variable written in the branch joins its type from before the `if`
  with the type the branch wrote.
- After an `if ... else`, a variable joins the outcomes of the two branches, and a branch that does
  not write it contributes the state from before the `if`.
- A loop body may run zero or more times, so reads inside the body and after the loop join the state
  from before the loop with the state flowing around the back edge.
- `repeat` runs at least once, so after the loop the variable has the state the body left it in.
- Joining equal types keeps the type, joining different types gives their union, and joining with
  `Unknown` gives `Unknown`.

- `f <- function(flag) { x <- 1L; if (flag) { x <- 2L }; x }` is clean, and `x` reads as `integer`.
- `f <- function(flag) { x <- 1L; if (flag) x <- "two"; x + 1L }` is a type error, because `x` reads
  as `integer | character`, and `+` rejects the `character` member.

A variable with exactly one reaching write keeps that write's generalized type, so
`f <- function(x) x` inside a body stays `<T> fn(x: T) -> T`. When writes merge at a join, the
variable holds a single type instead, so a conditional reassignment loses the polymorphism: two
conditionally assigned functions read as a union of both signatures, rather than being unified into
one.

Definite assignment follows four rules:

- A read that some path reaches with no prior write reports [`maybe-undefined`](/reference/diagnostic-codes),
  because R raises `object 'x' not found` on that path. The finding is off by default; turn it on
  with `[check] maybe-undefined = true`. Two conditions that always agree at run time are still two
  branches to the checker, so `if (ok) v <- …` followed by `if (ok) use(v)` is reported, although it
  is safe.
- A `repeat` is left through its `break` points, so one that always assigns before breaking reports
  nothing. A branch that cannot fall through, such as one ending in `stop()`, contributes no path.
- A read that no write can reach does not resolve to the variable at all (see the shadowing rule
  under [value names](#value-names)).
- A top-level variable is different, because a path without a write reaches the enclosing
  environment. The read then sees the name's binding elsewhere in the script or package, and that type
  joins into the slot like any other reaching write. After `p <- "word"`, the body of
  `while (cond) p <- p - 1L` is a type error, because the first iteration reads a `character`. A name
  with no such binding stays `Unknown`.

An item whose check reports an error exports `Unknown`, so one mistake does not cascade across a file.
An item with a `#:` annotation is the exception: the annotation is what the author says the binding
is, so even when its body violates the annotation, every call site is still checked against the
declared signature.

#### Unused assignments

With the `unused` check enabled, an assignment whose value no read can observe on any path reports
`unused` on the assigned name. Top-level assignments visible to the package, parameters, `for`
variables, and names starting with `.` or `_` are never reported.

A read inside a nested function is a *capture*. The closure runs after its frame has finished, so
every write to the captured name stays observable, and none of them is a dead store. That holds only
for the frame the read resolves to: a same-named binding in an enclosing frame is shadowed, not read,
and still warns.

- `f <- function() { x <- 1L; x <- 2L; y <- x; y }` warns that the first write to `x` is unused.
- `f <- function() { x <- 1L; g <- function() x; x <- 2L; g }` is clean, because both writes stay
  alive through the capture.
- `f <- function() { x <- "outer"; g <- function() { x <- TRUE; function() x } }` warns, because the
  innermost function reads the `x` of `g`.

`on.exit(expr)` reads the same way: R runs the expression when the function returns, so it observes
the last value of every name it mentions. The standard rollback guard is therefore clean:

```r
with_transaction <- function(con, body) {
  committed <- FALSE
  on.exit(if (!committed) dbRollback(con))
  body(con)
  committed <- TRUE          # read by the exit handler, not a dead store
  invisible(TRUE)
}
```

#### Names bound at run time

In a file that calls `R6Class`, the names `self`, `private`, and `super` resolve. R6 creates those
bindings when an object is constructed, so they exist nowhere lexically, and reading one is not an
unresolved name. The recognition is syntactic, so a local binding that shadows `R6Class` is not
honored, and it is scoped to the file, so a file that defines no R6 class still warns about `self`.
These names type as `Unknown`, because R6 field and method types are not described.

A top-level `globalVariables(c("a", "b"))` or `utils::globalVariables(...)` call with literal string
arguments declares those names as bound dynamically for the whole package, which suppresses the
unresolved-name warning for them everywhere. Any undeclared name keeps warning.

### Type names

`@type` and `@alias` declarations share one project-global namespace:

- A type reference may resolve to a declaration in the same file or another, and forward references
  are allowed.
- A duplicate type name is an error whatever the declaration kind: `@type` twice, `@alias` twice, and
  one of each all conflict, and every declaration involved in the conflict is marked as erroneous.
- A duplicate is judged within the namespace the declaration lives in. Package files share the
  project-global namespace, so two package files that declare one name conflict. A script's
  declarations belong to that script alone: a name declared in one script is invisible to the next,
  so two scripts may each declare `Thing`, while declaring `Thing` twice within one script is a
  duplicate. Without this rule, the later declaration would silently win, and nothing in the visible
  source could explain the diagnostics it produced.
- Type parameters are local binders, and shadow project-global type names.
- A type reference that resolves to nothing is an error at the referencing token, with a hint when a
  close match exists. A reference can resolve to a built-in type, a binder in scope, a project `@type`
  or `@alias` declaration, or a class declared in a stub. An undeclared name then compares like
  `Unknown` everywhere, so the typo is reported exactly once and never cascades into mismatches
  between values.

All `@type` and `@alias` declarations are top-level and project-global: a type declared in one package
file can be named from every other, and there are no file-local types.

### Non-package documents

A file that is not a package source file, such as a script under `scripts/`, contributes neither to
the package's global value namespace nor to the project-global type namespace. A script runs top to
bottom, so its top level is one sequential lexical scope, like a function body:

- A top-level binding is visible only after its assignment, and rebinding a name changes the uses
  after it, just as local rebinding does.
- A use before any script-local or package-global definition is an unresolved name. That includes a
  read inside the very statement that first binds the name, such as `x <- x + 1L` with no earlier `x`,
  which fails at run time.
- A read from inside a nested function is deferred. The closure runs after the frame has settled, so
  it resolves against the whole document, and the last top-level binding of the name wins. That
  includes the enclosing statement's own binding, so self-recursion resolves, and a self-recursive
  closure types through the cycle fixpoint.
- A conditional top-level write, meaning one inside a top-level `if`, `for`, `while`, or `repeat`,
  creates the document's variable slot exactly as in package files, and later reads in the same
  document resolve to it. The slot exports no type yet, so such reads are `Unknown`.
- A masked read, from `with` or from data.table indexing, and a read inside an opaque operator, which
  is `&`, a user `%op%`, or a pipe R would reject, are never reported as unresolved. Each still counts
  as a use: it keeps the binding it would fall back to alive for the unused check, and navigation
  (goto and references) connects it. A well-formed `|>` is not opaque, because it types as the call
  it stands for.

Scripts are type-checked like package files, against the package-global value schemes and
project-global types, plus their own local bindings and type declarations:

- A script may resolve package-global value names and project-global `@type` and `@alias` names from
  package files.
- A script's top-level value bindings and type declarations are visible neither to package files nor
  to other scripts.
- A package file and a script may reuse the same top-level value or type name without conflict.
- Duplicate top-level value names inside a script do not produce the package-global duplicate-binding
  warning; they behave like ordinary rebinding. R scripts commonly rely on the global environment, so
  warning about rebinding there would only add noise.

## Data frames and non-standard evaluation

R evaluates some arguments inside a data frame's own environment, where a bare name is a column
reference that no lexical scope can see. These *masked* positions are recognized structurally, and a
read there that resolves to no binding is treated as a column reference: a silent `Unknown`, with no
unresolved-name warning and no strict-mode origin. The recognized masks are:

- A single `[` bracket whose subject types as the `data.table` nominal masks all of its index
  arguments, whatever they look like. Because the subject's class is known, `DT[speed > 20]` and
  `DT[, x]` are column references even without any syntactic marker.
- A `[` call carrying an unambiguous data.table signature masks all of that bracket's index arguments,
  even when the subject's type is unknown. The signatures are a `by =` or `keyby =` argument, a `:=`
  column assignment, a `.()` list call, and the specials `.SD`, `.N`, `.I`, `.BY`, `.GRP`, and
  `.EACHI`.
- The base masking family, `with()`, `within()`, `subset()`, and `transform()`, masks every argument
  except the data. Which argument is the data follows R's own matching: a named argument claims its
  formal first (`data` for the `with` pair, `x` for `subset` and `transform`), and the remaining
  positional arguments fill what is left. So `with(data = frame, speed > 20)` and
  `with(speed > 20, data = frame)` both mask the condition. The `base::` spelling of any of the four
  masks exactly like the bare one, while another package's export of the same name is a different
  function and masks nothing.

A name inside a mask that does resolve, such as a local variable used in `j` or a function like
`sum`, keeps its ordinary resolution and typing, because data.table itself falls back to the lexical
scope for names that are not columns. Base indexing such as `m[i, j]` carries no data.table marker,
so it keeps full lexical checking. A function written inside a masked argument is masked too, because
a closure created in `j` is created inside the data's frame.

#### data.table result classes

A bracket with a data.table signature but an unknown subject types as `Unknown`, because base
indexing rules cannot judge `[.data.table`. When the subject *is* the `data.table` nominal, the class
of the result follows from the bracket's syntax, even though the columns are unknown. In this table,
`j` is the second positional slot or a `j =` argument:

| Bracket shape | Result |
| --- | --- |
| no `j`, or an empty `j` slot, as in `DT[i]` and `DT[on = …]` | the subject's class, because a row filter and a join both return tables |
| `j` is a `:=` call, as in `DT[, x := …]` and `` DT[, `:=`(a = …) ] `` | the subject's class, returned invisibly |
| `j` is a `.()` or `list()` call, as in `DT[, .(m = mean(x))]` | the subject's class |
| any `j` with a `by =` or `keyby =` argument, as in `DT[, sum(x), by = g]` | the subject's class, because a grouped result is always assembled into a table |
| anything else, such as a bare column `DT[, x]`, an ungrouped computed `j`, or a `with =` form | `Unknown`, and a strict-mode origin, because the shape would need column knowledge |

The class is a real type. It flows through chains, so `DT[a > 1][, .(m = mean(b)), by = g]` stays a
`data.table` from end to end; it satisfies or violates annotations; and it constrains call
arguments. Column-level knowledge is out of scope: element types, membership checks, and the
evolution of columns through `:=` are not tracked.

#### Conditional stub namespaces: data.table, dplyr, ggplot2 and testthat

Four packages have shipped stubs that do not join name resolution by default:

- `data.table`, with the `data.table` nominal, `fread`, and the `set*()` family;
- `dplyr`, with the `@masked` verbs, the joins, the tidy-select helpers, and the rest of its
  vocabulary;
- `ggplot2`, with the `ggplot` and `gg` nominals, `+.ggplot`, the geoms and scales, and the `@masked`
  `aes`;
- `testthat`, with the expectations and `test_that`.

R does not attach these packages by default either, and their names must not suppress typo warnings
in projects that never use them. So a conditional namespace activates only when:

- the project declares the package, through a `DESCRIPTION` dependency field or a `NAMESPACE`
  `import` or `importFrom` that names it;
- any project file attaches it, with a `library()`, `require()`, `requireNamespace()`, or
  `loadNamespace()` call whose package argument is a literal name or string; or
- the project ships its own `stubs/<pkg>.Rtypes` override for the namespace.

While a namespace is inactive, it behaves exactly like any package the stubs do not describe.

The shipped dplyr verbs preserve their data argument's class, as `<T> fn(.data: T, ...) -> T`, so a
native-pipe chain keeps its class from end to end: `fread(path) |> mutate(r = a / b)` stays a
`data.table`, and every column reference inside those verbs' `...` stays masked.

A project stub can declare its own masking function with the `@masked` attribute. A dplyr-style verb
is declared like this:

```
filter : @masked fn(.data: Any, ...: Any) -> Any
mutate : @masked fn(.data: Any, ...: Any) -> Any
```

A call to a `@masked` name evaluates the arguments absorbed by its `...` rest parameter inside the
data's frame, where a bare name is a column reference. That applies to the bare name and to
`pkg::name` alike. An argument that matches a formal declared before the `...`, such as `.data`
above, resolves normally, by position or by name. A declaration whose only parameter is `...`, such
as `join_by : @masked fn(...: Any) -> Any`, masks every argument. In all of these cases, a locally
defined function of the same name masks nothing. `@masked` on a declaration that is not variadic is a
stub error.

## Object systems (S3, S4, R6)

The checker covers the parts of R's object systems that are written down as declarations, and stays
out of the parts decided at run time from a value's class attribute:

| Construct | What the checker does |
| --- | --- |
| An operator on a nominal (`+.Class`, `Arith.Class`, `Ops.Class`) | Dispatches statically; see [operator methods on a class](#operator-methods-on-a-class) |
| A directly called S3 method (`speak.dog(x)`) | Treats it as an ordinary call, checked against that function's own signature |
| `UseMethod("speak")`, and any call to an S3 generic | Gives `Any`, with no strict-mode finding |
| `structure(list(...), class = "dog")` | Keeps the argument's type, because a `class` attribute is data, not a type, so the record's fields stay checkable. A `dim` attribute is the exception: it makes the value an array, whose shape is not tracked, so such values stay `Unknown` |
| `setClass`, `setGeneric`, `setMethod`, `new` | Does not model them; `new(...)` is `Unknown` |
| `x@slot` read or write | Lowers it fully and types it as `Unknown`; see below |
| `R6Class(...)`, `$new(...)`, fields, methods | Does not model them; they are `Unknown` |
| `self`, `private`, `super` inside an R6 method | Resolves them as names, typed as `Unknown` |

`x@slot` reads an S4 slot and `x@slot <- v` writes one. The slot's type is unknown, but the construct
is still analyzed: a slot read types as `Unknown` and is a strict-mode origin; the subject expression
is inferred, so its own type errors surface; the subject's variable counts as a read for naming, the
unused check, references, and rename; and a slot write is an ordinary
[replacement-form assignment](#replacement-form-assignment) of its base variable.

### Declaring a checked type for your own classes

A class is a nominal type with a representation, which is
[something you can declare](#type-parameters-aliases-and-nominal-types). Wrapping the constructor is
enough to get slot types, constructor arity, and field access checked on an S4 or R6 class:

```r
#: @type Point {list{x: double, y: double}}

setClass("Point", representation(x = "numeric", y = "numeric"))

#: fn(x: double, y: double) -> Point
make_point <- function(x, y) {
  #: @new Point
  new("Point", x = x, y = y)
}

#: fn(p: Point) -> double
norm2 <- function(p) sqrt(p$x^2 + p$y^2)

norm2("nope")   # type-mismatch: the argument is not a `Point`
make_point(1)   # type-mismatch: a required argument is missing
```

The `setClass` call stays opaque, and the annotation is what the checker reads. Operators on the
class work the same way: declare `Arith.Point`, and `p1 + p2` is checked.

Method dispatch, on the other hand, needs the argument's class at the call site, and inside an
unannotated `function(x) speak(x)`, that class is not known there.

## Strict mode

Strict mode is an opt-in check, switched on with `[check] strict` and off by default. It changes
nothing about inference and adds no typing rules. The type checker already computes every type;
strict mode reads those types and reports the places where the checker genuinely could not determine
one, and it escalates unresolved references to errors.

It also reports each read that the attached-package tolerance silenced. Attaching a package whose
exports cannot be known makes every otherwise-unresolved name tolerated. That is right for an
ordinary run, where each of the package's exports would otherwise be a false `unresolved`, but it
switches a whole class of checking off across the project, which would make a clean run
indistinguishable from one that never happened. Such a read is genuinely undetermined, so strict mode
names it and points at the [package declaration](/type-checking/stubs) that would close the gap. The
classification is shared with the ordinary `unresolved` check, so these are exactly the reads that
check let through. A near miss of a name your own project binds was never tolerated, and stays an
`unresolved` finding.

### Unresolved references escalate to errors

An unresolved reference carries the `unresolved` code, and comes in three kinds:

- a bare name the resolver cannot find in the package, its imports, or the builtins;
- an unknown package namespace in `pkg::name`;
- a name that a known namespace does not export, read as `pkg::name`.

Outside strict mode these are warnings; under strict mode they are errors, whether strict mode comes
from the configuration or from the per-file directive, because a name the checker cannot resolve is
an unchecked part of the program. Turning strict mode on can therefore raise the severity of findings
that were already there without changing their count, which matters when a `--min-severity error`
gate reads them.

Two `unresolved` findings are errors in every mode, because they stop the package from loading rather
than describing a gap in the checker's view: a `NAMESPACE` `importFrom` naming something the namespace
does not export, and an `export()` naming something the package never defines.

### Per-file directive

A plain top-level comment sets one file's typing mode, overriding the configured `[check]` switches
in both directions:

```r
# typing: off      # no type or strict diagnostics for this file
# typing: on       # type checking on for this file, strict off
# typing: strict   # type checking and strict mode on for this file
```

- `off` silences the file's type errors and strict diagnostics, even when the configuration checks
  types. `on` opts a single file into type checking in an otherwise unchecked workspace. `strict`
  also switches on [strict mode](#strict-mode) for the file.
- The older `#: @strict` form is still supported: `#: @strict` means `# typing: strict`, and
  `#: @strict off` means `# typing: on`, which type-checks the file but not strictly.
- If a file has several directives, the last one wins. A `typing:` comment with any other value is
  reported as an error rather than silently ignored.
- The directive changes only which diagnostics are published for the file. Inference and every other
  check are untouched, so hover and the other editor features keep working under `off`.

### What strict mode flags

Strict mode reports an expression or binding whose type is `Unknown` at the point where it is
introduced, and nothing else. `Unknown` is the type for "could not determine", which is what strict
mode exists to surface. `Any` is an explicit, intentional opt-out, and strict mode always tolerates
it: a value typed `Any` never produces a strict diagnostic.

### Where an undetermined type is reported

An undetermined value is reported once, where it first became undetermined, not at every later
expression that carries it. There are three such origins: a construct the type system does not
describe, a reference to a binding whose type is not known, and a recursive definition the fixpoint
could not type.

A reference to a binding defined in this project is not an origin. The binding's own definition is
reported instead, so one undetermined value does not produce a finding in every file that reads it.
An unresolved name is not an origin either, because naming already reports it.

### Diagnostics

Strict diagnostics carry the code `strict`, so they can be filtered separately from type errors. Each
origin is reported once, at the exact range of the originating expression. A binding and a bare
expression are worded differently, because a binding can be annotated and a bare expression cannot.

## Syntax errors

A file with syntax errors is still analyzed, under one rule: a broken region reports its syntax error
and nothing else, because the checker draws no conclusions from source it could not read.

- Every well-formed statement in the file is analyzed normally. Definitions keep their exports,
  references resolve, and a genuine type error outside the broken region still surfaces.
- A broken statement contributes nothing: no names, no reads, and no diagnostics beyond the syntax
  error covering it.
- An unterminated argument or parameter list ends at the next statement, so the mistake stays on the
  line that made it. A list that runs onto the next line is ordinary R, and a fragment there such as
  `beta)` really is a forgotten separator and is reported as one. But a line that assigns is the next
  statement: adopting it would report a confident "missing separator" on that line and every line
  after it, and cost each adopted line its own definitions.
- A broken assignment whose name side is intact keeps its definition. The value becomes a hole that
  types as `Unknown`, so its dependents neither lose resolution nor see a wrong type while the value
  is being edited. The hole is not a strict-mode origin, because the syntax error already marks it.
- A checked annotation on such a broken definition binds its declared type without checking it, so
  the definition keeps its contract for callers until the value parses again, and is then checked
  against the annotation as usual.

## Unsupported constructs

A syntactically valid construct that the type system does not describe may infer as `Unknown`, which
lets checking carry on even where the checker cannot model the construct precisely. Whether such a
construct also produces a diagnostic is decided construct by construct.
