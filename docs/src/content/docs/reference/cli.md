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
ry check --fix                 # apply the safe fixes, then report what remains
ry check --fix --unsafe-fixes  # also apply fixes that guess, such as a spelling suggestion
```

Naming a file limits what is reported, not what is analyzed: ry always reads the whole
[project](/reference/configuration#which-file-applies) the file belongs to, so a name defined in
another file resolves however you spell the paths. The exception is a file matched by `exclude`,
which is analyzed only when you name it.

`ry check` reads `.R` files and the R chunks of `.Rmd`, `.qmd`, and `.Rnw` documents. Findings go to
stderr and the one-line summary to stdout. A terminal gets color and Unicode box drawing, `NO_COLOR`
turns off only the color, and a pipe gets plain ASCII. [Diagnostic
codes](/reference/diagnostic-codes) shows how to read a finding.

A finding with a mechanical fix says so under its snippet, and the summary counts them:

```text
  help: Change to `TRUE` (`ry check --fix` applies it)
...
3 problems in 1 file
2 fixable with `ry check --fix` (1 more with `--unsafe-fixes`)
```

`--fix` rewrites the files in place and then reports what is left. It applies only safe fixes, which
keep what the code means: `T` to `TRUE`, `=` to `<-`, and removing a trailing comma. A fix that
guesses what you meant, such as changing a misspelled `totl` to `total`, is unsafe and needs
`--unsafe-fixes` as well. One fix can expose another, so ry repeats the pass until nothing changes. A file with a syntax
error is left unchanged, because its fixes were worked out on a guess at what the broken code means.
The editor offers the same fixes as quick fixes.

### The check cache

`ry check` records its findings in `.ry_cache/check.json` at the project root, and the next run
reuses them for every file whose findings cannot have changed. On a 300,000-line package, a repeat
check of an unchanged project takes about 0.1 s instead of 1.5 s. After an edit inside a function
body, only the edited file and the files whose check read it are checked again, which takes about
0.45 s. Any other change checks every file again: an edit outside a function body (a new top-level
name, a changed signature, a class definition), an added or removed file, a different `ry.toml`,
`NAMESPACE`, `DESCRIPTION` or stub, or a different ry binary. An edit inside a body that changes
what the rest of the project sees also checks every file, for example one that adds `UseMethod`,
`<<-`, `assign`, `library`, `setClass` or a `#: @type` declaration. A directory with no `ry.toml`
or `DESCRIPTION`, such as one holding a single downloaded script, gets no cache.

The output is the same with or without the cache. `--no-cache` checks every file without reading or
writing the record, and deleting `.ry_cache/` is always safe. ry writes a `.gitignore` into the
directory, so the record stays out of version control.

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

Options go before the file, because everything after the file is the script's: `ry run script.R --help` hands `--help` to the script. A top-level error
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
package. Base graphics, grid and device packages such as ragg draw as they do on GNU R. The
built-in `pdf`, `svg`, `postscript`, `png`, `jpeg`, `bmp` and `tiff` devices write their files when
the device closes or the script ends. A script sees `interactive()` as `FALSE` on both runtimes, as under `Rscript`, and a console
sees `TRUE`.

The console completes names with Tab, and `--keybindings vi` switches it to vi keys. [The console
guide](/guides/console) describes what its completion knows. Started inside an R package, either
console [loads it](/guides/loading-packages) and reloads what changed before each command, and
`--no-load` turns that off. `--keybindings` and `--no-load` apply
only to a console, so passing either with a script is a usage error. With `-i FILE`, the file is
checked under its own name before the console opens, and a strict-mode refusal exits 1.

With standard input piped (`echo '1 + 1' | ry run`), the console skips the editor, the prompt and
the banner, evaluates each line, prints the result, and exits 0 at the end of input. It behaves
like `R --no-echo`, except that it keeps going after an error.

`ry repl` still works. It is the earlier name of `ry R`'s console, and it keeps its `-f FILE`, which
is `ry R -i FILE`.

## load and server

`ry load` [loads the package](/guides/loading-packages) at `PATH` (default: the one containing the
current directory) on ry's runtime, then runs each `-e EXPR` and the `-f FILE` script, and exits
like `ry run`. `ry load --r-entry` prints the R code that gives a plain R session the same
`load_all()`, for `source(pipe("ry load --r-entry"))`. `load_all()` is also available in the scripts
and consoles of `ry run` and `ry R`.

`ry server --debug` (or `RY_DEBUG=1`) adds internal analysis facts to hover, for working on ry itself.
`--experimental-features range_formatting` lets the server format a selection instead of the whole
file.

## debug-adapter

`ry debug-adapter` serves one debug session over the
[Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/) on standard input and
output. The editor launches an R script, which runs on ry's runtime, and the command exits with the
script's status. [Editors](/guides/editors#running-and-debugging) lists the launch arguments.

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
{"code":"type-mismatch","column":21,"endColumn":27,"endLine":4,"fix":null,"line":4,"message":"expected `integer`, found `character`","path":"/home/you/demo/main.R","related":[],"severity":"error"}
```

`path` is absolute. Lines and columns start at 1 and count characters, as your editor does, and
`endColumn` is one past the end. `severity` is `"warning"` or `"error"`, and `code` is what a
`# ry: allow(...)` comment names. `related` lists companion locations with the same position fields
and a `message`, such as the other definition of a `duplicate`. `fix` is `null` or an object with
the `title`, an `applicability` of `"safe"` or `"unsafe"`, the position fields of the range it
replaces, and the `replacement` text. These field names are a contract,
so tools built on them keep working across releases.
