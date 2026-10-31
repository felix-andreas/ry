---
title: NULL
description: How ry makes you handle a value that may be NULL
---

Many R functions return `NULL` for "nothing", and code that forgets the case fails far from where the
`NULL` came from. A value that may be `NULL` has a union type, such as `list{retries: integer} |
NULL`, and using it as the non-`NULL` part is an error:

```r
#: fn(config: list{retries: integer} | NULL) -> integer
retries <- function(config) {
  config$retries
}
```

```text
type-mismatch

  x expected a list, found `list{retries: integer} | NULL`
   --[R/null.R:3:3]
 3 |   config$retries
   |   ^^^^^^^^^^^^^^
```

Check for `NULL` first, and the error goes away:

```r
retries <- function(config) {
  if (is.null(config)) return(3L)
  config$retries
}
```

The guard *narrows* `config` to the list for the rest of the function. `is.character()`,
`is.numeric()`, `is.list()`, and the other `is.*` tests narrow the same way.

Narrowing applies to variables only, so to test a field, copy it into a variable first:
`r <- config$retries; if (is.null(r)) ...`.
