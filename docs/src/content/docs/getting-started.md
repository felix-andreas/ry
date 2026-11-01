---
title: Getting started
description: What ry is, how to install it, and your first check
---

ry finds the mistakes R reports only when a line runs (a misspelled name, a wrong argument, a
`NULL` you forgot) while you type, in the R code you already have. It checks, formats, loads and
runs R from one tool configured by one `ry.toml`, and it adds a type system on top of R that takes
your code, one step at a time, to typed code that is safe to change.

This page covers the first two of the
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

Run `ry check` in a project directory. Files under `R/` are read as a package that shares one
namespace, as R loads them, and every other `.R` file is a script read from top to bottom:

```r
# R/discount.R
apply_discount <- function(price, rate) {
  price * ratee
}
```

```text
unresolved

  ! I could not resolve `ratee` in this package, its imports, or builtins. Did you mean `rate`?
   --[R/discount.R:2:11]
 1 | apply_discount <- function(price, rate) {
 2 |   price * ratee
   |           ^^^^^
 3 | }

1 problem in 1 file
```

R itself would report this only when the line runs.

## Step 2: turn on type checking

Type errors are opt-in. Add a `ry.toml` to the project root:

```toml
[check]
typing = true
```

ry already infers types from how values are used. Now it also reports the mismatches:

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

## Run it

`ry run` runs a script on ry's own runtime, and `ry run` alone opens a console whose completion
comes from the type checker. `ry R` does the same on the GNU R installed on your machine. ry's
runtime is still incomplete, so use `ry R` for code that needs something it lacks:

```sh
ry run script.R   # on ry's runtime
ry R script.R     # on GNU R
ry R              # a GNU R console with typed completion
```

## Next

- [Checking code](/guides/checking): every kind of finding, and how to silence one
- [Console](/guides/console): running code, and what completion knows
- [Type checking](/type-checking/overview): how inference and `#:` annotations work
- [Inline annotations](/type-checking/inline-annotations): step 3, types written in the code itself
- [Records and tags](/type-checking/records-and-tags) and [strict mode](/type-checking/strict-mode):
  step 4, ry's own types and code that is typed throughout
