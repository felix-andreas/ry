---
title: Type system
description: The precise static-typing semantics contract for ry's R type checker
---

This page is the authoritative specification of ry's typing semantics. It is the precise contract that the type checker implements. The [Type Checker guide](/type-checking/tutorial) is a gentler introduction that works through examples.

Two things are not on this page. Diagnostic wording is in the [diagnostic codes reference](/reference/diagnostic-codes), because a message can be reworded without the semantics changing. What the type checker does not cover yet is in [Limitations](/type-checking/limitations).

## Typing comment syntax

A typing annotation is written in a `#:` comment. An annotation comes before the binding or expression it describes. This applies to every typing annotation, not only to function annotations.

- Consecutive `#:` lines with no blank line between them form one annotation block.
- Most annotation blocks attach to the binding or expression that follows them.
- Attachment works at any statement depth, not only at the top level. A block inside a function body annotates the local assignment or the block-final expression that follows it. The full checked, `@new`, and `@trust` semantics apply there.

The constructor idiom relies on attachment inside a function body:

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
- a plain `#` comment separates the block from the expression, or no expression follows at all
- the block has no content beyond the `#:` marker
- the block sits inside a call's argument list, for example beside a lambda passed to `lapply`. An argument is not a statement, so nothing there can be annotated

The last case has a remedy. Give the value its own binding, then annotate that binding. Moving the block above the enclosing statement annotates that statement instead. A lambda parameter has no annotatable position at all.

Three positions resemble an argument list but do attach, and are not errors: a braceless function body, a braceless `if` branch, and a parenthesised expression.

A block that contains only `@type` and `@alias` lines is a definition block. A definition block does not attach to the following binding or expression. It is a compact way to write several top-level `@type` or `@alias` declarations together. Definition blocks and `@strict` toggles stand alone, so the adjacency rules above do not apply to them.

There are four annotation forms:

- `#: TYPE`
  - checked annotation
- `#: @trust TYPE`
  - trusted coercion
- `#: @if-unknown TYPE`
  - unknown-only coercion
- `#: @new NOMINAL_TYPE`
  - nominal introduction

Additional block rules:

- a block may contain exactly one compact annotation line
- a block may contain an expanded function annotation made of multiple `@param` and `@return` or `@returns` lines
- a block may contain one or more `@type` and `@alias` lines
- compact, expanded, and definition forms cannot be mixed in the same block

A block that violates a shape rule is refused whole. A refused block reports its error and carries no typing payload, so a broken annotation never produces follow-on findings. These are the shape rules a block can violate:

- it mixes annotation forms
- it orders directives wrongly
- it declares a duplicate or an unknown type parameter
- it gives `@new` a payload that is not nominal
- it exceeds the nesting caps below

A block the annotation grammar could not read is refused the same way, and silently. The parse error has already reported what was wrong. A block that did not parse gives no information to report a second error from. A refused higher-rank annotation therefore reports the refusal alone, and the annotated definition types as though it carried no annotation at all.

The form rules above apply to whole `#:` lines. One line commits to one form, and only whole lines are compared. A line that yields a second item did not parse as the form it committed to. The extra item is what error recovery salvaged, not a second annotation, so such a line is a parse failure rather than a form clash.

Annotation types have two nesting caps.

- Past 128 levels, a type is refused for checking. This is a typing finding, so `# typing: off` removes it.
- Past 160 levels, the annotation shape itself is refused. This finding always reports.

Examples:

```r
#: integer
value <- 1L
```

```r
#: list[integer]
value <- list(1L, 2L, 3L)
```

```r
#: fn(count: integer) -> integer
double_count <- function(count) count + count
```

```r
#: @param render_count {fn(integer) -> character}
#: @param count {integer}
#: @param [label] {character | NULL}
#: @returns {character}
apply_renderer <- function(render_count, count, label = NULL) {
  if (!is.null(label)) paste0(label, ": ", render_count(count)) else render_count(count)
}
```

```r
#: @type Cat {list{ name: character }}
#: @type Dog {list{ name: character }}
```

## Types

### Atomic names

The semantics and the fixtures use the original R type names:

- `logical`
- `integer`
- `double`
- `complex`
- `character`
- `raw`
- `NULL`

Do not rename them to aliases such as `bool`, `int`, `float`, or `string`.

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
- mixing named and unnamed elements is a type error

Annotations produce most array-like and map-like list types. Coercing a structural list shape also produces them.

#### Current default and open design question

`list(...)` infers a fixed shape wherever it can, even when a homogeneous array-like or map-like reading would also fit.

- `list(1L, 2L, 3L)` infers as `list{integer, integer, integer}`, not as `list[integer]`
- `list(foo = 1L, bar = 2L)` infers as `list{foo: integer, bar: integer}`, not as `list[named: integer]`

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

When a record-like list is rejected, the finding names the one field that failed rather than the two whole types. There are three cases.

- Both sides declare the field, and the two types do not fit.
- The expected type declares a field that the value does not have. When the value carries a near-miss of that name, the finding names it, because a renamed field has exactly that shape.
- The value has a field that the expected type does not declare.

A nested record names the path, outermost field first, as `retry.count`.

Two whole types are printed only when the failure is not about one field. A record against a non-record, and a record against `list[T]`, are the cases two whole types explain well. For a single field they are two long, near-identical strings that the reader must diff by eye, and for a nested field they never name the path at all.

The finding is placed on the field, not on the whole value. A type carries no source ranges, so the field path is walked back against the `list(...)` call that built the record, whose tagged arguments are its fields. The caret then points at what the message is about.

- For a field whose type does not fit, the caret points at the offending value.
- For a field the type does not declare, the caret points at the field's name.
- For a missing field, the caret points at the innermost list that should have contained it, because the path itself has nothing to point at.

Where that walk finds nothing, the whole value stays the blame and the message still names the field. A variable holding a record has no field expression, so it is such a case.

#### Array-like lists

An array-like list `list[T]` represents a list whose elements all share a common element type `T`. An array-like list has no fixed positional semantics, and it does not require element names to be statically known. Annotations normally introduce array-like lists. Coercion from a tuple-like, record-like, or map-like shape also introduces them, when all values are compatible with `T`.

When a fixed-shape list flows into `list[T]` and `T` is still an open inference variable, `T` takes the join of the elements rather than unifying with each in turn. Every `lapply(x, f)` call has this shape. Without the join rule, the first element would pin `T` and every later element would be a mismatch, so `lapply(list(1L, "a"), f)` would fail while `for` over the same list is specified to bind `integer | character`. A `T` that is already concrete keeps the all-must-fit rule. Coercion into a map-like `list[named: T]` joins the same way.

#### Map-like lists

A map-like list `list[named: T]` represents a name-keyed collection whose values all share a common value type `T`. A map-like list does not require the set of names to be statically known. Annotations typically produce map-like lists. Coercion from a structural list shape whose element names are not statically available also produces them.

#### Mixed named and unnamed lists

A partially named `list(...)` is ordinary R, and `do.call(f, list(x, n = 1))` is the standard spelling. Neither the tuple-like shape nor the record-like shape can express it. The element names are therefore dropped, and the value types join into an array-like list. The result is less precise than either fixed shape, and it is never a false rejection of legal code.

Example:

- `list(1L, bar = "foo")` infers as `list[integer | character]`

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
- `Any` has two sources. You wrote it, or a standard-library declaration wrote it. The shipped stub corpus declares `Any` deliberately and often, in roughly 180 return positions. It does so where a precise type would reject calls R accepts, or would need a feature the type grammar does not have yet. Each stub file names its own compromises in its header. The recurring ones are a value-dependent result shape, a `T`-or-`NULL` hybrid, arbitrary identifier-named arguments, and a formal with a trailing dot. An `Any` in a hover or in a finding is therefore not by itself a sign that something is wrong

#### `Unknown`

