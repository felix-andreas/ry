---
title: CLI
description: Every ry command, flag, exit code, and JSON field
---

This page lists everything the `ry` binary accepts on the command line, what it prints, and how it
exits.

## Commands

| Command | What it does |
| ------- | ------------ |
| [`check`](#check) | Runs code analysis over your project and reports findings |
| [`fmt`](#fmt) | Formats R files, in place or as a diff |
| [`server`](#server) | Runs the language server over stdio |
| [`repl`](#repl) | Starts an interactive R console with an embedded R |
| [`run`](#run) | Runs an R file through the embedded R and exits |
| `help` | Prints help for the binary or for one command |
| `debug` | **Not stable.** Development commands: `debug ast` dumps a syntax tree, `debug analysis-stats` reports where analysis time and memory go. Output shape can change at any time |

`check`, `fmt`, and `server` need no R installation. `repl` and `run` find and load the R on your
machine, and [ry at the R console](/guides/r-console) walks through them.

Two aliases exist: `format` for `fmt`, and `lsp` for `server`.

### Global options

| Flag | Argument | Default | Effect |
| ---- | -------- | ------- | ------ |
| `-V`, `--version` | n/a | n/a | Prints the version and exits 0 |
| `-h`, `--help` | n/a | n/a | Prints help and exits 0. Available on every command |
| `--stdio` | n/a | n/a | Accepted and ignored. It exists so VS Code's default launch arguments do not error |
| `--experimental-features` | `FEATURES` | none | Space-separated feature names, or `all` |

The only feature today is `range_formatting`, which only the language server reads, so the flag does
nothing for `check`, `fmt`, `repl`, or `run`. An unknown feature name prints a warning on stderr and is
ignored rather than treated as a usage error.

## check

```bash
ry check                       # the current directory
ry check R/ tests/             # named directories
ry check R/model.R             # one file
ry check --output json         # machine-readable, on stdout
ry check --min-severity error  # only errors report and gate
```

| Flag | Argument | Default | Effect |
| ---- | -------- | ------- | ------ |
| positional | `FILES...` | `.` | Files and directories to report on. Directories are walked for `.R`, `.r`, and literate extensions; `renv`, `packrat`, `revdep`, `.Rproj.user`, and `.Rcheck` are skipped, and `.gitignore` is honored even outside a git checkout. A file you name explicitly is always checked, even when `[check] exclude` matches it |
| `--output` | `human` \| `json` | `human` | `human` renders diagnostics with source snippets on **stderr** and one summary line on **stdout**. `json` writes [JSON Lines](#json-output) on **stdout** and prints no summary |
| `--min-severity` | `warning` \| `error` | `warning` | Findings below the floor are neither reported nor counted toward the [exit code](#exit-codes) |

Analysis always covers the whole project a named path belongs to: the nearest ancestor directory
holding `ry.toml` or `DESCRIPTION`, or else the target's own directory. Cross-file names therefore
resolve the same way however you spell the paths, and only the reporting is limited to what you named.

Type errors are opt-in through `[check] typing` in [`ry.toml`](/reference/configuration), and every
code is listed in [Diagnostic codes](/reference/diagnostic-codes).

Each finding starts with its [diagnostic code](/reference/diagnostic-codes), followed by the message
and the source it was found in. A companion location, such as the other binding an overwrite warning
points at, is drawn nested under the finding, from its own file.

```console
$ ry check
duplicate

  ! Top-level binding `value` is overwritten by a later top-level binding in this package.
   --[R/main.R:1:1]
 1 | value <- 1
   | ^^^^^
 2 | value <- 2
  `->   > the later binding is here.
         --[R/main.R:2:1]
       1 | value <- 1
       2 | value <- 2
         | ^^^^^
       3 | result <- undefined_thing(3)

duplicate

  ! Top-level binding `value` overwrites an earlier top-level binding in this package.
   --[R/main.R:2:1]
 1 | value <- 1
 2 | value <- 2
   | ^^^^^
 3 | result <- undefined_thing(3)
  `->   > the earlier binding is here.
         --[R/main.R:1:1]
       1 | value <- 1
         | ^^^^^
       2 | value <- 2

unresolved

  ! I could not resolve `undefined_thing` in this package, its imports, or builtins.
   --[R/main.R:3:11]
 2 | value <- 2
 3 | result <- undefined_thing(3)
   |           ^^^^^^^^^^^^^^^

3 problems in 1 file
```

In a terminal the output uses Unicode box drawing and colour, and `NO_COLOR` turns the colour off.
A pipe or a file gets the plain ASCII shown above.

## fmt

```bash
ry fmt            # rewrite files in place
ry fmt --check    # list what would change, exit 1 (CI)
ry fmt --diff     # show the change without writing, exit 1
```

| Flag | Argument | Default | Effect |
| ---- | -------- | ------- | ------ |
| positional | `FILES...` | `.` | Files and directories to format. A literate document is walked but deliberately skipped, because the formatter rewrites whole-file layout and must not touch prose. `[check] exclude` does not apply here |
| `--check` | n/a | off | Writes nothing. Prints `Would reformat: <path>` per file that would change |
| `--diff` | n/a | off | Writes nothing. Prints a coloured unified diff per changed file. Takes precedence over `--check` |
| `-v`, `--verbose` | n/a | off | Adds a throughput line after the summary |

All `fmt` output, including the diffs, the `Would reformat:` lines, and the summary, goes to
**stderr**. The rules it applies are documented in [Formatting rules](/reference/formatting-rules).

```console
$ ry fmt --diff
Diff in ./messy.R:
1        |-f<-function(x,y){
2        |-x+y
    1    |+f <- function(x, y) {
    2    |+  x + y
3   3    | }
1 file would be reformatted, 0 files already formatted
```

## server

Runs the Language Server Protocol over stdio. Your editor starts it for you.

| Flag | Argument | Default | Effect |
| ---- | -------- | ------- | ------ |
| `--debug` | n/a | off | Adds internal analysis facts to hover, as a developer aid for working on ry itself. `RY_DEBUG=1` turns on the same thing |
| `--stdio` | n/a | n/a | Accepted and ignored, for VS Code compatibility |
| `-v`, `--verbose` | n/a | off | Declared but not wired to anything yet |

## repl

Starts an interactive R console backed by the R installed on your machine, with analysis-backed
completion. See [ry at the R console](/guides/r-console).

| Flag | Argument | Default | Effect |
| ---- | -------- | ------- | ------ |
| `--keybindings` | `emacs` \| `vi` | `emacs` | Line-editing keys for the console |
| `-f`, `--file` | `FILE` | none | Runs this R file before the first prompt, then keeps prompting |

```console
$ ry repl
ry R console, R at <the R_HOME it found>. Type q() or Ctrl-D to quit.
```

## run

Runs one R file through the same embedded R, then exits. The file argument is required.

| Argument | Required | Effect |
| -------- | -------- | ------ |
| `FILE` | yes | The R file to execute. A top-level error stops the script and exits 1, the way `Rscript` behaves |

`run` prints R's own startup banner, so it is not a silent runner.

## Exit codes

`check` and `fmt` share one scheme, which is what CI gates on:

| Code | `check` | `fmt` |
| ---- | ------- | ----- |
| `0` | No findings after the `--min-severity` filter, and no I/O failure. Finding no R files at all is also clean | Default mode finished, however many files were rewritten; or `--check`/`--diff` found nothing to change; or there were no files to format |
| `1` | At least one finding was counted. Warnings alone are enough at the default severity floor | `--check` or `--diff` was passed **and** at least one file would be reformatted |
| `2` | A usage, configuration, or I/O failure: unparseable `ry.toml`, a path that does not exist, an invalid `exclude` pattern, an unreadable source file or stub. This overrides code 1 | A file could not be read, parsed, or written back. This overrides code 1 |

The other commands:

| Command | `0` | `1` | `2` |
| ------- | --- | --- | --- |
| `server` | Always, once the server loop returns | n/a | n/a |
| `run` | The script ran to completion | An uncaught top-level R error | R could not be found or loaded, or the file could not be read |
| `repl` | Session ended normally | n/a | R could not be found or loaded, or a `--file` script could not be read |
| `debug ast` | The tree was printed. Parse errors are listed but do not change the code | n/a | The file could not be read |

Any usage error exits `2`: an unknown flag, a missing required argument, an invalid choice for a
flag, or `ry` with no command at all.

## JSON output

`ry check --output json` writes **JSON Lines** to stdout: one independent object per finding, no
wrapping array, no summary object, nothing else on the stream. Keys are alphabetical.

```json
{"code":"type-mismatch","column":21,"endColumn":27,"endLine":4,"line":4,"message":"expected `integer`, found `character`","path":"/home/you/demo/main.R","related":[],"severity":"error"}
```

| Field | Type | Meaning |
| ----- | ---- | ------- |
| `path` | string | Absolute path of the file the finding is in |
| `line` | integer | 1-based start line |
| `column` | integer | The 1-based start column, counted in characters. It is the same number the rendered output and your editor show |
| `endLine` | integer | 1-based end line |
| `endColumn` | integer | 1-based end column, exclusive, in characters |
| `severity` | `"warning"` \| `"error"` | The only two levels the CLI emits |
| `code` | string | The [diagnostic code](/reference/diagnostic-codes), which is exactly what a `# ry: allow(...)` comment spells |
| `message` | string | The rendered message |
| `related` | array | Companion locations, `[]` when there are none. Each entry has `path`, `line`, `column`, `endLine`, `endColumn`, and `message`. A `duplicate` finding names both sites |

A related location is dropped when its file is outside the files this run analyzed, rather than
reported with a path you could not use. Warnings the CLI emits about its own run, such as an unknown
configuration key or an unreadable file, stay on stderr as human-readable text and never appear as
JSON.

These field names are a contract. [Continuous integration](/guides/continuous-integration) shows a
job that consumes them.
