---
title: Typed ry as a complete language
description: "Design draft: ry's own value types (numbers, Bool, Str, List and the rest), measured against Roc feature by feature, with their syntax, typing rules, the explicit R boundary and the runtime contract"
---

This draft makes typed ry a complete language of its own, on par with Roc and built for speed. A
typed program can be written entirely in ry's own types (numbers, `Bool`, `Str`, `List`, `Dict`,
`Set`, records, tuples, tags and functions) without touching an R data type, and R values meet
those types only at an explicit boundary. Plain R code runs unchanged. Settled rules graduate into
the [type system reference](/reference/type-system/) and the
[standard library reference](/reference/standard-library/), which win wherever this page disagrees
with them.

## The shape in one example

<!-- The program is the typed `array_dot` benchmark written with ry's own types. -->

```r
dot <- function(x: List<F64>, y: List<F64>) -> F64 {
  total <- 0
  for (i in List.range(1, List.len(x))) total <- total + x[[i]] * y[[i]]
  total
}

values: List<F64> <- List.map(List.range(0, 999999), \(i) Num.to_f64(i) / 999999)
dot(values, List.reverse(values))
```

Every value here is ry's own. `List.len` returns an `I64`, and `List.range(a, b)` is the
`List<I64>` from `a` to `b`, both included and empty when `b < a`, so `i` is an `I64`. `0` is a literal, which takes the type its uses ask for, here `F64`. Nothing
can be `NA`, so the loop needs no missing-value checks, and `List<F64>` is one contiguous buffer of
unboxed doubles, so `x[[i]]` is a bounds check and a load.

The same function with an R vector is a type error that names the conversion (illustrative
wording):

```text
type-mismatch

  x expected `List<F64>`, found `double[]`. Convert an R vector with `List.from_r()`
```

## Goals and constraints

- **Plain R stays plain R.** No new form changes the meaning of text R accepts, and R's own literals
  keep their R values. `1` is still a `double` and `"a"` a `character` vector wherever nothing asks
  for a ry type.
- **Roc parity.** Where Roc has a feature, typed ry has it or records why not. The comparison is the
  gap table below.
