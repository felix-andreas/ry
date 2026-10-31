---
title: Getting started
description: What ry is, how to install it, and your first check
---

ry is a fast checker, language server, and formatter for R, written in Rust. It finds mistakes in
your R code before it runs (a misspelled name, a wrong argument, a `NULL` you forgot) and works on
the code you already have, with nothing to annotate or configure first.

## Install

- **VS Code:** install the
  [extension](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry), which bundles
  the binary for Linux x86_64, macOS on Apple silicon, and Windows x86_64.
- **Command line:** download the binary for your platform from
  [0.3.2-beta](https://github.com/felix-andreas/ry/releases/tag/0.3.2-beta), or build it with
  `cargo install --git https://github.com/felix-andreas/ry ry-lang`.

[Installation](/installation) covers Zed, RStudio, Nix, and the remaining cases.

## Check a project

Run `ry check` in a project directory:

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

R itself would report this only when the line runs. Files under `R/` are read as a package that
shares one namespace, as R loads them, and every other `.R` file is a script read from top to
bottom.

## Turn on type checking

Type errors are opt-in. Add a `ry.toml` to the project root:

```toml
[check]
typing = true
```

ry already infers a type for every value from how it is used. Now it also reports the mismatches:

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

## Next

- [Why ry](/why-ry): what it is good at, and what it deliberately leaves out
- [Checking code](/guides/checking): every kind of finding, and how to silence one
- [Type checking](/type-checking/overview): how inference and annotations work
