---
title: Packages
description: How ry knows about packages without running R, and how to describe one it does not know
---

ry never loads R, so it cannot ask a package what it exports. It reads *stubs* instead: declaration
files that list a package's names and their types, the way TypeScript reads `.d.ts` files.

## What ships

Base R and its default packages are typed, and so are `data.table`, `dplyr`, `ggplot2`, and
`testthat`. For the rest of R's own packages, the tidyverse, and `knitr`, `rlang`, `glue`,
`magrittr`, `scales`, `jsonlite`, and `R6`, ry ships export lists: the names without their types,
which is enough to keep unresolved-name checks working.

Bare names from base R's default packages always resolve. Every other package counts once your
project uses it, through a `library()` call, a `DESCRIPTION` dependency, or a `NAMESPACE` import.
Until then `mutate` is unresolved, as it would be in R.

## A package ry does not know

Attaching a package with no stub means any bare name in the project might be one of its exports. ry
cannot tell a typo from an export it has never heard of, so it stops reporting unresolved names
across the whole project:

```r
library(dbclient)
reslt <- 1
print(result)   # not reported
```

A stub turns the check back on. Put it under `stubs/`, named after the package:

```
# stubs/dbclient.Rtypes
@type Session
connect : fn(host: character) -> Session
```

Now the typo is reported, and with type checking on, so are calls that do not fit the declarations:

```r
library(dbclient)
session <- connect(42)    # expected `character`, found `double`
print(result)             # I could not resolve `result` in this package, its imports, or builtins.
dbclient::conect("db")    # `conect` is not exported by `dbclient`.
```

Declare only what you call: a name you leave out is reported wherever you use it, which tells you
what to add next. Even an empty stub file turns the unresolved-name check back on.

## Overriding a shipped stub

A declaration under `stubs/` replaces a shipped one with the same name, so you can fix a wrong
return type, or add a function your version of the package has, without waiting for a release.
`ry check` reports every line it could not load. [Authoring stubs](/contributing/authoring-stubs) has
the full format, including overloads and data-masking functions.
