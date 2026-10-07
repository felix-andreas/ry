---
title: CLI
description: Every ry command and flag, the exit codes, and the JSON output
---

```sh
ry check [PATHS]   # report problems
ry fmt [PATHS]     # format files in place
ry server          # the language server, over stdio; your editor starts it
ry repl            # an R console with typed completion
ry run FILE        # run an R script and exit
```

`format` and `lsp` are aliases for `fmt` and `server`. Only `repl` and `run` need R: they use the R in
`R_HOME` if it is set, and otherwise the one `R RHOME` reports on your `PATH`. It must be R 4.2 or
newer, built as a shared library, which every CRAN build is.

## check

```sh
ry check                       # the current directory
ry check R/model.R             # report on one file
ry check --output json         # one JSON object per finding, on stdout
ry check --min-severity error  # ignore warnings, also for the exit code
```

Naming a file limits what is reported, not what is analyzed: ry always reads the whole
[project](/reference/configuration#which-file-applies) the file belongs to, so a name defined in
another file resolves however you spell the paths. The exception is a file matched by `exclude`,
which is analyzed only when you name it.

`ry check` reads `.R` files and the R chunks of `.Rmd`, `.qmd`, and `.Rnw` documents. Findings go to
stderr and the one-line summary to stdout. A terminal gets color and box drawing, which `NO_COLOR`
turns off, and a pipe gets plain ASCII. [Diagnostic codes](/reference/diagnostic-codes) shows how to
read a finding.

## fmt

```sh
ry fmt           # rewrite files in place
ry fmt --check   # name the files that would change, and exit 1 if any would
ry fmt --diff    # show the changes without writing them, and exit 1 if any
```

`ry fmt` skips `.Rmd`, `.qmd`, and `.Rnw` files, because it rewrites a whole file's layout and must not
touch prose. `exclude` does not apply to it. All of its output goes to stderr. The rules it applies
are in [Formatting rules](/reference/formatting-rules).

## server, repl, and run

`ry server --debug` (or `RY_DEBUG=1`) adds internal analysis facts to hover, for working on ry itself.
`--experimental-features range_formatting` lets the server format a selection instead of the whole
file.

`ry repl --keybindings vi` switches the console to vi keys, and `ry repl -f setup.R` runs a script
before the first prompt. Completion comes from type-checking what you typed, not from the live
session, so completing `account$` lists a record's fields with their types, but data frame columns
and objects created by `source()` do not complete.

`ry run script.R` runs a script and exits. A top-level error stops it with exit status 1, as with
`Rscript`. R's startup banner is printed.

## Exit codes

- **0**: nothing found, or nothing to reformat.
- **1**: `check` counted a finding (warnings count, unless `--min-severity error`), `fmt --check` or
  `--diff` found a file to change, `run` hit an error in the script, or the server stopped after an
  analysis panic.
- **2**: ry could not do its job. That is a usage error, invalid TOML in `ry.toml`, a path that does
  not exist, a file or stub it cannot read, an invalid `exclude` pattern, or, for `repl` and `run`, no
  usable R. A `2` overrides a `1`, so a broken setup never looks like ordinary findings.

A misspelled key in `ry.toml` is only a warning, not a `2`, so read the log after changing the
configuration.

## JSON output

`ry check --output json` writes one object per finding to stdout and nothing else, not even a
summary, so the stream can be read line by line:

```json
{"code":"type-mismatch","column":21,"endColumn":27,"endLine":4,"line":4,"message":"expected `integer`, found `character`","path":"/home/you/demo/main.R","related":[],"severity":"error"}
```

`path` is absolute. Lines and columns start at 1 and count characters, as your editor does, and
`endColumn` is one past the end. `severity` is `"warning"` or `"error"`, and `code` is what a
`# ry: allow(...)` comment names. `related` lists companion locations with the same position fields
and a `message`, such as the other definition of a `duplicate`. These field names are a contract,
so tools built on them keep working across releases.
