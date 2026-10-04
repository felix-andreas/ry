---
title: Getting started
description: Install ry and run it on a project
---

ry is a language server, checker, formatter, and R console for R, in one binary written in Rust.
It works on the code you already have: nothing needs to be annotated or configured first.

## Install

- **VS Code:** install the [extension](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry).
  It bundles the binary for Linux x86_64, macOS on Apple silicon, and Windows x86_64.
- **Command line:** download a [release](https://github.com/felix-andreas/ry/releases), or build it
  with `cargo install --git https://github.com/felix-andreas/ry ry-lang`.
- **Zed, RStudio, other platforms:** see [installation](/installation).

## Run it

In a project directory:

```sh
ry check
```

```text
unresolved

  ! I could not resolve `ratee` in this package, its imports, or builtins. Did you mean `rate`?
   --[R/discount.R:2:11]
 1 | apply_discount <- function(price, rate) {
 2 |   price * ratee
   |           ^^^^^
```

Files under `R/` are read as a package and share one namespace, as R loads them. Every other file is
a script, read from top to bottom, that can use the package's functions. To also report type errors,
add a `ry.toml`:

```toml
[check]
typing = true
```

## Next

- [Tour](/tour): every feature, and why it works the way it does
- [Continuous integration](/guides/continuous-integration): a working CI job
- [Adopting an existing codebase](/guides/adopting): turning type checking on without drowning in
  findings
