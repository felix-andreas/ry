---
title: Tour
description: What ry checks, how its type system works, and why it works that way
---

ry is a language server, checker, formatter, and console for R. Everything except the console works
from the source text and never runs it, so it needs no R installation.

```sh
ry check    # report problems
ry fmt      # format files in place
ry server   # the language server; your editor starts it
ry repl     # an R console with typed completion
```

`ry check` reads `.R` files and the R chunks of `.Rmd`, `.qmd`, and `.Rnw` documents.

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

With no configuration, ry resolves every name by R's scoping rules. In R, a typo surfaces only when
execution reaches it, possibly at the end of a long job. ry reports it immediately:

```r
apply_discount <- function(price, rate) price * (1 - ratee)
```

```text
! I could not resolve `ratee` in this package, its imports, or builtins. Did you mean `rate`?
```

Files in a package's `R/` share one namespace, a script is read top to bottom, and `library()` calls
and `NAMESPACE` imports bring names into scope. The same analysis reports assignments nothing reads,
a top-level name defined twice in a package (R silently keeps one of them), `pkg::name` where the
package does not export `name`, and a `NAMESPACE` that imports or exports a name that does not exist,
which would stop the package from loading.

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
apply_discount <- function(price, rate) price * (1 - rate)
apply_discount(100, "0.2")
```

```text
x expected a numeric value (`integer` or `double`), found `character`
```

`-` and `*` are arithmetic, so both parameters must be numbers. Inference runs even with type errors
off, because hover, completion, and inlay hints are built on it.

## Annotations

R has no syntax for types, so annotations are `#:` comments. The file stays valid R that every other
tool can read, and removing ry changes nothing at run time.

```r
#: fn(price: double, rate: double) -> double
apply_discount <- function(price, rate) price * (1 - rate)
```

An annotation is checked against the body and against every call. Inference already covers the
inside of a function, so annotate where code meets other code: exported functions, values read from
files, and anything whose contract you want enforced. A value read from a file has no type until you
give it one, and then every use of it is checked:

```r
#: list{name: character, age: integer}
person <- readRDS("person.rds")
```

## Scalars and vectors

R has no scalars: `1L` is an integer vector of length one. But an `if` condition, an operand of
`&&`, and an endpoint of `:` must have exactly one element, and passing a longer vector there is a bug
R reports late or not at all. So ry tracks the length-one case separately:

```r
n    <- 1L                     # integer: exactly one element
ids  <- c(1L, 2L)              # integer[]: any length
ages <- c(ada = 36L, bo = 4L)  # integer[named]: any length, with names
```

A scalar is accepted where a vector is expected, but not the reverse, because a vector of unknown
length may not have exactly one element. A declared `double` accepts an `integer` or a `logical`,
which R converts without loss. A number is never accepted where `character` is expected: R would
convert it too, but a silent number-to-string conversion is usually a bug, so ry asks for an explicit
`as.character()`.

## Lists

R uses one list type for four different jobs, and each job has its own mistakes, so ry gives each
its own type:

```r
# tuple-like: list{integer, character}
pair <- list(1L, "ok")

# record-like: list{name: character, age: integer}
person <- list(name = "Ada", age = 36L)

# list-like: list[integer]
sizes <- lapply(words, nchar)

# dict-like: declared, because a literal infers as a record
#: list[named: integer]
counts <- list(apples = 3L)
```

At run time all four are the same R list, so nothing changes in your code. The difference is what
can be checked. A tuple-like or record-like list has a fixed shape, so ry knows each position and
field and its type. That makes `$` checkable, which matters because R answers a misspelled field with
a silent `NULL` that fails somewhere else:

```r
person$nmae
```

```text
x field `nmae` does not exist in `list{name: character, age: integer}`. Did you mean `name`?
```

A fixed shape is also exact, so passing a list with an extra field where a record is expected is an
error: an unexpected field is usually a misspelled one.

A list-like or dict-like list can have any length, so ry knows only its element type. Reading a key
gives the element type or `NULL`, because the key may be missing:

```r
counts[["pears"]]                        # integer | NULL
```

A `list(...)` literal infers as a fixed shape, so a dictionary that starts from a literal needs an
annotation, as `counts` does. An empty `list()` filled by computed keys, as in `d[[key]] <- value`,
needs none. Filled by literal keys, it becomes a record.

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

`[greeting]` marks a parameter callers may omit. ry requires a default in the definition to match it,
so that an omitted argument always has a value. `...: character` checks every extra argument.
Parameter names are part of the type, because R matches arguments by name as often as by position.
Calls are checked for types, names, and count.

Your own functions have exactly one signature; to accept several shapes, use a union such as
`integer | character`. Several signatures per name would make every call a search over candidates
instead of a single inference step, and would leave the function without one type to pass around as
a value. The shipped declarations do overload a few base functions such as `sum`, where the
candidates are written out and the search stays small.

## Generics

A function is as general as its body allows. Hover shows the inferred types:

```r
identity2 <- function(x) x           # <T> fn(x: T) -> T
increment <- function(x) x + 1L      # <T: numeric> fn(x: T) -> T
```

`T` stands for any type, and `T: numeric` for `integer` or `double`, so `increment(2.5)` is a
`double` and `increment("a")` is an error.

## Structural and nominal types

Every type so far is *structural*: two types are the same if they have the same shape. Any
`list(name = "Ada")`, wherever it was built, is a `list{name: character}`, and every `double` is
interchangeable with every other. That is the right default for R, where values are plain data, and
it is why inference needs no declarations.

But structure cannot tell Celsius from Fahrenheit, both `double`, or a user ID from an email
address, both `character`. A *nominal* type is distinct by name, even
from a type with the same representation. `@type` declares one, and `@new` creates a value of it:

```r
#: @type Money {list{amount: double, currency: character}}

#: fn(amount: double, currency: character) -> Money
money <- function(amount, currency) {
  if (amount < 0) stop("negative amount")
  #: @new Money
  structure(list(amount = amount, currency = currency), class = "Money")
}
```

A list with the right fields is still not a `Money`, so passing one where a `Money` is expected is an
error. Put `@new` in one constructor and nowhere else, and every `Money` in the program has passed
its checks: `@new` checks the shape when ry analyzes the code, and `stop()` checks the values when
it runs. Scalars work the same way: with `@type UserId {character}` and `@type Email {character}`,
passing an `Email` where a `UserId` is expected is an error, although both are strings at run time.

A nominal value is still accepted where its representation is, so `nchar()` works on a `UserId`, and
adding two values of `@type Celsius {double}` gives a plain `double`: ry cannot assume that the sum
of two temperatures is a temperature. `Money` is a list, so `+` on it is an error until you declare
the operator:

```r
#: fn(a: Money, b: Money) -> Money
`+.Money` <- function(a, b) money(a$amount + b$amount, a$currency)
```

Now `money(1, "EUR") + money(2, "EUR")` is a `Money`. R finds `+.Money` through the class attribute,
which is why the constructor sets one; ry itself needs no class to check the type.

A type can take parameters: with `@type Page<T> {list{items: list[T], total: integer}}`, reading
`items` from a `Page<integer>` gives a `list[integer]`. `@alias` names a type without making it
nominal, so `@alias Row {list{id: integer}}` is just a shorter way to write the shape.

Nominal types describe values. For shared mutable state, inheritance, or method dispatch, R6 and S4
are still the tools, and their objects are `Unknown`. To check who receives one, give it a nominal
type over `Any` and wrap its constructor, so that passing anything else is an error:

```r
#: @type Account {Any}

#: fn() -> Account
new_account <- function() {
  #: @new Account
  AccountClass$new()
}
```

## `Unknown` and strict mode

R has constructs no static checker can follow: S4 and R6 objects, data frame columns, `eval()`. Their
values are `Unknown`, which is compatible with everything. ry would rather skip a check than report
something false, so one unmodeled construct never causes a cascade of errors.

The cost is that a clean run does not say how much was checked. Strict mode reports the places
where a value became `Unknown`:

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