- **Maximum performance.** Every type has a representation the compiler can keep unboxed and
  contiguous: no `NA` encodings, no attributes, no recycling, value semantics with in-place update
  of a uniquely held value. The [runtime contract](#runtime-contract) states what the runtime must
  provide.
- **Hindley-Milner** (`decisions.md`, "The type system is Hindley-Milner"). Every feature here is
  unification over the existing constraint mechanism, and the unifier gains nothing: records stay
  closed and there are no row variables (`decisions.md`, native records).
- **One boundary.** An R value becomes a ry value only through a conversion function that checks
  it. A ry value whose representation R already has (`F64`, `Bool`, `Str` and Lists of them) fits
  the R type directly, because nothing R code relies on is lost.

## What changes for existing ry code

ry already has records, tuples, tags, `match`, `Dict`, `Set` and a strict vector, `Array<T>`. What
it lacks is its own scalars: numbers are R's `integer` and `double`, a string is a length-one
`character` vector and a truth value an R `logical`, each of which may be `NA`. The standard library
takes R lists (standard-library design, "Values: R lists, R scalars and two new native kinds").

This design changes four things:

- **Scalars.** `I8` to `I128`, `U8` to `U128`, `F32`, `F64` and `Dec` are ry's numbers, `Bool` its
  truth value and `Str` its string. None has an `NA`.
- **`List<T>` is ry's list.** It is today's `Array<T>` renamed, with the same `[a, b, c]` literal,
  and it holds ry values only. `Array` is removed rather than kept as a second name.
- **The standard library speaks ry types.** `List.len` returns an `I64`, `Str.split_on` returns a
  `List<Str>`, `List.sum` takes a `List` of numbers. R lists and vectors reach it through
  `List.from_r` and its siblings.
- **Records gain an update form and patterns gain list and `as` forms.** Records stay closed, with
  no row variables, because typed ry stays small until a feature earns its place.

## Gap analysis against Roc

Roc's facts are from its language reference (`docs/langref/`) and builtins module
(`src/build/roc/Builtin.roc`) in the current Zig compiler, checked against the builtin modules of the
older Rust compiler (`crates/compiler/builtins/roc/` at tag `alpha4-rolling`) where Roc changed.
"Roc" means the current compiler; "legacy Roc" means the Rust one. "Today" is ry before this design.

| Feature | Roc | ry today | Decision |
| --- | --- | --- | --- |
| Integer types | `I8`-`I128`, `U8`-`U128` | R's 32-bit `integer` with `NA` | all ten, as Roc |
| Fractional types | `F32`, `F64`, `Dec` | R's `double` | all three, as Roc |
| Literal default | `Dec` for every literal (legacy: `I64` for integers, `Dec` for fractions) | R's | R's (`double`, `integer`) when nothing constrains a literal |
| Literal suffixes | `5.U8`, `1.5.F32` (legacy: `5u8`, `1.5f32`) | `1L` | legacy Roc's glued suffixes `5u8`, `1.5dec`, plus R's `L`, because R parses `5.U8` as the name-like number `5.` followed by `U8` |
| Integer overflow | crashes; `_wrap`, `_saturated`, `_try` variants | `NA` with a warning | an error; `_wrap`, `_saturated`, `_checked` variants |
| Mixed number types | no implicit conversion | R's promotion | none, as Roc; `Num.to_*` converts |
| `Bool` | nominal tag union `[False, True]` | `logical` with `NA` | `Bool`, two values, written `TRUE` and `FALSE` |
| `Str` | UTF-8, small-string optimized | length-one `character` with `NA` | `Str`, UTF-8, no `NA` |
| Interpolation | `"${name}"` | none | `f"hello {name}"` |
| `List` | contiguous, unboxed, in place when unique | R list, or `Array<T>` | `List<T>`, contiguous, unboxed, in place when unique |
| List operators | none; functions only | elementwise on `Array` | item by item |
| `Dict`, `Set` | hash map, insertion order | native, insertion order | keys widen to every ry value but functions and floats-with-`NaN` |
| Records | structural, open (row variable) | structural, closed | unchanged: closed, no row variables |
| Record update | `{ ..r, x: 1 }` | `r$x <- 1` | both: `(..r, x = 1)` builds a copy |
| Field punning | `{ x, y }` | none | in patterns only: `(x =, y =)` binds `x` and `y` |
| Tuples | `(a, b)`, `t.0` | `(a, b)`, `t[[1L]]` | unchanged |
| Tag unions | open and closed, row variable | open with `..`, no row variable | unchanged |
| Recursive types | through nominal types | through `type` | unchanged |
| Pattern matching | exhaustive, guards, list patterns, `as` | exhaustive, guards, no list patterns | adds list patterns `[x, ..rest]` and `p as name` |
| Number patterns | literals | literals | adds suffixed literals |
| Closures | lambda sets, defunctionalized | R closures | R closures; lambda sets are the runtime's job (contract) |
| Purity | `->` pure, `=>` effectful, `!` names | `@pure` | `@pure` stays; effects stay untracked |
| Abilities | none: static dispatch on nominal types with `where` constraints (legacy: abilities `Eq`, `Hash`, `Inspect`, `Encode`, `Decode`) | none (traits declined) | built-in structural `==`, hashing and printing for every ry type but functions; no methods, no user abilities |
| Opaque types | `:=` | `type` nominal, `@type` opaque | unchanged |
| Modules | `import`, `exposing` | R namespaces, `std` | unchanged; R packages are the module system |
| Results | `Try(ok, err)` (legacy `Result`), postfix `?`, `??` for a default | `Result`, no operator | `Result`; `Result.with_default` for `??`; a `try` prefix |
| `crash`, `dbg`, `expect` | builtins | `stop`, `print`, testthat | R's |
| Inference | HM with rows | HM with joins | unchanged: HM with joins |
| Generics | type variables | `<T>`, constraints | unchanged; constraints gain `number`, `int`, `frac` |
| Mutable variables, loops | `var $x`, `for`, `while`, ranges `a..<b` | R's `<-`, `for`, `while`, `a:b` | R's; `for` over a `List<T>` binds `T` |
| Length and index type | `U64` | `integer` | `I64` |

The sections below give each decision's rules.

## Numbers

### Types

| Type | Values | R counterpart |
| --- | --- | --- |
| `I8`, `I16`, `I32`, `I64`, `I128` | two's complement, every bit pattern a value | none (`I32` is not R's `integer`, see below) |
| `U8`, `U16`, `U32`, `U64`, `U128` | unsigned | none (`U8` is not R's `raw`: `raw` has no arithmetic) |
| `F32`, `F64` | IEEE 754, including `NaN` and the infinities | `F64` shares R's `double` representation |
| `Dec` | fixed point, 18 decimal places in 128 bits | none |

An `I32` differs from R's `integer` in two ways: `-2147483648` is an ordinary `I32`, where R reads
that bit pattern as `NA`, and overflow is an error where R returns `NA` with a warning. That is why
no integer type shares R's representation.

An `F64` and an R `double` hold the same bits, and an `F64` is never `NA`. A `NaN` produced by
arithmetic is an ordinary `F64`, as in Roc. R's `NA_real_` is a `NaN` with a reserved payload, and
the boundary refuses it, so an `F64` never holds R's missing value.

### Literals

A number literal has R's type unless a ry number type asks for it: `5` and `1.5` are `double`, and
`5L` is `integer`. So plain R code types exactly as before. A ry type asks for a literal in three
places:

- **An expected type.** A literal checked against a ry number type takes it: an argument for a
  typed parameter, the value of an annotated variable, a function's result, a `List` item or a
  record field whose type is known.
- **The other operand.** In an arithmetic operator or comparison whose other operand has a ry number
  type, the literal takes that type, so `n + 1` with `n: I64` is an `I64`.
- **A variable written with a literal.** `total <- 0` gives `total` a literal type that its first
  arithmetic use decides. A use with a ry number type `N` makes it `N`. Any other use, or the end of
  the function, gives it R's type. So in

  ```r
  total <- 0
  for (x in xs) total <- total + x
  ```

  with `xs: List<F64>`, `total` is an `F64`, while in plain R `n <- 0; n <- n + 1L` keeps `n` a
  `double`, by R's promotion order. The literal type is an inference variable with a literal
  constraint. The variable is decided by unification before any join of the variable's writes sees
  it, so a join never meets an undecided literal.

`-` written directly before a literal is part of it, so `-2147483648` is an `I32` and `-128i8` an
`I8`, as in Roc.

| Literal | May become |
| --- | --- |
| `5`, `5L`, `0x1F`, `1e3` | any number type that holds the value, or R's `double` (`integer` for `5L`) |
| `1.5`, `1e-3` | `F32`, `F64`, `Dec`, or R's `double` |
| `5i64`, `5u8`, `1.5f32`, `1.5dec` | exactly that type |

A literal that does not fit the type it is asked to take, such as `300` as a `U8` or `1.5` as an
`I64`, is a `type-mismatch` at the literal. `NA`, `NaN` and `Inf` are R's constants and never take
a ry type: `Num.nan_f64` and `Num.infinity_f64` are the ry spellings.

The suffixes are new syntax. R reads `5u8`, `1.5f32` and `1.5dec` as a number followed by a name,
and `5i64` as the complex `5i` followed by `64`, so each is a syntax error in R today. `5i` alone
keeps its R meaning. A hex literal takes only the `i` and `u` suffixes (`0xFFu8`), because `f`, `d`,
`e` and `c` are hex digits: R reads `0x1Ff32` as the number `0x1FF32`, and so does ry.

At run time an unsuffixed literal is the R value it always was. Where the checker gave it a ry type,
the operator or typed parameter that receives it converts it, which the interpreter can do without
type information. This conversion applies only to literals and to unannotated values the checker
already accepted. It refuses `NA` and an inexact value, and a compiler folds it away.

### Arithmetic

Both operands of an arithmetic operator have the same number type, and the result has it too.
There is no implicit conversion between number types, so `x + i` with an `F64` and an `I64` is a
`type-mismatch` that suggests `Num.to_f64(i)`.

| Operator | Integer types | `F32`, `F64`, `Dec` |
| --- | --- | --- |
| `+ - *`, unary `-` | same type; overflow is an error | same type |
| `/` | a `type-mismatch` that suggests `%/%` | same type |
| `%/%`, `%%` | floored division and modulo, as R | as R |
| `^` | same type, a non-negative integer exponent | same type |
| `== != < > <= >=` | `Bool` | `Bool` |

`%%` and `%/%` keep R's floored meaning rather than Roc's truncating `%` and `//`, because an R
reader expects it and `Num.rem` and `Num.div_trunc` give Roc's. Integer division by zero is an
error. Float division by zero follows IEEE 754, as in Roc.

`Num.add_wrap`, `Num.add_saturated` and `Num.add_checked` (and their `sub`, `mul` siblings) give
the other overflow behaviors, which ry could not offer while R's `integer` reserved a bit pattern
for `NA`.

### Conversions

`Num.to_i8` through `Num.to_u128`, `Num.to_f32`, `Num.to_f64` and `Num.to_dec` convert between
number types. A conversion that may lose the value has a `_checked` form that returns
`Result<T, :OutOfBounds>`, and the unchecked form is an error when the value does not fit, as
Roc's crash. `Num.round`, `Num.floor`, `Num.ceiling` and `Num.truncate` take a fractional number
and return an `I64`, as Roc's do. This replaces the standard-library design's rounding that keeps
the number type, which was written for R's `double`.

## `Bool`

`Bool` has two values. `TRUE` and `FALSE` are literals that become a `Bool` where one is demanded,
and `Bool.true` and `Bool.false` are always one. A comparison of ry numbers or strings is a `Bool`,
and `!`, `&&`, `||`, `&` and `|` on `Bool` operands are a `Bool`. `if` and `while` take a `Bool` or
an R `logical`. A `match` on a `Bool` has two cases, so `TRUE` and `FALSE` arms cover it, where an R
`logical` also needs `NA`.

## `Str`

A `Str` is an immutable UTF-8 string with no `NA`. A string literal becomes a `Str` where one is
demanded. `==` and the ordering comparisons compare bytes, independent of the locale. There is no
indexing and no length, as in Roc, because a string's length counts bytes or code points and
neither is what a reader means by characters: `Str.count_utf8_bytes`, `Str.to_utf8` and
`Str.walk_utf8` cover the needs. `Str.concat`, `Str.join_with` and interpolation (`f"..."`) build
strings, and `Num.to_str` formats a number.

## `List<T>`

A `List<T>` is an ordered sequence of ry values of one type, with value semantics:

- `[a, b, c]` builds one, and every item has the item type. Items are never converted into each
  other, but literals take the item type, so `[1, 2.5]` is a `List<F64>` when the context asks for
  one.
- `xs[[i]]` reads the item at position `i`, 1-based, and a position outside the list is an error.
  `List.get(xs, i)` returns `Result<T, :OutOfBounds>` instead. `xs[[i]] <- v` rebinds `xs` to a list
  with that item replaced.
- `xs[i]` with a `List<I64>` of positions or a `List<Bool>` mask is a `List<T>`; negative positions
  leave items out, as in R.
- `for (x in xs)` binds `x` as `T`. `List.range(a, b)` counts up from `a` to `b` inclusive and is
  empty when `b < a`. `a:b` keeps R's meaning, including counting down, and takes R numbers only.
- A `List` has no names, no attributes, no `NA` and no recycling.

A `List<T>` holds ry values only: `List<double>` is an `annotation` error that suggests
`List<F64>`. Today's `Array<T>` held R scalars, which is what this design removes.

## Records, tuples and tags

The literals, `match` and exhaustiveness are unchanged (`records-and-tags.md` and the type system
reference). Records stay closed and tag unions keep their anonymous `..`: Roc's row variables (open
records, pass-through tags) are left out, because typed ry adds a feature only once it earns its
place, and a closed record with a nominal type covers the programs ry targets. Two additions remain:

