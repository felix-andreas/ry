---
title: Stubs
description: How ry knows about packages without running R, and how to describe one it does not know
---

ry never loads R, so it cannot ask `nchar()` what it returns or a package what it exports. It reads
*stubs* instead: declaration files that list a package's names and their types, the way TypeScript
reads `.d.ts` files. A stub for a package you use looks like this:

```
# stubs/dbclient.Rtypes
@type Session
connect : fn(host: character) -> Session
query   : fn(session: Session, sql: character) -> Any
```

Put it under `stubs/` in your project, named after the package. ry now types `connect()`, reports
`dbclient::conect` as a name the package does not export, and accepts `Session` in annotations.
A `@type` in a stub has no shape, so callers can pass a `Session` around, and reading a field from
it gives `Unknown`: a package's objects are its own business. Declare only what you call, because a
name you leave out is reported wherever you use it, which tells you what to add next.

## Why an unknown package matters

Without a stub, attaching a package with `library()` means that any bare name in the project might be
one of its exports. ry cannot tell a typo from an export it has never heard of, so it stops reporting
unresolved names across the whole project. Only a near miss of a name your own project defines
(`repositry` next to a `repository` parameter) is still reported, and
[strict mode](/reference/type-system#strict-mode) lists every name that got through. A stub, even
the two lines above, turns the check back on.

## What ships

Base R and its default packages (`stats`, `utils`, `methods`, `graphics`, `grDevices`, and
`datasets`) are typed, and so are `data.table`, `dplyr`, `ggplot2`, and `testthat`. For the rest of
R's own packages, the tidyverse, and `knitr`, `rlang`, `glue`, `magrittr`, `scales`, `jsonlite`, and
`R6`, ry ships export lists: the names without their types, which is enough to keep unresolved-name
checks working next to them.

R's own packages are always available. The others count only once your project uses them, through a
`library()` call, a `DESCRIPTION` dependency, or a `NAMESPACE` import. Until then `mutate` is
unresolved, as it would be in R, so a typo is not hidden by a package the project never loads. A
`pkg::name` call does not count as use, so in a script without `library(dplyr)`, `dplyr::mutate()`
reports `dplyr` as an unknown package.

A declaration under `stubs/` replaces a shipped one with the same name, so you can fix a wrong
return type, or add a function your version of the package has, without waiting for a release.
`ry check` reports every line it could not load, so a broken stub never fails silently.
[Authoring stubs](/contributing/authoring-stubs) has the full format, including overloads and
data-masking functions.
