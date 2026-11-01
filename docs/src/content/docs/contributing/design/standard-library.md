---
title: The standard library
description: "Design draft: ry's Roc-style standard library of Result, List, Str, Num, Dict and Set functions, how its names avoid R's, how it is typed, and how it loads"
---

This draft designs ry's standard library: a set of typed, native functions modeled on Roc's
builtins, which build on ry's [records, tuples and tags](/type-checking/records-and-tags/). It
settles which modules exist, how they are named and typed, and how they reach a program. The user
contract will be the reference page once the library ships.

## What it is for

R has a large base library, but none of it knows ry's own values. A function that can fail has no
typed way to say so, `match` has nothing to consume, and R's collections are either untyped lists or
environments. Roc solves the same problem with a small builtin library organized around `Result`,
`List`, `Str`, `Num`, `Dict` and `Set`, where a failure is a tag (`Err(ListWasEmpty)`) rather than an
exception. ry copies that library where it fits, so this works:

```r
first_word <- function(text: character) -> character {
  words <- Str.split_on(text, " ")
  Result.with_default(List.first(as.list(words)), "")
}
```

## Names are a convention, not a namespace object

A module is a naming convention: `Result.map` is one ordinary R identifier, bound like any other
function. There is no `Result` object, so `Result$map` does not exist, and `Result.map` needs no new
syntax because `.` is already part of an R name.

The alternative, a namespace value that holds the functions, was declined for three reasons:

- `Result.map` already parses as one symbol, so a namespace object would need `Result$map`, which
  reads as a list access and costs a lookup plus a `$` on every call.
- One symbol per function is what the checker, hover, completion and go-to-definition already
  understand, through the existing stub machinery. A namespace object would need a new kind of
  type for "module" in a Hindley-Milner checker that has none.
- Static dispatch (`x.map()`) is out by directive, so nothing needs the module as a value.

## No name changes what an R program does

Every standard-library name is `Module.function`, where `Module` is one of `Result`, `List`,
`Str`, `Num`, `Dict` and `Set`. No exported name of base R or of any recommended package starts
with one of those prefixes, which a test pins against every shipped export manifest. Three cases
show that an existing program keeps its meaning:

- A program that defines its own `List.map` keeps it: the global environment and package namespaces
  come before the base environment in R's lookup order, and the checker ranks a project definition
  above a stub.
- S3 dispatch only finds `Result.map` as a method when some code calls the generic `Result()` on an
  object of class `map`. No base or recommended package has generics with these names.
- A program that referred to `List.map` without defining it failed before, so it has no meaning to
  keep.

## Which modules, and which functions

The names follow Roc's current builtins, which are snake_case, as R style guides are. Roc moved its
builtins from camelCase to snake_case, so `Result.with_default` and `List.walk_until` are Roc's own
spellings, and nothing needs translating twice.

### Taken from Roc

| Module | Functions |
| --- | --- |
| `Result` | `is_ok`, `is_err`, `with_default`, `map`, `map_err`, `map_both`, `and_then`, `on_err` |
| `List` | `len`, `is_empty`, `get`, `set`, `update`, `append`, `prepend`, `concat`, `single`, `repeat`, `reverse`, `join`, `map`, `map_with_index`, `map2`, `join_map`, `keep_if`, `drop_if`, `keep_oks`, `keep_errs`, `map_try`, `walk`, `walk_backwards`, `walk_until`, `any`, `all`, `contains`, `count_if`, `find_first`, `find_last`, `find_first_index`, `find_last_index`, `first`, `last`, `take_first`, `take_last`, `drop_first`, `drop_last`, `drop_at`, `sublist`, `split_at`, `chunks_of`, `intersperse`, `sort_with`, `sum`, `product`, `min`, `max`, `starts_with`, `ends_with` |
| `Str` | `is_empty`, `concat`, `join_with`, `split_on`, `repeat`, `starts_with`, `ends_with`, `contains`, `trim`, `trim_start`, `trim_end`, `drop_prefix`, `drop_suffix`, `with_prefix`, `replace_each`, `replace_first`, `replace_last`, `split_first`, `split_last`, `count_utf8_bytes`, `to_utf8`, `from_utf8`, `to_integer`, `to_double`, `with_ascii_lowercased`, `with_ascii_uppercased` |
| `Num` | `to_str`, `compare`, `min`, `max`, `is_zero`, `is_positive`, `is_negative`, `is_even`, `is_odd`, `add_checked`, `sub_checked`, `mul_checked`, `div_checked`, `div_trunc_checked`, `rem_checked`, `sqrt_checked`, `log_checked`, `to_integer_checked` |
| `Dict` | `empty`, `single`, `from_list`, `to_list`, `len`, `is_empty`, `get`, `contains`, `insert`, `remove`, `update`, `keys`, `values`, `walk`, `map`, `keep_if`, `drop_if`, `insert_all`, `keep_shared`, `remove_all` |
| `Set` | `empty`, `single`, `from_list`, `to_list`, `len`, `is_empty`, `contains`, `insert`, `remove`, `union`, `intersection`, `difference`, `walk`, `map`, `keep_if`, `drop_if` |

### Changed on the way

- **`Result.try` is `Result.and_then`.** In R, `try` already means "catch an error", and a reader who
  sees `Result.try(r, f)` would expect that. `and_then` is the name Rust and Elm use for the same
  chaining.
- **Positions are 1-based.** `List.get(xs, 1L)` is the first element, and `map_with_index` passes
  1-based indices, because every R reader expects that and `xs[[1L]]` agrees with it.
- **Strings come back as character vectors.** `Str.split_on` returns `character[]` rather than a list
  of strings, and `Str.join_with` takes one, because a character vector is R's list of strings. Every
  other collection a function returns is an R list (`list[T]`), which is what the `List` functions
  take.
