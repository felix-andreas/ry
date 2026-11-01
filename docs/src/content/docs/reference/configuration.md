---
title: Configuration
description: Every ry.toml key, which file applies, and the editor settings
---

Everything that changes what ry reports lives in one `ry.toml`, so the editor, the command line, and
CI always agree. This is every key, with its default:

```toml
[check]
typing = false           # report type errors
strict = false           # report unknown types and untyped parameters (needs typing for type errors)
inline-annotations = true  # type inline annotations such as `x: integer <- 1L`
object-systems = true    # allow S4, R6 and Reference Classes; false makes each definition an error
unused = true            # report values that are never read
maybe-undefined = false  # report reads some path reaches before a write
exclude = []             # gitignore-style patterns that ry check skips

[lint]                   # each takes "off", "warn", "error", or "default"
assignment-operator = "warn"  # `=` used for assignment
boolean-shorthand = "warn"    # T or F instead of TRUE or FALSE
trailing-comma = "error"      # a comma after a call's last argument
unused-parameter = "off"      # a parameter the body never reads
unused-import = "off"         # a NAMESPACE importFrom nothing uses
shadows-builtin = "off"       # a top-level name that hides a base function
shadows-namespace = "off"     # a top-level name that hides another export
# naming-style = "snake_case" # or "camelCase"; unset means not checked
object-system = "warn"        # a class defined with S4, R6 or Reference Classes

[format]
indent-width = 2
line-ending = "auto"     # keep the file's own; or "lf", "cr-lf"
```

The [diagnostic codes](/reference/diagnostic-codes) page explains each finding. `strict` also raises
unresolved names to errors, but it does not turn on type errors: set `typing` as well. A
`# typing: off`, `on`, or `strict` comment in a file overrides both keys for that file, and
`# typing: strict` does imply type errors.

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

`ry run` and `ry repl` on ry's runtime (`--runtime ry`) enforce the setting too. A call to
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
caught. GNU R (`--runtime gnu-r`) does not enforce the setting.

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
  "ry.args": ["server"],
  "ry.experimentalFeatures": ["range_formatting"]  // format a selection
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
