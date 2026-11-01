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
`setkey(x, col)` sorts and keys a frame, and `key(x)` reads the key. These and `copy` keep
working on frames after `library(data.table)`, and on a data.table they call data.table's own.

## Existing data.table code

Code written for data.table runs unchanged under `ry run --runtime ry`, and most queries on a
data.table run on the same engine as frames. A data.table keeps all of data.table's behavior:
`:=` still updates it in place, and every alias sees the change. ry answers a query only when it
can produce exactly what data.table would, meaning the same values, attributes, printing and
warnings. Otherwise data.table answers it. These go to data.table:

- a keyed table, a table with extra attributes or classes, and code inside data.table's own namespace;
- a filter that data.table answers through an automatic index (`col == value`, `col %in% values`);
- `sum`, `mean`, `median`, `var`, `sd` and `prod` where data.table's result depends on its own
  summation order, such as a sum without `by` or a mean with `na.rm = TRUE` on doubles;
- a join without `on =`, `x[]`, and any `i` or `j` that calls a function with side effects;
- a query that warns or fails.

Set `options(ry.datatable.trace = TRUE)` to see which engine answers each query: ry prints
`[ry]` or `[data.table]` and the call on standard error. `options(ry.datatable = FALSE)` sends
every query to data.table.

```r
library(data.table)
options(ry.datatable.trace = TRUE)
dt <- data.table(g = c("a", "b", "a"), x = 1:3)
dt[x > 1L, .(n = .N), by = g]
setkey(dt, g)
dt[x > 1L]
```

```
[ry] dt[x > 1L, .(n = .N), by = g]
   g n
1: b 1
2: a 1
[data.table] dt[x > 1L]
   g x
1: a 3
2: b 2
```

## Speed

`tools/bench/table/` runs db-benchmark's group-by and join questions, `:=` update loops and a
keyed rolling join, written as the same R script for data.table under GNU R and for frames under
`ry run --runtime ry`. On 1 million rows with 4 threads, both produce the same checksums, and frames
are faster on every group-by question (2 to 39 times), every join (2 to 4 times), and the full and
added-column `:=` loops (5 to 6 times). A filtered `:=` loop and the keyed rolling join run about
as fast as data.table. The [design notes](/contributing/design/r-runtime/#7-the-table-type) have the details.