- **Record update.** `(..point, x = 3)` is a copy of `point` with `x` replaced, and every replaced
  field must exist, as in Roc. `point$x <- 3` stays the statement form. `..point` is an ordinary R
  identifier, so `(..point, x = 3)` is today a record holding a variable named `..point`; a name
  starting with `..` first in a parenthesized record literal becomes a spread. A `..rest` in a
  record pattern binds the remaining fields as a record.
- **Punning in patterns.** `(x =, y =)` binds the fields `x` and `y` to names of the same spelling,
  where `(x, y)` would be a tuple pattern. A record literal does not pun, because `(x, y)` is a tuple.

## Pattern matching

Two pattern forms join the table in the type system reference:

| Pattern | Matches |
| --- | --- |
| `[]`, `[p1, p2]` | a `List` of exactly that length whose items match |
| `[p1, ..]`, `[p1, ..rest]`, `[.., last]` | a `List` with at least those items; `rest` binds the remainder as a `List<T>` |
| `p as name` | what `p` matches, binding the whole value to `name` |

Patterns are ry syntax inside `match`, so `..rest` there is a binder, not the R identifier it is in
an expression. Coverage treats a list pattern as a constructor per length, with `..` covering every
longer length, as Maranget's algorithm does for Roc's list patterns. A suffixed number literal is a literal
pattern of its type.

