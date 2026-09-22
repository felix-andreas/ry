---
title: Stubs
description: What the checker already knows about base R and packages, and how to teach it about one it does not
---

The checker has no R runtime, so it cannot look up what `nchar()` returns. It reads **stubs**:
declaration-only files that describe a package's types, the way rust-analyzer knows `std` without
running it.

## What ships

Only a small kernel is built in: the operators, plus `c()` and `list()`. Everything else comes from
the shipped corpus.

| | Namespaces |
| --- | --- |
| **Fully typed** | `base`, `stats`, `utils`, `methods`, `graphics`, `grDevices`, `datasets`, all attached by default |
| **Typed when your project uses them** | `data.table`, `dplyr`, `ggplot2`, `testthat` |
| **Export lists only** | the tidyverse (and `library(tidyverse)` itself), `knitr`, `rlang`, `glue`, `magrittr`, `scales`, `jsonlite`, `R6`, and every namespace R ships |

The difference between the last two rows matters. A **typed** namespace gives calls through it real
types. An **export list** gives no types, but it tells the checker which names exist, which is what
keeps [`unresolved`](/reference/diagnostic-codes) working, so a typo next to a real export is still
caught.

## When a package is not known

Attaching a package ry has never heard of weakens the `unresolved` check across the project: any
bare name *could* be one of that package's exports, so unresolved names are tolerated rather than
reported.

Two things limit the damage. A near miss of a name your **own** project binds is still reported,
because `library(shiny)` cannot explain `repositry` sitting next to a `repository` parameter. And
[strict mode](/reference/type-system#strict-mode) reports every tolerated read, so you can see
exactly how much the attachment switched off instead of reading a clean run as a clean bill of
health.

The fix is to declare the package yourself.

## Teaching it about a package

Drop a `.Rtypes` file under `stubs/` in your project. **The file name is the namespace**, so
`stubs/mypkg.Rtypes` declares `mypkg`.

A couple of lines are enough to restore full checking for the names you actually use:

```
@type Session
connect: fn(host: character) -> Session
```

Stub declarations are bare. They take no `#:` prefix, and `@type` in a stub names an opaque type
rather than giving it a representation. That is usually what you want for a package's own objects: what matters
is that `connect()` returns a `Session` and that a `Session` is not a `character`, not what is
inside it.

That gives you three things at once: `connect()` gets a real signature, `mypkg::connect` validates
as a known namespace, and the unknown-namespace warning goes away.

You do not have to describe the whole package. Declare what you call.

## Overriding a shipped declaration

The same mechanism corrects the shipped corpus. A declaration in your `stubs/` directory
**replaces** the shipped one of the same name, so if a return type is wrong, or a name is missing
for your version of a package, you can fix it locally without waiting for a release.

Overriding a name does not remove it from its own namespace: `stats::sd` stays valid under an `sd`
override.

Nothing here fails silently. `ry check` reports every declaration it had to drop as an error on that
stub line, whether the line did not parse or it named a type that does not exist. Your editor shows
the same while the `.Rtypes` file is open.

## Next

- [Authoring stubs](/contributing/authoring-stubs) has the full declaration format, overload sets, and export manifests
- [Limitations](/type-checking/limitations) covers what stubs cannot fix
