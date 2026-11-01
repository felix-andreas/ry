---
title: Frames
description: ry's built-in table, queried with data.table syntax and run as one Polars plan
---

A frame is ry's built-in table. You query it with data.table's `x[i, j, by]` syntax, but the whole
query runs as one plan in the [Polars](https://pola.rs) engine instead of as R code, and the type
checker knows every column's type. Frames need ry's own runtime, so run scripts that use them with
`ry run --runtime ry script.R`.

```r
sales <- frame(team = c("a", "b", "a"), amount = c(1.5, 2, 3))
sales[amount > 1, .(total = sum(amount), n = .N), by = team]
```

```text
     team total     n
   <char> <num> <int>
1:      a   4.5     2
2:      b   2.0     1
```

## Building frames

- `frame(name = value, ...)` builds a frame from vectors; a length-one value is recycled.
- `as.frame(x)` converts a list, a `data.frame` or a `data.table`.
- `frame_read_csv(path)` and `frame_read_parquet(path)` read a file. `strings_as_factors = TRUE`
  reads text columns as factors, and a whole-number column becomes `integer` when every value fits,
  as `fread` does.
- `frame_write_csv(x, path)` and `frame_write_parquet(x, path)` write one.

`as.data.frame(x)` converts back, and so does `as.data.table(x)` once `library(data.table)` is
attached. A frame is not a `data.frame`, so these
conversions are explicit: `is.data.frame(x)` is `FALSE`, and passing a frame where the checker
expects a `data.frame` is a type error.

## Queries

`x[i, j, by]` works as in data.table: `i` filters or orders rows, `j` selects or computes columns,
and `by` or `keyby` groups. Joins take `on =` (including non-equi conditions such as `on = .(id, t >= lo)`),
`nomatch = NULL`, `mult =`, `roll =` and `by = .EACHI`, and `.N`, `.SD`,
`.SDcols`, `.I` and `.GRP` are available. Columns are in scope inside the brackets, so `amount`
above is the column.

Common aggregations (`sum`, `mean`, `min`, `max`, `median`, `sd`, `var`, `first`, `last`, `head`,
`cor` and `.N`) and arithmetic, comparisons and `ifelse` on columns run inside the engine. Most
other R code still works: ry evaluates it as R code, once per group, with the columns it mentions
bound as vectors, so it is slower but correct. A `by` that computes groups, `:=` with `by`, and a
join whose `j` needs R code are not supported yet on that path and stop with an error saying so.
Where data.table warns, for example when a group's integer sum overflows into doubles or `:=`
truncates a value into an integer column, a frame query raises the same warning.

## Updating columns

`x[, y := value]` adds or replaces a column, and `x[, y := NULL]` removes it. Unlike data.table,
a frame follows R's copy-on-modify rule, so a copy you took earlier keeps its old columns:

```r
before <- sales
sales[, tax := amount * 0.2]
names(sales)    # "team" "amount" "tax"
names(before)   # "team" "amount"
```

When nothing else refers to the frame, the update happens in place and costs only the new column.
`setkey(x, col)` sorts and keys a frame, and `key(x)` reads the key. Attaching data.table masks
ry's `setkey`, `setkeyv`, `key` and `copy`; data.table's versions do not accept a frame, so call
`ry::setkey(x, col)` then.

## Speed

`tools/bench/table/` runs db-benchmark's group-by and join questions, `:=` update loops and a
keyed rolling join, written as the same R script for data.table under GNU R and for frames under
`ry run --runtime ry`. On 1 million rows with 4 threads, both produce the same checksums, and frames
are faster on every group-by question (2 to 39 times), every join (2 to 4 times), and the full and
added-column `:=` loops (5 to 6 times). A filtered `:=` loop and the keyed rolling join run about
as fast as data.table. The [design notes](/contributing/design/r-runtime/#7-the-table-type) have the details.

`tools/bench/table/ops.R` and the `ops_benchmark` bench in `crates/table` time data.table's helpers
on the db-benchmark group-by file (10 million rows, 4 threads), and both give the same checksums.
The table engine's numbers are measured from Rust: calls from R reach these kernels only once the
runtime routes data.table's functions to them, which it does not do yet. Times are in seconds.

| Operation | data.table | ry |
|---|---|---|
| `fread` | 2.92 | 1.39 |
| `fwrite` | 0.87 | 0.70 |
| `setkey` | 0.42 | 0.46 |
| `setorder` | 0.97 | 0.69 |
| `unique` | 0.10 | 0.09 |
| `uniqueN` | 0.13 | 0.10 |
| `rbindlist` | 0.88 | 0.29 |
| `melt` | 1.20 | 0.39 |
| `dcast` | 0.26 | 0.26 |
| `shift` | 0.024 | 0.004 |
| `fifelse` | 0.084 | 0.050 |
| `fcoalesce` | 0.059 | 0.047 |
| `nafill` | 0.081 | 0.042 |
| `rleid` | 0.026 | 0.011 |
| `between` | 0.018 | 0.013 |
| `%chin%` | 0.049 | 0.036 |
| `tstrsplit` | 5.17 | 1.86 |

Each cell is the faster of two runs.
