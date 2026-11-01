---
title: The standard library
description: "Design draft: ry's Roc-style standard library of Result, List, Str, Num, Dict and Set functions, how its names stay out of R's way, how it is typed, and how it loads"
---

This draft designs ry's standard library: a set of typed, native functions modeled on Roc's
builtins, which build on ry's [records, tuples and tags](/type-checking/records-and-tags/). It
settles which modules exist, how they are named and typed, and how they reach a program. Once the
library ships, the user contract is the [standard library reference](/reference/standard-library/).

## What it is for

R has a large base library, but none of it knows ry's own values. A function that can fail has no
typed way to say so, `match` has nothing to consume, and R's collections are either untyped lists or
environments. Roc solves the same problem with a small builtin library organized around `Result`,
`List`, `Str`, `Num`, `Dict` and `Set`, where a failure is a tag rather than an exception (written
`Err(ListWasEmpty)` in Roc, and `:Err(:ListWasEmpty)` in ry). ry copies that library where it
fits, so this works and types `first_word` as `fn(text: character) -> character`:

```r
first_word <- function(text: character) -> character {
  Result.with_default(List.first(Str.split_on(text, " ")), "")
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

## Where the names live

The functions live in the `ry` namespace, which is attached at startup as `package:ry` in front of
the default packages, beside the DataFrame API (`DataFrame`, `col`, `setkey` and the rest), so `search()`
starts with `".GlobalEnv"`, `"tools:ry"`, `"package:ry"`, `"package:stats"`. `ry::List.map` spells
the same function after something else masks the bare name. (Without base R, as in the
interpreter's own tests, the builtins bind them in a `package:ry` just above `package:base`.)

Placing them there rather than in the base environment is what keeps R programs meaning what they
meant. R looks a name up from the calling environment outwards: top-level code goes through the
global environment and then the search path, and package code goes through its namespace, its
imports, the base namespace, and only then the global environment and the search path. Either way,
every environment a program could have defined a name in comes before `package:ry`:

- A script that defines its own `List.map` keeps it, and so does a package.
- A package that attaches a package which exports a `Str.trim` sees that one, because `library()`
  attaches in front of `package:ry`. The default packages come after it, which is safe because
  none of them exports a library name.
- S3 dispatch only finds `Result.map` as a method when some code calls a generic `Result()` on an
  object of class `map`, and no shipped package has generics with these names.

No exported name of base R or of a package with a shipped export manifest (`types/*.exports`)
starts with `Result.`, `List.`, `Str.`, `Num.`, `Dict.` or `Set.`, which a test pins. The
recommended packages (Matrix, MASS, survival and the rest) have no shipped manifest, so the claim is
unverified for them. Names that differ only in case, such as `list.files` and `set.seed`, are
distinct R names and are not affected.

Two things do change, and both are deliberate:

- Reflection sees the names. `exists("List.map")` is `TRUE`, `search()` lists `package:ry`, and
  `ls("package:ry")` lists the library, so a program that branches on `exists()` before defining a
  fallback takes the other branch. That is the same as running it under a newer R that added the
  name.
- GNU R does not have them. A script that calls `List.map` and checks clean still fails under
  `ry R`, which runs GNU R, but with an error that names the cause: the GNU R session's `tools:ry`
  binds every library name GNU R lacks to a function that stops with "`List.map()` is part of ry's
  standard library, which only ry's runtime provides". So `exists("List.map")` is `TRUE` there too.
  Plain `Rscript` still says "could not find function". Records, tags and `match` have the same
  split without the friendlier error.

## Which modules, and which functions

The names follow Roc's builtins as they stood before its `Result` became `Try` and `walk` became
`fold` (roughly the 0.0 releases through 2025), spelled in snake_case. R style guides favor
snake_case too, and Roc's newer builtins switched to it. That earlier API is the one Roc's guides
and most of its code still use, and it is the one the orchestrating directive names
(`Result.with_default`, `List.walk`). Roc's later renames are recorded below so that a reader who
knows today's Roc can translate.

The rule for what a module holds is the same everywhere. A function belongs in the library when it
is typed precisely, never meets `NA`, and reports failure as a tag; that holds even when R has a
function with the same job (`Str.trim` beside `trimws`, `List.walk` beside `Reduce`), because the
point is a library that composes with `match` and the checker. What it does not take is machinery
that exists only for Roc's type system or memory model.

### Taken from Roc

| Module | Functions |
| --- | --- |
| `Result` | `is_ok`, `is_err`, `with_default`, `map`, `map_err`, `map_both`, `map2`, `and_then`, `on_err` |
| `List` | `len`, `is_empty`, `get`, `set`, `update`, `append`, `prepend`, `concat`, `single`, `repeat`, `reverse`, `join`, `map`, `map_with_index`, `map2`, `map3`, `map4`, `join_map`, `keep_if`, `drop_if`, `keep_oks`, `keep_errs`, `map_try`, `walk`, `walk_backwards`, `walk_until`, `walk_backwards_until`, `walk_with_index`, `walk_with_index_until`, `walk_from`, `walk_from_until`, `walk_try`, `for_each`, `for_each_try`, `any`, `all`, `contains`, `count_if`, `find_first`, `find_last`, `find_first_index`, `find_last_index`, `first`, `last`, `take_first`, `take_last`, `drop_first`, `drop_last`, `drop_at`, `sublist`, `split_at`, `split_first`, `split_last`, `split_on`, `split_on_list`, `chunks_of`, `intersperse`, `sort_with`, `sort_asc`, `sort_desc`, `swap`, `replace`, `append_if_ok`, `prepend_if_ok`, `concat_utf8`, `with_capacity`, `reserve`, `sum`, `product`, `min`, `max`, `starts_with`, `ends_with` |
| `Str` | `is_empty`, `concat`, `join_with`, `split_on`, `repeat`, `starts_with`, `ends_with`, `contains`, `trim`, `trim_start`, `trim_end`, `drop_prefix`, `drop_suffix`, `with_prefix`, `replace_each`, `replace_first`, `replace_last`, `split_first`, `split_last`, `count_utf8_bytes`, `to_utf8`, `from_utf8`, `from_utf8_lossy`, `with_ascii_lowercased`, `with_ascii_uppercased`, `caseless_ascii_equals`, `walk_utf8`, `walk_utf8_with_index` |
| `Num` | `to_str`, `compare`, `min`, `max`, `is_lt`, `is_gt`, `is_lte`, `is_gte`, `is_approx_eq`, `is_zero`, `is_positive`, `is_negative`, `is_even`, `is_odd`, `is_multiple_of`, `is_finite`, `is_infinite`, `abs`, `abs_diff`, `neg`, `ceiling`, `floor`, `round`, `pow`, `sin`, `cos`, `tan`, `atan`, `add_checked`, `sub_checked`, `mul_checked`, `add_saturated`, `sub_saturated`, `mul_saturated`, `div_checked`, `div_trunc_checked`, `div_ceil_checked`, `rem_checked`, `sqrt_checked`, `log_checked` |
| `Dict` | `empty`, `single`, `from_list`, `to_list`, `len`, `is_empty`, `get`, `contains`, `insert`, `remove`, `update`, `keys`, `values`, `walk`, `walk_until`, `map`, `join_map`, `keep_if`, `drop_if`, `insert_all`, `keep_shared`, `remove_all`, `reserve`, `clear` |
| `Set` | `empty`, `single`, `from_list`, `to_list`, `len`, `is_empty`, `contains`, `insert`, `remove`, `union`, `intersection`, `difference`, `walk`, `walk_until`, `map`, `join_map`, `keep_if`, `drop_if`, `reserve` |

### Added or changed on the way

- **`Result.try` is `Result.and_then`.** In R, `try` already means "catch an error", and a reader who
  sees `Result.try(r, f)` would expect that. `and_then` is the name Rust and Elm use for the same
  chaining. The module stays `Result` rather than Roc's newer `Try` for the same reason.
- **Positions are 1-based.** `List.get(xs, 1L)` is the first element, and `map_with_index` passes
  1-based indices, because every R reader expects that and `xs[[1L]]` agrees with it. A position is
  an `integer`, and a whole-number double literal such as `2` is accepted where one is expected.
- **Every collection is a list.** `Str.split_on` returns `list[character]` and `Str.join_with` takes
  one, so string functions compose with `List` without a conversion.
- **`List.from_vector` and `List.to_vector` are new.** R's own `as.list()` is typed
  `fn(x: Any) -> list[Any]`, so it would throw away an element type. `List.from_vector(c(1L, 2L))`
  is `list[integer]`, and `List.to_vector` goes back for an atomic element type.
- **`Str.to_integer` and `Str.to_double` replace Roc's per-type parsers** (`Str.toI64`,
  `Str.toF64`), because R has exactly those two number types.
- **`Num` checked operations return a tag per failure:** `:Overflow` for integer overflow and for a
  finite double result that overflows to infinity, `:DivByZero`, `:SqrtOfNegative`, and
  `:LogNeedsPositive`. `Num.compare` and `List.sort_with` use `:LT | :EQ | :GT`.
- **`List.split_at(list, count)` takes a count**, so `before` holds the first `count` elements and
  `others` the rest, which reads the same in 1-based and 0-based terms.
- **`Result.and_then` widens the error:** it returns `Result<B, E | F>`, so a step may fail with a
  tag of its own, as Roc's `Result.try` allows through open tag unions.
- **`Num.ceiling`, `Num.floor` and `Num.round` keep the number type.** Roc's return an integer, but
  R's `integer` is 32-bit, so a rounded double stays a `double` and `Num.to_integer_checked`
  converts it, returning `:Err(:OutOfBounds)` when it does not fit. `round` rounds half away from
  zero, as Roc does, not half to even as R's `round` does.
- **`Num.add_saturated` and its siblings clamp an integer to plus or minus 2147483647**, because
  R's smallest integer is `NA`. On doubles they are plain arithmetic.
- **`Num.pow`, `Num.sin`, `Num.cos`, `Num.tan` and `Num.atan` return a `double`**, because R has no
  wider integer for `pow` to stay in.
- **`List.sort_asc` and `List.sort_desc` take numbers only**, as Roc's do; `List.sort_with` sorts
  anything.

Readers who know Roc's newer builtins will find `fold`, `fold_until` and `fold_rev` here as
`walk`, `walk_until` and `walk_backwards`, `map_ok` as `map`, `ok_or` as `with_default`, `rev` as
`reverse`, and the per-type `plus_try` family as `Num.add_checked` and its siblings.

### Skipped

- **`Bool`**: R's `TRUE`, `FALSE`, `!`, `&&`, `||` and `identical` already cover it.
- **`Box`, `Hash`, `Inspect`, `Encode` and `Decode`**: `Box` controls Roc's memory layout, which ry's
  heap decides by itself, and the others exist to serve Roc's abilities, which ry does not have
  (traits are declined). R's `print`, `format` and `serialize` play the inspection and encoding
  roles.
- **`List.release_excess_capacity` and capacity queries**: ry's heap decides when a resizable
  vector shrinks. `List.with_capacity`, `List.reserve`, `Dict.reserve` and `Set.reserve` are taken,
  because updates now happen in place (see "Running it"). `Dict.with_capacity` and
  `Set.with_capacity` are not: an empty collection has no key vector until its first key fixes the
  key type, so there is nothing to size. Roc's `Str` capacity functions are skipped because a
  string is an immutable `CHARSXP`.
- **Bitwise operations, shifts and wrapping arithmetic** (`bitwise_and`, `shift_left_by`,
  `add_wrap` and the rest): R's 32-bit integer reserves its smallest value for `NA`, so wrapping
  has no total definition, and R's `bitwAnd` family already covers the bit operations.
  `Num.pow_int` is skipped for the same overflow reason; `Num.pow` returns a double.
- **Unchecked `sqrt` and `log`, and `asin`, `acos` and `is_nan`**: they produce `NaN` for part of
  their domain, which the library never returns. `Num.sqrt_checked` and `Num.log_checked` remain,
  and R's own functions cover the rest.
- **`Num.pi`, `Num.tau` and `Num.e`**: R's `pi` and `exp(1)` already exist, and a constant is not a
  function the stub could type as one.
- **An `Option` module**: Roc has none, and returns a `Result` with a descriptive error tag instead
  (`:Err(:ListWasEmpty)`). Code that wants an optional value writes `:Some(x) | :None` directly.
- **`List.range` and `Str.len`**: R's `seq_len` and `seq` already build ranges. A string length
  counts bytes or code points, never what a reader means by characters, which is why Roc's own
  `Str.len` only points the reader at the alternatives; `Str.count_utf8_bytes` and R's `nchar`
  remain.

## Values: R lists, R scalars and two new native kinds

The library takes R values where R has a good one, so it composes with existing code:

- A list is an R list. `List.map(xs, f)` takes any R list, including one that holds records or tags,
  and returns a new list. An atomic vector is not a list, and `List.from_vector` converts one.
- A string is a length-one `character`, and a number is a length-one `integer` or `double`. `NA`
  is refused with an error, because a ry function has no `NA` case to fall into.
- A result is a tag, `:Ok(value)` or `:Err(error)`.
- Equality, wherever the library needs it (`List.contains`, `Dict.keep_shared`), is `identical()`,
  because ry has no equality ability to dispatch on.

R has no good dictionary or set: a named list allows duplicate names and matches them partially, and
an environment holds only string keys and has reference semantics. So `Dict` and `Set` are two new
native kinds beside records, tuples and tags, with value semantics like theirs: `typeof()` is
`"dict"` or `"set"`, the implicit class is `ry_dict` or `ry_set`, and every R function that refuses
a record refuses them too, at run time and in the checker. They print as the call that rebuilds
them, `identical()` ignores insertion order (as Roc's equality does), `as.list()` converts one to a
list of `(key, value)` tuples or of elements, and `saveRDS()` and C code refuse them like any
other ry value.

Keys and elements are `logical`, `integer`, `double` or `character` scalars, never `NA` or `NaN`.
The checker's `key` constraint admits exactly those four, and the runtime refuses any other type
that reaches it through `Unknown` code. Keys compare exactly, `-0` equals `0`, and the integer `1L` and the double `1` are the same
key, because R's own `==` treats them as equal. A dictionary keeps insertion order, as Roc's does,
so `Dict.to_list` and `Dict.walk` are deterministic.

## Typing

The signatures live in a shipped stub file, `types/ry.Rtypes`, read as the stubs of a namespace
named `ry`. That namespace is always active, like `base`, so the names resolve in every file and
hover names `ry` as their home. Every signature is a rank-1 generic in the existing type grammar:

```
@alias Result<T, E> {:Ok(T) | :Err(E)}
@type Dict<K, V>
@type Set<T>

Result.map : <A, B, E> fn(result: Result<A, E>, f: fn(A) -> B) -> Result<B, E>
List.walk : <T, S> fn(list: list[T], state: S, f: fn(S, T) -> S) -> S
List.first : <T> fn(list: list[T]) -> Result<T, :ListWasEmpty>
List.sum : <T: scalar numeric> fn(list: list[T]) -> T
Dict.get : <K: key, V> fn(dict: Dict<K, V>, key: K) -> Result<V, :KeyNotFound>
```

Three additions to the checker make this possible:

- `@alias NAME<T, ...> {TYPE}` in a stub declares a structural alias, exactly as `#: @alias` does
  in a project, so `Result<integer, character>` works in every annotation. A project's own
  `Result` alias shadows the shipped one. The typing reference's statement that no generic tag
  union is built in changes to name `Result`.
- `@type NAME<T, ...>` in a stub declares a generic opaque type, so `Dict<character, double>` is a
  type that only the library's functions produce or read. The checker treats the `Dict` and `Set`
  nominals as native values, so `sum(dict)` is a `type-mismatch` before it runs.
- The rule that a record, tuple or tag passed to an R function is a `type-mismatch` exempts
  functions the `ry` namespace declares. The runtime marks the same functions on their builtin
  definitions, and a test compares the stub's names with the runtime's table, so the stub is the
  one list and nothing is exempt by spelling alone (`List.mapp` is still an unknown name).

The checker needs no new inference: unifying `:Ok(integer) | :Err(character)` against
`:Ok(A) | :Err(E)` already binds both variables member by member.

## Running it

The functions are native Rust builtins in `crates/interp/src/standard_library.rs`, bound in
`package:ry` at startup, so they are always available and cost nothing beyond registering the
builtins. Higher-order functions call the R function they are given through the interpreter's
ordinary call path, so a closure, a builtin and a function that uses `match` all work.

Errors are R errors that name the function. A wrong argument kind (`List.map("a", f)`) stops with a
message naming the parameter and the value it needs.

`Dict` and `Set` hash their keys into an open-addressing index stored in an integer vector next to
the keys, so a lookup is a hash and a probe, and the collector traces the index like any other
vector.

Updates happen in place when nothing else can observe the value, the way Roc and Koka reuse a
uniquely held value. For `x <- f(x, ...)` where `f` is flagged `reusing_first_argument`, the
evaluator checks that the binding being reassigned holds the value's only reference and that no
other argument is the same value, and passes it as `Call::reusable`. A temporary with reference
count zero is reusable as well. A dictionary's and a set's key and value vectors are resizable and
double when full, so a loop of `d <- Dict.insert(d, k, v)` costs amortized constant time per
insert. Any other holder (a second binding, a list, a closure's environment) makes the function
copy first, so value semantics never change.

Two extensions widen the `x <- f(x, ...)` form (`decisions.md`, "In-place updates through nested
targets"):

- A nested target, `state$seen <- Dict.insert(state$seen, k, v)`, reuses when the target is a chain
  of `$` or `[[` with a name, a string or a number, every list on the chain has one reference, and
  no other argument is the value or one of those lists.
- Compiled code keeps values in registers that do not count as references. An environment that a
  compiled body runs on is flagged, and an update from outside that body (`eval.parent()`, a forced
  promise) reuses only a value no register holds.

A closure never receives the value for reuse. Moving the caller's value into the parameter was
tried and removed: after the callee updates it in place, the caller's variable no longer holds the
old value, and a second argument, a nested function, `get()` and an error unwinding through the
assignment can all read that variable.

## Open questions

- A check-time diagnostic when a project that targets GNU R calls a library function, once a
  project can declare its runtime. A pure-R build for GNU R was declined: many functions return
  result tags, which GNU R cannot represent, and a partial build would split the library into two
  dialects.
- A helper that updates a collection it receives copies on every call, so `d <- helper(d, k)` in a
  loop is quadratic. Reusing the value needs proof that nothing reads the caller's variable until
  the assignment completes, which takes escape analysis of the callee and an undo path for errors.
