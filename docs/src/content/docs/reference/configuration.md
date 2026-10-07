---
title: Configuration
description: Every ry.toml key, which file applies, and the editor settings
---

Everything that changes what ry reports lives in one `ry.toml`, so the editor, the command line, and
CI always agree. This is every key, with its default:

```toml
[check]
typing = false           # report type errors
strict = false           # report unknown types; unresolved names are errors
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

[format]
indent-width = 2
line-ending = "auto"     # keep the file's own; or "lf", "cr-lf"
```

The [diagnostic codes](/reference/diagnostic-codes) page explains each finding. `maybe-undefined` is
off because it treats two conditions that always agree at run time as independent, so
`if (ok) v <- 1` followed by `if (ok) print(v)` is reported although it is safe. A
`# typing: off`, `on`, or `strict` comment in a file overrides `typing` and `strict` for that file.

## Which file applies

ry walks up from the file you check, or from the editor's workspace folder, and uses the first
`ry.toml` it finds. Nothing is merged: there is no home-directory file and no environment variable,
because a second source of settings is how the editor and CI start to disagree. The language server
reloads the file when it changes. A `roughly.toml` from before the rename is still read.

The project root is a separate question: it decides which files see each other's definitions. It is
the nearest directory with a `ry.toml` or a `DESCRIPTION`. Without one, it is the directory you
named, or the parent of `R/` for a file directly inside `R/`, or else the file's own directory. In
the editor it is the first workspace folder.

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
written for a newer ry still works with an older one. The catch is that a typo is silent in CI:
`typng = true` leaves type checking off and only prints a warning.

```text
! ignoring unknown config key `check.typng`. Check the spelling, or update ry
```

Invalid TOML, a value of the wrong type, or an invalid `exclude` pattern is an error: `ry check`
exits with status 2, and the language server keeps the last good configuration and marks the line
in `ry.toml`.

The older top-level keys `case` and `spaces` still work, as `naming-style` and `indent-width`.

## Editor settings

Editor settings only say where the binary is. The server ignores any configuration the editor sends,
so that every setting that changes a finding is in `ry.toml`.

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
