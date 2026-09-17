---
title: Type system
description: The precise static-typing semantics contract for ry's R type checker
---

This page is the authoritative specification of ry's typing semantics. It is the precise contract that the type checker implements. The [Type Checker guide](/type-checking/tutorial) is a gentler introduction that works through examples.

## Typing comment syntax

A typing annotation is written in a `#:` comment, directly above the binding or expression it describes. Consecutive `#:` lines with no blank line between them form one annotation block.

There are four annotation forms:

- `#: TYPE` is a checked annotation
- `#: @trust TYPE` is a trusted coercion
- `#: @if-unknown TYPE` is an unknown-only coercion
- `#: @new NOMINAL_TYPE` introduces a nominal value

A block holds exactly one of those lines, or one expanded function annotation written as `@param` and `@return` lines, or one or more `@type` and `@alias` lines. The three kinds cannot be mixed in one block.

A block attaches at any statement depth, not only at the top level. A block inside a function body annotates the assignment or the block-final expression that follows it. A function that builds a value of a named type uses this:

```r
#: @type Person {list{name: character}}

#: fn(name: character) -> Person
make_person <- function(name) {
  #: @new Person
  list(name = name)
}
```

Attachment requires adjacency. The annotated expression must start on the line directly after the block. A block that needs a target and has none is an error, and the annotation does not apply. There are four such cases.

- a blank line separates the block from the expression
- a plain `#` comment separates them, or no expression follows at all
- the block has no content beyond the `#:` marker
- the block sits inside a call's argument list, for example beside a lambda passed to `lapply`, because an argument is not a statement

For the last case, give the value its own binding and annotate that. A braceless function body, a braceless `if` branch, and a parenthesised expression do attach, and are not errors.

A block that contains only `@type` and `@alias` lines is a definition block. It does not attach to anything, and neither does a `@strict` toggle, so the adjacency rules do not apply to them.

A block is refused whole when it mixes forms, orders directives wrongly, declares a duplicate or unknown type parameter, or gives `@new` a payload that is not nominal. A refused block reports its error and carries no typing payload, so a broken annotation never produces follow-on findings. A block the annotation grammar could not read is refused silently, because the parse error has already reported what was wrong.

Examples:

```r
#: integer
value <- 1L
```

```r
#: fn(count: integer) -> integer
double_count <- function(count) count + count
```

## Types

### Atomic names

Types use R's own names:

- `logical`
- `integer`
- `double`
- `complex`
- `character`
- `raw`
- `NULL`

`bool`, `int`, `float`, and `string` are not accepted.

### Reserved constants

R's reserved constants infer their fixed scalar atomic type.

- `TRUE` and `FALSE` infer as `logical`
- `NA` infers as `logical`. `NA_integer_`, `NA_real_`, `NA_complex_`, and `NA_character_` infer as `integer`, `double`, `complex`, and `character`
- `Inf` and `NaN` infer as `double`
- an imaginary literal such as `1i` infers as `complex`
- `NULL` infers as `NULL`

### Vector shapes

An atomic vector type has three user-facing shapes:

- scalar-like
- array-like
- map-like

#### Scalar-like vectors

A bare atomic type name means a scalar-like value.

Examples:

- `character`
- `integer`
- `double`

#### Array-like vectors

Appending `[]` means an array-like vector.

Examples:

- `character[]`
- `integer[]`
- `double[]`

#### Map-like vectors

Appending `[named]` means a map-like vector keyed by names.

Examples:

- `character[named]`
- `integer[named]`
- `double[named]`

#### Vector coercions

- a scalar-like vector `T` coerces to an array-like vector `T[]`
- a map-like vector `T[named]` coerces to an array-like vector `T[]`
- an `integer` shape coerces to the corresponding `double` shape. This covers `integer` to `double`, `integer[]` to `double[]`, and `integer[named]` to `double[named]`. It also covers compositions such as scalar `integer` to `double[]`. The reverse never holds

- a reverse coercion is not allowed unless another rule states it explicitly

Whether a coercion changes the resulting type depends on the construct that uses it.

### List shapes

List types appear in four user-facing forms:

- tuple-like, rendered as `list{T1, T2, ...}`
- record-like, rendered as `list{name: T, ...}`
- array-like, rendered as `list[T]`
- map-like, rendered as `list[named: T]`

R uses `list(...)` for several different collection meanings, and the type system must distinguish them.

A tuple-like list and a record-like list are fixed-shape collections. Their positions or field names are part of the type. An array-like list and a map-like list are homogeneous collections. Every element has the same type, and the specific position or name is not part of the type.

| Shape | Fixed size | Homogeneous | Names or positions meaningful in the type |
| --- | --- | --- | --- |
| `list{T1, T2, ...}` | yes | no | positions |
| `list{name: T, ...}` | yes | no | names |
| `list[T]` | no | yes | no |
| `list[named: T]` | no | yes | no |

A `list(...)` expression may correspond to any of these meanings. It infers a fixed shape when the elements carry enough information.

- it infers tuple-like when all elements are unnamed
- it infers record-like when all elements are named
- mixing named and unnamed elements drops the names and infers an array-like list, so `list(1L, bar = "foo")` infers as `list[integer | character]`

A fixed shape wins wherever the elements allow one, so `list(1L, 2L, 3L)` infers as `list{integer, integer, integer}` rather than `list[integer]`, and `list(foo = 1L, bar = 2L)` infers as `list{foo: integer, bar: integer}` rather than `list[named: integer]`.

Annotations produce most array-like and map-like list types. Coercing a structural list shape also produces them.

#### List coercions

- a tuple-like list coerces to an array-like `list[T]` when each tuple element is compatible with `T`
- a record-like list coerces to an array-like `list[T]` when each field value is compatible with `T`
- a map-like list coerces to an array-like `list[T]` when each field value is compatible with `T`
- a record-like list coerces to a map-like `list[named: T]` when each field value is compatible with `T`
- a map-like list coerces to a map-like `list[named: T]` when each field value is compatible with `T`
- reverse coercions are not allowed:
  - an array-like `list[T]` value does not coerce back into a tuple-like, record-like, or map-like value
  - a map-like `list[named: T]` value does not coerce back into a fixed-shape record-like value

#### Tuple-like lists

A `list(...)` expression with only unnamed elements infers as tuple-like, even when all element types are the same.

Examples:

- `list()` infers as `list{}`
- `list(1L, 2L, 3L)` infers as `list{integer, integer, integer}`
- `list(1L, "foo")` infers as `list{integer, character}`

#### Record-like lists

A `list(...)` expression with only named elements infers as record-like when the element names are known statically.

Examples:

- `list(foo = 1L, bar = "foo")` infers as `list{foo: integer, bar: character}`

Two record-like lists are compatible when they declare the same field names, and when each field's type is compatible with the field of that name on the other side. Fields pair by name, so declaration order does not matter. `list(label = "a", id = 1L)` therefore satisfies `list{id: integer, label: character}`.

R lets a list name be any string. A field name that is not a syntactic R name is written, and rendered, in backticks, so `list(\`max size\` = 10L)` has the type `` list{`max size`: integer} ``. The quoting is not cosmetic. Unquoted, a name containing a comma would read back as two fields, so the type copied out of a finding would be a different type rather than a syntax error.

##### Reporting a record that does not fit

When a record-like list is rejected, the finding names the one field that failed rather than the two whole types. It covers three cases: a field both sides declare whose types do not fit, a field the expected type declares that the value does not have, and a field the value has that the expected type does not declare. A nested record names the path, outermost field first, as `retry.count`.

#### Array-like lists

An array-like list `list[T]` represents a list whose elements all share a common element type `T`. An array-like list has no fixed positional semantics, and it does not require element names to be statically known. Annotations normally introduce array-like lists. Coercion from a tuple-like, record-like, or map-like shape also introduces them, when all values are compatible with `T`.

When a fixed-shape list flows into `list[T]` and `T` is still an open inference variable, `T` takes the join of the elements rather than unifying with each in turn. Every `lapply(x, f)` call has this shape. Without the join rule, the first element would pin `T` and every later element would be a mismatch, so `lapply(list(1L, "a"), f)` would fail while `for` over the same list is specified to bind `integer | character`. A `T` that is already concrete keeps the all-must-fit rule. Coercion into a map-like `list[named: T]` joins the same way.

#### Map-like lists

A map-like list `list[named: T]` represents a name-keyed collection whose values all share a common value type `T`. A map-like list does not require the set of names to be statically known. Annotations typically produce map-like lists. Coercion from a structural list shape whose element names are not statically available also produces them.

#### The empty list

`list()` infers as the empty tuple-like shape `list{}`. It is compatible with any element-typed list shape, which covers `list[T]` and `list[named: T]` alike. It has no element whose type or name could conflict. `function(options = list())` is therefore a usable default for a `list[named: T]` parameter. A record-like expectation with required fields still rejects it, because those fields are genuinely missing.

### `NULL`

- the R literal `NULL` has type `NULL`
- `NULL` is the default unit type in this type system
- `NULL` is incompatible with every other type

Examples:

- `NULL` infers as `NULL`
- an empty block infers as `NULL`

### `Any` and `Unknown`

#### `Any`

- `Any` is the explicit opt-out from static type checking
- every type is compatible with `Any`
- `Any` is compatible with every type
- `Any` has two sources. You wrote it, or a standard-library declaration wrote it. The shipped stub corpus declares `Any` in roughly 180 return positions. It does so where a precise type would reject calls R accepts, or would need a feature the type grammar does not have yet. Each stub file names its own compromises in its header. The recurring ones are a value-dependent result shape, a `T`-or-`NULL` hybrid, arbitrary identifier-named arguments, and a formal with a trailing dot. An `Any` in a hover or in a finding is therefore not by itself a sign that something is wrong

#### `Unknown`