`Any` is also compatible with everything, but a value declared `Any`, such as the result of a
function declared `-> Any`, is a deliberate choice, so strict mode ignores it. Calls to S3 generics
that dispatch through `UseMethod()` return `Any` and are not reported either. Strict mode does not
yet report R6 objects or values from packages ry knows only by name, and it wrongly reports a
`stop()` guard such as the one in `money()` above.

A plain `#:` annotation gives an `Unknown` value a type, and every later use is checked against it.
`#: @if-unknown TYPE` does the same, but becomes an error once ry can infer the value's type, so it
cannot go stale. `#: @trust TYPE` overrides a type ry did infer, for the cases where you know
better. In strict mode none of them clears the finding yet, which is a known bug;
`# ry: allow(strict)` on the line does.

A `# typing: strict`, `# typing: on`, or `# typing: off` comment in a file overrides the project
setting, so you can adopt type checking one file at a time.

## Data frames

Columns are not typed, so `df$amount` is `Unknown`. Inside data-masking functions, a bare name
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
`NAMESPACE`. Until then, `mutate` is an unresolved name, which is what R would say too.

## Packages

ry does not load R, so it cannot ask a package what it exports. Declarations ship for base R, the
default packages, and dplyr, data.table, ggplot2, and testthat, and export lists ship for the rest
of the tidyverse and other common packages.

This matters beyond types. Once you attach a package ry knows nothing about, any bare name might come
from it, so ry stops reporting unresolved names across the project. A two-line
[declaration file](/type-checking/stubs) turns the check back on:

```
# stubs/dbclient.Rtypes
@type Session
connect : fn(host: character) -> Session
```

## Formatting

`ry fmt` normalizes spacing, indentation, and braces:

```r
x<-c(1,2,3)                 # becomes  x <- c(1, 2, 3)
if(x>1){y<-2}               # becomes  if (x > 1) { y <- 2 }
```

It never reflows code to a column limit, because a formatter that does rewrites lines you did not
touch and buries your change in layout churn. Your line breaks decide the layout instead: a call
written on one line stays on one line, and a call broken across lines gets one argument per line.
Every loop body, and every `if` or function body that spans lines, gets braces, so a line added later
cannot fall outside the block it belongs to. The only settings are indent width and line endings.

## Editors

`ry server` provides hover with inferred types, completion (including record fields), go-to
definition, references, rename, signature help, inlay hints, and formatting in any LSP editor. Rename edits the
binding you picked and nothing else: a local `total`, a global `total`, and the word "total" in a
string are three different things.

## The console

`ry repl` runs the R installed on your machine, unchanged, behind a line editor whose Tab completion
comes from the type checker. Completing `account$` lists the record's fields with their types, worked
out from the code you typed rather than from the live session. `ry run script.R` runs a script
through the same embedded R and exits. These are the only commands that need R.

## Findings and suppressions

Every finding carries a code above its message, such as `unresolved`, `type-mismatch`, or
`assignment-operator`:

```text
assignment-operator

  ! Use <-, not =, for assignment
```

Besides names and types, three lints are on by default: `=` for assignment, `T` and `F` for `TRUE`
and `FALSE`, and a trailing comma in a call, which is an error because `c(1, 2, )` fails when it
runs. [Diagnostic codes](/reference/diagnostic-codes) gives the reason for each.

Codes are what you configure in `ry.toml` and what you name to silence one finding:

```r
total = 2L  # ry: allow(assignment-operator)
```

A suppression covers its own line and the line below, so it can sit at the end of the line or above
it, and nothing else. To turn a lint off everywhere, set it in [`ry.toml`](/reference/configuration).

In CI, `ry check` exits with status 1 when it finds anything, and `ry fmt --check` does when a file
is not formatted, so a CI job needs nothing more than those two commands.

## Next

- [Stubs](/type-checking/stubs): describing a package ry does not know
- [Limitations](/type-checking/limitations): what is not checked, and how much that matters
- [Type system](/reference/type-system): every rule, precisely