- `Unknown` means the checker could not infer a more specific type
- `Unknown` may arise from an unsupported construct, an unresolved name, a partially supported construct, or insufficient type information
- `Unknown` is compatible with every type, in both directions. This is the same blanket compatibility that `Any` has. It keeps one unmodelled value from cascading into a run of follow-on errors. It is also why a gap in the checker's knowledge means checks are skipped rather than wrong. A value the checker could not type flows into a `double` parameter without complaint
- `Unknown` differs from `Any` in intent, not in compatibility. `Any` is a declared instruction not to check the value. `Unknown` records that the checker could not tell. The one place that intent changes behaviour is [`@if-unknown`](#unknown-only-coercions), which supplies a type where one is missing. It applies to an `Unknown` and is refused on an `Any`, because overriding a deliberate opt-out is a different act from filling a gap
- [Strict mode](#strict-mode) reports every site where the checker could not determine a type. It does not report a type that happens to be `Unknown`. Each of these records its own origin, and that origin is the finding: an unmodellable construct, a reference with no known type, a binding that does not stabilize across a loop, and a recursive definition. A type is not reported for being `Unknown`. A declaration whose return type is `Unknown` produces no strict finding at its call sites. An `Unknown` nested inside a larger type is not reported either, and the `fn(p: Unknown) -> Unknown` that an aliased generic closes to is such a case. Whether they should be reported is an open question, not a guarantee this page makes
- `Unknown` is not an explicit opt-out
- `Unknown` should remain visible in user-facing output and in fixture expectations
- `Unknown` preserves progress and reduces cascading secondary diagnostics

### `Never`

- `Never` has no values
- it represents an expression that does not return normally
- `Never` is compatible with every type
- it is useful for non-returning constructs and calls

### Type parameters, aliases, and nominal types

A type expression may bind type parameters with a leading universal binder:

- `<T> TYPE`
- `<T, U, ...> TYPE`

Examples:

- `<T> list[T]`
- `<T> list{ value: T }`
- `<T, U> fn(T) -> U`
- `<T> fn(T) -> T | NULL`

A binder name may carry a constraint, written `NAME: CONSTRAINT`:

- `<T: numeric> fn(values: T) -> T`
- `<T: numeric, U> fn(x: T, y: U) -> T`

Two constraint names are writable.

- `numeric` restricts the parameter to a numeric scalar or a numeric vector. The numeric scalars are `integer` and `double`. The numeric vectors are `integer[]`, `double[]`, and their `[named]` forms.
- `atomic` restricts the parameter to one of the six atomic scalar types. This is the same bound that using a parameter as a vector element `T[]` imposes.

Any other constraint name is an annotation error, and the error names the available constraints. An argument whose type violates a constraint is a type error at the call that imposed it. A written constraint composes with positional bounds exactly like an inferred one. A `T: numeric` used as a `T[]` element holds both bounds, so it instantiates only to a scalar `integer` or `double`. See [Numeric inference variables](#numeric-inference-variables).

The constraint works in both directions. It restricts what a caller may instantiate `T` to. It is also a promise the annotated function's own body may rely on. With `<T: numeric> fn(x: T) -> T`, the body may use `x` numerically, for example `x + 1L` and `x > 0L`, because every admissible instantiation is numeric. A bound the binder does not declare stays refused. A plain `<T>` body that does arithmetic is therefore a type error, because the annotation admits non-numeric arguments. `atomic` does not imply `numeric`.

Universal binders are rank-1.

- a `<...>` binder is allowed only at the outermost level of a user-facing type expression
- a nested binder is not allowed inside another type expression
- higher-rank polymorphism is not supported

A directive's `{...}` payload is not the outermost level. The expanded form declares its type parameters with `@forall`, and a named type declares them on its name, as in `@type Pair<T>`. A binder inside `@param f {…}` or inside `@type Name {…}` is therefore refused like any other nested one.

A refused binder reports exactly once, and the refusal is the only finding. The type is then read as though the binder were not written, so the rest of the annotation still parses. The block carries no typing payload, as described under [Typing comment syntax](#typing-comment-syntax), so the names the binder would have bound are not reported as unknown types on top of the refusal.

Examples of forms that are not allowed:

- `fn(f: <T> fn(T) -> T) -> integer`
- `list{ value: <T> list[T] }`
- `@param f {<T> fn(T) -> T}`

Named type definitions use `#:` lines with directive syntax.

- `#: @type NAME {TYPE}`
  - defines a nominal type named `NAME` with underlying representation type `TYPE`
- `#: @type NAME<T, U, ...> {TYPE}`
  - defines a generic nominal type named `NAME` with type parameters `T, U, ...`
- `#: @alias NAME {TYPE}`
  - defines a structural alias named `NAME` for `TYPE`
- `#: @alias NAME<T, U, ...> {TYPE}`
  - defines a generic structural alias named `NAME` with type parameters `T, U, ...`

Type definitions and alias definitions share one namespace.

A `@type` or `@alias` definition that reuses a name already defined by either form is an error.

Consecutive `@type` and `@alias` lines in the same block are allowed. They are equivalent to writing the same lines as separate blocks.

Examples:

```r
#: @type Cat {list{ name: character }}
#: @type Dog {list{ name: character }}
```

This is equivalent to:

```r
#: @type Cat {list{ name: character }}

#: @type Dog {list{ name: character }}
```

A definition block cannot mix `@type` or `@alias` lines with an ordinary checked annotation, an assertion, a nominal introduction, or an expanded function annotation line.

Examples of invalid mixed blocks:

```r
#: @type Person {list{ name: character, age: double }}
#: list{ name: character, age: double }
value <- list(name = "bob", age = 20)
```

```r
#: @type Person {list{ name: character, age: double }}
#: @param value {Person}
identity_person <- function(value) value
```

A definition block is allowed only at the top level of a file. A `@type` or `@alias` block inside a function body or in any other nested position is an error, and the definition does not enter the vocabulary.

Definitions are project-global rather than block-local. That has three consequences.

- consecutive `@type` and `@alias` lines in one block are still equivalent to separate blocks
- a named type reference is not limited to an earlier line in the same block
- forward references are allowed across block boundaries and across file boundaries

#### Type parameters and generic application

A generic application must match its declaration's arity, checked against the project vocabulary.

- applying the wrong number of type arguments is an error at the applied name. `Box<integer, double>` for a one-parameter `Box<T>` is such an error
- applying type arguments to a non-generic declaration, such as `Meters<integer>`, is an error
- a bare reference to a generic, such as `Box` without arguments, is an error everywhere except after `@new`. There an unapplied generic infers its arguments through the representation check
- a mis-applied name compares like `Unknown` in the relations afterwards, so the one arity error never cascades into value-level mismatches

Type parameters may appear inside structural type expressions and inside function types.

Examples:

- `list[T]`
- `list{ value: T }`
- `fn(T) -> T`
- `T | NULL`

Type parameters are also allowed in the atomic vector suffix forms:

- `T[]`
- `T[named]`

Using a type parameter as a vector element restricts it. A `T` in `T[]` carries the atomic-element bound, so it can instantiate only to one of the six atomic types: `logical`, `integer`, `double`, `complex`, `character`, and `raw`. This bound makes element-preserving signatures expressible. With `sort : <T> fn(x: T[]) -> T[]`, `sort(c("b", "a"))` types as `character[]` and `sort(c(1L))` as `integer[]`. A list argument cannot bind `T` at all, because a list is not an atomic element type.

`sort(list(1))` is nonetheless accepted as `Any`. The shipped `sort` ends its [overload set](#overload-sets) with an `Any` fallback, and the first-match rule picks it. A single-candidate `<T> fn(x: T[])` rejects the same call.

- a scalar argument coerces into a generic vector parameter and binds the element. A `<T> fn(x: T[])` called with `2.5` binds `T := double`
- `[[` on a generic vector `T[]` extracts `T`
- an arithmetic operator over a `T[]` operand also requires the element to be numeric. The variable then holds both bounds, so it is a scalar `integer` or `double`. The result keeps the element, so `sort(x) + 1L` is still `T[]`, unless a `double` operand promotes the result to `double[]`
- a comparison over a `T[]` operand yields `logical[]`. A numeric partner constrains the element to be numeric

A bound that can no longer be satisfied is a type error at the expression that imposed it. Binding an element variable to a non-atomic type is such a case, and so is requiring a `character` element to be numeric.

Writing `X[]` where `X` is neither an atomic type nor a type parameter is an error. A record, a function, and a nominal type are all such an `X`. Vectors hold atomic elements only, and the diagnostic points at the `list[X]` spelling for a list of such values. An alias element expands first, so `Id[]` with `@alias Id {integer}` is fine, while an alias of a record is refused at the `[]` use site. This is a typing finding, so `# typing: off` removes it. The annotation still applies otherwise, and hover and navigation keep the declared shape.

A named generic alias and a named nominal type are applied with angle brackets.

Examples:

- `Box<integer>`
- `Pair<integer, character>`
- `Person<integer>`

`@new` uses the same generic application syntax when it introduces a value of a generic nominal type.

- `#: @new Person<integer>` is valid when `Person<T>` is declared with `@type`
- `#: @new Person` is also valid on a generic `Person<T>`. The arguments come from the representation check, which is the one place an unapplied generic is allowed

Everywhere else, the type argument count must match the declared parameter count exactly.

In `@type NAME<T, U, ...> {TYPE}` and in `@alias NAME<T, U, ...> {TYPE}`, the declared type parameters are in scope only within `TYPE`. A type parameter shadows a project-global type of the same name. A type parameter is not a generic, so applying type arguments to it is an annotation error rather than a reference to the shadowed global. `Wrap<integer>` where `Wrap` is a parameter is such an error.

- `Pair<integer, character>` is valid for `Pair<T, U>`
- `Pair<integer>` is an error for `Pair<T, U>`
- `Pair<integer, character, double>` is an error for `Pair<T, U>`

#### Type aliases

A type alias is purely structural.

- using an alias name in a type annotation is equivalent to writing its underlying type directly
- an alias may appear anywhere an ordinary type expression may appear
- a generic alias may use its type parameters anywhere inside its underlying type expression
- an alias does not create a fresh type identity
- an alias is compatible with other types exactly as its underlying type is
- an alias definition cycle is an error

Example:

```r
#: @alias PersonShape {list{ name: character, age: double }}

#: PersonShape
value <- list(name = "bob", age = 20)
```

An alias may also appear inside a larger type expression.

```r
#: @alias Person {list{ name: character, age: double }}

#: list{ owner: Person }
value <- list(owner = list(name = "bob", age = 20))
```

This behaves exactly as if `Person` were replaced with `list{ name: character, age: double }`.

A generic alias may abstract over structural types.

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

An opaque nominal type has no representation to project. Standard-library stubs declare a type the type grammar cannot describe structurally as a bare `@type NAME`. `data.frame`, `factor`, `connection`, and `Date` are such types. See [Standard library stubs](/type-checking/stubs). Four rules apply to them.

- `$`, `[`, and `[[` are accepted, and the result is `Unknown` rather than an error. The R object behind such a class commonly supports value-dependent access, for example `df$amount` and `df[rows, ]`, and refusing would reject ordinary R
- the access is not checked further. There is no field-existence check, no index-count check, and no index-type check, so `df[i, j]` and `df[rows, ]` both pass
- every such access is an unsupported construct under [strict mode](#strict-mode). The untyped result is deliberate and visible, not silent
- every other structural requirement on an opaque nominal remains a type error, unless the class declares the corresponding [operator method](#arithmetic-operators). Arithmetic and loop iteration are such requirements. The nominal identity itself still checks exactly like any other nominal type

Examples:

```r
#: @type Person {list{ name: character, age: double }}
#: @type Pet {list{ name: character, age: double }}
```

`Person` and `Pet` are distinct and incompatible nominal types.

```r
#: @type Person {list{ name: character, age: double }}

#: @new Person
person <- list(name = "bob", age = 20)

#: list{ name: character, age: double }
shape <- person
```

This is valid, because a nominal value is compatible with its underlying representation type.

```r
#: @type Person {list{ name: character, age: double }}

#: fn(value: Person) -> character
get_name <- function(value) value$name
```

A nominal type name may be used in a function annotation and in a nested type expression.

```r
#: @type Person {list{ name: character, age: double }}

#: fn(value: Person) -> character
get_name <- function(value) value$name

get_name(list(name = "bob", age = 20))
```

This is an error, because an ordinary structural value is not compatible with `Person` without `@new`.

A generic nominal type is parameterized on the declared name.

```r
#: @type Person<T> {list{ value: T }}

#: @new Person<integer>
person <- list(value = 1L)

#: list{ value: integer }
shape <- person
```

#### Type-argument variance

Two applications of the same generic nominal type are checked against each other one type argument at a time. `Box<integer>` against `Box<integer | NULL>` is such a check. The direction of each argument check comes from where its type parameter occurs in the representation type, so the variance of each parameter follows from its occurrences.

- A covariant position preserves the checking direction. `Box<integer>` is therefore compatible where `Box<integer | NULL>` is expected, because a narrower argument satisfies a wider one. The covariant positions are a function return, a container or structural element, and a direct occurrence. A container or structural element covers a `list` item, a `list{...}` field, a tuple item, a vector element, and a union member.
- A function parameter position is contravariant, so it flips the checking direction. Take `@type Handler<T> {fn(value: T) -> NULL}`. `Handler<integer | NULL>` is compatible where `Handler<integer>` is expected. `Handler<integer>` is not compatible where `Handler<integer | NULL>` is expected, because otherwise a `NULL` could reach a function that only accepts `integer`.
- A parameter that occurs in both a covariant and a contravariant position is invariant. Its argument must match exactly in both directions. Take `@type Cell<T> {list{ get: T, set: fn(value: T) -> NULL }}`. `Cell<integer>` and `Cell<integer | NULL>` are then mutually incompatible.
- A parameter that does not occur constrains nothing, and it accepts any argument.

A type parameter that occurs inside a nested generic application is treated as invariant. A `T` inside `Sink<T>` within `@type Outer<T> {Sink<T>}` is such an occurrence. This is conservative, because the inner type's own per-parameter variance does not compose with the outer direction. The rule is sound, and it never admits an unsound widening or narrowing.

When a generic nominal has no visible definition, every argument is checked invariantly. This is deliberately conservative. A missing definition over-rejects by requiring an exact argument match, rather than over-accepting an unsound widening.

The covariance of container and structural element positions is an explicit assumption. R lists and vectors are mutable, and compatibility still treats their element positions covariantly. Without that treatment, `@new` inference, checked inference, and the structural coercions would not work. Scalar-to-vector and `T` into `T | NULL` are such coercions. This trades the soundness a mutable invariant container would require for the inference ergonomics those coercions depend on.

Unification is the invariant floor. Where unification must produce a single representative type, it unifies every nominal argument by equality, regardless of the parameter's compatibility variance. Inferring a type argument shared by two occurrences is such a case. This is consistent with compatibility, because a unified pair is compatible in both directions. Unification is strictly stronger than compatibility.

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
- A flexible argument checked against an expected union binds to the whole union at that first use, exactly as unification would bind it. A flexible argument is an inference variable, which is an unannotated parameter or a local that is not yet pinned. Uses commit in program order, so a later use that requires a different type reports its error at that later site, against the already-committed union. When two union-typed contracts share only some members, the checker does not compute the intersection. `integer | character` at one call and then `logical | character` at the next is such a pair. Annotate the value with the intended member type to satisfy both. Intersection constraints are deliberately out of scope, and the design notes cover this under the traits question. First-use commitment keeps checking deterministic in program order, which is the order R evaluates in

### Union unification

Unification is stricter than compatibility, and it is the invariant floor. It applies where two types must become one representative type, for example when inferring a shared type argument.

- an inference variable may be bound to a union, exactly like it is bound to any other type
- two unions unify only when their member sets are equal. Member order is presentation, not identity
- the nullable shape is the single member-wise case. `T | NULL` unifies with `U | NULL` by unifying `T` with `U`, when each side has exactly one non-`NULL` member. This lets a `<T> ... T | NULL` scheme instantiate against a concrete nullable
- there is no member-matching search inside unification. Directional member-wise reasoning lives entirely in compatibility

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

A function may declare a rest parameter to accept a variable number of arguments.

- `fn(...) -> RETURN_TYPE` accepts any number of arguments of any type. `...` is shorthand for `...: Any`
- the rest parameter is anonymous. It is written `...: TYPE`, and naming it as `...items: TYPE` is an annotation error, because rest arguments are matched by position and never by that name
- `fn(prefix: TYPE, ...: TYPE) -> RETURN_TYPE` shows that a rest parameter may follow fixed parameters
- `fn(...: TYPE, [option]: TYPE) -> RETURN_TYPE` shows that named parameters may also follow the rest parameter. They are matched by name only, exactly like R formals declared after `...`

There may be at most one rest parameter. Its position is part of the signature, and it mirrors the position of `...` in the R formal list. Parameters written before it fill positionally, and parameters written after it fill by name only. See [Function calls](#function-calls).

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

An unannotated `function(...)` expression infers a function type directly from its definition.

- every parameter appears as a named parameter, using its definition name, because R parameters are always matchable both by name and by position
- a parameter with a default value is optional at call sites
- a formal the body tests with `missing(name)` is also optional at call sites. This is R's optional-without-default idiom, so `function(name, punct) if (missing(punct)) … else …punct…` may be called without `punct`
- `missing(name)` on a defaultless formal of the current function also narrows the formal's supplied state along the branch edges, exactly like a type guard. Six rules apply:
  - on the edge where `missing(name)` is true, reading `name` is an error, because R would fail the read at run time with "argument is missing, with no default". Writing it is legal, and it supplies the formal, as in `if (missing(punct)) punct <- "!"`
  - on the edge where `missing(name)` is false, the formal is supplied and reads are ordinary
  - a diverging true edge, such as `if (missing(x)) stop(...)`, leaves the rest of the body on the supplied edge. `!missing(name)` swaps the edges
  - after the branches rejoin, the formal counts as unsupplied only when it is unsupplied on both edges, so only definite runtime failures are reported
  - a formal with a default is never narrowed. Reading such a formal while unsupplied evaluates the default, which is legal
  - `missing()` applies only to the immediate function's own formals, which matches R. An enclosing function's formal is not narrowed inside a nested function
- a `...` formal becomes a rest parameter with element type `Any`, at the position it holds in the formal list. `function(x, ...) …` therefore infers as `fn(x: T, ...: Any) -> …`, and calls check against it by the [rest-parameter rules](#function-calls). Those rules absorb surplus positional arguments and unmatched keywords, and they match formals after the `...` by name only
- the values reaching `...` are not tracked into the body. A body use of `...`, such as forwarding it to another call, types as `Unknown`
- parameter types and return types are inferred. An unconstrained parameter generalizes at a binding boundary, like any other inferred type
- a constraint that an inference variable still carries at an item's export edge survives as a scheme binder. `mixed_apply <- invoke(mirror)` therefore exports `<T: numeric> fn(x: T) -> T`, so cross-item calls keep checking it. An unconstrained residual variable erases to `Unknown`
- default value expressions are typechecked. An error inside a default is reported, and a default for an annotated parameter must be compatible with the declared type
- a `NULL` default is checked like any other default. `function(title = NULL)` is R's usual spelling for an optional argument, and it does not make the parameter optional to the body. When the caller omits the argument, `title` is `NULL` in the body, so a declared `character` is a promise the function does not keep. Declare the parameter `character | NULL` and narrow it with `if (is.null(title))`. `if (title == "draft")` is then an error rather than a run-time `argument is of length zero`. Marking the parameter `[title]` relaxes only the call. It says that callers may omit the argument, not that the body may receive nothing
- an unannotated parameter's type comes from its uses, not from its default, so a non-`NULL` default does not pin the inferred parameter type. `function(x = 1) x` is `<T> fn([x]: T) -> T`, and passing a character to it is not a finding, because R runs it
- a call that omits the argument takes the default's type, because that is the value R puts in the frame. With `f <- function(x = 1) x`, `f()` is a `double` and `f("a")` is a `character`. The two rules fit together. The parameter is polymorphic, and omitting the argument is the one call where the default chooses the instantiation rather than the caller
- for the same reason, a default is checked against an instantiation of the declared parameter type rather than against the binder itself. `#: <T> fn([x]: T) -> T` over `function(x = 1) x` is therefore accepted. A concrete declared type is unaffected, so `fn(title: character)` still refuses a `NULL` default, and `<T: numeric>` still refuses a character one

Examples:

- `function(x) x` infers as `<T> fn(x: T) -> T` at a binding boundary
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

Parameter names are part of the call interface. R matches call arguments against the definition's formal names, so names participate in compatibility.

- a named parameter pairs by name. `fn(a: integer, b: character)` accepts a function defined `function(b, a)`, and each annotation type binds to the same-named formal regardless of order
- an unnamed positional parameter type pairs with the remaining parameters left to right, so `fn(count: integer) -> NULL` and `fn(integer) -> NULL` are mutually compatible
- an annotation may not rename a parameter. `fn(count: integer) -> integer` over `function(n) n` is an error, because it would promise callers a name the runtime rejects
- parameter counts must match
- an expected-optional parameter promises callers that they may omit it, so the actual function must have a default for that parameter:
  - `fn(count: integer, [label]: character) -> integer` does not accept `function(count, label) count`
  - `fn(count: integer, label: character) -> integer` accepts `function(count, label = NULL) count`

Function compatibility is contravariant in parameters and covariant in the return type. A function value is compatible with an expected function type under three conditions.

- Each expected parameter type is compatible with the corresponding actual parameter type. This is the contravariant direction, and it means the actual function must accept every argument the expected interface may pass. Parameters pair by name where both sides name them, as R matches call arguments. Unnamed parameters take the remaining slots left to right.
- Arity is a range, not a number. An interface promises its callers every call shape from its required count up to everything it declares, and a function serves that interface when it accepts all of them. The actual function may therefore declare more parameters than the interface ever passes, provided the extras have defaults. It may not require more than the interface supplies, and it may not refuse an argument the interface may send.
- The actual return type is compatible with the expected return type. This is the covariant direction.

Examples:

- a function of type `fn(integer | NULL) -> integer` is accepted where `fn(integer) -> integer` is expected, because `integer` is compatible with `integer | NULL`
- a function of type `fn(integer) -> integer` is rejected where `fn(integer | NULL) -> integer` is expected, because the expected interface may pass `NULL`, which the actual function does not accept
- `fn(a: integer, [b]: integer) -> integer` is accepted where `fn(integer) -> integer` is expected, because `b` defaults and the one-argument call the interface makes is valid. This lets a standard-library reduction serve a callback interface. `lapply(list(mean, sd), function(g) g(1:3))` types as `list[double]`, even though `mean` and `sd` each declare optional formals the callback never passes
- `fn(a: integer, b: integer) -> integer` is rejected there, because the interface never supplies `b`. `fn() -> integer` is rejected too, because it cannot receive the argument the interface sends

#### Callback forwarding at variadic call sites

R's apply family invokes its callback as `FUN(element, ...)`, forwarding the caller's surplus arguments. A callback with more formals than the declared interface is therefore still correct when the call forwards the difference. At a call to a variadic function, a function-typed argument that fails the plain interface check is re-checked as that forwarded invocation.

- forwarded named arguments consume the callback's same-named formals first, each checked against its formal's type. These are the arguments the rest parameter would absorb
- the interface's parameter types then fill the callback's remaining formals in order, followed by the forwarded positional arguments. The interface's parameter types are the elements the callee will pass
- a formal that the invocation leaves unfilled must have a default
- the callback's return type must satisfy the interface's return type, in the covariant direction
- the re-check binds nothing on failure, and the reported error is the plain interface mismatch

There are three consequences. `lapply(words, gsub, pattern = "a", replacement = "o")` checks `gsub(word, pattern = "a", replacement = "o")` and types as `list[character]`. `lapply(words, nchar)` accepts the optional display formals of `nchar`. A forwarded argument of the wrong type fails the probe, and the call errors.

Variadic compatibility is conservative.

- a variadic function type is compatible only with another variadic function type. Their rest element types are contravariant, like ordinary parameters, and the fixed prefixes must match by the rules above
- the rest parameters must sit at the same position. The number of parameters declared before `...` must agree on both sides, because that position decides which parameters callers may fill positionally
- a variadic function type and a fixed-arity function type are never compatible, in either direction

This over-rejects some safe pairings, such as a fixed function that happens to accept the same arguments. It never admits an unsound one.

Inference gives a `...` formal a rest parameter at its formal position. See [Inferred function types](#inferred-function-types). An annotation with a rest parameter therefore checks against a `function(…, ..., …)` definition like any other function annotation.

#### Reporting a function that does not fit

When a function value is rejected at a parameter position, the finding names the one position in its signature that failed. It does not print the two whole signatures. There are two cases.

- The interface passes a parameter a value the function will not take. The finding names that parameter, and says either what the parameter accepts or what constraint it carries.
- The function produces a return value the interface will not take. The finding names the return.

The pairing is the one described above, so the position named is the position R's argument matcher would fill.

Two whole signatures are printed only when the shapes cannot pair at all. A different arity, an optionality disagreement, and a rest parameter on one side are such shapes. That is the only case the signatures explain. For a position mismatch they are actively misleading, because a [constraint](#numeric-inference-variables) is not part of a rendered type. `fn(s: T) -> T` prints the same whether `T` accepts anything or only numbers, so against an expected `fn(character) -> U` it describes a call that should have fit.

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
- it is the unchecked override, and it plays the same role as `as` in TypeScript
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
- Narrowing filters union members. A member whose family cannot be decided statically is kept on both edges. An inference variable, a flexible-element vector, and an opaque nominal are such members.
- `is.null(x)` on an `Any` or `Unknown` variable refines the true edge to `NULL`, because the runtime guarantees it. A family guard does not refine `Any` or `Unknown`. Inventing a concrete shape there would produce false positives against scalar-claim standard-library signatures.
- `is.null(x)` on a completely unconstrained inference variable shapes it. Such a variable is an unannotated parameter that nothing has used yet. The test asserts that `NULL` is a possible inhabitant, so the variable becomes `T | NULL` for a fresh `T`. The edges then narrow as an ordinary union. The true edge keeps `NULL` and the undecidable `T`, and the false edge is `T`. This types the unannotated coalesce idiom, so `function(value, fallback) if (is.null(value)) fallback else value` generalizes to `<T> fn(value: T | NULL, fallback: T) -> T`. That is the same scheme its annotated form declares. There are two consequences. Testing a parameter for `NULL` and then using it unguarded is a genuine finding, because the test itself declared `NULL` possible. The shaping never fires on a variable that already carries a constraint, because a numeric-constrained variable cannot hold `NULL`, and it never fires on a declared rigid type parameter, because an annotation's contract is not reshaped.
- When a guard cannot fire, no refinement happens. `is.null(x)` on a union with no `NULL` member is such a guard. The checker does not type dead branches specially.
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

- Only a read of a variable narrows. Parameters, function locals, and a top-level variable that an earlier statement assigned are such variables. An arbitrary expression does not narrow, so `is.null(f(x))` and `is.null(x$field)` do not.
- A refinement does not outlive the statement it was made in. Checking runs one top-level statement at a time, so a guard narrows within its own `if`. That covers the whole `if` and `else`, and everything nested in it. A following top-level statement reads the variable's unrefined type again. This separates the two spellings of the early-return idiom. Inside a function body, or inside one braced top-level block, `if (is.null(x)) stop(...)` narrows `x` for the rest of the body, because the guard and the later reads are one statement. Written as bare top-level statements, the `stop()` guard and the read that follows it are two statements, and the read is not narrowed.
- A read from inside a closure narrows when the guard is in the same statement, which matches how a local behaves. The guard's own subject must not itself be a deferred read. That body runs later, so the test proves nothing about the value it will see then.
- Conditions combined with `&&` or `||` are not decomposed yet.
- `is.na(x)` is not a type guard. In this system, being `NA` is a value property rather than a type property.
- Narrowing never touches an unresolved inference variable, so a guard does not pin an unannotated parameter.

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

### Name references

- a name reference evaluates to the type currently bound to that name
- when the name is not bound, the checker reports an unknown-name diagnostic
- after an unknown-name diagnostic, the reference expression is treated as `Unknown`, so that checking continues without cascading secondary type errors

### Namespace access

`pkg::name` and `pkg:::name` read one name directly from a package namespace, bypassing lexical scoping.

- A namespace is known when stubs declare it. The shipped standard-library packages are known, and so is any namespace a project stub file declares. `stubs/dplyr.Rtypes` declares the namespace `dplyr`. See [Standard library stubs](/type-checking/stubs).
- The project's own package is always known, whatever the stubs say. Qualifying a name with the package you are editing reads the definition the checker already holds, so the read has that definition's type rather than `Unknown`. `withr::defer()` inside `withr` is such a read, where `DESCRIPTION` names the package `withr`. This case wins over a stub namespace of the same name, in the way a package binding shadows a stub name. The name itself is not validated. A package exports names its sources never bind, such as a re-export, an S4 generic from `setGeneric`, a dataset under `data/`, and a binding installed by `.onLoad`. A name the definitions do not cover is therefore left alone rather than reported.
- When the stubs declare `name` in `pkg`, the qualified read has the stub's type, exactly like the bare name. A name that only the namespace's [export manifest](#standard-library-exports) lists validates the same way, and it types `Unknown`.
- An unknown namespace warns. A known namespace that neither declares nor manifest-lists the name warns that the name is not exported. A warning here and an error for the same mistake in a `NAMESPACE` `importFrom` is not an inconsistency. A bad import stops the package from loading at all, while a bad qualified read fails only if that line runs.
- Exports are declaration-level. A project stub that overrides a shipped name's type does not remove the name from its shipped namespace, so `stats::sd` stays valid under an `sd` override.
- An unvalidated qualified read types as `Unknown`, and that reference is a strict origin.
- `::` and `:::` are not distinguished. The split between exported and internal names is not modelled.

### Function calls

- a function call evaluates to the callee's return type
- when the callee expression is `Unknown`, the call evaluates to `Unknown`
- when the callee's return type is `Unknown`, the call evaluates to `Unknown`
- when the callee is a union whose members are all function types, the call must be valid against every member, because the value could be any of them. The call then evaluates to the union of the member return types. Each member is checked in an isolated probe, so no member's argument bindings leak into another's. The dispatch-table idiom `handlers[[name]](...)` has this shape
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

A call argument that is the enclosing function's bare `...` forwards an unknown number of arguments, possibly zero. `function(x, ...) helper(x, ...)` is this forwarding idiom. Such a call skips both arity checks, because neither missing-required nor too-many-arguments can be decided statically, and the `...` argument itself matches no parameter. The call's concrete arguments are still checked against their parameters as usual.

#### The native pipe

R's parser rewrites `x |> f(y)` into `f(x, y)` before it evaluates the code. The pipe types as that call and nothing else. The piped value becomes the first positional argument. All call rules above apply to it: arity, argument compatibility, and overload selection. Chains compose from left to right. A type error on the piped value blames the left-hand expression.

The `_` placeholder follows R's rule. It is legal only as the whole value of exactly one named argument. That argument then receives the piped value instead of the first positional slot, so `x |> lm(y ~ z, data = _)` is `lm(y ~ z, data = x)`. Note that `2 |> f(tag = _)` supplies only `tag`, so other required parameters really are missing.

A pipe R itself would reject is not guessed at. Three shapes are such pipes: a right-hand side that is not a call, a positional or repeated `_`, and a `_` nested inside a subexpression. Such a pipe stays an opaque operator, so it types as a silent `Unknown` and its reads stay quiet.

Optionality comes from the formals, not from the annotation. A formal with a default is optional in R, and no annotation can change that. The exported signature therefore takes each parameter's optionality from the function, and an annotation that disagrees is reported once at the definition. The disagreement is never reported as a missing argument at the call sites, because those call sites are correct. Both directions report: a required declaration over a defaulted formal, and an `[optional]` declaration over a formal with no default.

Argument checking is compatibility-based, not exact-equality-based.

- The ordinary coercions defined in this document apply at parameter positions. Scalar-like `T` into array-like `T[]` is such a coercion, and so is `T` or `NULL` into `T | NULL`.
- R's numeric promotion ladder widens in compatibility. The ladder is `logical` < `integer` < `double` < `complex`. A lower rung is accepted where a higher one is expected, for scalar-like, array-like, and map-like alike. `mean(1L)`, `sd(c(1L, 2L))`, and `sum(x > threshold)` are therefore not errors. The widening is directional. `double` is never accepted where `integer` is expected, and unification does not widen. `character` and `raw` are deliberately off the ladder. R reaches `character` only through an explicit coercion, and accepting it implicitly would leave argument-order mistakes unreported.
- A whole-number `double` literal counts as `integer` at a parameter position. `10` and `3` are such literals, so `seq_len(10)` and `substr(x, 1, 3)` are as valid as their `10L`, `1L`, and `3L` spellings. This generalizes the rule the `:` operator already applies to its endpoints. A fractional literal such as `2.5` is still rejected at an `integer` parameter, and so is a `double`-typed variable that holds a whole number.
- An argument whose type is `Unknown` is accepted at any parameter. The reason the value became `Unknown` was already diagnosed where it happened, and repeating it at every later use would add nothing.

A rest parameter, written `...: TYPE`, changes how surplus arguments are handled. Its position in the signature mirrors the position of `...` in the R formal list. Argument matching follows R's rule for formals around the dots.

- a rest parameter adds no required arguments, so a variadic function may be called with none. `paste()` is legal
- a positional argument first fills the unfilled parameters declared before the rest parameter, in order. This is exactly how R fills formals before `...` positionally. `wrap("a", "b")` on `fn(x: character, ...: character)` gives `x = "a"` and sends `"b"` to the rest
- once the pre-rest parameters are filled, the rest parameter absorbs any number of remaining positional arguments, each checked against its element type
- a positional argument never fills a parameter declared after the rest parameter. Those are matched by name only, as in R. `sum(1, 2, na.rm = TRUE)` with `fn(...: integer[] | logical[], [na.rm]: logical)` therefore sends `1` and `2` to the rest, and `na.rm` by name
- the rest parameter also absorbs a named argument that matches no declared parameter, and checks it against the element type. R collects unmatched keywords into `...`, which is the pass-through idiom that variadic wrappers rely on. `read.csv(file, colClasses = "character")` uses it
- a named argument that duplicates a declared parameter already given stays a named-parameter error, even with a rest parameter. R rejects a formal matched by multiple actual arguments. Without a rest parameter, any unmatched named argument is an error as before

### Overload sets

A standard-library stub name may declare several signatures, which form an ordered overload set. The [stdlib stubs page](/type-checking/stubs) describes the declaration surface. A call to such a name resolves per call site.

- candidates are tried in declaration order, and the call commits the first candidate whose parameters accept the arguments. That candidate's return type is the call's type. `sum(1L, 2L)` is therefore `integer`, and `sum(1.5, 2.5)` is `double`
- each failed candidate is probed in isolation. Nothing a failed candidate bound leaks into the next candidate or into the committed result
- When an argument's type is still an undetermined inference variable, a candidate may fit only because unification narrowed that variable. An unannotated parameter of an enclosing function is such an argument. A candidate accepted only because of that narrowing is not established by the call itself. Every candidate is still tried. A candidate that fits while leaving the caller's undetermined types exactly as they were beats one that does not, whatever their declaration order. Among fits of the same kind, the first declared candidate wins. A wrapper such as `function(x) sum(x)` keeps its parameter unconstrained this way. A candidate whose parameter is `Any` accepts without binding anything, so the general fallback is selected ahead of the narrower candidates declared above it. A single fitting candidate is established by the call, because it is the only signature that accepts it. It is selected and its narrowing stands, so `f(function(v) v, 1L)` selects the candidate whose second parameter is `integer`, even though the lambda's parameter type was open
- The [whole-number literal rule](#function-calls) does not affect which candidate is selected. Candidates are first tried against the arguments' true types, so `sum(1, 2)` selects the `double` candidate, matching what R computes. Only if no candidate accepts them is the set retried with the literal-as-integer allowance. A name whose only fitting candidate wants `integer` therefore still accepts `foo(1)`
- When no candidate accepts the arguments, the call is a type error. The error names the overloaded callee and how many signatures were tried, and it gives the first candidate's failure as the concrete hint. That is the form when the candidates disagree about what is wrong, because then no single candidate's complaint is the answer. One candidate's own finding is reported instead, at that candidate's own argument range, in two cases. The first is when every candidate rejects the call for the identical reason. The second is when one candidate got strictly further into the argument list than every other, which makes it the signature the call meant
- Every non-call use of an overloaded name sees the last declaration. Passing the name as a value and hovering over it are such uses. By corpus convention the last declaration is the most general one, so a value-use never carries a narrower contract than the calls it might make. Go-to-definition on the name points at the first declaration, where the set begins

Only a declaration file can overload a name, and that boundary is deliberate. Overloading is the one place this type system departs from Hindley-Milner. A name with several signatures has no single most general type, so a call has to be resolved by search rather than inferred. That costs both the principal-type guarantee and the speed that plain unification gives. The cost is acceptable for a fixed, curated corpus describing a standard library nobody designed with types in mind. It is not acceptable across a whole codebase.

A `#:` annotation on your own function therefore declares exactly one signature, and always will. To make one name accept several shapes, give the parameter a [union type](#union-types), or split the shapes into separate functions.

A local or package binding that shadows a stub name disables its overload set. The binding is used everywhere, calls included. A project [override stub](/type-checking/stubs#overriding-a-shipped-declaration) may declare sets, because a `.Rtypes` file is a declaration file for foreign code wherever it lives.

### Indexing

`[[` is single-element extraction.

`[` is the general subsetting operator in R. In the current supported semantics, it is defined only for certain list forms.

`$name` behaves as `[["name"]]` on lists, on records, and on the tolerated opaque nominals. It does not behave that way on atomic vectors. R rejects `$` on every atomic vector, including a named one, and reports `$ operator is invalid for atomic vectors`. `c(foo = 1L)$foo` is therefore a type error that points at `[[`, while `c(foo = 1L)[["foo"]]` extracts `integer | NULL`.

A backtick-quoted name follows the same rule.

A field on a union subject may be absent from some members. R answers `NULL` for a name a list does not carry. A field that exists in some of the subject's shapes and not in others therefore reads as that field's type unioned with `NULL`. This makes the accumulator idiom check:

```r
args <- list()
if (escape) args$escape <- TRUE
args$escape        # logical | NULL
```

A field that no shape carries is still an error, because that is a typo rather than an absence the program is prepared for. The "did you mean" suggestion is drawn from every field any member carries, so a misspelling is still suggested against a union whose other member has no fields at all.

#### `[[` on vectors

`[[` is allowed on scalar-like, array-like, and map-like vectors, and it extracts a single element.

- for a scalar-like vector `T`, `[[` returns `T`
- for an array-like vector `T[]`, `[[` returns `T`
- for a map-like vector `T[named]`, a name-based `[[` returns `T | NULL`

The type system does not model runtime indexing failures.

#### `[[` on lists

`[[` is allowed on lists.

- for an array-like `list[T]`, `[[` returns `T`
- for a map-like `list[named: T]`, a name-based `[[` returns `T | NULL`. A positional `[[` and a computed `[[` return `T`. The type system does not model runtime indexing failures here, as it does not for array-like lists

For a tuple-like list, `[[` with a literal position is precise. A computed position gives the union of the item types.

- when the literal position exists, the result is that element's type
- when a literal position does not exist, the access is a type error
- when the position is not known statically as a literal, the result is the union of the item types. A computed position could reach any item. This is the same rule that `for` iteration over a fixed-shape list uses

For a fixed-shape record-like list, `[[` with a literal field name or a literal position is precise. A computed index gives the union of the field types. Record fields are declaration-ordered, so `x[[1L]]` extracts the first field exactly as R does.

- when the literal field exists, the result is that field's type
- when the literal position exists, the result is that position's field type
- when the index is neither a literal name nor a literal position, the result is the union of the field types. This types the dispatch-table idiom `handlers[[name]](...)`
- when a literal field name or a literal position does not exist, the access is a type error

The type system does not model runtime indexing failures.

#### `[` on vectors

`[` subsets a vector. The result depends on the subject's shape and on the index's shape.

These are the index shapes.

- A scalar-like `integer`, `double`, or `character` index selects one position, and the result is the scalar-like element type. This is a deliberate scalar claim. A scalar negative index such as `x[-1]` actually drops one element and returns the rest. A scalar result coerces into every vector position, so the claim can never produce a false error downstream. The shape rules apply the same compromise to flexible operands.
- An array-like or map-like numeric or character index selects many positions, and the result keeps the subject's vector shape. `x[c(1L, 3L)]` and `x[ids]` are such indexes.
- A `logical` index of any shape is a mask, and the result keeps the subject's vector shape. `x[x > 0]` is such a mask, and a scalar `TRUE` or `FALSE` recycles over the whole vector.
- `NULL` selects nothing, and the result is the array-like vector of the element type.
- An index whose shape is still undetermined counts as scalar-like, and is left unconstrained. An unannotated parameter, an opaque nominal such as a factor, `Unknown`, and `Any` are such indexes.
- A `complex` or `raw` index is a type error, and so is a list, a function, or any other non-vector index.

These are the subject shapes, where `E` is the element type.

- scalar-like `E`: a scalar-like index yields `E`, and a vector-like or mask index yields `E[]`
- array-like `E[]`: a scalar-like index yields `E`, and a vector-like or mask index yields `E[]`
- map-like `E[named]`: a scalar-like index yields `E`, and a vector-like or mask index yields `E[named]`. `[` keeps names, unlike arithmetic

A character index is allowed on any vector shape, not only on a map-like one. R returns `NA` rather than erroring when the subject has no names. Map-likeness is also deliberately fragile, because most operations erase it, so requiring it would flag legal programs.

Examples:

- `c(1L, 2L, 3L)[2L]` is `integer`
- `c(1L, 2L, 3L)[c(1L, 3L)]` is `integer[]`
- `x[x > 0]` on `x: double[]` is `double[]`
- `c(a = 1L, b = 2L)[c("a", "b")]` is `integer[named]`
- `x[list(1)]` is a type error

An out-of-range position and a missing name produce `NA` at run time. Those are value-level outcomes the type system does not model, as it does not for `[[`.

#### `[` on lists

`[` slices a list. The result is a sub-list, so the subject's fixed shape does not survive into the result type.

- for an array-like `list[T]`, `[` returns `list[T]`
- for a map-like `list[named: T]`, `[` returns `list[named: T]`
- for a tuple-like list, `[` returns `list[T]`, where `T` is the union of the item types. `list(1L, "foo")[1L]` is therefore `list[integer | character]`
- for a record-like list, `[` returns `list[named: T]`, where `T` is the union of the field value types
- slicing the empty list yields `list[NULL]`, because `T` is the union of zero item types, which is `NULL`

For a homogeneous fixed-shape list the union collapses, so the result matches the plain coercion to the array-like or map-like shape.

#### Indexing opaque nominal types

`$`, `[`, and `[[` on an opaque nominal type yield `Unknown` without further checking. `data.frame` and `factor` are such types. See [Nominal types](#nominal-types) for the rule and its rationale.

#### Indexing an unresolved inference variable

`$`, `[[`, and `[` on an unresolved inference variable yield `Unknown`, and they leave the variable unconstrained. Such a variable is an unannotated parameter whose shape nothing pins down, as in `function(node) node$value`, `function(x) x[[1L]]`, and `function(x) x[1L]`. There is no "not a list" error and no "unsupported `[`" error there.

Ordinary R reads a field, an element, or a slice off a value whose shape the author never wrote down. A tree fold and a generic accessor both do it, so refusing here would report correct code. The access is therefore left undescribed rather than refused, and it is reported as an unsupported construct under [strict mode](#strict-mode), exactly as for an opaque nominal.

This covers multi-index subsetting too. `function(m, i, j) m[i, j]` is silent, because such a function is written for a caller that knows the shape when the callee does not. A subject whose shape was written down still refuses a shape no rule covers, so `c(1L, 2L)[1L, 2L]` is an error.

### Numeric inference variables

An unannotated value used as a numeric operand is constrained to be numeric rather than rejected. A numeric constraint restricts an inference variable to `integer` or `double`, in any vector shape. It also admits any nominal that declares an arithmetic operator method. Those methods are `+.Class`, `Arith.Class`, and `Ops.Class`, and they count whether they come from the standard library or from your own sources. See [operator methods on a class](#operators). Without that allowance, a numeric constraint would refuse a class whose `+` the checker itself dispatches, and `function(x) x + 1L` would reject every `Date`, matrix, and units-style class in the language.

Two other bounds exist alongside it. The atomic-element bound restricts a variable to a scalar atomic type. Using a type parameter as a vector element introduces it, as `T[]` does. See [Type parameters and generic application](#type-parameters-and-generic-application). It renders as `<T: atomic>`. A variable that acquires both bounds is a generic vector element used arithmetically. It holds the meet of the two bounds, which renders as `<T: scalar numeric>` and admits a scalar `integer` or `double`. It defaults to `double` at a binding boundary, exactly like a plain numeric variable.

All three spellings are writable as well as rendered: `numeric`, `atomic`, and `scalar numeric`. Every type the checker prints can be copied into an annotation unchanged, so the `#:` grammar accepts every bound that the checker renders. There is no fourth, internal-only bound.

A declared bound is believed inside the annotated body, not merely enforced at the call site. With `<T: numeric> fn(x: T) -> T`, the body may use `x` numerically, because every admissible instantiation of `T` is numeric. That includes a self-recursive call, which instantiates the scheme afresh and passes the body's own rigid `T` back into it.

- when the constraint reaches a binding boundary still unresolved, and a function parameter abstracts it, it generalizes into a numeric-constrained type parameter, rendered `<T: numeric>`
- a numeric-constrained variable that escapes a binding without a function parameter abstracting it defaults to `double`. This matches R's treatment of bare numbers as doubles
- instantiating a `<T: numeric>` scheme yields a fresh numeric-constrained variable, so calling such a function with a non-numeric argument is a type error at the call site
- a comparison against a concrete numeric operand also constrains a flexible operand to numeric. A comparison against a non-numeric family leaves it unconstrained, because the system has no character-or-logical constraint

A constraint belongs to the variable, not to the type it appears in. It is therefore visible in a binder prefix such as `<T: numeric>`, and nowhere else. A finding that must talk about a constrained position never prints the type. It says what the position needs instead. Passing a character-valued list to `lapply(words, function(s) s + 1L)` therefore reports *its parameter `s` is used as a numeric value*, rather than showing `fn(s: T) -> T`. See [Reporting a function that does not fit](#reporting-a-function-that-does-not-fit).

Examples:

- `function(x) x + 1L` infers as `<T: numeric> fn(x: T) -> T`
- `function(x) -x` infers as `<T: numeric> fn(x: T) -> T`
- `function(x) x > 0L` infers as `<T: numeric> fn(x: T) -> logical`
- `function(a, b) a + b` infers as `<T: numeric> fn(a: T, b: T) -> T`
- `function(x) x / 2` infers as `<T: numeric> fn(x: T) -> double`
- calling `(function(x) x + 1L)` with `"oops"` is a type error, and reports ``expected a numeric value (`integer` or `double`), found `character``

### Arithmetic operators

`a %op% b` is the call `` `%op%`(a, b) ``. A `%…%` operator the standard-library corpus declares is checked and typed as exactly that call, so `"a" %in% valid` is `logical` and `m %*% m` is a `matrix`.

Every other `%…%` operator stays an opaque construct whose result is `Unknown`, and that result is a strict-mode origin. A project's own operator is such an operator. This is deliberate. A user operator may be a non-standard-evaluation wrapper whose right operand is quoted rather than evaluated, and the `%>%` operator from magrittr is the canonical example. Checking such an operator as an ordinary call would reject correct code. A project definition of a corpus-declared operator also wins, as it does everywhere else, and it reverts that operator to opaque. A use of any `%…%` operator counts as a read of its name, so a project's own operator is never reported unused.

Before the numeric rules below apply, an operator whose operand is a nominal dispatches to that class's declared operator method. This is how R dispatches `d + 30L` on `Date` through `+.Date`.

Lookup mirrors R's own order. The operator-specific method comes first, such as `+.Date`. The operator's S3 group generic comes next, which is `Arith.Date` for arithmetic and `Compare.Date` for comparison. `Ops.Date` comes last. Either operand's class may supply the method, left first, so `30L + d` behaves like `d + 30L`.

A declaration is an ordinary stub or annotation declaration, named the way R names the method. The result therefore stays precise per operand pairing. Differencing two `Date` values gives a `difftime`, and offsetting one by a count gives a `Date`.

A class that declares an operator but accepts no candidate for the operands at hand reports the `type-mismatch` error *`+` is not defined between `Date` and `Date`*. It does not fall back to the numeric rules, and R rejects that expression too. A class that declares nothing falls through to the rules below unchanged, so an opaque nominal is still a type error under arithmetic.

Your own classes count, not only the standard library's. A method declared anywhere the global scope reaches makes its class arithmetic exactly as a shipped stub does. A package's `R/` sources and a script's own top level are such places. This matters beyond dispatch, because it is also what lets the class satisfy a [numeric constraint](#numeric-inference-variables). Passing a `Money` to `function(x) x + 1L` is therefore accepted when the project defines `+.Money`, and refused when the project defines no arithmetic method at all. R behaves the same way in both cases.

`c()` dispatches too. A class that declares a `c.Class` method keeps its class through concatenation, so `c(d1, d2)` on two `Date` values is a `Date`, and a real error on the result is still caught. A nominal with no such method is indeterminate rather than an error. R's default `c()` strips attributes and returns something the checker cannot name, so the result is `Unknown` and a strict-mode origin.

The method name's suffix is the nominal's name, not R's full class vector. A nominal here carries one name, so a class declared `@type ggplot` takes `+.ggplot`, even though R registers the method as `+.gg`.

The shipped corpus uses this for `Date`, `POSIXct`, and `difftime`. It is also how a project types a `+`-based DSL. A `stubs/*.Rtypes` declaring `+.ggplot : fn(e1: ggplot, e2: Any) -> ggplot` gives that class its operator.

Arithmetic operators are defined only for numeric operands:

- `integer`
- `double`
- `logical`. R promotes a logical operand to `integer` before arithmetic, so `TRUE + TRUE` is `2L`. A logical operand therefore computes as `integer`, and the atomic result rules below need no logical case
- an inference variable constrained to be numeric. See `Numeric inference variables`

A map-like vector may participate through its compatibility with an array-like vector.

Arithmetic does not preserve map-likeness.

An operand whose shape is still an inference variable is treated as scalar-like. This applies in the shape rules below and in the comparison rules. An unannotated parameter is such an operand. This is a deliberate scalar claim, and it is the same compromise the standard-library corpus applies to elementwise functions. A scalar result coerces into every vector position, so the claim can never produce a false error downstream. The cost is that vector-in and vector-out shape is not tracked through such a function.

There is one exception. An operand that carries the atomic-element bound is a generic vector, written `T[]`, and its operator results are genuinely vector-shaped. See [Type parameters and generic application](#type-parameters-and-generic-application).

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

### Comparison operators

`<`, `<=`, `>`, `>=`, `==`, and `!=` compare two operands of the same comparison family.

- there are two comparison families:
  - the numeric family holds `logical`, `integer`, and `double`, freely mixed. R promotes a logical operand to `integer` before comparing, exactly as it does for arithmetic, so `flags > 0` and `flag == TRUE` are both ordinary numeric comparisons
  - the `character` family holds `character`
- both operands must belong to the same family. Comparing across families is a type error
- a flexible operand is constrained to the numeric family when the other operand is concretely numeric, and left unconstrained otherwise. A flexible operand is an inference variable, such as an unannotated parameter. Both operands stay unconstrained when both are flexible, so `function(a, b) a < b` infers as `<T, U> fn(a: T, b: U) -> logical` and a cross-family call of such a function is accepted. There is deliberately no comparable constraint kind. R's comparison coerces across atomic families at runtime, so `1 < "2"` is legal R, and tying flexible operands to each other or to a family would reject legal programs. The same-family rule applies only where both families are concretely known
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
- a whole-number `double` literal operand counts as `integer` here, which matches R's runtime behavior for `:`. `1` and `10` are such literals
- otherwise, when either operand is `double`, the result is `double[]`
- an array-like operand is a type error, and so is a non-numeric operand
- an inference-variable operand acquires the scalar numeric bound, which admits a scalar `integer` or `double`. An unannotated parameter, as in `1:n`, is such an operand. Passing a numeric vector through the enclosing function is therefore a type error at the call, because R's endpoint truncation warning marks a bug. The result is `double[]`, because the endpoint may instantiate at `double`

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
- a non-concrete argument whose element type is not statically known is tolerated rather than rejected. `Any`, `Unknown`, and an unresolved inference variable are such arguments, and `function(x) c(x, 1L)` has an unannotated parameter of that kind. The combined element atomic is then indeterminate, so the whole result is `Unknown`. That result is a strict-mode origin when the argument is an unresolved variable. This rule keeps `c` from reporting a false "expected `integer`, found `T`" on a generic wrapper, and from cascading on an already-`Unknown` value. Claiming a concrete element type would be unsound, because a later argument could widen the atomic
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
- `function(x) c(x, 1L)` infers as `fn(x: T) -> Unknown`, because the unannotated `x` leaves the element atomic indeterminate

### Assignment operator `<-`

- `name <- expr` writes the type of `expr` into the variable slot of `name` in the current scope, and creates the slot on the first write. See `Value names` for the slot model
- a string where the name belongs binds that name. `"x" <- 1` is `x <- 1`, and `` `x` <- 1 `` is too. All three forms create the same slot, and a later `x` resolves to it. The finding range for a string target is the literal, quotes included, because that is what was written
- an assignment target that is not a name is reported as a `syntax-error`. A computed value and a number are such targets. R parses them and refuses them at run time. See [diagnostic codes](/reference/diagnostic-codes#syntax) for the exact shapes and for the one deliberate exemption
- when the assignment has an attached typing annotation, the assigned expression is checked using the annotation rules from this document
- the assignment expression itself has the type of the assigned expression
- a later assignment in the same scope writes the same variable. On a straight-line path the new write replaces the old type. Writes that merge from different control-flow paths join. See `Control-flow joins`

Recursion follows the letrec rule for closures. A function-valued assignment's own name is visible inside its body. The target is pre-bound to a fresh type variable before the body is inferred, and that variable unifies with the inferred function type. This is monomorphic recursion, which is the classic `let rec` rule. `fact <- function(k) if (k <= 1L) 1L else k * fact(k - 1L)` therefore types as `fn(integer) -> integer`, and a call that violates the recursively inferred signature is an error. The recursive uses share one instantiation, so there is no polymorphic recursion.

Mutual recursion between two local closures is out of the per-binding reach of letrec. The forward reference resolves, because a capture sees later frame writes, but it stays `Unknown`-tolerant rather than precisely typed.

At the package top level, a self-recursive definition and a mutually recursive group both resolve through the interface fixed point. Every member starts at `Unknown` and re-derives each round until the schemes converge. Simple recursion converges to its precise type. A top-level `fact <- function(n) if (n <= 1L) 1L else n * fact(n - 1L)` exports `fn(n: integer) -> integer`, and the pair `is_even` and `is_odd` exports `<T: numeric> fn(n: T) -> logical`.

A heterogeneous self-reference whose type grows without bound cannot converge in a system without recursive types. The idiomatic tree fold is such a case, because its parameter would need the recursive type `T = double | list[T]`. Such a group settles at `Unknown`. A cycle can also converge with `Unknown` embedded, and a pure self-call such as `f <- function() f()` settles at `fn() -> Unknown`. Either way the `Unknown` is gradual tolerance, so an unannotated consumer flows through it, and strict mode attributes it. See `What strict mode flags`. An explicit annotation on the binding closes the cycle exactly.

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

## Loops

`for`, `while`, and `repeat` all evaluate to `NULL`.

A loop body is checked to a control-flow fixed point. Variables written in the body join across iterations, and they join with the pre-loop state. See `Control-flow joins`.

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
- any other source is an error reported on the source expression, and a function is such a source

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
- when several files define the same top-level value name, the later file wins
- when several package files define the same top-level value name, both the overwritten earlier definition and the overwriting later definition should warn
- a bare top-level `{ }` block executes unconditionally, so its direct-child assignments are package globals as well, exactly like a top-level `name <- value`
- an assignment inside an `if`, `for`, or `while` body executes conditionally, so it is not a package global. A cross-file reference to such a name is unresolved

Cross-file references are scheme-based.

- a reference to another file's top-level binding sees that binding's generalized exported type scheme
- type information does not flow back into the exporting file through inference. A call in one file never changes the inferred type of a function defined in another file
- within one file, a top-level name also resolves to the final exported scheme of that name, so a use placed before the definition still sees the definition's type

Inside executable code, value naming is lexical over mutable variable slots. This matches R's environment semantics. A scope holds one variable per name, and an assignment mutates that variable.

- a function body, a `local(expr)` call, and a script's top level each form one variable scope, called a frame
- a function parameter introduces a variable slot in the function's frame. Assigning to the parameter name writes that same slot
- the first `<-` or `=` assignment to a name in a frame creates its variable slot. Every later assignment to that name in the same frame writes the same slot. It does not create a new shadowing binding
- an assignment inside a conditional branch or a loop body writes the enclosing frame's slot, exactly like an unconditional assignment. Braces and control flow do not introduce scopes
- a variable slot shadows an outer binding and a package-global binding of the same name. A slot that no write reaches at a read does not shadow, and the read resolves outward, as R's runtime lookup would
- `for` introduces a loop-local slot for the iteration variable, and re-initializes it from the iterable on every iteration. Assigning to the loop variable inside the body writes that slot
- `local(expr)` evaluates `expr` in a fresh child scope. The whole expression takes the type of `expr`, which for the common `local({ ... })` is the block's last-expression type. An assignment inside is local and does not leak to the enclosing scope, while a reference still sees enclosing names. The syntactic single-argument `local(...)` call is this construct. Rebinding `local` to a user function does not change that, which is a current limitation
- `library(pkg)`, `require(pkg)`, and `help(topic)` evaluate their first argument non-standardly. A bare name there is the package name or the topic name, so `library(stats)` means `library("stats")`. The argument reads as that character literal. It never resolves as a variable, and never warns. This applies to a syntactic call to the bare function name whose first argument is positional and a bare identifier. A string argument, a named first argument such as `library(package = pkg)`, and a qualified callee are all ordinary calls. Rebinding `library` to a user function does not change the quoting, which is the same limitation as `local`
- `quote(expr)`, `substitute(expr)`, `bquote(expr)`, and `expression(expr)` build an expression instead of running it. An assignment written inside one therefore binds nothing. `quote(x <- 1)` leaves `x` undefined, and a later read of it is reported, matching R's `object 'x' not found`. Nothing inside the quotation is judged, because the program does not run that code at that point. A call there is not checked for arity or argument types, and a name the quotation mentions need not exist. Naming a variable that a quotation is about to create is ordinary metaprogramming. The names the quotation mentions still count as reads, because `eval` may run the expression later. A write those names refer to therefore stays live rather than becoming a false "assigned but never used". This follows the same syntactic rule and carries the same limitation as `local` above

At a package document's top level, a conditionally executed assignment is not package-visible. This covers an assignment inside a top-level `if`, `for`, `while`, or `repeat`. Within the same document such an assignment still behaves like a variable slot. A later top-level read resolves to it, and reports the maybe-undefined warning below when an unassigned path also reaches the read. A conditional reassignment of a name that already has an unconditional top-level definition keeps resolving to the package-global winner.

Such a slot also types. A cross-item read sees the join of every conditional writer's settled type. For example, `for (i in 1:3) total <- i` followed by `report <- function() total` types `report` as `fn() -> integer`. In scripts this follows the same sequential and deferred visibility as named definitions.

Past eight conditional writers of one name, the slot is `Unknown` instead of the join. A name written at that many top levels has no useful joined type, because a union of dozens of unrelated types is not a fact a check can act on. A real conditional slot, such as a top-level `if` and `else` picking a default, stays far below the bound.

### Package imports (`NAMESPACE` and `DESCRIPTION`)

Hosts read the package's `NAMESPACE` and `DESCRIPTION` files at the package root. The facts in those files extend the resolution universe package-wide. Analysis without them behaves as if both were empty. This covers a single file and a project with neither file.

- `importFrom(pkg, name)` makes `name` a known bare read, and it is never reported unresolved. The read types as the stub corpus's declaration for the name when one exists, and as `Unknown` otherwise. An import typo is validated once at the import site. An `importFrom` naming something a stub-described namespace does not export is an error there, because R refuses to load such a package. That error never appears at a use site.
- `import(pkg)` of a namespace the stub corpus describes makes exactly `pkg`'s exports known bare reads. When no stubs describe `pkg`, its export set is unknowable. Every otherwise-unresolved bare read in the package is then tolerated rather than guessed at, which is the zero-false-positive rule. Unresolved-name detection for such a package resumes once stubs for `pkg` exist.
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

A read of a variable sees every write that can reach it. Control flow therefore joins the states a variable can be in.

- after `if` without `else`, a variable written in the branch has the join of its pre-`if` type and the branch's written type
- after `if ... else`, a variable has the join of the two branch outcomes. A branch that does not write contributes the pre-`if` state
- a loop body may run zero or more times. A read inside the body and a read after the loop see the join of the pre-loop state and the state flowing around the loop's back edge. The body is re-checked until this stabilizes, and a variable whose type keeps growing structurally is widened to `Unknown`
- `repeat` runs at least once, so after the loop the variable has the body's resulting state. Back edges still join inside the body
- an expression whose type grows past a size ceiling takes `Unknown` instead. The ceiling applies to the type read as a tree, counting each path separately. A value that refers to itself grows that way, for example a constructor returning a record whose fields all return that record. No real type approaches the ceiling. This is the same refusal as the loop rule above: the value is left undescribed rather than described at a size nothing can consume
- joining equal types keeps the type. Joining genuinely different types produces their union, exactly as `if ... else` result values do. Joining with `Unknown` produces `Unknown`

Joins interact with generalization in three ways.

- a variable with exactly one reaching write keeps that write's generalized scheme, which may be polymorphic. Inside a body, `f <- function(x) x` therefore stays `<T> fn(x: T) -> T`
- when writes merge at a join, the variable holds the join of the written types as a monotype. A scheme-producing write contributes its instantiated body. Conditional reassignment therefore monomorphizes
- a join involving an instantiated scheme unions rather than unifies, even where the two sides would unify. Instantiation gives each path independent variables. `fn(x: T) -> T` and `fn(x: U) -> character` unify only by binding `T := character`, which is a signature that belongs to neither path and links variables that were made separate on purpose. Two conditionally assigned functions therefore read as a union of both signatures, and a call on that union returns the union of their return types

Definite assignment follows four rules.

- A read that some path can reach with no prior write to the variable keeps resolving to the variable, and reports [`maybe-undefined`](/reference/diagnostic-codes). The name is introduced only in conditionally executed code, and R raises `object 'x' not found` on the other path. The finding is off by default, and `[check] maybe-undefined = true` turns it on. Definite assignment is a flow property, so two conditions that always agree at run time are two independent branches. `if (ok) v <- …` followed by `if (ok) use(v)` therefore reports although it is safe, and that shape is most of what fires.
- The loop and branch rules are exact where the shape allows it. A `repeat` is left through its `break` points, so a `repeat` that always assigns before breaking reports nothing, while a `break` that precedes the write does report. A branch that cannot fall through, such as one ending in `stop()`, contributes no path at all.
- A read that no write can reach does not resolve to the variable. See the shadowing rule above.
- A top-level variable's unwritten path is different. At run time it reaches the enclosing environment, so the read observes the name's cross-item binding. In a script that is the nearest earlier statement's binding, and in a package it is the name's definition elsewhere in the package. A loop's first iteration and a rebinding statement's right-hand side therefore read the earlier binding, and its type joins into the slot like any other reaching write. After `p <- "word"`, the body of `while (cond) p <- p - 1L` is a type error on the first iteration's `character` read. After `n <- 1L`, `n <- n + 0.5` types the rebinding as `double`. A name with no known cross-item binding stays tolerated as `Unknown`, and so does a name with only a self-referential one.

An item whose check reports an error exports `Unknown`. Later items then do not check against a shape the checker could not establish, so one mistake does not cascade across a file. An item carrying an explicit declaration is the exception. A `#:` annotation is what the author says the binding is, and it stays that whether or not the body honours it. A function whose body violates its annotation therefore reports the body error and still checks every call site against the declared signature. Otherwise a caller's mistake would stay hidden until the body was fixed.

Unused analysis, also called dead-store analysis, follows from the same reaching sets when the `unused` check is enabled. An assignment whose written value no read can observe on any path reports `unused` on the assigned name. It does not report on the whole assignment, because the value being computed is not what is dead. Package-visible top-level assignments, parameters, `for` variables, and `.`-prefixed and `_`-prefixed names are not reported.

Examples:

- `f <- function(flag) { x <- 1L; if (flag) { x <- 2L }; x }` is clean. Both writes reach the read, and `x` reads as `integer`
- `f <- function() { total <- 0L; for (i in 1:3) { total <- total + i }; total }` is clean. The accumulator write is read on the next iteration and after the loop, and `total` stays `integer`
- `f <- function(flag) { x <- 1L; if (flag) x <- "two"; x + 1L }` is a type error. `x` reads as `integer | character`, and `+` rejects the `character` member
- `f <- function() { x <- 1L; x <- 2L; y <- x; y }` warns that the first write to `x` is unused, which is a dead store

A read inside a nested function is a capture. The closure runs after its frame has finished, so every write of the captured name stays observable and no such write is a dead store. This holds only for writes in the frame that the read resolves to. An enclosing frame may hold a binding with the same name. The inner binding shadows it, so the closure does not read it, and it still warns.

- `f <- function() { x <- 1L; g <- function() x; x <- 2L; g }` is clean. Both writes to the `x` of `f` stay alive through the capture
- `f <- function() { x <- "outer"; g <- function() { x <- TRUE; function() x } }` warns that `x <- "outer"` is unused. The innermost function reads the `x` of `g`, which shadows it

`on.exit(expr)` reads the same way. R stores the expression and runs it when the function returns, so the expression observes the last value of every name it mentions rather than the value at the `on.exit` line. A read inside it therefore keeps every write of that name in the frame alive, exactly as a capture does. The standard rollback guard is therefore clean:

```r
with_transaction <- function(con, body) {
  committed <- FALSE
  on.exit(if (!committed) dbRollback(con))
  body(con)
  committed <- TRUE          # read by the exit handler, not a dead store
  invisible(TRUE)
}
```

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

A file that is not a package source file does not contribute to the package-global value namespace or to the project-global type namespace. Script-like documents under `scripts/` are such files.

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

## Object systems (S3, S4, R6)

ry checks the parts of R's object systems that are written down as declarations, and declines the parts that are decided at run time from a value's class attribute. The boundary is deliberate rather than pending work, so this section states both what happens and why.

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

### Why the boundary falls there

The inference algorithm is not the reason. Nominal types, record projection, and declaration-ordered overload sets are all part of the checker, and S3 operator dispatch already runs on top of them. Dispatching on a class is therefore a mechanism the checker already has. Three specific properties of run-time dispatch keep the rest out.

- **Dispatch needs a class the checker knows at the call site.** Inside an unannotated `function(x) speak(x)`, the argument's type is still undetermined, so there is nothing to dispatch on. Guessing a method would be unsound, so the result is `Unknown`. R code is most dynamic exactly where dispatch matters most.
- **Inheritance is subtyping.** Nominal types match by name, and there is no class hierarchy in the compatibility rules. The `contains=` of S4 and the `inherit=` of R6 both need one. Adding subtyping changes how every type relates to every other type, not only how classes do.
- **A generic's method set is open.** Any file may add `print.foo`, and so may any package loaded at run time. A static answer is therefore always incomplete. Treating the method set as an input to every call site of a generic would also make one new method re-check an entire workspace.

The consequence is a limit on coverage, not on soundness. An unmodelled construct is `Unknown`, and the checker never substitutes a type it cannot establish. [Strict mode](#strict-mode) reports every place where that happened, so a project that wants the full guarantee can see exactly what the checker could not see.

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

Outside strict mode these are warnings. Under strict mode they are errors, whether strict mode comes from the configuration or from the per-file directive. A name the checker cannot see is a hole in the checked surface, not a hint. Turning strict mode on can therefore raise the severity of findings that were already there, without changing their count. That matters when a `--min-severity error` gate reads them.

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

### Data-masked evaluation (NSE)

R evaluates some argument positions inside a data frame's own environment. A bare name there is a column reference that no lexical scope can see. These positions are recognized structurally, and a read there that resolves to no binding is treated as a column reference. That means a silent `Unknown`, no could-not-resolve warning, and no strict origin.

These are the recognized masks.

- A single `[` bracket whose subject types as the `data.table` nominal masks all of its index arguments, whatever they look like. With the subject's class known, `DT[speed > 20]` and `DT[, x]` are column references even though they carry no syntactic marker.
- A `[` call carrying an unambiguous data.table signature masks all of that bracket's index arguments, even when the subject's type is unknown. Such a signature is a `by =` or `keyby =` argument, a `:=` column assignment, a `.()` list call, or one of the `.SD`, `.N`, `.I`, `.BY`, `.GRP`, and `.EACHI` specials.
- The base masking family masks every argument other than the data. That family is `with()`, `within()`, `subset()`, and `transform()`. A locally defined function of the same name masks nothing. Which argument is the data follows R's own matcher. A named argument claims its formal first, which is `data` for the `with` pair and `x` for `subset` and `transform`. The remaining positional arguments fill what is left, so `with(data = frame, speed > 20)` and `with(speed > 20, data = frame)` both mask the condition. The `base::` spelling of any of the four masks exactly as the bare one does. Another package's same-named export is its own function, and it masks nothing.

A name inside a mask that does resolve keeps its ordinary resolution and typing. A local variable used in `j` and a standard-library function such as `sum` are such names, and data.table itself falls back to the lexical scope for names that are not columns. Base-R indexing such as `m[i, j]` carries no data.table marker, so it keeps full lexical checking. A nested function body written inside a masked argument is masked too, because a closure created in `j` is created inside the data's frame.

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

A call to a `@masked` name evaluates the arguments that the `...` rest parameter absorbs inside the data's frame, and a bare name there is a column reference. This applies to the bare name and to `pkg::name` alike. An argument matching a formal declared before the `...`, such as `.data` above, resolves normally, by position or by name. A declaration whose only parameter is `...` masks every argument, and `join_by : @masked fn(...: Any) -> Any` is such a declaration. A locally defined function of the same name masks nothing. `@masked` on a non-variadic declaration is a stub error.

A file that calls `R6Class` resolves `self`, `private`, and `super` inside it. R6 builds those bindings at construction, so they resolve nowhere lexically, and a read of one is not an unresolved name. `this` in a JavaScript class works the same way. The recognition is syntactic, so a local binding that shadows `R6Class` is not honored. It is also file-scoped, so a file that defines no R6 class still warns about `self`. Their type is `Unknown`, because R6 field and method types are not described.

For a dynamic binding outside any recognized mask, the ecosystem-standard mechanism works. A top-level `globalVariables(c("a", "b"))` or `utils::globalVariables(...)` call with literal string arguments declares those names as dynamically bound for the whole package, and could-not-resolve is suppressed for them everywhere. An undeclared name keeps warning.

### What strict mode flags

In strict mode, an expression or a binding whose inferred type is `Unknown` at the point it is introduced is a diagnostic. Strict mode targets `Unknown` only.

- `Unknown` is the could-not-determine type, and it is what strict mode reports
- `Any` is the explicit, intentional opt-out, and strict mode always tolerates it. A value typed `Any` never produces a strict diagnostic, even in strict mode

### Origins, not propagation

`Unknown` is also used internally as an error-recovery value and as a propagation value. A binary operator with an `Unknown` operand yields `Unknown`. A call whose callee or return is `Unknown` yields `Unknown`. A block whose last expression is `Unknown` yields `Unknown`. Unifying with `Unknown` yields the other type. If strict mode flagged every expression that resolves to `Unknown`, a single root cause would produce one duplicate diagnostic at every downstream use.

Strict mode therefore flags an `Unknown` only at its origin, which is the site that first introduces a non-error `Unknown`. It never flags a site that merely propagated `Unknown` from a child, an operand, a callee, or a referenced binding. Each of those is already flagged at its own origin, or will be.

These are the origin sites.

- An unsupported construct, which is a syntactically valid construct the type system does not describe. This is where `Unknown` enters.
- A name reference whose resolved type is `Unknown` because the referenced binding has no known type. A base-environment or library binding that has not been given a type yet is such a binding. This composes with library typing, described below.
- A recursive definition that the interface fixed point could not fully type. Such a definition sits in a reference cycle, raises no other origin in its body, and still carries `Unknown` in its exported scheme. `f <- function() f()` exports `fn() -> Unknown`. The cycle itself is the source, so the whole binding is attributed once. A cycle that instead settles at `Unknown` surfaces through the ordinary undetermined-reference origin at its recursive read.

These are explicitly not strict origins.

- An `Unknown` that arose from error recovery. The underlying type error was already reported when the expression failed to type-check, and the recovered `Unknown` is not flagged again. There is no double report.
- An `Unknown` that was merely propagated into a parent expression from a child that is itself an origin or a propagation of one. Binary operators, calls, blocks, indexing, `if` and `else`, and assignments all propagate this way.
- A reference to a local binding or to a package-global binding whose type is `Unknown`. The origin is the defining site of that binding, in its own file, so the reference propagates rather than re-originates. This keeps a single root `Unknown` from producing a diagnostic in every file that references it.
- An unresolved name reference. Naming already reports "could not resolve", so strict mode does not double-report it. An unresolved name is a naming diagnostic, not an `Unknown` origin.

Every downstream use of a flagged `Unknown` is a propagation site rather than an origin, so a single origin used in many later expressions produces exactly one strict diagnostic. That includes a cross-item reference. Reading a name this project defines propagates, because that definition has its own attributable site, and an earlier script statement and a package definition are both such definitions. Only a name with no such site originates at the reference, and a stub or import the corpus cannot type is such a name.

A call whose callee has no expressible signature yet is an origin at the call. A stub declared as a bare `Any`, such as `subset` or `data.frame`, is such a callee. The call is where the `Unknown` enters the program. Attributing it instead to whatever later line first reads the result would make one untyped binding produce a diagnostic per line that touched it.

### Composition with library typing

Strict mode is defined as a property of the inferred type at origin sites, so a genuine `Unknown` origin is an error. It is not defined as an enumerated denylist of today's unsupported constructs. As inference and the library and standard-library stubs improve, fewer origins exist. A library function that has no known type today will resolve to a real type once it is stubbed. Strict mode's diagnostics therefore shrink automatically, without any change to the strict-mode rule itself.

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

## Where a finding points

A finding underlines the smallest expression its message is about, so the underlined text and the message agree. Five rules follow from that.

- A binary operator blames the operand the message names, never the whole expression. An arithmetic mismatch underlines the offending side. A cross-family comparison underlines the right operand, which is the `found` half of `expected …, found …`.
- A `$` or `[[` finding about a field or a position underlines the key, not the access chain, because the chain contains the subject too. `outer$inner$dep` therefore reports `dep`.
- A surplus positional argument is underlined at the first argument with no formal left to take it. A missing argument has no argument to point at, so the callee is blamed.
- A declared return is checked against each expression that can produce the result, following a block to its tail and an `if` and `else` into both arms. Each expression that fails reports at its own site, which is the rule an explicit `return` follows. When no single expression is at fault, the whole construct keeps the one finding. An `if` with no `else` is such a case, because it contributes an implicit `NULL` that belongs to none of the expressions.
- A [record field](#reporting-a-record-that-does-not-fit) underlines the field the message names, found by walking the field path back against the `list(...)` that built the record, and falls back to the whole value when the record did not come from one.

A finding's range never crosses a line break. An error reported at the end of a line would otherwise blame the newline itself, and the end of a `#:` region is the most common such error. A newline's span runs from the end of one line to the start of the next, and an editor draws that as a squiggle across the break that points at neither line. Such a range therefore collapses onto the last character of code on its own line, which is where the reader has to look anyway.

## Unsupported constructs

- a syntactically valid construct the type system does not describe may infer as `Unknown`
- this lets checking continue even when the checker cannot model the construct precisely
- whether an unsupported construct also produces a diagnostic is a construct-specific decision