## Equality, hashing and printing

ry has no abilities or methods, because traits are declined (`decisions.md`). Roc derives
`is_eq`, `to_hash` and `to_inspect` for every structural type whose parts have them, and ry builds
the same three in, for every ry type except functions:

- **Equality.** `a == b` on two values of the same ry type is structural equality, a `Bool`, except
  on two `List`s, whose `==` works item by item and gives a `List<Bool>`. A function is a
  `type-mismatch` there, as Roc's functions have no `is_eq`, and generic code says so with an `eq`
  constraint (`<T: eq>`), which every ry type but a function satisfies. Floats compare as IEEE 754,
  so `NaN != NaN`.
- **Hashing.** A `Dict` key or `Set` element is any ry value but a function, hashed structurally. A
  `NaN` key is an error when it is inserted.
- **Printing.** `print` and `format` show a ry value as the literal that rebuilds it (`5i64`,
  `"a"`, `[1, 2]`, `(x = 1)`), as records and dictionaries already print.
- **Ordering.** `<` and its siblings are defined on numbers and `Str`. `List.sort_with` takes a
  comparison returning `:LT | :EQ | :GT`, and `List.sort_asc` and `List.sort_desc` take numbers or
  strings.
- **Encoding.** Converting to JSON or another format is a library function over the R boundary,
  not an ability.

