---
title: Stubs
description: How ry knows about base R and packages without running R, and how to teach it about one it does not know
---

ry never loads R, so it cannot ask `nchar()` what it returns or a package what it exports. It reads
*stubs* instead: declaration files that describe a package's names and types, the way TypeScript reads
`.d.ts` files.

## What ships

| | Always available | Once your project uses them |
| --- | --- | --- |
| **Typed** | `base`, `stats`, `utils`, `methods`, `graphics`, `grDevices`, `datasets` | `data.table`, `dplyr`, `ggplot2`, `testthat` |
| **Export lists only** | the rest of R's own packages: `tools`, `grid`, `parallel`, `splines`, `stats4`, `tcltk`, `compiler` | the tidyverse, `knitr`, `rlang`, `glue`, `magrittr`, `scales`, `jsonlite`, `R6` |

A typed package gives calls real types. An export list gives no types, but it tells ry which names
exist, which is what keeps unresolved-name detection working next to it.

A project uses a package through a `library()` or `require()` call, a `DESCRIPTION` dependency, or a
`NAMESPACE` import. Before that, `mutate` and `fread` are unresolved, as they would be in R, instead of
hiding typos in projects that never load these packages. A `pkg::name` call does not count as use, so
`dplyr::mutate()` in a script with no `library(dplyr)` and no `DESCRIPTION` reports `dplyr` as an
unknown namespace.

## Unknown packages switch checks off

Attaching a package ry has no stub for means any bare name in the project might be one of its
exports. ry cannot tell a typo from an export it has never heard of, so it stops reporting unresolved
names project-wide. Two kinds of finding survive: a near miss of a name your own project defines
(`repositry` next to a `repository` parameter), and, under [strict mode](/reference/type-system#strict-mode),
every name the tolerance let through.

## Writing a stub

Put a `.Rtypes` file under `stubs/`. The file name is the package name:

```
# stubs/dbclient.Rtypes
@type Session
connect : fn(host: character) -> Session
query   : fn(session: Session, sql: character) -> Any
```

That restores unresolved-name checking, types `connect()`, validates `dbclient::conect` as a name the
package does not export, and makes `Session` a type you can use in annotations. `@type` in a stub
declares an opaque type: callers can pass it around, and reading a field from it gives `Unknown`.
That is usually what you want for a package's own objects, whose insides are its business.

Declare only what you call. A name you leave out of a stub is reported as not exported when you use
it qualified, which tells you what to add next.

## Overriding a shipped declaration

A declaration under `stubs/` replaces the shipped one of the same name, so a wrong return type or a
name missing for your package version can be fixed in your project without waiting for a release.
`ry check` reports every line it could not load, so a broken stub never fails silently.

[Authoring stubs](/contributing/authoring-stubs) has the full format, including overload sets and
data-masking functions.
