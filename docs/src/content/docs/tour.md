---
title: Tour
description: What ry checks, how its type system works, and why it works that way
---

ry is a language server, checker, and formatter for R. It reads source text and never runs it, so
it gives the same answers in your editor, in CI, and on code that has never been executed.

```sh
ry check    # report problems
ry fmt      # format files in place
ry server   # the language server; your editor starts it
```

## Syntax errors

R's parser is built to run code, so it stops at the first error and names the token it choked on.
ry's parser is built for tooling: it names what is missing and keeps analyzing the rest of the file.

```r
config <- list(
  title = "Revenue"
  subtitle = "by quarter"
)
```

```text
R:   Error: unexpected symbol in: "  title = "Revenue"
     subtitle"

ry:  x missing `,` between these arguments
      --[R/config.R:2:20]
    2 |   title = "Revenue"
      |                    ^
```

## Names

With no configuration, ry resolves every name the way R would at run time. A typo in R surfaces only
when execution reaches it, possibly at the end of a long job. ry reports it immediately:

```r
apply_discount <- function(price, rate) price * ratee
```

```text
! I could not resolve `ratee` in this package, its imports, or builtins. Did you mean `rate`?
```

Resolution follows R's rules: files in a package's `R/` share one namespace, a script is read top to
bottom, and `library()` calls and `NAMESPACE` imports bring names into scope. The same analysis
reports assignments nothing reads, and top-level names defined twice in a package, where R would
silently keep whichever file is sourced last.

## Types

Type errors are opt-in, because an existing codebase can have many and you should choose when to
face them:

```toml
# ry.toml
[check]
typing = true
```

You do not have to annotate anything. ry infers types from how values are used:

```r
apply_discount <- function(price, rate) price * rate
apply_discount(100, "0.2")
```

```text
x expected `double`, found `character`
```

`*` is arithmetic, so both parameters must be numbers. Inference runs even with type errors off,
because hover, completion, and inlay hints are built on it.

## Annotations

R has no syntax for types, so annotations are `#:` comments. The file stays valid R that every other
tool can read, and removing ry changes nothing at run time.

```r
#: fn(price: double, rate: double) -> double
apply_discount <- function(price, rate) price * rate
```

An annotation is checked against the body and against every call. Inference already covers the
inside of a function, so annotate where code meets other code: exported functions, values read from
files, and anything whose contract you want enforced.

## Scalars and vectors

R has no scalars: `1L` is an integer vector of length one. But an `if` condition, an operand of
`&&`, and an endpoint of `:` must have exactly one element, and passing a longer vector there is a bug
R reports late or not at all. So ry tracks the length-one case separately:

| Type | Meaning |
| --- | --- |
| `integer` | length one, such as `1L` |
| `integer[]` | any length, such as `c(1L, 2L)` |
| `integer[named]` | any length, with names |

A scalar is accepted where a vector is expected, but not the reverse, because a vector of unknown
length may be empty. Numbers widen along `logical` < `integer` < `double` < `complex`, as in R. A
number is never accepted where `character` is expected: R would convert it, and when that happens
silently it is usually a bug, so ry asks for an explicit `as.character()`.

## Lists

R has one list type, but programs use it in four different roles, and each role has its own
mistakes. ry gives each role its own type:

| Role | Names | Elements | Length | Type | Example |
| --- | --- | --- | --- | --- | --- |
| tuple-like | none | heterogeneous | fixed | `list{integer, character}` | `list(1L, "ok")` |
| list-like | none | homogeneous | dynamic | `list[integer]` | results appended in a loop |
| record-like | named | heterogeneous | fixed | `list{name: character, age: integer}` | `list(name = "Ada", age = 36L)` |
| dict-like | named | homogeneous | dynamic | `list[named: integer]` | counts keyed by category |

At run time all four are the same R list, so nothing changes in your code. The difference is what
can be checked. A tuple-like or record-like list has a fixed shape, so ry knows which positions and fields exist and
what type each one has. That makes `$` checkable, which matters because R answers a misspelled field
with a silent `NULL` that fails somewhere else:

```r
person <- list(name = "Ada", age = 36L)
person$nmae
```

```text
x field `nmae` does not exist in `list{name: character, age: integer}`. Did you mean `name`?
```

A list-like or dict-like list can grow, so ry knows only the element type, and reading a key from a
dict-like list gives `T | NULL`, because the key may be missing. ry infers the fixed shapes from
`list(...)` literals; the growing ones come from annotations, or from code that adds elements with
computed keys.

## `NULL`

Many R functions return `NULL` for "nothing", and code that forgets the case fails far from where the
`NULL` came from. A value that may be `NULL` has a union type, and must be checked before use:

```r
#: fn(config: list{retries: integer} | NULL) -> integer
retries <- function(config) {
  if (is.null(config)) return(3L)
  config$retries
}
```

Without the `if`, `config$retries` is an error. The guard narrows `config` to the list for the rest
of the function. `is.character()`, `is.numeric()`, `is.list()`, and the other `is.*` tests narrow the
same way. Narrowing applies to variables only, so to test a field, copy it into a variable first:
`r <- config$retries; if (is.null(r)) ...`.

