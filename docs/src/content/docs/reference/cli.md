---
title: CLI
description: Every ry command and flag, the exit codes, and the JSON output
---

```sh
ry check [PATHS]               # report problems
ry fmt [PATHS]                 # format files in place
ry migrate data-table [PATHS]  # rewrite data.table code as DataFrame code
ry run [FILE]                  # run an R script on ry's runtime; without one, open ry's console
ry R [FILE]                    # the same on GNU R
ry load [PATH]                 # load an R package from source, run code against it, and exit
ry test [PATH]                 # run a package's testthat tests against the loaded package
ry document [PATH]             # write a package's Rd files and NAMESPACE with roxygen2
ry build [PATH]                # build a package's source tarball
ry install [PATH]              # build and install a package
ry debug-adapter               # a debugger for editors, over stdio
ry server                      # the language server, over stdio; your editor starts it
ry debug COMMAND               # developer aids for working on ry itself
```

`format` and `lsp` are aliases for `fmt` and `server`. Only `run`, `R`, `load`, `test`, `document`,
`build`, `install` and `debug-adapter` run R, and all of them except `R` run it on ry's runtime.
[How ry finds R](#how-ry-finds-r) says where that runtime and base R come from.

## check

```sh
ry check                       # the current directory
ry check R/model.R             # report on one file
ry check --output json         # one JSON object per finding, on stdout
ry check --min-severity error  # ignore warnings, also for the exit code
ry check --fix                 # apply the safe fixes, then report what remains
ry check --fix --unsafe-fixes  # also apply fixes that guess, such as a spelling suggestion
ry check --annotate            # write inferred types as annotations, then report what remains
```

Naming a file limits what is reported, not what the check sees: ry reads the whole
[project](/reference/configuration#which-file-applies) the file belongs to, so a name defined in
another file resolves however you spell the paths. When every path you name is in the package's
`R/`, as in `ry check R`, the tests and scripts are left out, because they add nothing to the
package and cannot change its findings. A file matched by `exclude` is analyzed only when you name
it.

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

`--annotate` writes the inferred type of each unannotated top-level function and binding in the
named files, inline or as a `#:` comment as [`inline-annotations`](/reference/configuration#inline-annotations)
says, prints how many it added, and then reports what is left.
[Strict mode](/tutorial/fully-typed#strict-mode) shows it at work. A binding
whose type inference cannot write down, such as one involving `Unknown`, is skipped.

### The check cache

`ry check` records its findings in `.ry_cache/check.json` at the project root, and the next run
reuses them for every file whose findings cannot have changed. On a 300,000-line package, a repeat
check of an unchanged project takes about 0.1 s instead of 1.2 s. After an edit inside a function
body, only the edited file and the files whose check read it are checked again, which takes about
0.45 s. Any other change checks every file again: an edit outside a function body (a new top-level
name, a changed signature, a class definition), an added or removed file, a different `ry.toml`,
`NAMESPACE`, `DESCRIPTION` or stub, or a different ry binary. An edit inside a body that changes
what the rest of the project sees also checks every file, for example one that adds `UseMethod`,
`<<-`, `assign`, `library`, `setClass` or a `#: @type` declaration. A directory with no `ry.toml`
or `DESCRIPTION`, such as one holding a single downloaded script, gets no cache.

A run checks only the files it was asked about, so `ry check R` records no findings for the files
in `tests/`, and a later `ry check` checks those files. The strict check of `ry load` asks about
`R/` alone in the same way and shares the record.

The output is the same with or without the cache. `--no-cache` checks every file without reading or
writing the record, and deleting `.ry_cache/` is always safe. ry writes a `.gitignore` into the
directory, so the record stays out of version control.

The language server reads the same record when it starts. If the record still holds for a file
under `R/`, opening that file shows its recorded findings at once, before the server has analysed
the project, and the server's own analysis replaces them when it settles. A file that changed since
the check, or whose findings depend on one that did, starts without them. Any edit drops the
recorded findings of every file, and a record written by a different ry binary is never read.

## fmt

```sh
ry fmt           # rewrite files in place
ry fmt --check   # name the files that would change, and exit 1 if any would
ry fmt --diff    # show the changes without writing them, and exit 1 if any
ry fmt --range 3-8 R/report.R   # format only lines 3 to 8 of one file
ry fmt --verbose # also print how many bytes were formatted, and how fast
```

`--range` takes `LINE`, `START-END`, or `LINE:COLUMN-LINE:COLUMN`, counting from 1. It formats the
lines the range touches exactly as formatting the whole file would, at the indentation the file
already uses, and leaves every other line alone. A statement is laid out as one unit, so a line
just outside the range changes when it belongs to a statement the range reaches into. A range that
starts past the file's last line is an error, and an end past it stops at the last line.

In `.Rmd`, `.qmd`, and `.Rnw` files, `ry fmt` formats each R chunk on its own and leaves the prose,
the fences, inline `` `r expr` `` code, and indented chunks (such as one inside a list item) exactly
as written. `exclude` does not apply to it. All of its output goes to stderr. The rules it applies
are in [Formatting rules](/reference/formatting-rules).

## migrate

```sh
ry migrate data-table             # print the rewrites for the project as a diff
ry migrate data-table R/ --write  # rewrite the files under R/ in place
```

`ry migrate data-table` rewrites data.table code as [DataFrame](/reference/dataframes) code, applying
the same rewrites the editor offers as code actions. It takes files and directories, or the whole
project when given none. Without `--write`, it prints a diff of each file to stderr and leaves the
files alone. Either way it prints a one-line summary to stdout and exits 0. Each form it rewrites,
and each it leaves alone, is listed in
[Moving from data.table](/reference/from-data-table#rewriting-automatically).

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
the project's `ry.toml`. They print the [inline annotation](/tutorial/runtime#inline-annotations)
warnings, and in [strict mode](/tutorial/fully-typed#strict-mode) they print every error and refuse code
that has one.

The runtimes differ in what they do with the annotations. ry's runtime turns them into [checks at
function boundaries](/reference/type-system#inline-annotations-at-run-time), and GNU R runs the
script with them erased. ry's runtime is under construction: a script that calls something it
lacks stops with an error naming it, such as a missing `.Internal` or a native routine of a default
package. Base graphics, grid and device packages such as ragg draw as they do on GNU R. The
built-in `pdf`, `svg`, `postscript`, `png`, `jpeg`, `bmp` and `tiff` devices write their files when
the device closes or the script ends. They draw gradients, patterns, clipping paths, masks, groups
and even-odd fills except on `postscript`, which refuses them and drops semi-transparent colours
with a warning, as GNU R's does. Like GNU R's, `pdf` cannot repeat or reflect a gradient, and warns
and stops it at its ends instead. knitr and rmarkdown documents with plot chunks render. A script sees `interactive()` as `FALSE` on both runtimes, as under `Rscript`, and a console
sees `TRUE`.

Started inside an R package, either console [loads it](/reference/package-loading) and reloads what
changed before each command; `--no-load` turns that off. In the console:

- Tab completes names from the type checker's view of the code you typed, with each field's type.
  `--keybindings vi` switches to vi keys.
- Lines typed while a command runs are queued and run in order. Ctrl-C interrupts the command and
  discards the queue.
- `browser()` stops at a `Browse[1]>` prompt: `n` runs the next statement, `s` steps in, `f`
  finishes the function, `c` continues, `Q` quits, and `where` shows the call stack. In a script,
  `browser()` does nothing, as in a non-interactive R session.
- `?mean` and `help()` print R's help pages; `help_type = "html"` opens them in a browser.

`--keybindings` and `--no-load` apply only to a console, so passing either with a script is a usage
error. With `-i FILE`, the file is checked under its own name before the console opens, and a
strict-mode refusal exits 1.

With standard input piped (`echo '1 + 1' | ry run`), the console skips the editor, the prompt and
the banner, evaluates each line, prints the result, and exits 0 at the end of input. It behaves
like `R --no-echo`, except that it keeps going after an error.

`ry repl` still works. It is the earlier name of `ry R`'s console (`ry run`'s with `--runtime ry`),
and it keeps its `-f FILE`, which is `ry R -i FILE`.

### Memory

ry's runtime collects garbage on GNU R's schedule, vectors of a megabyte or more go back to the
kernel when they die, and a dead large vector or list is freed at the next allocation once its
reference count reaches zero, without waiting for a collection. A script's peak memory is therefore
at or below GNU R's. These scripts from a CI machine that ran out of memory peak at (in MiB):

| Script | GNU R 4.3.3 | ry |
| --- | --- | --- |
| `id <- rep(1:1000, 50000)` | 239 | 239 |
| 20 rounds of `keep <- v > 0.5; s <- list(id[keep], v[keep])` over `runif(5e7)` | 1386 | 1097 |
| 20 rounds of `dt[v > 0.5]` on a 5e6-row data.table | 340 | 277 |

To trade time for a lower peak, for example to fit more test processes into a CI machine, set GNU
R's own variables, which ry's runtime reads at startup:

| Variable | Effect |
| --- | --- |
| `R_GC_MEM_GROW` | `0` to `3`, default `1`: how much headroom the heap keeps above the live data after a full collection. `0` keeps the least and `3` the most. |
| `R_GC_GROWFRAC` | `0.35` to `0.75`, default `0.70`: the share of the heap the live data must fill before the heap grows. |
| `R_GC_VGROWINCRFRAC`, `R_GC_GROWINCRFRAC` | `0.05` to `0.80`, default `0.2`: the share by which the heap grows. |

`R_GC_MEM_GROW=0` lowers the data.table script's peak from 277 to 239 MiB (GNU R: 282 MiB). It costs
time where many objects outlive a collection: on a generated package's 1,907 testthat tests it
runs 66 full collections instead of 5, and collecting takes 4.8 s of the 25 s instead of 0.7 s.
A value outside a variable's range is ignored, as on GNU R.

## load and server

`ry load` [loads the package](/reference/package-loading) at `PATH` (default: the one containing the
current directory) on ry's runtime, then runs each `-e EXPR` and the `-f FILE` script, and exits
like `ry run`. `ry load --r-entry` prints the R code that gives a plain R session the same
`load_all()`, for `source(pipe("ry load --r-entry"))`. `load_all()` is also available in the scripts
and consoles of `ry run` and `ry R`.

`ry server --debug` (or `RY_DEBUG=1`) adds internal analysis facts to hover, for working on ry itself.
No feature is experimental, so `--experimental-features` ignores every name with a warning
(`range_formatting` is always on).

## test

`ry test` loads the package at `PATH` the way `ry load` does and runs its `tests/testthat` files
with testthat, as `devtools::test()` does. testthat must be installed. A test file is one whose name
starts with `test` and ends in `.R` or `.r`, so `test-parse.R` and `test_parse.R` both count. The
output is testthat's own reporter's, and the command exits with `1` when a test fails or errors.

- `--filter REGEX` runs only the test files whose names match, as `devtools::test(filter =)` does.
  The pattern matches the name without a leading `test-` or `test_` and without the extension, so
  `--filter '^parse$'` selects `test-parse.R` and `test_parse.R`.
- `--reporter NAME` picks a testthat reporter, such as `summary`, `progress` or `check`.
- `--watch` keeps running: after every edit under `R/`, `src/`, `data/`, `inst/`, `tests/`,
  `DESCRIPTION` or `NAMESPACE` it reloads only what changed (rebuilding compiled code after a
  `src/` edit), runs first the test files the edit can affect, and then the rest. It learns of
  edits from the operating system's file notifications, so it costs nothing while you type. A file is affected
  when you edited it, or when it mentions a function whose definition reaches a changed one. An
  edit to a helper or setup file, or a reload that has to run top-level code, reruns everything.

## document

`ry document` loads the package at `PATH` the way `ry load` does and runs roxygen2 against the
loaded namespace, as `devtools::document()` does, writing the same `man/*.Rd` files and `NAMESPACE`
byte for byte. roxygen2 must be installed. `--roclet NAME` (repeatable) runs only the named
roclets, such as `rd` or `namespace`. Without it, the package's own roxygen options decide, and a
package that sets none gets `collate`, `namespace` and `rd`.

Three things make it faster than roxygen2 alone, without changing what it writes. The parsed
documentation blocks of each file are cached in ry's cache directory, so a run parses again only
the files whose content changed. The finished Rd text of every topic is cached too, so after an
edit only the topics that can have changed are rebuilt: those with a block in an edited file, those
that inherit from them (`@inheritParams` and the other `@inherit` tags) or share a `@family` with
them, and, every time, the package topic and any topic built from a block ry cannot track. A block
is tracked only when it documents a function defined right below it under a name without a dot (a
dotted name may become an S3 method when another file adds a generic) and uses none of the tags
that read other files or the namespace: `@eval`, `@evalRd`, `@evalNamespace`, `@template`,
`@templateVar`, `@example`, `@includeRmd`, `@describeIn`, `@method`, `@exportS3Method` and
`@R6method`. Every other topic is written from the cache. Finally, the steps of roxygen2 that
take time quadratic in the number of topics (collecting them, ordering them for `@inherit`, and
finding them by name) run in linear time.

The caches keep two things roxygen2 would recompute: the markdown links a block resolved against
installed packages, and documentation inherited from another package. After you install or remove
a package a link or an `@inherit` points at, edit the file or delete the cache, which lives in
`~/.cache/ry/roxygen/` (or under `$XDG_CACHE_HOME`).

## build and install

```sh
ry build                  # write ../<package>_<version>.tar.gz
ry build -o dist          # write it into dist/ instead
ry install                # install into the first library on .libPaths()
ry install -l ~/rlib      # install into ~/rlib
```

`ry build` writes the source tarball of the package at `PATH` (default: the one containing the
current directory), as `devtools::build()` does: it runs `R CMD build --no-resave-data
--no-manual`, rebuilds the vignettes unless you pass `--no-vignettes`, and prints the tarball's path
on stdout. The tarball goes beside the package directory unless `-o DIR` names another.

`ry install` installs the package as `devtools::install()` does: it builds a tarball without
rebuilding the vignettes in a temporary directory, installs it with `R CMD INSTALL`, and deletes the
tarball. `-l DIR` names the library, which is created if it does not exist. The package's dependencies must
already be installed.

Both run R's own `build` and `INSTALL` scripts on ry's runtime, from ry's R home, so compiled code is
built against ry's libR, as `ry run` loads it. `--runtime gnu-r` (or `RY_RUNTIME=gnu-r`) uses GNU R
instead.

## debug-adapter

`ry debug-adapter` serves one debug session over the
[Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/) on standard input and
output. The editor launches an R script, which runs on ry's runtime, and the command exits with the
script's status. The launch arguments are `program` (the script), `args`, `stopOnEntry`, `prelude`
(R code run first, such as `load_all("."); library(testthat)`) and `testName` (run only the
`test_that()` block with that description).

On Unix, the `ry run` console serves the same protocol on a socket, so an editor can attach to it
while you work: connect to the socket named in `$TMPDIR/ry-<uid>/console-<pid>.json` and send
`initialize` and then `attach`.

## debug

`ry debug ast FILE` prints the syntax tree of a file, `ry debug analysis-stats [PATH]` analyzes
a workspace and reports where time and memory go, and `ry debug erase FILE...` rewrites files in
place as the plain R that `ry R` runs, with every inline annotation removed. All three are developer
aids for working on ry itself.

## Exit codes

- **0**: nothing found, or nothing to reformat.
- **1**: `check` counted a finding (warnings count, unless `--min-severity error`), `fmt --check` or
  `--diff` found a file to change, `run`, `R` or `load` hit an error in the script or the package,
  `test` saw a test fail, `document` hit an error in roxygen2, strict mode refused a script, or the
  server stopped after an analysis panic. A script that calls `q(status = n)` exits with `n`.
- **2**: ry could not do its job. That is a usage error, invalid TOML in `ry.toml`, a path that does
  not exist, a file, script or stub it cannot read, an invalid `exclude` pattern, for `load`, `test`
  and `document` a path inside no package, or, for the commands that need R, no usable R or no ry
  runtime library. A `2` overrides a `1`, so a broken setup never looks like ordinary findings.

A [misspelled key](/reference/configuration#mistakes-in-rytoml) in `ry.toml` is only a warning, not
a `2`.

## How ry finds R

`ry check`, `ry fmt` and `ry server` run no R. The other commands except `ry R` run on ry's runtime,
which needs base R (the R code of the base and default packages):

- **Built in.** A build from source carries base R 4.6.2 inside `ry` and extracts it into
  `~/.cache/ry/base/` on first use, once per release. `R.version`, `getRversion()` and `R.home()`
  then describe that copy. The 0.4.1-beta release does not have it and needs `R_HOME` or a GNU R on
  `PATH`.
- **`R_HOME`.** When set to a GNU R installation (4.3 or newer, built as a shared library), ry's
  runtime loads base R from it and behaves like that release where R changed between releases.
  An `R_HOME` that holds no base R is an error, not a fallback.
- **`RY_BASE_R`** names a base R tree built by `just base-r`. Without it, a `base-r/` directory
  beside `ry`, one or two directories up, or in `../lib/ry/base-r` takes precedence over the built-in
  copy, which is how a development build and a distribution package find theirs.

ry's runtime itself (`libR.so`, `libR.dylib` or `R.dll`) is built into `ry` and extracted into
`~/.cache/ry/runtime/` (`$XDG_CACHE_HOME`, or `%LOCALAPPDATA%` on Windows) on first use. If that
directory cannot hold a library (read-only, or mounted `noexec`), set `XDG_CACHE_HOME` to another
directory, or `RY_LIBR` to a runtime you placed yourself. A runtime beside `ry`, in `deps/`, or in
`../lib/ry/` takes precedence over the built-in one.

`ry R` and `--runtime gnu-r` run GNU R itself: the R in `R_HOME` if it is set, and otherwise the one
`R RHOME` reports on your `PATH`. It must be built as a shared library, as the CRAN builds are, and
on Windows it must be R 4.2 or newer.

Linear algebra (`solve`, `qr`, `chol`, `svd` and the rest) loads the system's BLAS and LAPACK on
first use: `libblas.so.3` and `liblapack.so.3` on Linux, Accelerate on macOS. With `R_HOME` set, ry
also looks in the directories GNU R's `etc/Makeconf` records (which is how it finds them on NixOS),
then for GNU R's own `lib/libRblas.so` and `lib/libRlapack.so`. If none loads, the error lists every
place it searched. On Windows, linear algebra needs `R_HOME`, for GNU R's `Rblas.dll` and
`Rlapack.dll`. Compiled package code links the same libraries.

On macOS, `ry run` loads packages installed by CRAN's or Homebrew's GNU R unchanged: it loads a copy
of each package library, cached under `~/Library/Caches/ry/rebound/`, that names ry's runtime
instead of GNU R's `libR`.

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
