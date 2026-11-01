---
title: Getting started
description: Install ry, check the R code you have, and turn on type checking
---

R reports a misspelled name, a wrong argument or a forgotten `NULL` only when the line runs, which
may be in production. ry reports them as you type, in the R code you already have, with nothing to
annotate. Run it in any R project:

```r
# R/discount.R
apply_discount <- function(price, rate) {
  price * ratee
}
```

```text
$ ry check
unresolved

  ! I could not resolve `ratee` in this package, its imports, or builtins. Did you mean `rate`?
   --[R/discount.R:2:11]
 1 | apply_discount <- function(price, rate) {
 2 |   price * ratee
   |           ^^^^^
 3 | }

1 problem in 1 file
```

When you work on a package, `ry run` started inside it reloads what you changed before each
command, so you never call `load_all()` or restart R:

```text
$ ry run
Loaded tally: 1 definition bound lazily, 0 top-level statements run, in 5 ms.
ry console on ry's runtime. Type q() or Ctrl-D to quit.
> count_words("the  quick fox")
[1] 4
> count_words("the  quick fox")     # after editing R/count.R
Reloaded tally: 1 definition rebound, in 3 ms.
[1] 3
```

The same tool formats your code and powers your editor, all configured by one `ry.toml`. This page takes you through the first two of the
[four steps](/why-ry#from-plain-r-to-fully-typed-code-in-four-steps): checking the code you have,
then turning on types.

## Install

- **VS Code:** install the
  [extension](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry), which bundles
  the binary for Linux x86_64, macOS on Apple silicon, and Windows x86_64.
- **Command line:** download the binary for your platform from
  [0.3.2-beta](https://github.com/felix-andreas/ry/releases/tag/0.3.2-beta), or build it with
  `cargo install --git https://github.com/felix-andreas/ry ry-lang`.

- **From a clone of the repository:** `just try` builds ry and its runtime, then runs each command
  on this page, and more, in a demo project.

[Installation](/installation) covers Zed, RStudio, Nix, and the remaining cases.

## Step 1: check the code you have

Run `ry check` in the project directory, as above. Files under `R/` are read as one package that
shares a namespace, as R loads them, and every other `.R` file is a script read from top to bottom.
`ry check` exits with status 1 when it finds anything, so the same command works in CI.
[Checking code](/guides/checking) shows everything it finds without configuration.

The rest of step 1 is the everyday tooling. To reload your package as you edit, start `ry run`
anywhere inside it, as in the transcript above. It runs on ry's own runtime, which is still
incomplete, so for a package it cannot run yet, `ry R` opens the same console on the GNU R installed
on your machine. Both need R installed; checking does not. `ry run script.R` and `ry R script.R`
run a script, and `load_all()` in it loads the package ([Console](/guides/console)). In RStudio, put
`source(pipe("ry load --r-entry"))` in your `~/.Rprofile` and call `load_all()` as you do today.
[Loading packages](/guides/loading-packages) has the details.

## Step 2: turn on type checking

Type errors are opt-in, because an existing code base can have many and you should choose when to
face them. Add a `ry.toml` to the project root:

```toml
[check]
typing = true
```

ry infers types from how values are used, so it now reports a call that passes the wrong kind of
value, still without a single annotation:

```r
apply_discount <- function(price, rate) price * (1 - rate)
apply_discount(100, "0.2")
```

```text
type-mismatch

  x expected a numeric value (`integer` or `double`), found `character`
   --[R/discount.R:2:21]
 1 | apply_discount <- function(price, rate) price * (1 - rate)
 2 | apply_discount(100, "0.2")
   |                     ^^^^^

1 problem in 1 file
```

In VS Code, the extension shows the same findings as you type, plus hover with each value's
inferred type ([Editors](/guides/editors)).

## Next

- [Type checking](/type-checking/overview): how inference and `#:` annotations work
- [Inline annotations](/type-checking/inline-annotations): step 3, types written in the code itself
- [DataFrames](/guides/dataframes): ry's typed table, which also runs existing data.table code
- [Records and tags](/type-checking/records-and-tags) and [strict mode](/type-checking/strict-mode):
  step 4, ry's own types and code that is typed throughout