## Functions and purity

Functions are R closures. Their types, generics and the inference of a function literal in context
are unchanged. Roc separates pure `->` functions from effectful `=>` ones. ry keeps its `@pure`
declaration (type system reference, "Purity") and does not track effects otherwise, because an R
function may perform any effect and a typed program calls R functions.

## The R boundary

An R value enters ry's types only through a conversion that checks it, and a failed check is an
error that names the position and the reason.

| From R | Function | To ry |
| --- | --- | --- |
| `double` | `Num.from_r` | `F64` |
| `integer` | `Num.from_r` | `I32` |
| `logical` | `Bool.from_r` | `Bool` |
| `character` | `Str.from_r` | `Str` |
| `double[]`, `integer[]`, `logical[]`, `character[]`, `raw[]` | `List.from_r` | `List<F64>`, `List<I32>`, `List<Bool>`, `List<Str>`, `List<U8>` |
| `list[T]` where `T` converts | `List.from_r` | `List<T'>` |
| named list | `as.record` | a record |

Each refuses `NA`, attributes (names are dropped, a class is refused) and, for a scalar, a length
other than one. Going the other way, a ry value whose representation R shares fits the R type
directly, because R code loses nothing it relies on:

- `F64` fits `double`, `Bool` fits `logical` and `Str` fits `character`;
- `List<F64>`, `List<Bool>` and `List<Str>` fit `double[]`, `logical[]` and `character[]`, and a
  `List` of records, tuples or tags fits `list[...]`.

