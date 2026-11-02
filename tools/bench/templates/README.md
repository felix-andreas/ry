# Benchmark templates

Each `.R` file here is one template. The generator instantiates a template many times with
fresh parameters, appends the code part to a package file, and appends the test part to the
matching test file. A template is ordinary R apart from the `#@` header lines and the
`@NAME@` placeholders, so it reads and parses like any other R file.

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

In the typed program (`generate.py --typed`), a run of `#:= CODE` lines replaces, one for one,
the lines right after it. That is how a template uses ry's records, tuples and tags where the
untyped program, which must run under GNU R, keeps plain lists: the untyped program drops every
`#:` line, so both programs draw the same instances. The `tables`, `market_data` and `trials`
templates use it the same way for DataFrames in place of data.table, and `just bench-frames` times
both variants of those modules end to end.

`@ID@` is always available and is unique per instance across the package, so every function
name must end in `@ID@` (or contain it). Top-level `name <- function` and `name <- R6::R6Class`
assignments are exported automatically.

A test must pass under GNU R, must not touch the network, and must stay deterministic. Calls
to services that cannot run in a test (Azure, Bloomberg, ODBC) take an injectable client or
source argument, and the test passes a fake.
