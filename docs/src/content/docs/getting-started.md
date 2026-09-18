---
title: Getting started
description: What ry is, what it finds on your first run, and where to install it
---

ry is a toolchain for R, written in Rust. You install one thing and get five jobs done:

1. **A language server.** Hover, completion, go-to-definition, references, rename, and inlay hints,
   in any editor that speaks LSP.
2. **A linter.** Typos, unresolved names, and dead assignments, with no configuration.
3. **A formatter.** One consistent style. The only settings are indent width and line endings.
4. **An R console.** A REPL with project-aware completion.
5. **A type checker.** Optional, and its inferred types also power the editor features.

Your code needs no changes, and you run the same thing in your editor and in CI.

## Install

Install ry as a command-line tool, as an editor extension, or both:

- **CLI:** Download a [prebuilt binary](https://github.com/felix-andreas/ry/releases)
- **VS Code Extension:** Install from [marketplace](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry)
- **Zed Extension**: Install manually [from here](https://github.com/felix-andreas/ry/tree/main/editors/zed)

For detailed instructions or other installation methods (e.g. for RStudio or building from source) see the [installation page](/installation).

## Your first run

No configuration, no annotations:

```bash
ry check
```

```r
apply_discount <- function(price, rate) {
  price * ratee
}

apply_discount(100, 0.2)
```

```text
unresolved

  ! I could not resolve `ratee` in this package, its imports, or builtins. Did you mean `rate`?
   --[discount.R:2:11]
 1 | apply_discount <- function(price, rate) {
 2 |   price * ratee
   |           ^^^^^
 3 | }

1 problem in 1 file
```

One transposed letter, found without running anything. R reports the same mistake only when
execution reaches that line.

## Now turn on the type checker

Fix the typo and make a different mistake, one no linter can catch, because catching it requires
knowing what a value *is*:

```toml
# ry.toml
[check]
typing = true
```

```r
apply_discount(100, "0.2")
```

```text
type-mismatch

  x expected `double`, found `character`
   --[discount.R:5:21]
 4 | 
 5 | apply_discount(100, "0.2")
   |                     ^^^^^
```

Nothing was annotated. ry worked out that `rate` is a number because you multiply by it, and that
same knowledge is what hover, completion, and the console's tab completion read. It runs whether or
not you turn the errors on.

## Next

- [Features](/features) lists everything you get before configuring anything
- [Tutorial](/type-checking/tutorial) puts the type checker on real code
- [Why ry](/why-ry) explains why this exists, and how far along it is