Every other ry value needs an explicit conversion: `Num.to_r` turns an integer type into R's
`integer` when it fits in 32 bits and into a `double` otherwise (an error past 2^53), `List.to_r`
does the same item by item, and `as.list` converts records, tuples, tags, dictionaries and sets, as
it does today. Passing such a value to an R function is a `type-mismatch` that names the
conversion, and at run time the R function refuses it with the error records already give.

## Typing rules in brief

- The ry scalar types are atomic types of their own. None fits another, and none fits an R type
  except the three shared ones above.
- A literal is typed by a fresh inference variable with a literal constraint (`number`, `frac`,
  `str` or `bool` literal). Unification binds it like any constrained variable, and a variable
  still unbound when its function is generalized takes R's type. This is Roc's literal typing with
  R's defaults, and it is Hindley-Milner defaulting.
- New constraints for generic code: `number` (every ry number type), `int` (the integer types),
  `frac` (`F32`, `F64`, `Dec`) and `eq` (every ry type but a function), so
  `List.sum : <T: number> fn(list: List<T>) -> T`. The literal constraints are internal and never
  written.
- The constraint lattice gains a ry side. Two ry constraints meet at the narrower one (`int` and
  `frac` meet only at an error, `number` with either is that one, `eq` with any is the other). A
  ry constraint never meets an R one (`numeric`, `atomic`, `scalar numeric`, `key`), so a variable
  is either an R number or a ry number, and the meet is a `type-mismatch`. `key` and `item` widen to
  admit every `eq` ry type. A literal constraint meets `numeric` by taking R's type and meets a ry
  constraint by staying a literal of that constraint.
- The rule that a whole-number `double` literal fits an `integer` parameter is the expected-type
  case of literal typing above, so it stays and needs no special case.
- The existing `numeric` constraint and R's coercion order are unchanged, and apply to R types only.

## Runtime contract

This section is the interface to the runtime work (`crates/heap`, `crates/interp`, `crates/jit`).
The language side relies on these properties, and the runtime may choose any representation that
provides them.

**Scalars.**

- Compiled code keeps every number, `Bool` and `Str` handle unboxed in registers and stack slots.
  No number type reserves a bit pattern, so an `I32` uses all 2^32 values.
- The boxed form, used by the interpreter and anywhere R code can observe a value, is:
  - `F64`: a length-one R `double` without attributes, which is exactly what the literal `1.5`
    evaluates to, so a literal needs no conversion;
  - `Bool`: a length-one R `logical` holding `TRUE` or `FALSE`;
  - `Str`: a length-one R `character` holding a UTF-8 `CHARSXP` that is not `NA_STRING`;
  - every other number: one native number object, carrying its type and a 16-byte payload, with no
    attributes, distinguishable from every R type by its type code.