- **`Num` keeps only what R lacks.** R already has the arithmetic, comparison, rounding and
  predicate functions Roc's `Num` provides, so `Num` keeps the checked operations (which return
  `:Err(:DivByZero)` or `:Err(:Overflow)` instead of `Inf` or `NA`), the three-way `compare`, and
  the parity and sign tests.

### Skipped

- **`Bool`**: R's `TRUE`, `FALSE`, `!`, `&&`, `||` and `identical` already cover it.
- **`Box`, `Hash`, `Inspect`, `Encode` and `Decode`** exist in Roc to serve its abilities, which
  ry does not have (the type system is Hindley-Milner, and traits are declined). R's `print`,
  `format` and `serialize` play the inspection and encoding roles.
- **Capacity functions** (`List.with_capacity`, `List.reserve`, `List.release_excess_capacity`):
  R values are not grown in place, so there is no capacity to manage.
- **An `Option` module**: Roc has none, and returns a `Result` with a descriptive error tag instead
  (`:Err(:ListWasEmpty)`). Code that wants an optional value writes `:Some(x) | :None` directly.
- **`List.range` and `Str.len`**: R's `seq_len` and `seq` already build ranges, and Roc leaves out a
  string length on purpose (it would count bytes or code points, never what the reader means).
  `Str.count_utf8_bytes` and R's `nchar` remain.

## Values: R lists, R scalars and two new native kinds

The library takes R values where R has a good one, so it composes with existing code:

- A list is an R list. `List.map(xs, f)` takes any R list, including one that holds records or tags,
  and returns a new list. An atomic vector is not a list, so `as.list()` converts one first, which the
  checker already enforces (`integer[]` does not fit `list[T]`).
- A string is a length-one `character`, and a number is a length-one `integer` or `double`.
- A result is a tag, `:Ok(value)` or `:Err(error)`.

R has no good dictionary or set: a named list allows duplicate names and matches them partially, and
an environment holds only string keys and has reference semantics. So `Dict` and `Set` are two new
native kinds beside records, tuples and tags, with value semantics like theirs. An R function given
one refuses it with the same boundary error a record gets, and `as.list()` converts one to a list of
`(key, value)` tuples or of elements.

Keys and elements are atomic scalars (`logical`, `integer`, `double`, `character`), never `NA`.
That is what ry's `atomic` constraint expresses, and it gives every key an exact equality and a
cheap hash without traits. A dictionary keeps insertion order, as Roc's does, so `Dict.to_list` and
`Dict.walk` are deterministic.

## Typing

The signatures live in a shipped stub file, `types/std.Rtypes`, beside base R's, so the checker,
hover, completion, signature help and go-to-definition work on them through the existing stub
machinery. Every signature is a rank-1 generic in the existing type grammar:

```
@alias Result<T, E> {:Ok(T) | :Err(E)}
@type Dict<K, V>
@type Set<T>

Result.map : <A, B, E> fn(result: Result<A, E>, f: fn(A) -> B) -> Result<B, E>
List.walk : <T, S> fn(list: list[T], state: S, f: fn(S, T) -> S) -> S
List.first : <T> fn(list: list[T]) -> Result<T, :ListWasEmpty>
Dict.get : <K: atomic, V> fn(dict: Dict<K, V>, key: K) -> Result<V, :KeyNotFound>
```

Three additions to the stub format make this possible, and each mirrors what a project annotation
can already write:

- `@alias NAME<T, ...> {TYPE}` declares a structural alias, so `Result<integer, character>` works
  in stubs and in a project's own annotations.
- `@type NAME<T, ...>` declares a generic opaque type, so `Dict<character, double>` is a type that
  only the library's functions produce or read.
- The checker's rule that a record, tuple or tag passed to an R function is a `type-mismatch`
  exempts the standard library, because its functions are written for those values. The exemption
  is the module prefix, kept in `syntax::native_values` beside the existing list of R functions that
  accept them, so the checker and the runtime read the same fact.

The checker needs no new inference: unifying `:Ok(integer) | :Err(character)` against
`:Ok(A) | :Err(E)` already binds both variables member by member, which was verified before this
draft was written.

## Running it

The functions are native Rust builtins in `crates/interp/src/standard_library.rs`, bound in the base
environment on ry's runtime, the way a new GNU R release adds base functions. That makes them always
available, with nothing to attach, and costs nothing at startup beyond registering the builtins.
Higher-order functions call the R function they are given through the interpreter's ordinary call
path, so a closure, a builtin and a function that uses `match` all work.

They exist only on ry's runtime (`ry run --runtime ry`), as records and tags do: GNU R cannot hold
the values they return. A test compares the stub file's names with the runtime's table, so a
function cannot be typed without existing, or exist without a type.

Errors are R errors with the function's name. A wrong argument kind (`List.map("a", f)`) stops
with a message naming the parameter and the expected value, and an `NA` key or string stops rather
than silently propagating, because a ry function has no `NA` case to fall into.

`Dict` and `Set` hash their keys into an open-addressing index stored in an integer vector next to
the keys, so a lookup is a hash and a probe, and the collector traces the index like any other
vector. Values are immutable, so `Dict.insert` copies the keys, values and index; building a large
dictionary goes through `Dict.from_list` or `Dict.insert_all` in one pass. Reusing an unshared
dictionary in place, as Roc does, is left for later, because R's reference counts do not yet say
reliably when a value has one owner.

## Open questions

- Whether `List` should also accept atomic vectors, with a separate vector-returning signature, once
  overloads on generic stubs are cheap enough.
- In-place update of an unshared `Dict`, `Set` or list, which needs the runtime's reference counts
  to be exact first.
- Whether GNU R should get a pure-R build of the functions that touch no ry values (`Str`, `Num`).