## Functions

```r
#: fn(name: character, [greeting]: character, ...: character) -> character
greet <- function(name, greeting = "hello", ...) paste(greeting, name, ...)
```

`[greeting]` marks a parameter callers may omit, and it must match a default in the definition,
since that is what lets R omit it. `...: character` checks every extra argument. Parameter names are
part of the type, because R matches arguments by name as often as by position. Calls are checked for
types, names, and count.

## Generics

A function is as general as its body allows. Hover shows the inferred types:

```r
identity2 <- function(x) x           # <T> fn(x: T) -> T
increment <- function(x) x + 1L      # <T: numeric> fn(x: T) -> T
```

`increment("a")` is an error. Your own functions have exactly one signature; to accept several
shapes, use a union such as `integer | character`. Allowing several signatures per name would make
each call a search over candidates instead of a single inference step, which is what keeps checking
fast enough to run on every keystroke.

## Named types

A `double` cannot tell Celsius from Fahrenheit, and a `character` cannot tell a user ID from an
email address. `@type` declares a type that is distinct even from types with the same
representation, and `@new` is the only way to create a value of it:

```r
#: @type Celsius {double}

#: fn(value: double) -> Celsius
celsius <- function(value) {
  if (value < -273.15) stop("below absolute zero")
  #: @new Celsius
  value
}
```

Passing a plain `double` or a `Fahrenheit` where a `Celsius` is expected is an error. Because `@new`
can only appear where you write it, putting it in one constructor means every `Celsius` in the
program passed that constructor's checks. At run time the value is still a plain number: arithmetic
works and nothing is wrapped. `@alias` is the opposite: a shorthand that stays interchangeable with
the type it names.

[Domain modeling](/type-checking/domain-modeling) covers records, generic types, and when R6 is still
the better tool.

## `Unknown` and strict mode

R has constructs no static checker can follow: `UseMethod` dispatch, S4 and R6 objects, data frame
columns. Their values are `Unknown`, which is compatible with everything. ry would rather skip a
check than report something false, so one unmodeled construct never causes a cascade of errors.

The cost is that a clean run does not say how much was checked. Strict mode reports every place a
value became `Unknown`:

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
x strict mode: this expression has an undetermined type (`Unknown`)
```

`Any` is also compatible with everything, but it is a deliberate choice, made by an annotation or a
package declaration, so strict mode ignores it. Two annotations override inference: `#: @trust TYPE`
asserts a type the checker cannot verify, and `#: @if-unknown TYPE` fills in a type only where
inference found none.

A comment at the top of a file overrides the project setting, so you can adopt typing one file at a
time:

```r
# typing: strict    # or: on, off
```

## Data frames

Columns are not typed yet, so `df$amount` is `Unknown`. Inside data-masking functions, a bare name
refers to a column, so ry does not report it as unresolved:

```r
library(dplyr)

by_region <- function(sales) {
  sales |>
    filter(amount > 0) |>
    mutate(net = amount * (1 - fee))
}
```

dplyr's verbs, `data.table`'s `[`, and base `with()`, `subset()`, and `transform()` are recognized.
A package's declarations switch on once the project uses it, through `library()`, `DESCRIPTION`, or
`NAMESPACE`, so that names like `filter` stay unresolved in projects that never load dplyr.

## Packages

ry does not load R, so it cannot ask a package what it exports. Declarations ship for base R, the
default packages, and dplyr, data.table, ggplot2, and testthat, and export lists ship for the rest
of the tidyverse and other common packages.

This matters beyond types. Once you attach a package ry knows nothing about, any bare name might come
from it, so ry stops reporting unresolved names across the project. A two-line declaration file turns
the check back on:

```
# stubs/dbclient.Rtypes
@type Session
connect : fn(host: character) -> Session
```

## Formatting

`ry fmt` fixes spacing, indentation, and braces, and leaves line breaks where you put them:

```r
x<-c(1,2,3)                # becomes  x <- c(1, 2, 3)
if(x>1){y<-2}              # becomes  if (x > 1) { y <- 2 }
```

A formatter that reflows code to a column limit rewrites lines you did not touch, so every diff
shows layout churn instead of your change. ry keeps a one-line call on one line and a multi-line
call multi-line, and only adds braces where leaving them out would change what a later edit means.
The only settings are indent width and line endings.

## Editors

`ry server` provides hover with inferred types, completion (including record fields), go-to
definition, references, rename, signature help, and inlay hints in any LSP editor. Rename edits the
binding you picked and nothing else: a local `total`, a global `total`, and the word "total" in a
string are three different things.

## Suppressing a finding

```r
total = 2L  # ry: allow(assignment-operator)
```

A suppression covers only its own line, or the line below it. A file-wide switch would also hide
mistakes added later.

## Next

- [Getting started](/getting-started): installation and the first run
- [Limitations](/type-checking/limitations): what is not checked, and how much that matters
- [Type system](/reference/type-system): every rule, precisely
