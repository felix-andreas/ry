---
title: Tour
description: Every important ry feature, one short example each
---

This page walks through what ry does, one example at a time. It assumes you know R and nothing about
ry. Every output below comes from a real run.

## Commands

```sh
ry check      # report problems in the project
ry fmt        # format the project in place
ry server     # the language server; your editor starts it
```

None of these runs your code or needs R installed. [Installation](/installation) lists the ways to
get the binary.

## Names

With no configuration, `ry check` reports names that resolve nowhere:

```r
apply_discount <- function(price, rate) {
  price * ratee
}
```

```text
unresolved

  ! I could not resolve `ratee` in this package, its imports, or builtins. Did you mean `rate`?
   --[R/demo.R:2:11]
 1 | apply_discount <- function(price, rate) {
 2 |   price * ratee
   |           ^^^^^
```

It also reports unused assignments and names defined twice across a package's files. Every finding
has a [diagnostic code](/reference/diagnostic-codes).

## Type checking

Type errors are opt-in:

```toml
# ry.toml
[check]
typing = true
```

Types are inferred from how values are used. Nothing here is annotated:

```r
apply_discount <- function(price, rate) price * rate

apply_discount(100, "0.2")
```

```text
type-mismatch

  x expected `double`, found `character`
   --[R/demo.R:3:21]
 3 | apply_discount(100, "0.2")
   |                     ^^^^^
```

`*` is arithmetic, so both parameters are numbers. Inference runs whether or not type errors are
reported, because hover, completion, and inlay hints use it.

## Annotations

An annotation is a `#:` comment above the definition, so annotated code is still ordinary R:

```r
#: fn(price: double, rate: double) -> character
apply_discount <- function(price, rate) price * rate
```

```text
type-mismatch

  x expected `character`, found `double`
   --[R/demo.R:2:41]
 2 | apply_discount <- function(price, rate) price * rate
   |                                         ^^^^^^^^^^^^
```

The annotation is checked against the body and against every call. Annotate where code meets other
code: exported functions, values read from files, and anything you want held to a contract.

The same signature can be written one directive per line:

```r
#: @param price {double}
#: @param rate {double}
#: @return {double}
apply_discount <- function(price, rate) price * rate
```

## Vectors

Types use R's names: `logical`, `integer`, `double`, `complex`, `character`, `raw`, `NULL`. A
suffix gives the shape:

| Type | Meaning | Example |
| --- | --- | --- |
| `integer` | length one | `1L` |
| `integer[]` | any length | `c(1L, 2L)` |
| `integer[named]` | any length, with names | `c(a = 1L, b = 2L)` |

Literals are length one. A scalar is accepted where a vector is expected, but not the reverse.
Values widen along `logical` < `integer` < `double` < `complex`, so an `integer` is accepted as a
`double`. `character` is never reached implicitly.

## Lists

`list()` is typed by how it is built:

| Type | Built by |
| --- | --- |
| `list{name: character, age: integer}` | `list(name = "Ada", age = 36L)` |
| `list{integer, character}` | `list(1L, "ok")` |
| `list[integer \| character]` | `list(1L, b = "x")`, or an annotation |
| `list[named: integer]` | an annotation |

Fields of a record are checked:

```r
person <- list(name = "Ada", age = 36L)
person$nmae
```

```text
type-mismatch

  x field `nmae` does not exist in `list{name: character, age: integer}`. Did you mean `name`?
```

## `NULL` and narrowing

`|` writes a union. A value that may be `NULL` must be checked before use:

```r
#: fn(config: list{retries: integer} | NULL) -> integer
retries <- function(config) {
  config$retries
}
```

```text
type-mismatch

  x expected a list, found `list{retries: integer} | NULL`
```

An `is.null()` guard narrows the type, so this version is clean:

```r
#: fn(config: list{retries: integer} | NULL) -> integer
retries <- function(config) {
  if (is.null(config)) return(3L)
  config$retries
}
```

`is.character()`, `is.numeric()`, `is.list()`, and the other `is.*` tests narrow the same way.
Narrowing applies to a plain variable in an `if` condition, not to `x$field` or a condition joined
with `&&`.

## Functions

```r
#: fn(name: character, [greeting]: character) -> character
greet <- function(name, greeting = "hello") paste(greeting, name)

#: fn(...: character) -> character
shout <- function(...) toupper(paste(...))
```

`[greeting]` is optional, and a parameter with a default must be declared optional. `...: T` checks
every extra argument against `T`. Calls are checked for argument types, names, and count:

```r
greet("Ada", greeting = 1L)  # expected `character`, found `integer`
greet()                      # the function requires 1 argument, and this call supplies 0
shout("a", "b", 3L)          # expected `character`, found `integer`
```

