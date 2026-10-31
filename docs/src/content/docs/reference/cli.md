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
ry load [PATH]     # load an R package from source, run code against it, and exit
```

`format` and `lsp` are aliases for `fmt` and `server`. Only `repl`, `run`, and `load` need R: they
use the R in `R_HOME` if it is set, and otherwise the one `R RHOME` reports on your `PATH`. It must
be built as a shared library, as the CRAN builds are, and on Windows it must be R 4.2 or newer.

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
stderr and the one-line summary to stdout. A terminal gets color and Unicode box drawing, `NO_COLOR`
turns off only the color, and a pipe gets plain ASCII. [Diagnostic
codes](/reference/diagnostic-codes) shows how to read a finding.

## fmt

```sh
ry fmt           # rewrite files in place
ry fmt --check   # name the files that would change, and exit 1 if any would
ry fmt --diff    # show the changes without writing them, and exit 1 if any
```

`ry fmt` skips `.Rmd`, `.qmd`, and `.Rnw` files, because it rewrites a whole file's layout and must not
touch prose. `exclude` does not apply to it. All of its output goes to stderr. The rules it applies
are in [Formatting rules](/reference/formatting-rules).

## server, repl, run, and load

`ry server --debug` (or `RY_DEBUG=1`) adds internal analysis facts to hover, for working on ry itself.
`--experimental-features range_formatting` lets the server format a selection instead of the whole
file.

`ry repl --keybindings vi` switches the console to vi keys, and `ry repl -f setup.R` runs a script
before the first prompt. [The console guide](/guides/console) describes what its completion knows.
Started inside an R package, `ry repl` [loads it](/guides/loading-packages) and reloads what changed
before each command; `--no-load` turns that off. `ry repl --runtime ry` (or `RY_RUNTIME=ry`) runs
the console on ry's own interpreter, which loads no package and runs no startup code.

`ry run script.R` runs a script and exits. A top-level error stops it with exit status 1, as with
`Rscript`. Unlike `Rscript`, the session is interactive, so `interactive()` returns `TRUE`. R's
startup banner is not printed, so the output is only what the script prints. Arguments after the
file reach the script through `commandArgs(trailingOnly = TRUE)`.

Before anything runs, `ry run` checks the script and `ry repl` checks each command against the
project's `ry.toml`. They print the [inline annotation](/type-checking/inline-annotations) warnings,
and in [strict mode](/type-checking/strict-mode) they print every error and refuse code that has
one. GNU R then runs the script with its inline annotations erased.

`ry run --runtime ry script.R` (or `RY_RUNTIME=ry`) runs the script on ry's own interpreter instead
of GNU R. It still reads base R's R code and the default packages from the GNU R installation, so R
must be installed, but no GNU R code runs. This runtime is under construction: a script that calls
something it lacks stops with an error naming it. That error can be a missing `.Internal`, a native
routine of a default package, or a graphics device, since only the null device exists.

`ry load` [loads the package](/guides/loading-packages) at `PATH` (default: the one containing the
current directory), then runs each `-e EXPR` and the `-f FILE` script, and exits like `ry run`.
`ry load --r-entry` prints the R code that gives a plain R session the same `load_all()`, for
`source(pipe("ry load --r-entry"))`. `load_all()` is available in `ry repl` and `ry run` sessions
too.

## Exit codes

- **0**: nothing found, or nothing to reformat.
- **1**: `check` counted a finding (warnings count, unless `--min-severity error`), `fmt --check` or
  `--diff` found a file to change, `run` or `load` hit an error in the script or the package, `run` refused a script in strict
  mode, or the
  server stopped after an analysis panic.
- **2**: ry could not do its job. That is a usage error, invalid TOML in `ry.toml`, a path that does
  not exist, a file or stub it cannot read, an invalid `exclude` pattern, for `load` a path inside
  no package, or, for `repl`, `run`, and `load`, no usable R. A `2` overrides a `1`, so a broken
  setup never looks like ordinary findings.

A [misspelled key](/reference/configuration#mistakes-in-rytoml) in `ry.toml` is only a warning, not
a `2`.

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