- `Unknown` means the checker could not infer a more specific type
- `Unknown` may arise from an unsupported construct, an unresolved name, a partially supported construct, or insufficient type information
- `Unknown` is compatible with every type, in both directions. This is the same blanket compatibility that `Any` has. It keeps one unmodelled value from cascading into a run of follow-on errors. It is also why a gap in the checker's knowledge means checks are skipped rather than wrong. A value the checker could not type flows into a `double` parameter without complaint
- `Unknown` differs from `Any` in intent, not in compatibility. `Any` is a declared instruction not to check the value. `Unknown` records that the checker could not tell. The one place that intent changes behaviour is [`@if-unknown`](#unknown-only-coercions), which supplies a type where one is missing. It applies to an `Unknown` and is refused on an `Any`, because `Any` already says not to check the value
- [Strict mode](#strict-mode) reports every site where the checker could not determine a type. It does not report a type that happens to be `Unknown`. Each of these records its own origin, and that origin is the finding: an unmodellable construct, a reference with no known type, a binding that does not stabilize across a loop, and a recursive definition. A type is not reported for being `Unknown`. A declaration whose return type is `Unknown` produces no strict finding at its call sites. An `Unknown` nested inside a larger type is not reported either, and the `fn(p: Unknown) -> Unknown` that an aliased generic closes to is such a case
- `Unknown` is not an explicit opt-out

### `Never`

- `Never` has no values
- it represents an expression that does not return normally
- `Never` is compatible with every type
- it is useful for non-returning constructs and calls

### Type parameters, aliases, and nominal types

A type expression may bind type parameters with a leading binder, written `<T> TYPE` or `<T, U> TYPE`.

- `<T> list[T]`
- `<T> list{ value: T }`
- `<T, U> fn(T) -> U`
- `<T> fn(T) -> T | NULL`

A binder name may carry a constraint, written `NAME: CONSTRAINT`, as in `<T: numeric> fn(values: T) -> T`. Two constraints are writable.

- `numeric` admits `integer` and `double`, scalar or vector, and any class with an arithmetic operator method.
- `atomic` admits one of the six atomic scalar types. Using a parameter as a vector element `T[]` imposes the same bound.

Any other constraint name is an annotation error, and the error names the two that exist. An argument whose type violates a constraint is a type error at the call.

A constraint restricts what a caller may instantiate `T` to, and the annotated body may also rely on it. With `<T: numeric> fn(x: T) -> T` the body may use `x` numerically, because every admissible instantiation is numeric. A plain `<T>` body that does arithmetic is a type error, and `atomic` does not imply `numeric`.

Binders are rank-1. A binder is allowed only at the outermost level of a type expression, and never nested inside another. A directive's `{...}` payload is not the outermost level: the expanded form declares its parameters with `@forall`, and a named type declares them on its name, as in `@type Pair<T>`. All three of these are refused:

- `fn(f: <T> fn(T) -> T) -> integer`
- `list{ value: <T> list[T] }`
- `@param f {<T> fn(T) -> T}`

A refused binder reports once. The type is then read as though the binder were not written, and the block carries no typing payload.

Named types are declared with `@type` and `@alias`, and either may take parameters as `NAME<T, U>`.

- `#: @type NAME {TYPE}` declares a nominal type whose representation is `TYPE`
- `#: @alias NAME {TYPE}` declares a structural alias for `TYPE`

Both forms share one namespace, and reusing a name under either form is an error. A definition block is allowed only at the top level of a file. Definitions are project-global, so a reference need not follow the declaration, in the same block or in the same file. See [Type names](#type-names) for the scoping rules.

#### Type parameters and generic application

A generic application must match its declaration's arity, checked against the project vocabulary.

- applying the wrong number of type arguments is an error at the applied name, as `Box<integer, double>` is for a one-parameter `Box<T>`
- applying type arguments to a non-generic declaration, such as `Meters<integer>`, is an error
- a bare reference to a generic, such as `Box` without arguments, is an error everywhere except after `@new`. There an unapplied generic infers its arguments through the representation check
- a mis-applied name compares like `Unknown` in the relations afterwards, so the one arity error never cascades into value-level mismatches

A type parameter may appear inside a structural type, a function type, and the vector suffix forms: `list[T]`, `list{ value: T }`, `fn(T) -> T`, `T | NULL`, `T[]`, and `T[named]`.

Using a type parameter as a vector element restricts it. A `T` in `T[]` carries the atomic-element bound, so it can instantiate only to one of the six atomic types: `logical`, `integer`, `double`, `complex`, `character`, and `raw`. This bound makes element-preserving signatures expressible. With `sort : <T> fn(x: T[]) -> T[]`, `sort(c("b", "a"))` types as `character[]` and `sort(c(1L))` as `integer[]`. A list argument cannot bind `T` at all, because a list is not an atomic element type.

- a scalar argument coerces into a generic vector parameter and binds the element. A `<T> fn(x: T[])` called with `2.5` binds `T := double`
- `[[` on a generic vector `T[]` extracts `T`
- an arithmetic operator over a `T[]` operand also requires the element to be numeric. The variable then holds both bounds, so it is a scalar `integer` or `double`. The result keeps the element, so `sort(x) + 1L` is still `T[]`, unless a `double` operand promotes the result to `double[]`
- a comparison over a `T[]` operand yields `logical[]`. A numeric partner constrains the element to be numeric

A bound that can no longer be satisfied is a type error at the expression that imposed it, whether that is binding an element variable to a non-atomic type or requiring a `character` element to be numeric.

Writing `X[]` is an error where `X` is neither an atomic type nor a type parameter, because a vector holds atomic elements only. A record, a function, and a nominal type are all such an `X`, and `list[X]` is the spelling for a list of them. An alias expands first, so `Id[]` with `@alias Id {integer}` is fine while an alias of a record is refused.

A named generic alias and a named nominal type are applied with angle brackets, as in `Box<integer>` and `Pair<integer, character>`.

`@new` uses the same generic application syntax when it introduces a value of a generic nominal type.

- `#: @new Person<integer>` is valid when `Person<T>` is declared with `@type`
- `#: @new Person` is also valid on a generic `Person<T>`. The arguments come from the representation check, which is the one place an unapplied generic is allowed

Everywhere else, the type argument count must match the declared parameter count exactly.

In `@type NAME<T, U, ...> {TYPE}` and in `@alias NAME<T, U, ...> {TYPE}`, the declared type parameters are in scope only within `TYPE`. A type parameter shadows a project-global type of the same name. A type parameter is not a generic, so applying type arguments to it, as in `Wrap<integer>` where `Wrap` is a parameter, is an annotation error rather than a reference to the shadowed global.

- `Pair<integer, character>` is valid for `Pair<T, U>`
- `Pair<integer>` is an error for `Pair<T, U>`
- `Pair<integer, character, double>` is an error for `Pair<T, U>`

#### Type aliases

An alias is purely structural. Writing the alias name is the same as writing its underlying type, so it creates no new identity and is compatible with whatever the underlying type is. An alias may appear anywhere a type may, including inside a larger type. A definition cycle is an error.

```r
#: @alias PersonShape {list{ name: character, age: double }}

#: PersonShape
value <- list(name = "bob", age = 20)
```

A generic alias may use its parameters anywhere inside the underlying type.

```r
#: @alias Box<T> {list{ value: T }}

#: Box<integer>
value <- list(value = 1L)
```

#### Nominal types

A nominal type creates a fresh type identity. It does so even when another nominal type has the same underlying representation type.

- a nominal type name may appear anywhere an ordinary type expression may appear
- a generic nominal type may use its type parameters anywhere inside its underlying representation type
- a nominal type is compatible with itself
- two different nominal types are not compatible with each other, even when their representation types are identical
- an ordinary structural value is not compatible with a nominal type unless `@new` introduces it

- a value of a nominal type is compatible with its underlying representation type
- when an operator, an indexing form, or a loop iteration requires a structural shape, a nominal value projects to its underlying representation type. The projected result is structural, not nominal

Projection examples:

```r
#: @type Person {list{name: character}}

#: @new Person
person <- list(name = "bob")

person$name
```

`person$name` has type `character`, because `$` sees the representation type of `Person`.

```r
#: @type Meters {double}

#: @new Meters
height <- 1.8

height + height
```

`height + height` has type `double`. Arithmetic projects `Meters` to `double`, and the result does not keep the nominal identity.

An opaque nominal type has no representation to project. Standard-library stubs declare `data.frame`, `factor`, `connection`, `Date`, and other types the grammar cannot describe structurally as a bare `@type NAME`. See [Standard library stubs](/type-checking/stubs). Four rules apply to them.

- `$`, `[`, and `[[` are accepted, and the result is `Unknown` rather than an error. The R object behind such a class commonly supports value-dependent access, for example `df$amount` and `df[rows, ]`, and refusing would reject ordinary R
- the access is not checked further. There is no field-existence check, no index-count check, and no index-type check, so `df[i, j]` and `df[rows, ]` both pass
- every such access is an unsupported construct under [strict mode](#strict-mode)
- arithmetic, loop iteration, and every other structural requirement on an opaque nominal remain type errors, unless the class declares the corresponding [operator method](#operator-methods-on-a-class). The nominal identity itself still checks like any other nominal type

`Person` and `Pet` below are distinct and incompatible, although their representations are identical. A nominal value satisfies its representation type, so `shape` is valid. A structural value does not satisfy the nominal type, so the call is an error.

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

A generic nominal takes its parameters on the declared name.

```r
#: @type Person<T> {list{ value: T }}

#: @new Person<integer>
person <- list(value = 1L)

#: list{ value: integer }
shape <- person
```

#### Type-argument variance

Two applications of the same generic nominal type, such as `Box<integer>` against `Box<integer | NULL>`, are checked one type argument at a time. The direction of each argument check comes from where its type parameter occurs in the representation type, so the variance of each parameter follows from its occurrences.

- A covariant position preserves the checking direction. `Box<integer>` is therefore compatible where `Box<integer | NULL>` is expected, because a narrower argument satisfies a wider one. The covariant positions are a function return, a container or structural element, and a direct occurrence. A container or structural element covers a `list` item, a `list{...}` field, a tuple item, a vector element, and a union member.
- A function parameter position is contravariant, so it flips the checking direction. Take `@type Handler<T> {fn(value: T) -> NULL}`. `Handler<integer | NULL>` is compatible where `Handler<integer>` is expected. `Handler<integer>` is not compatible where `Handler<integer | NULL>` is expected, because otherwise a `NULL` could reach a function that only accepts `integer`.
- A parameter that occurs in both a covariant and a contravariant position is invariant. Its argument must match exactly in both directions. Take `@type Cell<T> {list{ get: T, set: fn(value: T) -> NULL }}`. `Cell<integer>` and `Cell<integer | NULL>` are then mutually incompatible.
- A parameter that does not occur constrains nothing, and it accepts any argument.

A type parameter inside a nested generic application, such as the `T` of `Sink<T>` within `@type Outer<T> {Sink<T>}`, is invariant. The inner type's own variance does not compose with the outer direction.

When a generic nominal has no visible definition, every argument is checked invariantly. That over-rejects by requiring an exact match rather than over-accepting an unsound widening.

R lists and vectors are mutable, and their element positions are still treated covariantly. A sound mutable container would have to be invariant, which would break the structural coercions such as scalar-to-vector and `T` into `T | NULL`.

Where a single representative type is needed, every nominal argument must match exactly, whatever the parameter's variance.

### Union types

A union type `A | B | ...` describes a value that has one of the member types. Any number of members is allowed, and any type may be a member. `T | NULL` is the two-member special case, and it is the nullable form of `T`.

- union syntax is allowed anywhere a type can appear, which includes:
  - a variable annotation
  - a function parameter
  - a function return
  - a compact function type annotation
  - a nested function type
  - a list annotation and a map-like list annotation
- a union describes which shapes a value can take. It does not merge or coerce its members
- a type may be parenthesized for grouping. `(TYPE)` means exactly `TYPE` and adds no structure of its own. Grouping makes a union with a function-type member writable. In `fn() -> integer | NULL` the `->` extends over the whole union, so that type is a function returning `integer | NULL`. An optional callback is therefore written `(fn() -> integer) | NULL`, which is also the form such a union renders as. A `<T>` binder may not appear inside parentheses, because binders stay at the outermost level of an annotation

Examples:

- `integer | character`
- `integer | character | NULL`
- `character[] | NULL`
- `integer[] | character[]`
- `fn(count: integer | NULL) -> character | logical | NULL`
- `(fn() -> integer) | NULL`, an optional callback, which is a function returning `integer`, or `NULL`

A union whose members all collapse to one type is that type. `NULL | NULL` is accepted and means `NULL`, by the same singleton rule that every other duplicate member follows below.

#### Union normalization

Unions are kept in one normal form, so that equivalent spellings mean the same type and render as the same type.

- **Flat.** A union member that is itself a union flattens into the enclosing union. An alias expanding to `(A | B) | C` therefore normalizes to `A | B | C`.
- **Deduplicated.** Repeated members collapse, keeping the first occurrence. `integer | character | integer` normalizes to `integer | character`.
- **Order-insensitive.** Member order does not affect meaning, so `integer | NULL` and `NULL | integer` are the same type. Rendering preserves first-occurrence order, except that `NULL` always renders last.
- **Singleton collapse.** A union whose members collapse to a single type is that type. `integer | integer` is `integer`, and a nullable of `NULL` itself normalizes to `NULL`.
- **`Any` absorbs.** A union with an `Any` member is `Any`, because every value already satisfies `Any`.
- **`Unknown` absorbs.** Otherwise, a union with an `Unknown` member is `Unknown`. Such a union claims no more than that the type is not statically known.

Normalization also applies to the unions the checker builds itself, which are branch joins, alias expansions, and `NULL`-producing lookups. A rendered union is therefore always flat, always deduplicated, and always at least two members.

### Union compatibility

Compatibility treats a union differently on the two sides.

- **Into a union, on the expected side.** A value fits an expected union when it fits any member.
  - `T` is compatible with any union containing `T`, so `integer` is compatible with `integer | character | NULL`
  - `NULL` is compatible with any union containing `NULL`
  - the usual coercions apply per member, so a value coercible to some member fits the union
- **Out of a union, on the actual side.** A union value must be accepted in every shape it can take, so a union is compatible with an expected type only when each of its members is.
  - a union is compatible with any wider union, so `integer | NULL` is compatible with `integer | character | NULL`
  - a union is not compatible with a plain member type. `integer | character` is not compatible with `integer`, and `T | NULL` is not compatible with plain `T`
- member checks are attempted in member order, and a failed member attempt leaks no inference bindings into the next attempt
- A flexible argument checked against an expected union binds to the whole union at that first use, exactly as unification would bind it. A flexible argument is an inference variable, which is an unannotated parameter or a local that is not yet pinned. Uses commit in program order, so a later use that requires a different type reports its error at that later site, against the already-committed union. When two union-typed contracts share only some members, such as `integer | character` at one call and `logical | character` at the next, the intersection is not computed. Annotate the value with the intended member type to satisfy both. First-use commitment keeps checking deterministic in program order, which is the order R evaluates in

## Function types

Function annotations use only `#:` comments.

A function may be annotated in exactly one of these two styles:

- expanded style, with an optional `@forall`, then `@param`, and `@return` or `@returns`
- compact style, with a single `fn(...)` annotation and an optional `-> RETURN_TYPE`

Mixing the two styles for the same function is not allowed.

When function annotations use consecutive `#:` lines, those lines are one annotation block for that function. They are not separate independent annotations.

### Expanded function annotations

Expanded function annotations use these forms:

- `@forall T,U,...`
- `@forall T`
- `@forall T: numeric`. A binder constraint uses the same names and semantics as the compact `<T: numeric>` form. See [Type parameters, aliases, and nominal types](#type-parameters-aliases-and-nominal-types)
- `@param name {TYPE}`
- `@param [name] {TYPE}` for an optional parameter
- `@return {TYPE}`
- `@returns {TYPE}`

Additional rules:

- repeated `@forall` lines are allowed, and they accumulate in source order
- duplicate type parameter names in the same annotation block are errors
- every `@forall` directive must appear before any `@param`, `@return`, or `@returns` directive
- the bracket syntax for an optional parameter follows JSDoc-style notation
- when no `@return` or `@returns` annotation is provided, the return type is elided. On a checked annotation of a function definition, it is inferred from the function's body. See [Elided return types](#elided-return-types). In every position with no body to infer from, it means `NULL`
- at most one `@return` or `@returns` directive may appear in the block
- every `@param` directive must appear before `@return` or `@returns`

Examples:

```r
#: @param count {integer}
#: @param [label] {character | NULL}
#: @return {integer}
double_count <- function(count, label = NULL) { count + count }
```

```r
#: @param count {integer}
log_count <- function(count) { }
```

```r
#: @forall T
#: @param value {T}
#: @return {T}
identity <- function(value) value
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

```r
#: @forall T
#: @forall U
#: @param left {T}
#: @param right {U}
#: @return {T}
keep_left <- function(left, right) left
```

### Compact function annotations

A compact function annotation uses a single function type:

- `fn(name: TYPE) -> RETURN_TYPE`
- `fn(TYPE) -> RETURN_TYPE`
- `fn(name: TYPE, [optional_name]: TYPE) -> RETURN_TYPE`
- `<T> fn(name: TYPE) -> RETURN_TYPE`
- `<T, U, ...> fn(TYPE) -> RETURN_TYPE`

An optional parameter must be named, as `[name]: TYPE`. A bare optional positional form such as `fn(integer, [character])` is not supported.

A function may declare one rest parameter to accept a variable number of arguments. It is written `...: TYPE`, and `fn(...)` is shorthand for `...: Any`. Naming it, as `...items: TYPE`, is an annotation error, because rest arguments are matched by position.

- `fn(prefix: TYPE, ...: TYPE) -> RETURN_TYPE` puts it after fixed parameters
- `fn(...: TYPE, [option]: TYPE) -> RETURN_TYPE` puts named parameters after it

Its position is part of the signature and mirrors the position of `...` in the R formal list. See [Function calls](#function-calls) for how arguments then match.

An annotation declares the types of a definition's parameters. It does not declare the parameter list. R matches a call's arguments against the formals in the `function(...)` header, so those formals are the call interface. That covers their names, their order, their defaults, and where `...` sits. An annotation cannot add, remove, or reorder them. Every parameter the annotation does not mention keeps its inferred type, so annotating one parameter of several is a supported partial form.

Where the declared shape disagrees with the definition, the definition wins at every call site, and the disagreement is reported once, at the definition. A call is never blamed for an annotation's mistake. This is the same rule that a [refused block](#typing-comment-syntax) follows, reaching the case where the annotation parses cleanly and only its shape is wrong. These are the disagreements:

- a declared parameter name that is not a formal. The annotation is describing a parameter the function does not have
- more declared parameter types than there are formals left to receive them
- a declared optional `[name]` over a formal with no default. A declared optional requires the actual formal to carry a default, because callers may omit it. The reverse is fine, so an actual default on a parameter the annotation declares required is not a disagreement
- a rest parameter at a different boundary in the annotation and in the formal list. The rest parameter must also exist on both sides or on neither, so a fixed annotation on a variadic function and a variadic annotation on a fixed function are both rejected

The last three are reported as a whole-signature mismatch, and the first names the parameter. In every case the body is still checked under the parameter types the annotation does pin down, so hover and navigation keep their facts.

Additional rules:

- when the return type is omitted, it is elided. It is inferred from the body on a checked definition annotation, and means `NULL` everywhere else. See [Elided return types](#elided-return-types)
- when a compact function annotation starts with `<...>`, the binder introduces rank-1 type parameters for the whole function type
- a compact function annotation does not use `fn<T>(...)`. The supported binder form is `<T> fn(...) -> ...`

Examples:

```r
#: fn(...: character) -> character
join <- function(...) paste0(...)

#: fn(x: character, ...: character) -> character
wrap <- function(x, ...) paste0(x, ": ", paste(...))
```

The `...` in the annotation must appear in the same position as the `...` formal of the function. Both positions count the parameters declared before them. See [Function type compatibility](#function-type-compatibility).

### Elided return types

Both annotation styles allow the return type to be left unwritten. An expanded block with no `@return` or `@returns` line elides it, and so does a compact `fn(...)` with no `-> RETURN_TYPE`. An elided return is not the same as a written `NULL`. What it means depends on whether there is a function body to infer from.

- On a checked annotation of a function definition, the return type is inferred from the body, exactly as it would be with no annotation at all. Such an annotation is attached to a `function(...)` literal whose body is checked against it. Annotating only the parameters is the common partial form, and it must not silently pin the return. `@param u {integer}` on `add_one <- function(u) u + 1L` therefore infers `fn(u: integer) -> integer`. Writing the return as `Unknown` has the same effect, because `Unknown` records that nothing is known, so it never overrides a body that shows otherwise. `Any` is the annotation that turns checking off for a value.
- In every position with no body to infer from, an elided return means `NULL`. This matches R functions that are called for their side effects. Three positions have no body. The first is a nested function type, such as a callback parameter written `@param cb {fn(integer)}`. The second is a [trusted coercion](#trusted-coercions) or an [`@if-unknown` coercion](#unknown-only-coercions), both of which adopt exactly the written type without consulting the body. The third is an annotation on a value that is not a function literal, such as `g <- f` with a `#: fn(integer)` annotation.

A function that genuinely returns `NULL` can always say so explicitly, with `@returns {NULL}` or `-> NULL`. That explicit form is enforced, so a body returning anything non-`NULL` against it is a type error.

Examples:

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

An unannotated `function(...)` expression infers its type from the definition.

- every parameter appears as a named parameter under its definition name, because R matches parameters both by name and by position
- a parameter with a default is optional at call sites, and so is one the body tests with [`missing()`](#missing-on-a-defaultless-formal)
- a `...` formal becomes a rest parameter of element type `Any`, at the position it holds in the formal list, so `function(x, ...) …` infers as `fn(x: T, ...: Any) -> …`
- the values reaching `...` are not tracked into the body, so forwarding `...` to another call types as `Unknown`
- parameter and return types are inferred, and an unconstrained parameter generalizes when the function is bound to a name
- a requirement the inference could not discharge survives into the exported type, so a parameter used numerically exports `<T: numeric>` and cross-file calls keep checking it

Defaults are typechecked, and four rules govern how they interact with the parameter's type.

- an error inside a default is reported, and a default for an annotated parameter must be compatible with the declared type
- a `NULL` default is checked like any other. `function(title = NULL)` is R's usual spelling for an optional argument, and it does not make the parameter optional to the body: when the caller omits it, `title` is `NULL` there, so a declared `character` is a promise the function does not keep. Declare `character | NULL` and narrow it with `if (is.null(title))`. Marking the parameter `[title]` relaxes only the call
- an unannotated parameter takes its type from its uses, not from its default, so `function(x = 1) x` is `<T> fn([x]: T) -> T` and passing a character is not a finding
- a call that omits the argument takes the default's type, because that is the value R puts in the frame, so `f <- function(x = 1) x` makes `f()` a `double` and `f("a")` a `character`. A default is therefore checked against an instantiation of the declared type rather than against the binder, so `#: <T> fn([x]: T) -> T` over `function(x = 1) x` is accepted. A concrete declared type is unaffected, and `fn(title: character)` still refuses a `NULL` default

Examples:

- `function(x) x` infers as `<T> fn(x: T) -> T`
- `function(count, label = NULL) count` may be called as `f(1L)`, `f(count = 1L)`, or `f(1L, "x")`

### Named and positional parameters

Parameter names in function types are part of the call interface.

- a named parameter may be called with a named argument
- an unnamed parameter is positional only

Example:

- `fn(count: integer) -> integer` allows a call with `count = 1L`
- `fn(integer) -> integer` makes a call with named arguments a type error

An optional parameter follows the same rule, and it must be named:

- `fn(count: integer, [label]: character) -> integer`

A parameter name and a record field name may contain an interior `.`. This matches R's identifier convention for arguments such as `na.rm` and `length.out`:

- `fn(x: double, na.rm: logical) -> double`
- `list{na.rm: logical}`

The leading character must still be a letter or `_`, and the dot is interior only. Type names and type parameter names are unaffected, so a type reference and a `<...>` binder name may not contain `.`.

### Function type compatibility

A function value is compatible with an expected function type when its parameters accept every call the interface may make, and its return type satisfies the interface's.

Parameter names are part of the interface, because R matches call arguments against the definition's formal names.

- a named parameter pairs by name, so `fn(a: integer, b: character)` accepts a function defined `function(b, a)`
- an unnamed parameter type pairs with the remaining parameters left to right, so `fn(count: integer) -> NULL` and `fn(integer) -> NULL` are mutually compatible
- an annotation may not rename a parameter. `fn(count: integer) -> integer` over `function(n) n` is an error, because it would promise callers a name the runtime rejects

Arity is a range rather than a fixed count. The function may declare more parameters than the interface passes, as long as the extras have defaults. It may not require more than the interface supplies, and it may not refuse an argument the interface may send. An expected-optional parameter promises callers they may omit it, so the actual formal must carry a default.

Parameters are contravariant and the return type is covariant. Each expected parameter type must be compatible with the actual parameter type, so the function must accept every argument the interface may pass. The actual return type must be compatible with the expected one.

- `fn(integer | NULL) -> integer` is accepted where `fn(integer) -> integer` is expected
- `fn(integer) -> integer` is rejected where `fn(integer | NULL) -> integer` is expected, because the interface may pass `NULL`
- `fn(a: integer, [b]: integer) -> integer` is accepted where `fn(integer) -> integer` is expected, because `b` defaults. This lets a standard-library function serve a callback interface, so `lapply(list(mean, sd), function(g) g(1:3))` types as `list[double]` although `mean` and `sd` declare optional formals the callback never passes
- `fn(a: integer, b: integer) -> integer` is rejected there, because the interface never supplies `b`, and `fn() -> integer` is rejected because it cannot receive the argument the interface sends
- `fn(count: integer, [label]: character) -> integer` does not accept `function(count, label) count`, because `label` has no default

#### Callback forwarding at variadic call sites

R's apply family invokes its callback as `FUN(element, ...)`, forwarding the caller's surplus arguments, so a callback with more formals than the interface declares is still correct when the call forwards the difference. At a call to a variadic function, a function-typed argument that fails the plain interface check is re-checked as that forwarded invocation.

- forwarded named arguments consume the callback's same-named formals first, each checked against its formal's type
- the interface's own parameter types then fill the remaining formals in order, followed by the forwarded positional arguments
- a formal the invocation leaves unfilled must have a default
- the callback's return type must satisfy the interface's return type
- the re-check binds nothing on failure, and the reported error is the plain interface mismatch

`lapply(words, gsub, pattern = "a", replacement = "o")` therefore checks `gsub(word, pattern = "a", replacement = "o")` and types as `list[character]`, and `lapply(words, nchar)` accepts the optional formals of `nchar`. A forwarded argument of the wrong type fails the probe and the call errors.

Variadic compatibility is conservative. A variadic function type is compatible only with another variadic function type, and never with a fixed-arity one in either direction. Their rest element types are contravariant like ordinary parameters, and the number of parameters declared before `...` must agree on both sides, because that position decides which parameters callers may fill positionally. This over-rejects some safe pairings, and never admits an unsound one.

#### Reporting a function that does not fit

When a function value is rejected at a parameter position, the finding names the one position that failed rather than the two whole signatures. It names either the parameter the interface passes a value the function will not take, or the return the function produces that the interface will not take.

### Higher-order function types

- a function type may appear inside another function type
- rank-1 polymorphism is supported, and higher-rank polymorphism is not

Examples:

- `fn(transform: fn(integer) -> character) -> character`
- `fn(fn(integer) -> character, integer) -> character`

Not allowed:

- `fn(transform: <T> fn(T) -> T, integer) -> integer`
- `fn(fn(value: <T> list[T]) -> integer) -> integer`

An expanded annotation may also use a function type directly.

Example:

```r
#: @param render_count {fn(integer) -> character}
#: @param count {integer}
#: @return {character}
apply_renderer <- function(render_count, count) { render_count(count) }
```

## Type annotations and assertions

### Checked annotations

`#: TYPE` is a checked annotation.

- the annotated value must be compatible with `TYPE`
- checking is compatibility-based, not exact-equality-based
- a checked annotation may therefore allow widening where the semantics explicitly define it
- when the annotation succeeds, the value is accepted through coercion where a coercion is needed, and the annotated binding or expression then has type `TYPE`

Example:

```r
#: list[integer]
value <- list(1L, 2L, 3L)
```

This is valid because `list{integer, integer, integer}` is compatible with `list[integer]`.

### Unknown-only coercions

`#: @if-unknown TYPE` is an unknown-only coercion.

- it is allowed only when the inferred type is `Unknown`
- when the checker already knows the source type, `#: @if-unknown` is an error, even if the requested type matches that known type
- when the coercion is allowed, the annotated binding or expression then has type `TYPE`

Examples:

```r
#: @if-unknown integer
value <- unsupported_value
```

This is valid only if `unsupported_value` has inferred type `Unknown`.

```r
#: @if-unknown integer
value <- 1L
```

This is an error because the checker already knows the type.

Use `#: @if-unknown TYPE` to fill an inference gap when the checker has no better type than `Unknown`. It never overrides information the checker already has.

### Trusted coercions

`#: @trust TYPE` is a trusted coercion.

- it tells the checker to treat the annotated value as `TYPE`, without requiring ordinary compatibility at that annotation site
- it is the unchecked override
- `#: @trust TYPE` has the same effect as coercing the value to `Any` and then to `TYPE`. The direct form exists because it is shorter to write

Examples:

```r
#: @trust integer
value <- external_input
```

```r
#: @trust fn(count: integer) -> character
render_count <- callback
```

A trusted coercion suppresses a real error as readily as a false one. Use it only when you know more than the checker does.

### Nominal introduction

`#: @new NOMINAL_TYPE` introduces a nominal value.

- `NOMINAL_TYPE` must be a nominal type reference declared with `@type`
- `NOMINAL_TYPE` may be a bare nominal name such as `Person`, or a generic nominal application such as `Person<integer>`
- an alias, a structural type, a union, a function type, and every other non-nominal type form is not allowed after `@new`
- a generic nominal may be written unapplied. `@new Person` on a `Person<T>` infers the type arguments from the representation check, so a value of `list{value: 1L}` mints a `Person<integer>`
- the annotated value must be compatible with that nominal type's underlying representation type
- when the annotation succeeds, the annotated binding or expression then has type `NOMINAL_TYPE`
- when the annotated value already has type `NOMINAL_TYPE`, the annotation is allowed and has no further effect
- `@new` is an annotation form, not a type expression, so it cannot appear inside compact type syntax or inside an expanded function annotation
- `@new` is the only nominal introduction. A checked annotation such as `#: Person` on a structural value is a type error even when the value matches the representation. The checked form asserts that the value already has the nominal type. It does not mint one

Examples:

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

The third example is an error. An ordinary checked annotation for a nominal type requires the value to be nominally typed as `Person` already.

## Operators

### Operators over union operands

Control-flow joins and heterogeneous containers produce union-typed operands. Every operator below therefore accepts a union member-wise.

- a union operand is accepted where every member is accepted. One unacceptable member rejects the whole operand, and the diagnostic shows the full union type
- the result is the join of the per-member results. For a binary operator, that is the join over every pair of left and right members

Examples:

- `(integer | double) + integer` is `integer | double`. Each member is numeric, `integer + integer` is `integer`, and `double + integer` is `double`
- `(integer | double) > 0L` is `logical`
- `(integer | character) + 1L` is a type error, because the `character` member is not numeric
- `(integer | NULL) + 1L` is a type error, because the `NULL` member is not numeric
- `rec$a` on `list{a: integer} | list{a: character}` is `integer | character`. The access is an error if any member lacks the field
- `for` over `integer[] | character[]` binds the loop variable as `integer | character`

#### Conditions

`if`, `while`, and the operands of `&&` and `||` all take a scalar condition. R decides what a scalar condition admits.

- `logical` is the ordinary case
- `integer` and `double` are accepted, and they coerce exactly as R coerces them. Zero is false, and anything else is true. This makes `if (length(x))`, `if (nrow(df))`, and `while (n)` ordinary R rather than mistakes
- `character`, `complex`, and `raw` are type errors. R refuses `complex` and `raw` outright. In a `character` condition R accepts only the spellings of `TRUE` and `FALSE`, such as `"T"` and `"true"`, and raises at run time on every other string, so `if ("yes")` is reported
- a vector is a type error, because a condition whose length is not one is an error in R too
- a condition whose type is still undetermined binds to `logical`. That is the useful default for an unannotated predicate, so `function(flag) if (flag) 1L` infers `flag: logical`

`!` follows the same coercion rule, and it always yields a `logical`. `!0` is `TRUE`, and `!5` is `FALSE`.

### Indexing

`[[` extracts a single element. `[` is R's general subsetting operator, defined for the vector and list forms below. Runtime indexing failures are not modelled anywhere in this section: an out-of-range position and a missing name produce `NA` at run time, which is a value-level outcome.

`$name` behaves as `[["name"]]` on lists, on records, and on the tolerated opaque nominals, and a backtick-quoted name follows the same rule. It does not work on atomic vectors, because R rejects `$` on every atomic vector including a named one. `c(foo = 1L)$foo` is therefore a type error, while `c(foo = 1L)[["foo"]]` extracts `integer | NULL`.

A field on a union subject may be absent from some members. R answers `NULL` for a name a list does not carry, so a field present in only some of the subject's shapes reads as that field's type unioned with `NULL`. Code that builds a list field by field therefore checks:

```r
args <- list()
if (escape) args$escape <- TRUE
args$escape        # logical | NULL
```

A field that no shape carries is still an error, because that is a typo rather than an absence the program is prepared for. The "did you mean" suggestion draws on every field any member carries.

#### `[[` on vectors

- scalar-like `T` returns `T`
- array-like `T[]` returns `T`
- map-like `T[named]` returns `T | NULL` for a name-based index

#### `[[` on lists

- array-like `list[T]` returns `T`
- map-like `list[named: T]` returns `T | NULL` for a name-based index, and `T` for a positional or computed one
- a tuple-like list returns the element at a literal position. A position that does not exist is an error, and a computed position returns the union of the item types
- a record-like list returns the field at a literal name or literal position. A name or position that does not exist is an error, and a computed index returns the union of the field types, which is what types a call to a function looked up in a list

#### `[` on vectors

The result depends on the subject's shape and the index's shape. These are the index shapes.

- A scalar-like `integer`, `double`, or `character` index selects one position and yields the scalar element type. The scalar result is not always exact, because a scalar negative index such as `x[-1]` drops one element and returns the rest. A scalar coerces into every vector position, so the claim can never produce a false error later.
- An array-like or map-like numeric or character index, such as `x[c(1L, 3L)]`, selects many positions and keeps the subject's shape.
- A `logical` index of any shape is a mask, such as `x[x > 0]`, and keeps the subject's shape. A scalar `TRUE` or `FALSE` recycles over the whole vector.
- `NULL` selects nothing and yields the array-like vector of the element type.
- An index whose shape is undetermined, such as an unannotated parameter or an `Unknown`, counts as scalar-like and is left unconstrained.
- A `complex` or `raw` index is a type error, and so is a list, a function, or any other non-vector index.

With `E` the element type: scalar-like `E` and array-like `E[]` both yield `E` for a scalar index and `E[]` otherwise. Map-like `E[named]` yields `E` for a scalar index and `E[named]` otherwise, because `[` keeps names.

A character index is allowed on any vector shape, not only a map-like one. R returns `NA` rather than erroring when the subject has no names, and most operations erase names, so requiring a map-like subject would flag legal programs.

- `c(1L, 2L, 3L)[2L]` is `integer`
- `c(1L, 2L, 3L)[c(1L, 3L)]` is `integer[]`
- `x[x > 0]` on `x: double[]` is `double[]`
- `c(a = 1L, b = 2L)[c("a", "b")]` is `integer[named]`
- `x[list(1)]` is a type error

#### `[` on lists

`[` slices a list, so the subject's fixed shape does not survive into the result.

- array-like `list[T]` returns `list[T]`
- map-like `list[named: T]` returns `list[named: T]`
- a tuple-like list returns `list[T]` where `T` is the union of the item types, so `list(1L, "foo")[1L]` is `list[integer | character]`
- a record-like list returns `list[named: T]` where `T` is the union of the field value types
- slicing the empty list yields `list[NULL]`

#### Indexing opaque nominal types

`$`, `[`, and `[[` on an opaque nominal type such as `data.frame` or `factor` yield `Unknown` without further checking. See [Nominal types](#nominal-types) for the rule and its rationale.

#### Indexing an unresolved inference variable

`$`, `[[`, and `[` on an unresolved inference variable yield `Unknown`, and they leave the variable unconstrained. Such a variable is an unannotated parameter whose shape nothing pins down, as in `function(node) node$value`, `function(x) x[[1L]]`, and `function(x) x[1L]`. There is no "not a list" error and no "unsupported `[`" error there.

Ordinary R reads a field, an element, or a slice off a value whose shape the author never wrote down. A tree fold and a generic accessor both do it, so refusing here would report correct code. The access is therefore left undescribed rather than refused, and it is reported as an unsupported construct under [strict mode](#strict-mode), exactly as for an opaque nominal.

This covers multi-index subsetting too. `function(m, i, j) m[i, j]` is silent, because such a function is written for a caller that knows the shape when the callee does not. A subject whose shape was written down still refuses a shape no rule covers, so `c(1L, 2L)[1L, 2L]` is an error.

### Unannotated values in arithmetic

An unannotated value used as a numeric operand is required to be numeric rather than rejected. It must end up as `integer` or `double`, in any vector shape, or as a class that declares an arithmetic operator method. See [operator methods on a class](#operator-methods-on-a-class).

- `function(x) x + 1L` infers as `<T: numeric> fn(x: T) -> T`, and calling it with `"oops"` is a type error
- `function(x) x / 2` infers as `<T: numeric> fn(x: T) -> double`
- `function(a, b) a + b` infers as `<T: numeric> fn(a: T, b: T) -> T`

A value that is still unconstrained when it reaches a binding defaults to `double`, matching R's treatment of bare numbers.

### Arithmetic operators

Arithmetic operators are defined for numeric operands:

- `integer`
- `double`
- `logical`. R promotes a logical operand to `integer` before arithmetic, so `TRUE + TRUE` is `2L`. A logical operand therefore computes as `integer`, and the atomic result rules below need no logical case
- an unannotated value required to be numeric. See [unannotated values in arithmetic](#unannotated-values-in-arithmetic)

They are also defined for a class that declares an operator method. See [operator methods on a class](#operator-methods-on-a-class).

A map-like vector may participate through its compatibility with an array-like vector, and arithmetic does not preserve map-likeness.

An operand whose shape is still unknown, such as an unannotated parameter, counts as scalar-like, here and in the comparison rules. A scalar coerces into every vector position, so the claim can never produce a false error later. The cost is that vector-in and vector-out shape is not tracked through such a function. A generic vector written `T[]` is the exception, and its operator results are genuinely vector-shaped.

#### Binary `+`, `-`, and `*`

Binary `+`, `-`, and `*` use these rules:

- atomic result:
  - `integer op integer` returns `integer`
  - when either operand is `double`, the result is `double`
- shape result:
  - when both operands are scalar-like, the result is scalar-like
  - otherwise, the result is array-like

Examples:

- `integer + integer` returns `integer`
- `integer - double` returns `double`
- `double * integer[]` returns `double[]`
- `integer[named] + integer` returns `integer[]`

#### Binary `/`, `**`, and `^`

Binary `/`, `**`, and `^` use these rules:

- `^` and `**` are the same operator, because `**` is R's parser alias for `^`
- atomic result:
  - always `double`
- shape result:
  - when both operands are scalar-like, the result is scalar-like
  - otherwise, the result is array-like

Examples:

- `integer / integer` returns `double`
- `double ** integer` returns `double`
- `2L ^ 3L` returns `double`
- `integer[] / integer` returns `double[]`

#### Binary `%%` and `%/%`

Modulo `%%` and integer division `%/%` follow the same rules as binary `+`, `-`, and `*`:

- atomic result:
  - `integer op integer` returns `integer`
  - when either operand is `double`, the result is `double`
- shape result:
  - when both operands are scalar-like, the result is scalar-like
  - otherwise, the result is array-like

Every other `%op%` special operator is an unsupported construct.

#### Unary `-`

Unary `-` accepts `integer` and `double`.

Its result rules are:

- atomic result:
  - `-integer` returns `integer`
  - `-double` returns `double`
- shape result:
  - a scalar-like operand and an array-like operand keep their shape
  - a map-like vector may participate through its compatibility with an array-like vector, and the result is array-like

Examples:

- `-1L` returns `integer`
- `-c(1L, 2L)` returns `integer[]`
- `-c(foo = 1L, bar = 2L)` returns `integer[]`

### Operator methods on a class

An operator whose operand is a nominal dispatches to that class's declared operator method before the numeric rules apply. This is how R dispatches `d + 30L` on `Date` through `+.Date`.

Lookup mirrors R's own order. The operator-specific method comes first, such as `+.Date`. The operator's S3 group generic comes next, which is `Arith.Date` for arithmetic and `Compare.Date` for comparison. `Ops.Date` comes last. Either operand's class may supply the method, left first, so `30L + d` behaves like `d + 30L`.

A method is declared the way R names it, in a stub or an annotation, so the result stays precise per operand pairing. Differencing two `Date` values gives a `difftime`, and offsetting one by a count gives a `Date`. A class that declares an operator but accepts no candidate for the operands at hand is a `type-mismatch` error rather than a fall-through to the numeric rules, which is what R does too. A class that declares nothing falls through to the numeric rules, so an opaque nominal is still a type error under arithmetic.

Your own classes count. A method declared anywhere the global scope reaches makes its class arithmetic exactly as a shipped stub does, which covers a package's `R/` sources and a script's own top level. That is also what lets the class satisfy the numeric requirement, so passing a `Money` to `function(x) x + 1L` is accepted when the project defines `+.Money` and refused when it defines no arithmetic method.

`c()` dispatches the same way. A class that declares a `c.Class` method keeps its class through concatenation, so `c(d1, d2)` on two `Date` values is a `Date`. A nominal with no such method gives `Unknown`, because R's default `c()` strips attributes.

The method name's suffix is the nominal's name, not R's full class vector, so a class declared `@type ggplot` takes `+.ggplot` even though R registers the method as `+.gg`.

`a %op% b` is the call `` `%op%`(a, b) ``. A `%…%` operator the standard-library corpus declares is checked and typed as that call, so `"a" %in% valid` is `logical` and `m %*% m` is a `matrix`. Every other `%…%` operator, including a project's own, gives `Unknown`, because a user operator may quote its right operand rather than evaluate it, as `%>%` from magrittr does. A use of any `%…%` operator still counts as a read of its name, so a project's own operator is never reported unused.

### Comparison operators

`<`, `<=`, `>`, `>=`, `==`, and `!=` compare two operands of the same comparison family.

- there are two comparison families:
  - the numeric family holds `logical`, `integer`, and `double`, freely mixed. R promotes a logical operand to `integer` before comparing, exactly as it does for arithmetic, so `flags > 0` and `flag == TRUE` are both ordinary numeric comparisons
  - the `character` family holds `character`
- both operands must belong to the same family. Comparing across families is a type error
- a flexible operand is constrained to the numeric family when the other operand is concretely numeric, and left unconstrained otherwise. A flexible operand is an inference variable, such as an unannotated parameter. Both operands stay unconstrained when both are flexible, so `function(a, b) a < b` infers as `<T, U> fn(a: T, b: U) -> logical` and a cross-family call of such a function is accepted. There is no comparable constraint kind. R's comparison coerces across atomic families at runtime, so `1 < "2"` is legal R, and tying flexible operands to each other or to a family would reject legal programs. The same-family rule applies only where both families are concretely known
- `complex` and `raw` operands are not supported
- a map-like vector participates through its compatibility with an array-like vector
- the result follows three rules:
  - the atomic result is always `logical`
  - when both operands are scalar-like, the result is scalar-like
  - otherwise, the result is array-like

Examples:

- `1L < 2L` returns `logical`
- `1L == 1.5` returns `logical`
- `"a" < "b"` returns `logical`
- `c(1L, 2L) > 1L` returns `logical[]`
- `c(TRUE, FALSE) > 0` returns `logical[]`
- `1L < "a"` is a type error

### Unary `!`

Logical negation `!` coerces its operand exactly as a [scalar condition](#conditions) does, and it always yields a logical result.

- `!logical` returns `logical`
- `!integer` and `!double` return `logical`, because R treats zero as false and every other number as true
- `!logical[]`, `!integer[]`, and `!double[]` return `logical[]`
- a map-like operand returns `logical[]`, because negation does not preserve map-likeness
- an `Any` or `Unknown` operand returns `Unknown`
- an operand whose type is still undetermined is constrained to `logical`, and the result is `logical`
- any other operand is a type error

### Range operator `:`

`from:to` builds a numeric sequence.

- both operands must be a scalar-like `integer` or `double`
- when both operands are `integer`, the result is `integer[]`
- a whole-number `double` literal operand such as `1` or `10` counts as `integer` here, matching R's runtime behavior for `:`
- otherwise, when either operand is `double`, the result is `double[]`
- an array-like operand is a type error, and so is a non-numeric operand
- an unannotated operand, as in `1:n`, is required to be a scalar `integer` or `double`. Passing a numeric vector through the enclosing function is therefore a type error at the call, because R's endpoint truncation warning marks a bug. The result is `double[]`, because the endpoint may instantiate at `double`

Examples:

- `1L:10L` returns `integer[]`
- `1:10` returns `integer[]`, even though the literals are `double`, because both are whole-number literals
- `1.5:3L` returns `double[]`
- `x:10L` returns `double[]` when `x` has type `double`

### Combine `c(...)`

`c(...)` builds an atomic vector from scalar-like, array-like, and map-like atomic arguments.

- with no arguments, `c()` returns `NULL`, which matches R
- `NULL` arguments are dropped, which matches R. `c(x, NULL)` is `c(x)`, and `c(NULL)` is `NULL`
- a union-typed argument participates member-wise. The `NULL` members are dropped first, because at runtime the value is either `NULL`, which `c` drops, or one of the other members. Every remaining member must be an atomic vector type, and it joins the coercion like a separate argument. An accumulator seeded with `NULL` therefore combines cleanly. With `acc` of type `double[] | NULL`, `c(acc, 1.0)` is `double[]`
- when any argument is list-shaped, `c` concatenates into a list rather than into an atomic vector. `c(list_a, list_b)` is the standard way to append to a list in R. The result is an array-like `list[T]` whose element type is the join of every argument's elements. An atomic argument contributes its own type, so `c(list(1L), "a")` is `list[integer | character]`. The atomic coercion rules below apply only when no argument is a list
- a non-concrete argument whose element type is not statically known is tolerated rather than rejected. `Any`, `Unknown`, and an unannotated parameter are such arguments, as in `function(x) c(x, 1L)`. The combined element atomic is then indeterminate, so the whole result is `Unknown`. That result is a strict-mode origin when the argument is an unresolved variable. This rule keeps `c` from reporting a false "expected `integer`, found `T`" on a generic wrapper, and from cascading on an already-`Unknown` value. Claiming a concrete element type would be unsound, because a later argument could widen the atomic
- mixed atomic arguments coerce to the widest type along R's coercion hierarchy, which is `logical < integer < double < complex < character`. `raw` does not participate, and it combines only with `raw`
- when every argument is named, the result is a map-like `T[named]`
- otherwise the result is an array-like `T[]`

Examples:

- `c(1L, 2L)` returns `integer[]`
- `c(1L, 2.5)` returns `double[]`
- `c(TRUE, 1L)` returns `integer[]`
- `c(1L, NA)` returns `integer[]`
- `c(1L, "a")` returns `character[]`
- `c(foo = 1L, bar = 2L)` returns `integer[named]`
- `c(list(1L), list(2L))` returns `list[integer]`
- `function(x) c(x, 1L)` infers as `<T> fn(x: T) -> Unknown`, because the unannotated `x` leaves the element atomic indeterminate

### Assignment operator `<-`

- `name <- expr` writes the type of `expr` into the variable slot of `name` in the current scope, and creates the slot on the first write. See `Value names` for the slot model
- a string where the name belongs binds that name. `"x" <- 1` is `x <- 1`, and `` `x` <- 1 `` is too. All three forms create the same slot, and a later `x` resolves to it. The finding range for a string target is the literal, quotes included, because that is what was written
- an assignment target that is not a name, such as a computed value or a number, is reported as a `syntax-error`. R parses them and refuses them at run time. See [diagnostic codes](/reference/diagnostic-codes#syntax) for the exact shapes and the one exemption
- when the assignment has an attached typing annotation, the assigned expression is checked using the annotation rules from this document
- the assignment expression itself has the type of the assigned expression
- a later assignment in the same scope writes the same variable. On a straight-line path the new write replaces the old type. Writes that merge from different control-flow paths join. See `Control-flow joins`

A function can call itself. Its own name is visible inside its body, because the target is bound to a fresh type variable before the body is inferred, and that variable then unifies with the inferred function type. `fact <- function(k) if (k <= 1L) 1L else k * fact(k - 1L)` therefore types as `fn(integer) -> integer`, and a call that violates the recursively inferred signature is an error. The recursive uses share one instantiation, so there is no polymorphic recursion.

Two local functions that call each other are beyond this, because it binds one name at a time. The forward reference resolves, because a capture sees later frame writes, but it stays `Unknown`-tolerant rather than precisely typed.

At the package top level, a self-recursive definition and a mutually recursive group both resolve through the interface fixed point. Every member starts at `Unknown` and re-derives each round until the schemes converge. Simple recursion converges to its precise type. A top-level `fact <- function(n) if (n <= 1L) 1L else n * fact(n - 1L)` exports `fn(n: integer) -> integer`, and the pair `is_even` and `is_odd` exports `<T: numeric> fn(n: T) -> logical`.

A heterogeneous self-reference whose type grows without bound cannot converge in a system without recursive types. A tree fold is one, because its parameter would need the recursive type `T = double | list[T]`. Such a group settles at `Unknown`. A cycle can also converge with `Unknown` embedded, and a pure self-call such as `f <- function() f()` settles at `fn() -> Unknown`. Either way the `Unknown` is gradual tolerance, so an unannotated consumer flows through it, and strict mode attributes it. See `What strict mode flags`. An explicit annotation on the binding closes the cycle exactly.

Examples:

- after `x <- 1L`, `x` has type `integer`
- after `x <- 1L; x <- "foo"`, a later use of `x` has type `character`
- after `x <- 1L; if (flag) x <- "foo"`, a later use of `x` has type `integer | character`
- `y <- (x <- 1L)` gives both `x` and `y` the type `integer`

### Boolean operators `&&` and `||`

- both operands are [scalar conditions](#conditions), so each is a `logical` or a numeric that coerces
- the result type is a scalar `logical`
- an array-like or map-like logical vector is not accepted

Examples:

- `TRUE && FALSE` returns `logical`
- `flag || other_flag` returns `logical`
- `c(TRUE, FALSE) && TRUE` is a type error
- `TRUE || c(FALSE, TRUE)` is a type error

## Calls

### Function calls

- a function call evaluates to the callee's return type
- when the callee expression is `Unknown`, the call evaluates to `Unknown`
- when the callee's return type is `Unknown`, the call evaluates to `Unknown`
- when the callee is a union whose members are all function types, the call must be valid against every member, because the value could be any of them. The call then evaluates to the union of the member return types. Each member is checked in an isolated probe, so no member's argument bindings leak into another's. Calling a function looked up in a list, as in `handlers[[name]](...)`, has this shape
- a function call also follows the named, positional, and optional parameter rules defined under `Function types`

Arguments are matched in R's two passes, and the order is observable.

1. Every argument given by name claims the parameter of that name, before any positional argument is placed.
2. The positional arguments then fill what is left. They fill the fixed positional parameters first, then the unclaimed named parameters declared before the rest parameter, in declaration order. When the function is not variadic, they fill all the unclaimed named parameters.
3. The rest parameter absorbs whatever remains, positional or named.

In `vapply(xs, character(1), FUN = f)` the named `FUN` is therefore claimed first, and `character(1)` reaches `FUN.VALUE`, exactly as R matches it. A positional argument never collides with a parameter that some later named argument has already claimed.

A function call is a type error when:

- a required argument is missing
- too many arguments are provided and the callee has no rest parameter
- an argument value is incompatible with the corresponding parameter type

A call argument that is the enclosing function's bare `...` forwards an unknown number of arguments, possibly zero. A wrapper that passes its own `...` straight through, such as `function(x, ...) helper(x, ...)`, does this. Such a call skips both arity checks, because neither missing-required nor too-many-arguments can be decided statically, and the `...` argument itself matches no parameter. The call's concrete arguments are still checked against their parameters as usual.

#### The native pipe

R's parser rewrites `x |> f(y)` into `f(x, y)` before it evaluates the code. The pipe types as that call and nothing else. The piped value becomes the first positional argument. All call rules above apply to it: arity, argument compatibility, and overload selection. Chains compose from left to right. A type error on the piped value blames the left-hand expression.

The `_` placeholder follows R's rule. It is legal only as the whole value of exactly one named argument. That argument then receives the piped value instead of the first positional slot, so `x |> lm(y ~ z, data = _)` is `lm(y ~ z, data = x)`. `2 |> f(tag = _)` supplies only `tag`, so any other required parameter is missing.

A pipe R itself would reject is not guessed at: a right-hand side that is not a call, a positional or repeated `_`, and a `_` nested inside a subexpression. Such a pipe stays an opaque operator, so it types as a silent `Unknown` and its reads stay quiet.

Optionality comes from the formals, not from the annotation. A formal with a default is optional in R, and no annotation can change that. The exported signature therefore takes each parameter's optionality from the function, and an annotation that disagrees is reported once at the definition. The disagreement is never reported as a missing argument at the call sites, because those call sites are correct. Both directions report: a required declaration over a defaulted formal, and an `[optional]` declaration over a formal with no default.

Argument checking is compatibility-based, not exact-equality-based.

- The ordinary coercions apply at parameter positions, including scalar-like `T` into array-like `T[]`, and `T` or `NULL` into `T | NULL`.
- `logical` is accepted where `integer` is expected, `integer` where `double` is expected, and `double` where `complex` is expected, for scalar-like, array-like, and map-like alike. `mean(1L)`, `sd(c(1L, 2L))`, and `sum(x > threshold)` are therefore not errors. The reverse is never accepted, and unification does not widen. `character` and `raw` are never accepted this way. R reaches `character` only through an explicit coercion, and accepting it implicitly would leave argument-order mistakes unreported.
- A whole-number `double` literal counts as `integer` at a parameter position, so `seq_len(10)` and `substr(x, 1, 3)` are as valid as their `10L`, `1L`, and `3L` spellings. This generalizes the rule the `:` operator already applies to its endpoints. A fractional literal such as `2.5` is still rejected at an `integer` parameter, and so is a `double`-typed variable that holds a whole number.
- An argument whose type is `Unknown` is accepted at any parameter. The reason the value became `Unknown` was already diagnosed where it happened, and repeating it at every later use would add nothing.

A rest parameter follows R's rule for formals around the dots.

- it adds no required arguments, so a variadic function may be called with none, and `paste()` is legal
- positional arguments fill the parameters declared before it first, so `wrap("a", "b")` on `fn(x: character, ...: character)` gives `x = "a"` and sends `"b"` to the rest
- it then absorbs any number of remaining positional arguments, each checked against its element type
- a parameter declared after it is matched by name only, so `sum(1, 2, na.rm = TRUE)` on `fn(...: integer[] | logical[], [na.rm]: logical)` sends `1` and `2` to the rest and `na.rm` by name
- it also absorbs a named argument that matches no declared parameter, which is how a wrapper passes an option through, as in `read.csv(file, colClasses = "character")`
- a named argument that duplicates a parameter already given is still an error, because R rejects a formal matched twice

### Overload sets

A standard-library stub name may declare several signatures, which form an ordered overload set. The [stdlib stubs page](/type-checking/stubs) describes the declaration surface. A call to such a name resolves per call site.

- candidates are tried in declaration order, and the call commits the first candidate whose parameters accept the arguments. That candidate's return type is the call's type. `sum(1L, 2L)` is therefore `integer`, and `sum(1.5, 2.5)` is `double`
- each failed candidate is probed in isolation. Nothing a failed candidate bound leaks into the next candidate or into the committed result
- When an argument's type is still an undetermined inference variable, a candidate may fit only because unification narrowed that variable.  A candidate accepted only because of that narrowing is not established by the call itself. Every candidate is still tried. A candidate that fits while leaving the caller's undetermined types exactly as they were beats one that does not, whatever their declaration order. Among fits of the same kind, the first declared candidate wins. A wrapper such as `function(x) sum(x)` keeps its parameter unconstrained this way. A candidate whose parameter is `Any` accepts without binding anything, so the general fallback is selected ahead of the narrower candidates declared above it. A single fitting candidate is established by the call, because it is the only signature that accepts it. It is selected and its narrowing stands, so `f(function(v) v, 1L)` selects the candidate whose second parameter is `integer`, even though the lambda's parameter type was open
- The [whole-number literal rule](#function-calls) does not affect which candidate is selected. Candidates are first tried against the arguments' true types, so `sum(1, 2)` selects the `double` candidate, matching what R computes. Only if no candidate accepts them is the set retried with the literal-as-integer allowance. A name whose only fitting candidate wants `integer` therefore still accepts `foo(1)`
- When no candidate accepts the arguments, the call is a type error. The error names the overloaded callee and how many signatures were tried, and it gives the first candidate's failure as the concrete hint. That is the form when the candidates disagree about what is wrong, because then no single candidate's complaint is the answer. One candidate's own finding is reported instead, at that candidate's own argument range, in two cases. The first is when every candidate rejects the call for the identical reason. The second is when one candidate got strictly further into the argument list than every other, which makes it the signature the call meant
- Passing an overloaded name as a value, or hovering over it, sees the last declaration. By corpus convention the last declaration is the most general one, so a value-use never carries a narrower contract than the calls it might make. Go-to-definition on the name points at the first declaration, where the set begins

Only a declaration file can overload a name. A name with several signatures has no single most general type, so a call must be resolved by search rather than inferred, which gives up the principal-type guarantee.

A `#:` annotation on your own function declares exactly one signature. To make one name accept several shapes, give the parameter a [union type](#union-types), or split the shapes into separate functions.

A local or package binding that shadows a stub name disables its overload set. The binding is used everywhere, calls included. A project [override stub](/type-checking/stubs#overriding-a-shipped-declaration) may declare sets, because a `.Rtypes` file is a declaration file for foreign code wherever it lives.

## Control flow

### `if` expressions

#### `if` without `else`

- requires a [scalar condition](#conditions)
- infers the branch body as type `T`
- produces the result type `T | NULL`, because the missing branch contributes `NULL` to the join
- applies union normalization. A `NULL` body stays `NULL`, an already-nullable body stays a single `T | NULL`, and an `Unknown` body stays `Unknown`

Examples:

- `if (flag) 1L` infers as `integer | NULL`
- `if (flag) { }` infers as `NULL`

#### `if ... else`

`if ... else` requires a [scalar condition](#conditions). It joins the two branch types into the result type, by seven rules.

- Branches that unify share that type. `if (flag) 1L else 2L` is `integer`, and `if (cond) a else b` over two unconstrained values keeps them unified as one polymorphic type.
- A `NULL` branch joins by union without constraining the other branch. One branch `T` and one branch `NULL` produce `T | NULL`.
- Branches with genuinely different types produce their union. `if (flag) 1L else "foo"` is `integer | character`. Different branch types are not a type error.
- The other branch never pins a branch whose type is still an unconstrained inference variable. `function(flag, x) if (flag) x else "s"` is `<T> fn(flag: logical, x: T) -> T | character`, not `fn(flag: logical, x: character)`. Unifying there would make the caller wrong for a line that is not wrong. The guard rule requires the same thing, because `if (is.character(x)) x else "other"` exists precisely for the case where the caller may pass something else.
- A branch whose variable the body has already constrained may unify with the other branch. That pin adds nothing the program did not already require, so `function(n) if (n <= 1L) 1L else n * fact(n - 1L)` converges to `fn(n: integer) -> integer`.
- Two branches that are both still open tie to each other, because neither pins the other. `function(value, fallback) if (is.null(value)) fallback else value` is `<T> fn(value: T | NULL, fallback: T) -> T`.
- An `Unknown` branch makes the whole conditional `Unknown`, rather than claiming the other branch's type.

The join does not merge or coerce branch types beyond unification. It only records the alternatives.

Examples:

- `if (flag) 1L else 2L` infers as `integer`
- `if (flag) 1L else NULL` infers as `integer | NULL`
- `if (flag) NULL else 2L` infers as `integer | NULL`
- `if (flag) 1L else "foo"` infers as `integer | character`
- `if (flag) { } else { }` infers as `NULL`
- `if (c(TRUE, FALSE)) 1L else 2L` is invalid, because a condition must be scalar rather than a vector

#### Diverging branches

A branch diverges when it never falls through to the code after the `if`. A diverging branch is `return(...)`, `stop(...)`, `break`, or `next`. A block ending in one of those diverges, and so does an `if ... else` whose branches both diverge.

A diverging branch contributes neither its value nor its variable-slot state.

- `x <- if (c) return(NULL) else 5` gives `x` the type `double`, not `NULL | double`
- a variable write inside a diverging branch does not join into the state after the `if`. Only the surviving branch's state flows on

`stop(...)` is recognized by its bare name, as `local` and `return` are. Rebinding `stop` is not modelled.

### Guard narrowing

A condition that applies a type-guard predicate to a plain local variable refines that variable's type along the `if` edges. The variable keeps the refined type inside each branch until a branch write replaces it. The refinements merge back at the join exactly like branch writes.

These are the recognized guards, where `x` is a local variable. A parameter counts as a local variable.

| condition | true edge | false edge |
|---|---|---|
| `is.null(x)` | `x : NULL` | the `NULL` member is removed from the union of `x` |
| `is.character(x)` | union members that are not `character`-family are removed | `character`-family members are removed |
| `is.logical(x)`, `is.integer(x)`, `is.double(x)`, `is.function(x)`, `is.list(x)` | as above, for that family | as above |
| `is.numeric(x)` | as above, where the family is `integer` or `double` | as above |
| `!cond` | the two edges swap | |

Ten rules and limits apply.

- A family membership test covers the scalar and the vector of the atomic type. `is.character` is true for `character` and for `character[]`. `is.list` covers every list shape, which is `list[T]`, `list[named: T]`, and the fixed-shape lists. `is.function` covers function types.
- Narrowing filters union members. A member whose family cannot be decided statically, such as an unannotated value or an opaque nominal, is kept on both edges.
- `is.null(x)` on an `Any` or `Unknown` variable refines the true edge to `NULL`, because the runtime guarantees it. A family guard does not refine `Any` or `Unknown`. Inventing a concrete shape there would report calls to standard-library functions that declare a scalar result for a value of any length.
- `is.null(x)` on a completely unconstrained inference variable shapes it. Such a variable is an unannotated parameter that nothing has used yet. The test asserts that `NULL` is a possible inhabitant, so the variable becomes `T | NULL` for a fresh `T`. The edges then narrow as an ordinary union. The true edge keeps `NULL` and the undecidable `T`, and the false edge is `T`. A function that returns a fallback when its argument is `NULL` therefore types without an annotation. `function(value, fallback) if (is.null(value)) fallback else value` generalizes to `<T> fn(value: T | NULL, fallback: T) -> T`, which is what its annotated form would declare. There are two consequences. Testing a parameter for `NULL` and then using it unguarded is a genuine finding, because the test itself declared `NULL` possible. The shaping never fires on a variable that already carries a constraint, because a numeric-constrained variable cannot hold `NULL`, and it never fires on a declared rigid type parameter, because an annotation's contract is not reshaped.
- When a guard cannot fire, such as `is.null(x)` on a union with no `NULL` member, no refinement happens. The checker does not type dead branches specially.
- Combined with a [diverging branch](#diverging-branches), the surviving edge's refinement persists after the `if`. This is the idiomatic early-exit guard:

  ```r
  #: fn(x: integer | NULL) -> integer
  f <- function(x) {
    if (is.null(x)) {
      return(0L)
    }
    x + 1L   # x : integer here
  }
  ```

- Only a read of a variable narrows, which covers parameters, function locals, and a top-level variable an earlier statement assigned. An arbitrary expression does not narrow, so `is.null(f(x))` and `is.null(x$field)` do not.
- A refinement does not outlive the statement it was made in. Checking runs one top-level statement at a time, so a guard narrows within its own `if`. That covers the whole `if` and `else`, and everything nested in it. A following top-level statement reads the variable's unrefined type again. This is what separates the two ways of writing a guard that exits early. Inside a function body, or inside one braced top-level block, `if (is.null(x)) stop(...)` narrows `x` for the rest of the body, because the guard and the later reads are one statement. Written as bare top-level statements, the `stop()` guard and the read that follows it are two statements, and the read is not narrowed.
- A read from inside a closure narrows when the guard is in the same statement, which matches how a local behaves. The guard's own subject must not itself be a deferred read. That body runs later, so the test proves nothing about the value it will see then.
- A condition combined with `&&` or `||` does not narrow.
- `is.na(x)` is not a type guard. In this system, being `NA` is a value property rather than a type property.
- Narrowing never touches an unresolved inference variable, so a guard does not pin an unannotated parameter.

#### `missing()` on a defaultless formal

A formal the body tests with `missing(name)` is optional at call sites, which is how R writes an optional argument with no default. `function(name, punct) if (missing(punct)) … else …punct…` may be called without `punct`.

The test also narrows the formal's supplied state along the branch edges, like a type guard.

- on the true edge, reading `name` is an error, because R fails that read with "argument is missing, with no default". Writing it is legal and supplies it, as in `if (missing(punct)) punct <- "!"`
- on the false edge the formal is supplied, and reads are ordinary
- a diverging true edge, such as `if (missing(x)) stop(...)`, leaves the rest of the body on the supplied edge, and `!missing(name)` swaps the edges
- after the branches rejoin, the formal counts as unsupplied only when it is unsupplied on both edges, so only definite runtime failures are reported
- a formal with a default is never narrowed, because reading it while unsupplied evaluates the default
- `missing()` applies only to the immediate function's own formals, as in R, so an enclosing function's formal is not narrowed inside a nested function

### Blocks

- a block evaluates to the type of its last expression
- a block with no contents evaluates to `NULL`
- a block whose last expression is terminated with `;` evaluates to `NULL`
- a block whose last expression has type `Unknown` evaluates to `Unknown`

### `return`

`return(x)` exits the enclosing function with `x`, and `return()` exits with `NULL`. It is a control-flow construct rather than a call. The syntactic call to the bare name `return` is recognized during lowering, as `local` is.

- a function's return type is the union of the type of every `return` value in its body with the body's trailing value. `function() { if (c) return("foo"); 5 }` is therefore `fn() -> character | double`
- the `return` expression itself yields no observable value where it stands. It therefore types as `NULL` locally and is not a strict origin, which is how `break` and `next` behave
- the returned value expression is checked like any other expression, and its errors surface normally
- a `return` inside a loop exits the whole function, so for control-flow purposes it abandons the loop iteration in the way `break` does

- a top-level `return` is an R runtime error. Its value is still checked, and it joins no function's return type

### `switch`

`switch(subject, a = ..., b = ..., default)` selects one branch by the subject's runtime value. The selection cannot be modelled statically, and the call is checked fully.

- the subject and every branch are type checked. An error inside any branch surfaces as it does anywhere else
- the call's type is the union of the branch value types. `NULL` joins the union unless a default branch exists, because an unmatched `switch` returns invisible `NULL`. A default branch is unnamed and is not the first branch
- a named branch with no value falls through to the next branch in R, and it contributes no type of its own
- the branches are alternatives rather than a sequence. Exactly one branch runs, so assignments inside them fork and join exactly as the arms of an [`if`](#if-expressions) do. A name written in several branches holds the join of what they write, and no branch's write shadows another's. A branch that cannot fall through, such as one ending in `stop()`, contributes no state to what follows. A name introduced only inside the branches is not defined on the no-match path, which is what R reports as `object 'r' not found`
- recognition is syntactic on the bare callee name, as it is for the quoting and masking families. A local binding named `switch` makes the call an ordinary one again

### `for`

`for` has the form `for (name in value) body`.

It requires an iterable iteration source.

- a scalar-like, array-like, or map-like vector iterates with the scalar element type
- an array-like `list[T]` and a map-like `list[named: T]` iterate with element type `T`
- a tuple-like list and a record-like list iterate with the union of their item types, which collapses to the single item type for a homogeneous list. A heterogeneous fixed-shape list is therefore iterable, and `for (item in list(a = 1L, b = "two")) ...` binds `item` as `integer | character`
- the empty list `list()` is iterable with element type `NULL`, which is the union of zero item types
- a union of iterables iterates member-wise, so `integer[] | character[]` binds the loop variable as `integer | character`
- `NULL` is iterable and runs zero iterations, which is legal R. It binds the loop variable as `NULL`
- `Any` iterates with `Any` items. `Unknown` iterates with `Unknown` items, so an already-failed source does not produce a second error on the loop
- an opaque nominal value iterates with `Any` items, because its element shape is not visible to the checker
- iteration does not constrain a still-unresolved inference variable, such as an unannotated parameter. R iterates vectors and lists, and neither shape may be committed for the caller, so the loop variable degrades to `Unknown`
- any other source, such as a function, is an error reported on the source expression

Four more rules apply to `for`.

- the iteration source is evaluated once, before any iteration
- `for` does not itself change the type of the iterated value outside the loop
- inside the loop body, the bound name has the iterated element type. It is re-initialized from the iterable on every iteration, so an assignment to it inside the body does not survive into the next iteration's start
- the loop variable is not visible after the loop

### `while`

- requires a [scalar condition](#conditions)
- re-evaluates the condition before every iteration, so reads in the condition also see the loop's joined state
- evaluates as a whole expression to `NULL`

### `repeat`

- has no condition
- runs its body at least once, so a variable written in the body is definitely assigned after the loop
- evaluates to `NULL`

## Naming and scoping

### Project file order

Project file order follows normal R package collation order.

- if `DESCRIPTION` provides `Collate`, that order applies
- otherwise the package source files order by the default `C`-locale collation

When this document refers to an earlier or a later file, it means earlier or later in that project file order.

### Value names

A top-level value name is package-global across files.

- another file may reference a top-level binding
- when several files define the same name, the later file wins, and both definitions should warn
- a bare top-level `{ }` block executes unconditionally, so its direct-child assignments are package globals too
- an assignment inside an `if`, `for`, or `while` body executes conditionally, so it is not a package global, and a cross-file reference to it is unresolved

A cross-file reference sees the binding's generalized exported type. Type information does not flow back into the exporting file, so a call in one file never changes the inferred type of a function defined in another. Within one file a top-level name also resolves to its final exported type, so a use placed before the definition still sees it.

Inside executable code, naming is lexical over mutable variable slots, matching R's environment semantics. A scope holds one variable per name, and assignment mutates it.

- a function body, a `local(expr)` call, and a script's top level each form one scope, called a frame
- a parameter introduces a slot in the function's frame, and assigning to the parameter name writes that slot
- the first `<-` or `=` assignment to a name in a frame creates its slot. Every later assignment writes the same slot rather than creating a shadowing binding
- an assignment inside a branch or a loop body writes the enclosing frame's slot, because braces and control flow do not introduce scopes
- a slot shadows an outer or package-global binding of the same name. A slot that no write reaches at a read does not shadow, and the read resolves outward as R's runtime lookup would
- `for` introduces a loop-local slot, re-initialized from the iterable on every iteration
- `local(expr)` evaluates `expr` in a fresh child scope and takes its type. Assignments inside are local, while references still see enclosing names

Four call forms evaluate an argument non-standardly, and each is recognized by its bare name. Rebinding the name to a user function does not change that.

- `library(pkg)`, `require(pkg)`, and `help(topic)` read a bare first argument as a package or topic name, so `library(stats)` means `library("stats")`. The name is never resolved as a variable and never warned about. A string argument, a named first argument, or a qualified callee is an ordinary call.
- `quote(expr)`, `substitute(expr)`, `bquote(expr)`, and `expression(expr)` build an expression instead of running it, so an assignment written inside one binds nothing. `quote(x <- 1)` leaves `x` undefined. Nothing inside is checked for arity or argument types, and a name it mentions need not exist. The names it mentions still count as reads, because `eval` may run the expression later.

At a package document's top level, a conditionally executed assignment is not package-visible, but within the same document it behaves like a slot: a later top-level read resolves to it, and reports the maybe-undefined warning when an unassigned path also reaches. A cross-item read sees the join of every conditional writer's type, so `for (i in 1:3) total <- i` followed by `report <- function() total` types `report` as `fn() -> integer`. A name with many conditional writers has no useful joined type, so past eight writers the slot is `Unknown`.

### Name references

- a name reference evaluates to the type currently bound to that name
- when the name is not bound, the checker reports an unknown-name diagnostic
- after an unknown-name diagnostic, the reference expression is treated as `Unknown`, so that checking continues without cascading secondary type errors

### Namespace access

`pkg::name` and `pkg:::name` read one name directly from a package namespace, bypassing lexical scoping.

- A namespace is known when stubs declare it. The shipped standard-library packages are known, and so is any namespace a project stub file declares. `stubs/dplyr.Rtypes` declares the namespace `dplyr`. See [Standard library stubs](/type-checking/stubs).
- The project's own package is always known, whatever the stubs say. Qualifying a name with the package you are editing reads the definition the checker already holds, so the read has that definition's type rather than `Unknown`. `withr::defer()` inside the package `withr` is such a read. This case wins over a stub namespace of the same name, in the way a package binding shadows a stub name. The name itself is not validated. A package exports names its sources never bind, such as a re-export, an S4 generic from `setGeneric`, a dataset under `data/`, and a binding installed by `.onLoad`. A name the definitions do not cover is therefore left alone rather than reported.
- When the stubs declare `name` in `pkg`, the qualified read has the stub's type, exactly like the bare name. A name that only the namespace's [export manifest](#standard-library-exports) lists validates the same way, and it types `Unknown`.
- An unknown namespace warns. A known namespace that neither declares nor manifest-lists the name warns that the name is not exported. The same mistake in a `NAMESPACE` `importFrom` is an error rather than a warning, because a bad import stops the package from loading at all, while a bad qualified read fails only if that line runs.
- Exports are declaration-level. A project stub that overrides a shipped name's type does not remove the name from its shipped namespace, so `stats::sd` stays valid under an `sd` override.
- An unvalidated qualified read types as `Unknown`, and that reference is a strict origin.
- `::` and `:::` are not distinguished. The split between exported and internal names is not modelled.

### Package imports (`NAMESPACE` and `DESCRIPTION`)

Hosts read the package's `NAMESPACE` and `DESCRIPTION` files at the package root. The facts in those files extend the resolution universe package-wide. Analysis without them behaves as if both were empty. This covers a single file and a project with neither file.

- `importFrom(pkg, name)` makes `name` a known bare read, and it is never reported unresolved. The read types as the stub corpus's declaration for the name when one exists, and as `Unknown` otherwise. An import typo is validated once at the import site. An `importFrom` naming something a stub-described namespace does not export is an error there, because R refuses to load such a package. That error never appears at a use site.
- `import(pkg)` of a namespace the stub corpus describes makes exactly `pkg`'s exports known bare reads. When no stubs describe `pkg`, its export set is unknowable. Every otherwise-unresolved bare read in the package is then tolerated rather than guessed at, so an unknown package never produces a false report. Unresolved-name detection for such a package resumes once stubs for `pkg` exist.
- A `library(pkg)` or `require(pkg)` call anywhere in the project is the script world's equivalent of `import(pkg)`, and follows the same rule. It attaches every export of `pkg` to the search path, so when nothing describes `pkg` its export set is unknowable and every otherwise-unresolved bare read is tolerated. The tolerance is project-wide, because R's search path is project-wide. The tolerance lifts as soon as the package's exports are known, which they now are for a long list of common packages. An [export manifest](/contributing/authoring-stubs#export-manifests) is enough, and types are not required. The tidyverse, `knitr`, `rlang`, `glue`, `jsonlite`, `R6`, and the rest of the shipped manifest set therefore keep unresolved-name detection on. A package nothing describes still switches it off, and a two-line `stubs/<pkg>.Rtypes` of your own turns it back on.
- Attaching a meta-package activates the packages it attaches rather than the names it exports. `library(tidyverse)` makes `mutate`, `read_csv`, and `str_to_upper` reachable because it attaches `dplyr`, `readr`, and `stringr`. Those namespaces activate with it, including their types where they have them.
- Attaching or importing the project's own package does not switch the tolerance on, even though no stubs describe it. The project's own package is the name in the `Package` field of `DESCRIPTION`. Its export set is not unknowable, because those exports are the project's own definitions, which the checker already sees. Without this rule, the `library(yourpkg)` that `usethis` writes into `tests/testthat.R` would switch off unresolved-name detection for the whole package.
- A `pkg::name` read of a namespace the stub corpus does not know warns about an unknown namespace. It does not warn when `pkg` is part of the package's declared universe. The declared universe is a `DESCRIPTION` dependency field (`Depends`, `Imports`, `Suggests`, or `Enhances`) and the source namespace of any `NAMESPACE` import. A namespace that is declared but not described stays quiet, and its reads type `Unknown`.

### Standard-library exports

The shipped stub corpus pairs each namespace with a vendored export manifest. A manifest is the complete list of names the namespace really exports, generated from a live R session. The [stdlib stubs page](/contributing/authoring-stubs#export-manifests) describes how. Every manifest name is a known global.

- a bare read of a manifest name always resolves, and never produces an unresolved-name warning. It types as the stub corpus's declaration when one exists, and as `Unknown` otherwise
- a qualified `pkg::name` read of a manifest name validates the same way, with no not-exported warning, and carries the same type
- typo suggestions on genuinely unresolved names draw on the manifest names as well as on the typed declarations

Manifests follow how R itself exposes each namespace.

- The default-attached packages are bare-visible in every session, so their manifest names resolve bare and qualified without any condition. These are `base`, `stats`, `utils`, `graphics`, `grDevices`, `methods`, and `datasets`.
- The R-shipped but unattached packages are reachable through `::` in every session, so their manifests always validate a qualified read. A bare read resolves only once the project attaches the package with a `library()`-family call or declares it a dependency, exactly as in R. These are `tools`, `parallel`, `compiler`, `grid`, `splines`, `stats4`, and `tcltk`.
- A conditional namespace's manifest activates together with its stubs. See [Conditional stub namespaces](#conditional-stub-namespaces-datatable-dplyr-ggplot2-and-testthat). While the namespace is inactive, its manifest names stay unknown, bare and qualified alike.
- A read satisfied only by an import still counts as a use for liveness. Strict mode attributes its `Unknown` exactly like any other undetermined reference.

### Replacement-form assignment

A replacement-form assignment reads the base variable, applies the write, and writes the result back to the base's slot. The slot's type therefore reflects the update. The replacement forms are `x$field <- v`, `x[["name"]] <- v`, `x[[key]] <- v`, and `x@slot <- v`.

- A known-field write on a record-like `x` sets that field's type to the type of `v`, and adds the field if it is absent. A later `x$field` reads the updated type. The known-field writes are `x$field <- v` and `x[["literal"]] <- v`. The same write on an empty `list()` starts a record-like `list{field: V}`.
- The same write on a nominal `x` is checked rather than applied. A nominal type's representation is fixed, which makes `@type` an invariant rather than a label. The write must therefore satisfy the representation. `v` is checked against the field's declared type, a field the representation does not declare is an error, and `x` keeps its nominal type either way. This is the one write that reports rather than retypes. The alternative is a value that still claims a nominal type while no longer matching its representation.
- A computed-key write cannot name a field statically, so it refines the container's element type rather than a specific field. A computed-key write is `x[[key]] <- v` with a key that is not a literal.
  - an empty `list()` becomes a map-like `list[named: V]`
  - a map-like `list[named: T]` becomes `list[named: T | V]`
  - an array-like `list[T]` becomes `list[T | V]`, and stays array-like, because its reads are not nullable
  - a record-like container and a fixed-shape tuple-like container are left unchanged. A dynamic write does not statically alter a shape whose fields are individually known, and widening such a shape would lose precision the code has not given up
- The accessor spine and the index or key expressions are ordinary reads, so their own errors surface. A replacement whose accessor spine has no variable at its root, such as `f(x)$a <- v`, is refused as an unsupported construct. It types `Unknown` and is a strict-mode origin.

A map-like name read is `T | NULL`, because the key may be absent. See [`[[` on lists](#-on-lists). Building a map with computed-key writes and then reading a key back therefore yields `V | NULL`. Guard the read with `is.null` before a use that needs `V`.

### Control-flow joins

A read of a variable sees every write that can reach it, so control flow joins the states the variable can be in.

- after `if` without `else`, a variable written in the branch joins its pre-`if` type with the branch's written type
- after `if ... else`, a variable joins the two branch outcomes. A branch that does not write contributes the pre-`if` state
- a loop body may run zero or more times, so reads inside the body and after the loop join the pre-loop state with the state flowing around the back edge
- `repeat` runs at least once, so after the loop the variable has the body's resulting state
- joining equal types keeps the type, joining different types produces their union, and joining with `Unknown` produces `Unknown`

A variable with exactly one reaching write keeps that write's generalized type, so `f <- function(x) x` inside a body stays `<T> fn(x: T) -> T`. When writes merge at a join the variable holds a single type instead, so a conditional reassignment loses the polymorphism. Two conditionally assigned functions read as a union of both signatures rather than being unified into one.

Definite assignment follows four rules.

- A read that some path reaches with no prior write reports [`maybe-undefined`](/reference/diagnostic-codes), because R raises `object 'x' not found` on that path. The finding is off by default, and `[check] maybe-undefined = true` turns it on. Two conditions that always agree at run time are still two branches, so `if (ok) v <- …` followed by `if (ok) use(v)` reports although it is safe.
- A `repeat` is left through its `break` points, so one that always assigns before breaking reports nothing. A branch that cannot fall through, such as one ending in `stop()`, contributes no path.
- A read that no write can reach does not resolve to the variable at all. See the shadowing rule above.
- A top-level variable is different, because an unwritten path reaches the enclosing environment. The read then sees the name's binding elsewhere in the script or package, and that type joins into the slot like any other reaching write. After `p <- "word"`, the body of `while (cond) p <- p - 1L` is a type error on the first iteration's `character` read. A name with no such binding stays `Unknown`.

An item whose check reports an error exports `Unknown`, so one mistake does not cascade across a file. An item carrying a `#:` annotation is the exception: the annotation is what the author says the binding is, so a function whose body violates it still checks every call site against the declared signature.

Examples:

- `f <- function(flag) { x <- 1L; if (flag) { x <- 2L }; x }` is clean, and `x` reads as `integer`
- `f <- function(flag) { x <- 1L; if (flag) x <- "two"; x + 1L }` is a type error, because `x` reads as `integer | character` and `+` rejects the `character` member

#### Unused assignments

With the `unused` check enabled, an assignment whose value no read can observe on any path reports `unused` on the assigned name. Package-visible top-level assignments, parameters, `for` variables, and `.`-prefixed and `_`-prefixed names are never reported.

A read inside a nested function is a capture. The closure runs after its frame has finished, so every write of the captured name stays observable and none is a dead store. This holds only for the frame the read resolves to: a same-named binding in an enclosing frame is shadowed, not read, and it still warns.

- `f <- function() { x <- 1L; x <- 2L; y <- x; y }` warns that the first write to `x` is unused
- `f <- function() { x <- 1L; g <- function() x; x <- 2L; g }` is clean, because both writes stay alive through the capture
- `f <- function() { x <- "outer"; g <- function() { x <- TRUE; function() x } }` warns, because the innermost function reads the `x` of `g`

`on.exit(expr)` reads the same way. R runs the expression when the function returns, so it observes the last value of every name it mentions. The standard rollback guard is therefore clean:

```r
with_transaction <- function(con, body) {
  committed <- FALSE
  on.exit(if (!committed) dbRollback(con))
  body(con)
  committed <- TRUE          # read by the exit handler, not a dead store
  invisible(TRUE)
}
```

A file that calls `R6Class` resolves `self`, `private`, and `super` inside it. R6 builds those bindings at construction, so they resolve nowhere lexically, and a read of one is not an unresolved name. The recognition is syntactic, so a local binding that shadows `R6Class` is not honored. It is also file-scoped, so a file that defines no R6 class still warns about `self`. Their type is `Unknown`, because R6 field and method types are not described.

A top-level `globalVariables(c("a", "b"))` or `utils::globalVariables(...)` call with literal string arguments declares those names as dynamically bound for the whole package, so could-not-resolve is suppressed for them everywhere. An undeclared name keeps warning.

### Type names

Top-level `@type` and `@alias` declarations share one project-global namespace.

- a type reference may resolve to a declaration in the same file or in another file
- forward references are allowed
- a duplicate type name is an error regardless of declaration kind. `@type` twice, `@alias` twice, and one of each all conflict
- every declaration that participates in a duplicate-name conflict is erroneous
- A duplicate is judged against the namespace the declaration lives in. Package files share the project-global namespace, so two package files that declare one name conflict. A script's declarations belong to its own file only. A name declared in one script is invisible to the next, so two scripts may each declare `Thing` without conflict, while declaring `Thing` twice inside one script is the duplicate. Without this rule the later declaration would silently win, and every diagnostic it produced would be unfalsifiable from the visible source
- type parameters are local binders, and they shadow project-global type names
- a type reference that resolves to nothing is an error at the referencing token, with a nearest-name hint when a close match exists. A reference resolves to a built-in type, an in-scope binder, a project `@type` or `@alias` declaration, or a stub-declared class. The undeclared name then compares like `Unknown` everywhere, so the typo is reported exactly once and never cascades into value-level mismatches

All current `@type` and `@alias` declarations are top-level and project-global.

### Non-package documents

A file that is not a package source file, such as a script under `scripts/`, contributes to neither the package-global value namespace nor the project-global type namespace.

A script executes top-down, so its top level is one sequential lexical scope, like a function body.

- a top-level binding is visible only after its assignment
- rebinding a name changes later uses, exactly like local rebinding
- a use before any script-local or package-global definition is an unresolved name. This includes a read inside the very statement that first binds the name, such as `x <- x + 1L` with no earlier `x`, which errors at runtime
- a read from inside a nested function is deferred. The closure runs after the frame has settled, so it resolves against the whole document and the last top-level binding of the name wins. This includes the enclosing statement's own binding, so self-recursion resolves and a self-recursive closure types through the cycle fixpoint
- a conditional top-level write creates the document's variable slot exactly as in package files, and later reads in the same document resolve to it. The slot exports no scheme yet, so such reads type `Unknown`. A conditional top-level write is one inside a top-level `if`, `for`, `while`, or `repeat`
- A masked read and a read inside an opaque operator are never reported unresolved, and each still counts as a use. It keeps the binding it would fall back to alive for the unused check, and navigation connects it, which covers goto and references. A masked read comes from `with` or from data.table indexing. An opaque operator is `&`, a user `%op%`, or a pipe R would reject. A well-formed `|>` is not opaque, because it types as the call it desugars to

Scripts are typechecked like package files. A script checks against package-global value schemes and project-global types, plus its own script-local bindings and type declarations.

- a non-package document may resolve package-global value names from package files
- a non-package document may resolve project-global `@type` and `@alias` names from package files
- a top-level value binding in a non-package document is not visible to package files or to other non-package documents through package-global naming
- a top-level `@type` or `@alias` declaration in a non-package document is not visible to package files or to other non-package documents through the project-global type namespace
- a package file and a non-package document may reuse the same top-level value name or type name without a package-global name conflict
- duplicate top-level value names inside a non-package document do not produce the package-global duplicate-binding warning. They behave like ordinary script-local rebinding. R scripts commonly rely on the global namespace, so warning on top-level rebinding in a non-package document would add noise outside package-visible naming

### Type namespace scope

The type namespace is project-global. A type declared in one package file is nameable from every other package file, and there is no file-local type.

## Data frames and non-standard evaluation

R evaluates some argument positions inside a data frame's own environment. A bare name there is a column reference that no lexical scope can see. These positions are recognized structurally, and a read there that resolves to no binding is treated as a column reference. That means a silent `Unknown`, no could-not-resolve warning, and no strict origin.

These are the recognized masks.

- A single `[` bracket whose subject types as the `data.table` nominal masks all of its index arguments, whatever they look like. With the subject's class known, `DT[speed > 20]` and `DT[, x]` are column references even though they carry no syntactic marker.
- A `[` call carrying an unambiguous data.table signature masks all of that bracket's index arguments, even when the subject's type is unknown. Such a signature is a `by =` or `keyby =` argument, a `:=` column assignment, a `.()` list call, or one of the `.SD`, `.N`, `.I`, `.BY`, `.GRP`, and `.EACHI` specials.
- The base masking family masks every argument other than the data. That family is `with()`, `within()`, `subset()`, and `transform()`. A locally defined function of the same name masks nothing. Which argument is the data follows R's own matcher. A named argument claims its formal first, which is `data` for the `with` pair and `x` for `subset` and `transform`. The remaining positional arguments fill what is left, so `with(data = frame, speed > 20)` and `with(speed > 20, data = frame)` both mask the condition. The `base::` spelling of any of the four masks exactly as the bare one does. Another package's same-named export is its own function, and it masks nothing.

A name inside a mask that does resolve, such as a local variable used in `j` or a function like `sum`, keeps its ordinary resolution and typing. data.table itself falls back to the lexical scope for names that are not columns. Base-R indexing such as `m[i, j]` carries no data.table marker, so it keeps full lexical checking. A nested function body written inside a masked argument is masked too, because a closure created in `j` is created inside the data's frame.

#### data.table result classes

A bracket with a signature but an unknown subject types as `Unknown`, because base indexing rules do not judge `[.data.table`. When the subject is the `data.table` nominal, the result class follows from the bracket's own syntax, even though the columns are unknown. In the table below, `j` is the second positional slot or a `j =` argument.

| bracket shape | result |
| --- | --- |
| no `j`, or an empty `j` slot, as in `DT[i]` and `DT[on = …]` | the subject's class, because a row filter and a join both return tables |
| `j` is a `:=` call, as in `DT[, x := …]` and `` DT[, `:=`(a = …) ] `` | the subject's class, returned invisibly |
| `j` is a `.()` or `list()` call, as in `DT[, .(m = mean(x))]` | the subject's class |
| any `j` with a `by =` or `keyby =` argument, as in `DT[, sum(x), by = g]` | the subject's class, because a grouped result always assembles into a table |
| anything else, such as a bare column `DT[, x]`, an ungrouped computed `j`, or a `with =` form | `Unknown`, and a strict-mode origin, because the shape would need column knowledge |

The class is a real type. It flows through chains, so `DT[a > 1][, .(m = mean(b)), by = g]` keeps `data.table` end to end. It satisfies or violates annotations, and it constrains call arguments. Column-level knowledge is out of scope: element types, membership checks, and `:=` evolution are not tracked.

#### Conditional stub namespaces: data.table, dplyr, ggplot2 and testthat

Shipped stubs exist for four packages that do not join the resolution universe by default.

- `data.table`, covering the `data.table` nominal, `fread`, and the `set*()` family
- `dplyr`, covering the `@masked` verb set, the joins, the tidy-select helpers, and the verb vocabulary
- `ggplot2`, covering the `ggplot` and `gg` nominals, `+.ggplot`, the geom and scale vocabulary, and the `@masked` `aes`
- `testthat`, covering the expectation vocabulary and `test_that`

R does not attach these packages by default either, and their names must not suppress typo warnings in projects that never use them. A conditional namespace activates in three ways.

- The project declares the package, through a `DESCRIPTION` dependency field or through any `NAMESPACE` `import` or `importFrom` naming it.
- Any project file attaches it with a `library()`, `require()`, `requireNamespace()`, or `loadNamespace()` call whose package argument is a literal name or a literal string.
- The project ships its own `stubs/<pkg>.Rtypes` override for the namespace.

While a namespace is inactive, it behaves exactly like any package the stub corpus does not describe.

The shipped dplyr verbs preserve their data argument's class, as `<T> fn(.data: T, ...) -> T`. A native-pipe chain therefore keeps its class end to end, so `fread(path) |> mutate(r = a / b)` stays a `data.table`. Every column reference inside the `...` of those verbs stays masked.

A project `.Rtypes` stub can declare its own masking function with the `@masked` attribute. Declare a dplyr-style verb like this:

```
filter : @masked fn(.data: Any, ...: Any) -> Any
mutate : @masked fn(.data: Any, ...: Any) -> Any
```

A call to a `@masked` name evaluates the arguments that the `...` rest parameter absorbs inside the data's frame, and a bare name there is a column reference. This applies to the bare name and to `pkg::name` alike. An argument matching a formal declared before the `...`, such as `.data` above, resolves normally, by position or by name. A declaration whose only parameter is `...`, such as `join_by : @masked fn(...: Any) -> Any`, masks every argument. A locally defined function of the same name masks nothing. `@masked` on a non-variadic declaration is a stub error.

## Object systems (S3, S4, R6)

ry checks the parts of R's object systems that are written down as declarations. It declines the parts that are decided at run time from a value's class attribute, and that boundary is not going to move.

| Construct | What the checker does |
| --- | --- |
| An operator on a nominal (`+.Class`, `Arith.Class`, `Ops.Class`) | Dispatches statically. See [operator methods on a class](#operators) |
| A directly called S3 method (`speak.dog(x)`) | An ordinary call, checked against that function's own signature |
| `UseMethod("speak")`, and any call to an S3 generic | The result is `Unknown`, and it is a strict-mode origin |
| `structure(list(...), class = "dog")` | The value keeps its argument's type, because a `class` attribute is data rather than a type, so the record's fields stay checkable. A `dim` attribute is the exception. It makes the value an array, whose shape is untracked, so those values stay `Unknown` |
| `setClass`, `setGeneric`, `setMethod`, `new` | Not modelled. `new(...)` is `Unknown` |
| `x@slot` read or write | Fully lowered, and types as `Unknown`. See below |
| `R6Class(...)`, `$new(...)`, fields, methods | Not modelled, and `Unknown` |
| `self`, `private`, `super` inside an R6 method | Resolve as names, and type as `Unknown` |

`x@slot` reads an S4 object slot, and `x@slot <- v` writes one. The slot's type is unknown, and the construct is still analyzed.

- a slot read types as `Unknown`, and it is a strict-mode origin
- the subject expression is inferred, so its own type errors surface
- the subject's variable read counts for naming, for unused analysis, for references, and for rename
- a slot write is an ordinary replacement-form assignment of its base variable

### Declaring a checked type for your own classes

A class is a nominal type with a representation, and that is [something you can declare](#type-parameters-aliases-and-nominal-types). Wrapping the constructor is enough to get slot types, constructor arity, and field access checked on an S4 or R6 class:

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

The `setClass` call stays opaque, and the annotation is what the checker reads. Operators on the class work the same way. Declare `Arith.Point`, and `p1 + p2` is checked.

Dispatch needs the argument's class at the call site, and inside an unannotated `function(x) speak(x)` it is not known there.

## Strict mode

Strict mode is an opt-in check, controlled by the `[check] strict` switch. It is off by default.

- it does not change inference, and it introduces no new typing rules
- it adds diagnostics at `Unknown` origins, and it escalates unresolved references
- the typecheck phase already runs to produce the inferred types. Strict mode reads those types and reports the places where the checker genuinely could not determine one
- it also reports a read that the attached-package tolerance silenced. Attaching a package whose exports cannot be known makes every otherwise-unresolved name tolerated. That is right for an ordinary run, because without it each of that package's exports would be a false `unresolved`. It also switches a whole class of checking off project-wide, which would otherwise make a clean run indistinguishable from a run that never happened. Such a read is genuinely undetermined, so strict mode names it and points at the [package declaration](/type-checking/stubs) that closes it. The classification is shared with the ordinary `unresolved` check, so these are exactly the reads that check let through. A near miss of a name your own project binds was never tolerated, and it stays an `unresolved` finding

### Unresolved references escalate to errors

An unresolved reference carries the `unresolved` diagnostic code. There are three kinds:

- a bare name the resolver cannot find in the package, in its imports, or in builtins
- an unknown package namespace in `pkg::name`
- a name a known namespace does not export, read as `pkg::name`

Outside strict mode these are warnings. Under strict mode they are errors, whether strict mode comes from the configuration or from the per-file directive. A name the checker cannot resolve is an unchecked part of the program. Turning strict mode on can therefore raise the severity of findings that were already there, without changing their count. That matters when a `--min-severity error` gate reads them.

Two `unresolved` findings are errors whatever the mode, because they stop the package from loading rather than describing a gap in the checker's view. The first is a `NAMESPACE` `importFrom` naming something the namespace does not export. The second is an `export()` naming something the package never defines.

### Per-file directive

A plain top-level comment sets one file's typing mode. It overrides the configured `[check]` switches in both directions:

```r
# typing: off      # no type or strict diagnostics for this file
# typing: on       # type checking on for this file, strict off
# typing: strict   # type checking and strict mode on for this file
```

- `off` silences the file's type errors and strict diagnostics, even when the configuration checks types. `on` opts a single file into type checking in an otherwise unchecked workspace. `strict` additionally enables [strict mode](#strict-mode) for the file
- the `#: @strict` form remains supported. `#: @strict` is `# typing: strict`, and `#: @strict off` is `# typing: on`, which type-checks the file but not strictly
- the last directive in the file wins. A `typing:`-prefixed comment with any other value is reported as an error rather than ignored silently
- the directive changes only which diagnostics are published for that file. Inference and every other check are untouched, so hover and the other IDE features keep working under `off`

### What strict mode flags

In strict mode, an expression or a binding whose inferred type is `Unknown` at the point it is introduced is a diagnostic. Strict mode targets `Unknown` only.

- `Unknown` is the could-not-determine type, and it is what strict mode reports
- `Any` is the explicit, intentional opt-out, and strict mode always tolerates it. A value typed `Any` never produces a strict diagnostic, even in strict mode

### Where an undetermined type is reported

An undetermined value is reported once, at the site where it first became undetermined, not at every later expression that carries it. There are three such sites: a construct the type system does not describe, a reference whose binding has no known type, and a recursive definition the fixed point could not type.

A reference to a binding defined in this project is not one of them. The binding's own definition is reported instead, so one undetermined value does not produce a finding in every file that reads it. An unresolved name is not one either, because naming already reports it.

### Diagnostics

Strict diagnostics carry the code `strict`, so that they can be filtered independently of type errors. Each origin is reported once, at the precise range of the origin expression. A binding and a bare expression are worded differently, because a binding can be annotated and a bare expression cannot.

## Syntax errors

A file with syntax errors is still analyzed. Analysis is error-tolerant, under one governing rule: a broken region reports its syntax error and nothing else. The checker draws no semantic conclusions from source that failed to parse.

- every well-formed statement in the file is analyzed normally. Definitions keep their exports, references resolve, and a genuine type error outside the broken region still surfaces
- a broken statement contributes nothing. It contributes no names, no reads, and no diagnostics beyond the syntax error covering it
- an unterminated argument or parameter list ends at the next statement, so the mistake stays on the line that made it. A list running onto the next line is ordinary R, and a fragment there such as `beta)` really is a forgotten separator, and is reported as one. A line that assigns is the next statement. Adopting it would report a confident missing separator on that line and on every line after it, and cost each adopted line its own definitions
- a broken assignment whose name side is intact keeps its definition. The value degrades to a hole that types as `Unknown`, so dependents neither lose resolution nor see a wrong type while the value is mid-edit. The hole is not a strict-mode origin, because the syntax error already marks it
- a checked annotation on such a broken definition binds its declared type unchecked. The definition keeps its contract for callers until the value parses again, at which point the value is checked against the annotation as usual

The practical consequence in an editor is that while one construct is half-typed, the rest of the file keeps its diagnostics, hovers, and completions stable. So does every other file in the package. The only new squiggle is the syntax error itself.

## Unsupported constructs

- a syntactically valid construct the type system does not describe may infer as `Unknown`
- this lets checking continue even when the checker cannot model the construct precisely
- whether an unsupported construct also produces a diagnostic is a construct-specific decision