## Generics

A function whose parameters nothing constrains is generic, and an operation can bound it. These are
the types hover shows for unannotated code:

```r
identity2 <- function(x) x           # <T> fn(x: T) -> T
increment <- function(x) x + 1L      # <T: numeric> fn(x: T) -> T
```

`increment("a")` is an error. Written by hand, the binder goes first:

```r
#: <T> fn(items: list[T]) -> T
pick <- function(items) items[[1L]]
```

`pick(list(1L, 2L))` is `integer`. The two constraints are `numeric` and `atomic`.

## Named types

`@type` declares a type that is distinct from everything else, even from types with the same
representation. `@new` is the only way to create a value of it:

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

to_fahrenheit(32)
to_fahrenheit(to_fahrenheit(freezing))
```

```text
x expected `Celsius`, found `double`
x expected `Celsius`, found `Fahrenheit`
```

At run time both are plain numbers, and arithmetic on them still works. A record type works the same
way, as `@type Person {list{name: character}}`, and takes parameters, as `@type Box<T> {list{value: T}}`.

`@alias` names a type without making it distinct, so a value of the underlying type is accepted:

```r
#: @alias Row {list{id: integer, label: character}}
```

## `Any` and `Unknown`

Both are compatible with every type. `Unknown` means the checker could not work a type out, as for
an S4 slot or a data frame column. `Any` means a declaration chose not to check, as the shipped
declaration of `readRDS()` does. A gap in what the checker knows therefore skips a check rather than
producing a false error.

Two annotations override inference:

```r
#: @trust integer
count <- readRDS("count.rds")        # take my word for it

#: @if-unknown integer
n <- some_unmodelled_value           # only where the type is Unknown
```

## Strict mode

A clean run says no contradictions were found, not that everything was checked. Strict mode reports
each place a type could not be determined:

```toml
[check]
typing = true
strict = true
```

```r
#: fn(df: data.frame) -> double
total <- function(df) sum(df$amount)
```

```text
strict

  x strict mode: this expression has an undetermined type (`Unknown`)
   --[R/demo.R:2:27]
 2 | total <- function(df) sum(df$amount)
   |                           ^^^^^^^^^
```

A comment at the top of a file overrides the project setting for that file:

```r
# typing: off      # or: on, strict
```

## Data frames and data masking

A `data.frame` is a type of its own, but its columns are not typed: `df$amount` is `Unknown`, so it is
neither checked nor reported. Inside a data-masking call, a bare name is a column, not a variable:

```r
library(dplyr)

#: fn(sales: data.frame) -> data.frame
by_region <- function(sales) {
  sales |>
    filter(amount > 0) |>
    mutate(net = amount * (1 - fee))
}
```

This checks clean. `dplyr`, `data.table`, `ggplot2`, and `testthat` ship with typed declarations,
and `with()`, `subset()`, and `transform()` are recognized too.

## Packages

ry knows base R, the default packages, and the export lists of common CRAN packages. For anything
else, add a declaration file under `stubs/`, named after the package:

```
# stubs/dbclient.Rtypes
@type Session
connect : fn(host: character) -> Session
```

```r
session <- dbclient::connect("localhost")
dbclient::conect("localhost")
```

```text
! `conect` is not exported by `dbclient`.
```

The same file can override a shipped declaration. In a package, ry reads `DESCRIPTION` and
`NAMESPACE`, and reports an `importFrom()` of a name a known package does not export.

## Suppressing a finding

```r
total = 2L  # ry: allow(assignment-operator)

# ry: allow(unused)
scratch <- 1L
```

The comment covers its own line, or the line below it.

## Formatting

`ry fmt` fixes spacing, indentation, and braces, and keeps your line breaks:

```r
x<-c(1,2,3)
if(x>1){y<-2}
```

```r
x <- c(1, 2, 3)
if (x > 1) { y <- 2 }
```

A call you wrote on one line stays on one line, and a call you spread over several stays that way.
`# fmt: skip` leaves one expression alone. `ry fmt --check` reports without writing, for CI.

## Editors

`ry server` gives any LSP editor hover with inferred types, completion (including record fields),
go-to-definition, references, rename, signature help, and inlay hints. These work with type errors
switched off. The [VS Code extension](/installation#vs-code) bundles the binary.

## Next

- [Type system reference](/reference/type-system): every rule
- [Limitations](/type-checking/limitations): what is not checked
- [Adopting an existing codebase](/guides/adopting): turning type checking on gradually
- [Configuration](/reference/configuration): every `ry.toml` key
