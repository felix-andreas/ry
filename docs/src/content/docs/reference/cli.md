---
title: CLI
description: Every ry command and flag, the exit codes, and the JSON output
---

```sh
ry check [PATHS]   # report problems
ry fmt [PATHS]     # format files in place
ry run [FILE]      # run an R script on ry's runtime; without one, open ry's console
ry R [FILE]        # the same on GNU R
ry load [PATH]     # load an R package from source, run code against it, and exit
ry server          # the language server, over stdio; your editor starts it
```

`format` and `lsp` are aliases for `fmt` and `server`. Only `run`, `R`, and `load` need R. They use
the R in `R_HOME` if it is set, and otherwise the one `R RHOME` reports on your `PATH`. It must be
built as a shared library, as the CRAN builds are, and on Windows it must be R 4.2 or newer. ry's
own runtime needs that installation too, because it still reads base R's R code and the default
packages from it.

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

## run and R

```sh
ry run script.R a b   # run on ry's runtime; the script reads a and b with commandArgs(TRUE)
ry run                # ry's console
ry run -i setup.R     # run setup.R, then open the console
ry R script.R         # run on GNU R
ry R                  # a GNU R console with ry's highlighting and completion
```

`ry run` and `ry R` take the same options, and they go before the file, because everything after
the file is the script's: `ry run script.R --help` hands `--help` to the script. A top-level error
stops a script with exit status 1, as with `Rscript`, and so does `q(status = 1)`. The output is
only what the script prints, with no startup banner.

Before anything runs, both commands check the script, and the console checks each command, against
the project's `ry.toml`. They print the [inline annotation](/type-checking/inline-annotations)
warnings, and in [strict mode](/type-checking/strict-mode) they print every error and refuse code
that has one.

The runtimes differ in what they do with the annotations. ry's runtime turns them into [checks at
function boundaries](/type-checking/inline-annotations#running-annotated-code), and GNU R runs the
script with them erased. ry's runtime is under construction: a script that calls something it
lacks stops with an error naming it, such as a missing `.Internal` or a native routine of a default
package. Base graphics draws into `pdf`, `svg`, `png`, `bmp` and `tiff` files, which are written
when the device closes or the script ends; `jpeg` and `postscript` devices open but write no image
yet. A script sees `interactive()` as `FALSE` on ry's runtime, as under `Rscript`, but as `TRUE` on
GNU R.

The console completes names with Tab, and `--keybindings vi` switches it to vi keys. [The console
guide](/guides/console) describes what its completion knows. Started inside an R package, `ry R`
[loads it](/guides/loading-packages) and reloads what changed before each command, and `--no-load`
turns that off. ry's console does not load the package yet.

With standard input piped (`echo '1 + 1' | ry run`), the console skips the editor, the prompt and
the banner, evaluates each line, prints the result, and exits 0 at the end of input. It behaves
like `R --no-echo`, except that it keeps going after an error.

`ry repl` still works. It is the earlier name of `ry R`'s console, and it keeps its `-f FILE`, which
is `ry R -i FILE`.

## load and server

`ry load` [loads the package](/guides/loading-packages) at `PATH` (default: the one containing the
current directory) on ry's runtime, then runs each `-e EXPR` and the `-f FILE` script, and exits
like `ry run`. `ry load --r-entry` prints the R code that gives a plain R session the same
`load_all()`, for `source(pipe("ry load --r-entry"))`. `load_all()` is also available in `ry run`
scripts and in `ry R` scripts and consoles, but not yet in ry's console.

`ry server --debug` (or `RY_DEBUG=1`) adds internal analysis facts to hover, for working on ry itself.
`--experimental-features range_formatting` lets the server format a selection instead of the whole
file.

## debug

`ry debug ast FILE` prints the syntax tree of a file, and `ry debug analysis-stats [PATH]` analyzes
a workspace and reports where time and memory go. Both are developer aids for working on ry itself.

## Exit codes

- **0**: nothing found, or nothing to reformat.
- **1**: `check` counted a finding (warnings count, unless `--min-severity error`), `fmt --check` or
  `--diff` found a file to change, `run`, `R` or `load` hit an error in the script or the package,
  strict mode refused a script, or the server stopped after an analysis panic. A script that calls
  `q(status = n)` exits with `n`.
- **2**: ry could not do its job. That is a usage error, invalid TOML in `ry.toml`, a path that does
  not exist, a file, script or stub it cannot read, an invalid `exclude` pattern, for `load` a path
  inside no package, or, for `run`, `R`, and `load`, no usable R or no ry runtime library. A `2` overrides a `1`, so a broken
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
