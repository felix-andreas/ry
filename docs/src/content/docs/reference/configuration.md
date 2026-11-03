---
title: Configuration
description: Every ry.toml key, which file applies, and the editor settings
---

Everything that changes what ry reports lives in one `ry.toml`, so the editor, the command line, and
CI always agree. This is every key, with its default:

```toml
[check]
typing = false           # report type errors
strict = false           # report unknown types and untyped parameters; implies typing
inline-annotations = true  # type inline annotations such as `x: integer <- 1L`
object-systems = true    # allow S4, R6 and Reference Classes; false makes each definition an error
prelude = true           # put the standard library in scope by bare name; false requires `std::`
unused = true            # report values that are never read
maybe-undefined = false  # report reads some path reaches before a write
exclude = []             # gitignore-style patterns that ry check skips

[lint]                   # each takes "off", "warn", "error", or "default"
assignment-operator = "warn"  # `=` used for assignment
boolean-shorthand = "warn"    # T or F instead of TRUE or FALSE
trailing-comma = "warn"       # a comma after a call's last argument
equals-na = "warn"            # x == NA, which is always NA
vector-logic = "warn"         # & or | in an if or while condition
length-sequence = "warn"      # 1:length(x), which counts down when x is empty
class-equals = "warn"         # class(x) == "name" instead of inherits()
unreachable-code = "warn"     # statements after return(), stop(), break or next
sprintf-arguments = "warn"    # a sprintf() format that wants more or fewer values
length-test = "warn"          # length(x == y) instead of length(x) == y
redundant-return = "off"      # return() as a function's last expression
commented-code = "off"        # a comment that is commented-out code
duplicate-argument = "off"    # the same argument name twice in one call
any-is-na = "off"             # any(is.na(x)) instead of anyNA(x)
paste-sep = "off"             # paste(..., sep = "") instead of paste0(...)
lengths = "off"               # sapply(x, length) instead of lengths(x)
literal-coercion = "off"      # as.integer(1) instead of 1L
redundant-equals = "off"      # x == TRUE instead of x
single-quotes = "off"         # 'text' instead of "text"
testthat-expectation = "off"  # expect_equal(x, NULL) instead of expect_null(x)
condition-message = "off"     # stop(paste0(...)) instead of stop(...)
comparison-negation = "off"   # !(x == y) instead of x != y
outer-negation = "off"        # all(!x) instead of !any(x)
fixed-regex = "off"           # a regex with no regex syntax and no fixed = TRUE
which-grepl = "off"           # which(grepl(...)) instead of grep(...)
length-levels = "off"         # length(levels(x)) instead of nlevels(x)
browser-call = "off"          # a browser() call left in the code
numeric-leading-zero = "off"  # .5 instead of 0.5
implicit-assignment = "off"   # an assignment inside an if or while condition
while-true = "off"            # while (TRUE) instead of repeat
scalar-in = "off"             # x %in% 1 instead of x == 1
order-subset = "off"          # x[order(x)] instead of sort(x)
matrix-apply = "off"          # apply(x, 1, sum) instead of rowSums(x)
empty-assignment = "off"      # x <- {} instead of x <- NULL
unnecessary-lambda = "off"    # lapply(x, function(y) f(y)) instead of lapply(x, f)
yoda-test = "off"             # expect_equal(2, x) instead of expect_equal(x, 2)
nzchar = "off"                # nchar(x) == 0 instead of !nzchar(x)
sample-int = "off"            # sample(1:n, k) instead of sample.int(n, k)
unnecessary-concatenation = "off" # c("a") or c() around a single constant
line-length = "off"           # a line longer than max-line-length characters
max-line-length = 80          # the limit line-length checks
unused-parameter = "off"      # a parameter the body never reads
unused-import = "off"         # a NAMESPACE importFrom nothing uses
shadows-builtin = "off"       # a top-level name that hides a base function
shadows-namespace = "off"     # a top-level name that hides another export
# naming-style = "snake_case" # or "camelCase"; unset means not checked
object-system = "warn"        # a class defined with S4, R6 or Reference Classes
data-table = "off"            # a call that creates a data.table

[format]
indent-width = 2
line-ending = "auto"     # keep the file's own; or "lf", "cr-lf"
```

The [diagnostic codes](/reference/diagnostic-codes) page explains each finding. `strict` turns on type errors
too and raises unresolved names to errors. A `# typing: off`, `on`, or `strict` comment in a file
overrides both keys for that file, and may end in a note such as `# typing: off  # generated code`.

## Which file applies

ry walks up from the file you check, or from the editor's workspace folder, and uses the first
`ry.toml` it finds. Nothing is merged, and there is no home-directory file or environment variable.
Each path you pass on the command line finds its own file, so two paths can use two different
configurations. A `..` in a path is resolved before the search, so `project/ry.toml` does not apply
to `project/../outside.R`. The language server re-reads the file whenever it changes, and deleting
it falls back to an ancestor's file or to the defaults. A `roughly.toml` from before the rename is
still read.

The project root is a separate question: it decides which files see each other's definitions. It is
the nearest directory with a `ry.toml` or a `DESCRIPTION`. Without one, it is the directory you
named, or the parent of `R/` for a file directly inside `R/`, or else the file's own directory. In
the editor it is the first workspace folder.

## A lintr `.lintr` file

A package set up for lintr keeps its lint choices without a `ry.toml`. ry reads the `linters` field
of the `.lintr` at the project root, the nearest directory with a `ry.toml`, a `roughly.toml` or a
`DESCRIPTION`, and fills every lint level that `ry.toml` leaves unset:

```
linters: linters_with_defaults(
    line_length_linter(120),
    commented_code_linter = NULL,
    any_is_na_linter()
  )
```