- Integer arithmetic traps on overflow, integer division by zero is an error, and float arithmetic
  is IEEE 754. No typed operation checks for `NA`, because no ry value holds one.

**Lists.**

- A `List<T>` is contiguous, with items stored unboxed at their natural width when `T` is a number
  (8 bytes for `F64`, `I64` and `U64`, 4 for `I32` and `F32`, 1 for `U8`), as R's 4-byte
  `logical` for `Bool`, and as handles otherwise. `List<F64>`, `List<Bool>` and `List<Str>` use R's `double`, `logical` and `character`
  vector layouts, so they pass to R code and C code without a copy, as `Array<T>` does today.
- An update through `xs[[i]] <- v`, `List.set`, `List.append` and the like reuses the storage when
  the list is uniquely held (`decisions.md`, "Standard library updates reuse a uniquely held
  value"), so a loop that appends is amortized constant time per item.
- A `List` carries no attributes, so a compiled loop over one needs only a bounds check per read.

**Literals and boundaries.**

- An R number reaching an operator or a typed parameter that requires a ry number converts there,
  and an inexact conversion is an error. Compiled code folds literal conversions.
- The boundary functions in [The R boundary](#the-r-boundary) are the only places an R vector is
  scanned for `NA`. The conversion of a literal or an accepted scalar at an operator checks one
  value.

**Closures.**

- A typed closure captures by value whenever no variable it captures is written after the capture
  or through `<<-`, and the checker reports this per closure, so such a closure needs no R
  environment. A closure that fails the test keeps R's environment semantics.
- `match` coverage is decided statically: a non-exhaustive `match` on a closed type is an error, so
  compiled code needs no fallback arm.

**What the language side provides now.** Until the runtime has native integer kinds, the
interpreter carries a correct implementation: native numbers as heap objects, `List`s of them as
flagged vectors, and arithmetic through a generic path. It is slow, and the runtime work replaces
it with the representations above.

## The typed benchmark

The typed programs in `tools/bench/why/typed` use only ry types: `List<F64>` instead of
`double[]`, `F64` and `I64` scalars, `Str` for names, and the `List` and `Num` modules instead of
`seq_len`, `sum` and `vapply`. The harness's clock and output are R calls at the boundary
(`proc.time`, `cat`), converted with `Num.from_r`.

## Answered questions

The draft asked four questions of taste. Each took the recommended answer:

- **String interpolation** is an `f"..."` literal: `f"hello {name}"` is a `Str`. R reads `f"..."`
  as a syntax error, so no R text changes meaning.
- **Operators on lists** work item by item, as R's vectors and the old `Array` did, on two lists of
  one length or a list and a single value, never recycled.
- **Propagating an error** is a `try` prefix: `try f(x)` unwraps an `:Ok` and returns an `:Err` from
  the enclosing function. `try (f(x))` and `try` before anything but a name or call keep their R
  meanings.
- **Lengths and positions** are `I64`, so `List.len(xs) - 3` may go negative and `xs[-1]` stays
  expressible.

## Settled choices

- ry has Roc's thirteen number types, `Bool` and `Str`, none with `NA`.
- Literals have R's type unless an expected type, the other operand or a later use asks for a ry
  number type.
- `List<T>` replaces `Array<T>` and holds ry values only.
- `F64`, `Bool`, `Str` and Lists of them share R's representation and fit R types directly; every
  other crossing is an explicit conversion.
- Records stay closed, with no row variables; list patterns, `as` patterns, record update and
  pattern punning join the syntax.
- Equality, hashing and printing are built in for every ry type but functions.

## Open questions

- Whether `Dec` arithmetic in the interpreter should be exact 128-bit or start as a checked `i128`
  scaled by 10^18 with an error on overflow of intermediate products.
- Whether a project can declare that unconstrained literals default to ry types instead of R's.
