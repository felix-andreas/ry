# Benchmark templates

Each `.R` file here is one template. The generator instantiates a template many times with
fresh parameters, appends the code part to a package file, and appends the test part to the
matching test file. A template is ry code with inline type annotations, apart from the `#@`
header lines and the `@NAME@` placeholders. An untyped run erases the annotations, which gives the
plain R program. ry's records, tuples and tags have no erasure, so code that uses them gives its
plain-R spelling beside it in a variant block:

```r
#@ typed
  (time = times, flow = flows)
#@ untyped
  list(time = times, flow = flows)
#@ end
```

The typed run keeps the first branch and the untyped run the second. Both runs count the untyped
branch toward the line target, so the same seed picks the same instances. The typed branch is where
the typed code base models its data with records and tags instead of lists, S4 or R6.

- `shared/` holds infrastructure used by all three code bases (settings, caching, logging,
  serialization, files, strings, conditions, parallelism, HTTP, markup, calendars, S4,
  storage backends, progress, repositories).
- `finance/`, `datascience/` and `biostatistics/` hold the domain code.

Header lines, all before the code:

| Header | Meaning |
| --- | --- |
| `#@ weight N` | Relative frequency among the templates of its directory (default 1). |
| `#@ param NAME int LOW HIGH` | A random integer, substituted for `@NAME@`. |
| `#@ param NAME float LOW HIGH DIGITS` | A random decimal. |
| `#@ param NAME choice a\|b\|c` | One of the listed words. |
| `#@ ns LINE` | A line added to the package `NAMESPACE`, for S4 exports and imports. |
| `#@ test` | Everything after this line is the test code. |
| `#@ typed`, `#@ untyped`, `#@ end` | A variant block, in the code or the test. An `#@ ns` line inside one belongs to that variant only. |

Templates are written with inline annotations, which the untyped program erases. Where ry's
records, tuples or tags model the data better than plain R can, the template keeps both spellings
in a variant block: `#@ typed` opens the typed program's lines, `#@ untyped` the plain-R lines that
must run under GNU R, and `#@ end` closes the block. The line target counts the untyped variant, so
a typed and an untyped run with the same seed draw the same instances. The `tables`,
`market_data` and `trials` templates use variant blocks the same way for DataFrames in place of
data.table, and `just bench-frames` times both variants of those modules end to end.

`@ID@` is always available and is unique per instance across the package, so every function
name must end in `@ID@` (or contain it). Top-level `name <- function` and `name <- R6::R6Class`
assignments are exported automatically.

A test must pass under GNU R, must not touch the network, and must stay deterministic. Calls
to services that cannot run in a test (Azure, Bloomberg, ODBC) take an injectable client or
source argument, and the test passes a fake.