A linter called in the field turns on the ry lints that report the same thing, so this file turns
on `any-is-na` and `line-length` with a limit of 120. A linter set to `NULL` turns its lints off.
`linters_with_defaults()` also turns on lintr's defaults that are off in ry: `commented-code`,
`line-length` at 80, `naming-style` in snake_case, `redundant-return` and `single-quotes`. A setting
that asks for the opposite of a ry lint turns it off: `return_linter(return_style = "explicit")` turns
off `redundant-return`, and `quotes_linter("'")` turns off `single-quotes`.
`object_name_linter(styles = "camelCase")` sets camelCase, and a list of styles that allows both
camelCase and snake_case leaves `naming-style` unset. ry warns once for each linter it has no lint for,
except lintr's whitespace and layout linters, which `ry format` covers, and a `.lintr` that is not
UTF-8 text is ignored with a warning. The other fields, such as `exclusions`, are not read. The
language server re-reads a `.lintr` in the workspace folder when it changes.

## `inline-annotations`

Inline annotations, such as `count: integer <- 0L` and `function(x: integer) -> integer`, are on by
default. R itself cannot run them, so a project that must stay runnable by R can set
`inline-annotations = false`. Inline annotations then type nothing, each one is reported as an
`inline-annotation` warning, and a `#:` comment on the same statement applies alone. For as long as
the setting is off, `ry check` warns about it once per run, and the language server once whenever it
loads `ry.toml`:

```text
! inline type annotations are turned off (`[check] inline-annotations = false`), so ry does not check them and reports each one it finds
```

## `object-systems`

S4, R6 and Reference Classes keep working, but ry recommends
[records and tagged unions](/type-checking/records-and-tags), which it checks statically, so each
class or method definition gets an `object-system` warning. Set `[lint] object-system = "off"` to
silence it. A project that wants none of these classes sets `object-systems = false`, and every
definition becomes an error, whatever the lint level says. Only definitions are reported, so a call
to `new()` on a class from another package is not.

`ry run` and its console, on ry's runtime, enforce the setting too. A call to
`setClass`, `setClassUnion`, `setGeneric`, `setMethod`, `setValidity`, `setRefClass` or `R6Class`
from your own code stops with the same message as the lint:

```text
Error in setClass("P", representation(x = "numeric")) :
  S4 classes are turned off in this project (`[check] object-systems = false`), so `setClass` cannot define an S4 class. Use a record or a tagged union instead
Execution halted
```

Your own code means code that runs at the top level of a script or the console, or inside a
function defined there. Code inside a package, installed or loaded with `ry load`, may still define
classes, so packages such as `stats4` keep loading. The runtime recognizes the call by name
(`setClass(...)` or `methods::setClass(...)`), so a renamed alias such as `f <- setClass` is not
caught. GNU R (`ry R`) does not enforce the setting.

## `prelude`

ry's [standard library](/reference/std/) lives in the namespace `std`. The prelude, on by default,
also puts every function in scope by its bare name, so `List.map` and `std::List.map` are the same
function. A project that wants every use of the library spelled out sets `prelude = false`, and a
bare name is then unbound unless the project defines it itself:

```text
unresolved

  ! `List.map` is in ry's standard library, which `[check] prelude = false` keeps out of scope. Write `std::List.map`.
   --[a.R:1:6]
 1 | x <- List.map(list(1L, 2L), function(v) v + 1L)
   |      ^^^^^^^^
  help: Change to `std::List.map` (`ry check --fix --unsafe-fixes` applies it)
```

The types `Result`, `Dict`, `Set` and `Array` stay in scope either way. `ry run` and its console
follow the setting too: with the prelude off, ry's runtime does not attach `package:std`, so a bare
`List.map` fails there as well, while `std::List.map` works. GNU R (`ry R`) has no standard library
at all.

## `exclude`

Patterns follow gitignore rules and are anchored at the directory holding `ry.toml`, so
`exclude = ["scripts/", "**/generated"]` skips that subtree and any `generated` directory.

- Excluded files drop out of analysis, not just out of the report, so names they define become
  unresolved everywhere else. Exclude only code nothing else calls into.
- A file you name on the command line, or open in the editor, is still analyzed.
- `ry fmt` ignores the key.
- `renv/`, `packrat/`, `revdep/`, `.Rproj.user/`, `.Rcheck/`, and whatever `.gitignore` lists are
  always skipped, because they hold dependencies and build output rather than your code.

## Mistakes in `ry.toml`

An unknown or misplaced key is a warning, and the rest of the file still loads, so a configuration
written for a newer ry still works with an older one. The cost is that a typo does not fail CI: with
`typng = true`, `ry check` leaves type checking off, exits 0 on clean code, and only prints this:

```text
! ignoring unknown config key `check.typng`. Check the spelling, or update ry
```

Invalid TOML, a value of the wrong type, or an invalid `exclude` pattern is an error: `ry check`
exits with status 2, and the language server keeps the last good configuration and marks the line
in `ry.toml`.

The older top-level keys `case` and `spaces` still work, as `naming-style` and `indent-width`.

## Editor settings

Editor settings only say how to start the server. The server ignores any configuration the editor
sends.

In VS Code, the extension uses, in order, the `SERVER_PATH` environment variable, `ry.path`, its
bundled binary, and `ry` on your `PATH`. A change takes effect after **ry: Restart Server**.

```jsonc
{
  "ry.path": "/usr/local/bin/ry",
  "ry.args": ["server"]
}
```

In Zed, set the path under `lsp.ry`. Zed highlights R with tree-sitter, which treats a `#:`
annotation as a plain comment, so turn on semantic tokens to color annotations:

```jsonc
{
  "lsp": { "ry": { "binary": { "path": "/usr/local/bin/ry" } } },
  "languages": { "R": { "semantic_tokens": "combined" } }
}
```
