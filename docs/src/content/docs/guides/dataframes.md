---
title: Working with DataFrames
description: ry's built-in table, queried with a Polars-style API and run on the Polars engine
---

A `DataFrame` is ry's built-in table, for code where data.table is too slow or too hard to read.
Each query runs inside the [Polars](https://pola.rs) engine instead of as R code, which on
db-benchmark's questions at a million rows takes about a tenth of the time data.table does
([Speed](#speed)), and the type checker knows every column's type at every step, so a misspelled
column is reported before the query runs. Code you already wrote for data.table keeps working, and
most of its queries run on the same engine
([Existing data.table code](#existing-datatable-code)).

You query it with Polars' verbs (`select`, `with_columns`, `filter`, `group_by` and `agg`, `sort`,
`join` and `head`) and column expressions written as `col("x")`. DataFrames need ry's own runtime,
so run scripts that use them with `ry run script.R`:

```r
sales <- DataFrame(
  region = c("eu", "us", "eu", "asia", "us"),
  units = c(3L, 5L, 2L, 7L, 1L),
  price = c(1.5, 2, 3, 0.5, 4)
)
sales |>
  DataFrame.filter(col("units") > 1L) |>
  DataFrame.with_columns(revenue = col("units") * col("price")) |>
  DataFrame.group_by("region") |>
  GroupBy.agg(
    col("revenue") |> Expr.sum(),
    orders = Expr.len()
  ) |>
  DataFrame.sort("revenue", descending = TRUE)
```

```text ry run runtime=ry exact
$ ry run sales.R
   region revenue orders
   <char>   <num>  <int>
1:     eu    10.5      2
2:     us    10.0      1
3:   asia     3.5      1
```

Every function has a qualified name, such as `DataFrame.filter` or `Expr.sum`, and takes the value
it works on as its first argument, so a query reads top to bottom with R's pipe `|>`. There is no
method dispatch: `DataFrame.filter` always means the same function.

## Existing data.table code

Code written for data.table runs unchanged under `ry run`, and most queries on a
`data.table` run on the same engine as DataFrames. A `data.table` keeps all of data.table's
behavior: `:=` still updates it in place, and every alias sees the change. ry answers a query only
when it can produce exactly what data.table would, meaning the same values, attributes, printing
and warnings. Otherwise data.table answers it. These go to data.table:

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

```text
[ry] dt[x > 1L, .(n = .N), by = g]
        g     n
   <char> <int>
1:      b     1
2:      a     1
[data.table] dt[x > 1L]
Key: <g>
        g     x
   <char> <int>
1:      a     3
2:      b     2
```

## Building a DataFrame

- `DataFrame(name = value, ...)` builds one from vectors. A length-one value is recycled.
- `as.DataFrame(x)` converts a list, a `data.frame` or a `data.table`, and `is.DataFrame(x)` tests
  for one.
- `DataFrame.read_csv(path)` and `DataFrame.read_parquet(path)` read a file.
  `strings_as_factors = TRUE` reads text columns as factors, and a whole-number column becomes
  `integer` when every value fits, as `fread` does.
- `DataFrame.write_csv(df, path)` and `DataFrame.write_parquet(df, path)` write one.
- `DataFrame.schema(df)`, `DataFrame.columns(df)`, `DataFrame.height(df)` and
  `DataFrame.width(df)` describe it:

```r
DataFrame.schema(sales)
```

```text ry run runtime=ry exact continue
       region         units         price
"character[]"   "integer[]"    "double[]"
```

## Expressions

`col("units")` refers to a column. Arithmetic (`+`, `-`, `*`, `/`, `^`, `%/%`, `%%`), comparisons,
`&`, `|` and `!` combine columns and plain R values into a new expression, so
`col("units") * col("price")` and `col("price") > 2` are both expressions. `lit(value)` makes a
constant column.

Aggregations reduce a column to one value: `Expr.sum`, `Expr.mean`, `Expr.median`, `Expr.min`,
`Expr.max`, `Expr.first`, `Expr.last`, `Expr.std`, `Expr.var` and `Expr.count`. `Expr.len()`
counts rows, `Expr.cor(a, b)` correlates two columns, `Expr.is_null` tests for missing values, and
`Expr.if_else(test, yes, no)` picks per row.

An expression's output column takes the name of the first column it reads. Rename it with
`Expr.alias`, or by passing it as a named argument to a verb, as `revenue = ...` does above. Mind
R's precedence when you pipe into `Expr.alias`: `|>` binds tighter than `*`, so wrap an arithmetic
expression in parentheses first.

```r
(col("price") * 2) |> Expr.alias("double")
```

```text ry run runtime=ry exact continue
(col("price") * 2.0).alias("double")
```

## Verbs

| Verb | What it does |
| --- | --- |
| `DataFrame.select(df, ...)` | keeps only the columns or expressions given |
| `DataFrame.with_columns(df, ...)` | adds or replaces columns, keeping the rest |
| `DataFrame.filter(df, ...)` | keeps the rows where every condition is `TRUE` |
| `DataFrame.group_by(df, ...)` then `GroupBy.agg(groups, ...)` | one row per group, with one aggregation per column |
| `DataFrame.sort(df, ..., descending = FALSE)` | orders the rows |
| `DataFrame.head(df, n = 5)` and `DataFrame.tail(df, n = 5)` | the first or last rows |
| `DataFrame.join(df, other, on, how = "inner")` | joins two DataFrames |

A string names a column wherever a verb takes an expression, so `DataFrame.select(df, "region")`
is short for `DataFrame.select(df, col("region"))`. Inside `GroupBy.agg`, each expression must
give one value per group. `col("units")` alone gives one value per row, so the checker and the
runtime both reject it until you aggregate it.

```r
sales |> DataFrame.select("region", cost = col("price") * 1.2, col("units") >= 3L)
```

```text ry run runtime=ry exact continue
   region  cost  units
   <char> <num> <lgcl>
1:     eu   1.8   TRUE
2:     us   2.4   TRUE
3:     eu   3.6  FALSE
4:   asia   0.6   TRUE
5:     us   4.8  FALSE
```

### Joins

`DataFrame.join` takes the key columns as `on`, or as `left_on` and `right_on` when the names
differ. `how` is `"inner"`, `"left"`, `"full"`, `"semi"` or `"anti"`. The result has the left
columns, then the right's non-key columns, and a right column whose name is already taken gets the
`suffix` (`"_right"` by default), as in Polars.

```r
regions <- DataFrame(region = c("eu", "us"), manager = c("Ana", "Bo"))
DataFrame.join(sales, regions, on = "region", how = "left")
```

```text ry run runtime=ry exact continue
   region units price manager
   <char> <int> <num>  <char>
1:     eu     3   1.5     Ana
2:     us     5   2.0      Bo
3:     eu     2   3.0     Ana
4:   asia     7   0.5    <NA>
5:     us     1   4.0      Bo
```

## Lazy queries

`DataFrame.lazy(df)` turns a DataFrame into a `LazyFrame`, and `LazyFrame.scan_csv(path)` or
`LazyFrame.scan_parquet(path)` starts one from a file without reading it. A `LazyFrame` has the
same verbs under the `LazyFrame.` prefix, but each verb only records a step.
`LazyFrame.collect` optimizes the whole plan and runs it, and `LazyFrame.explain` shows the plan
it would run:

```r
plan <- sales |>
  DataFrame.lazy() |>
  LazyFrame.with_columns(revenue = col("units") * col("price")) |>
  LazyFrame.filter(col("region") == "eu") |>
  LazyFrame.select("region", "revenue")
LazyFrame.explain(plan)
LazyFrame.collect(plan)
```

```text ry run runtime=ry exact continue
SELECT [col("region"), col("revenue")]
  WITH_COLUMNS [(col("units") * col("price")).alias("revenue")]
    FILTER col("region") == "eu"
      DATAFRAME 5 rows, 3 columns
   region revenue
   <char>   <num>
1:     eu     4.5
2:     eu     6.0
```

The plan reads bottom up. The optimizer moved the filter below `with_columns`, so `revenue` is only
computed for the rows that survive. It also pushes filters and the columns a query needs into a
file scan, and a `head` into the scan's row limit, so a lazy query over a large file reads only
what it uses.

## Converting to R's tables

`as.data.frame(df)` gives a plain `data.frame`, and `as.data.table(df)` gives a `data.table` once
`library(data.table)` is attached. Both conversions keep every column's type, factor levels and
missing values. A DataFrame is not a `data.frame`, so the conversion is always explicit:
`is.data.frame(df)` is `FALSE`, and passing a DataFrame where the checker expects a `data.frame`
is a type error.

```r
as.data.frame(sales)
```

```text ry run runtime=ry exact continue
  region units price
1     eu     3   1.5
2     us     5   2.0
3     eu     2   3.0
4   asia     7   0.5
5     us     1   4.0
```

## data.table syntax

A DataFrame also answers data.table's `df[i, j, by]` queries, including `:=`, joins with `on =`,
`.N` and `.SD`, so code written for data.table keeps working. This syntax is there for
compatibility, and the editor completes its query symbols (`.N`, `.SD`, `.I`, `.GRP`, `.NGRP` and
`.BY`) inside the brackets. New code should use the verbs above, which the checker and the editor understand
better and which read in the order they run. The
[typing reference](/reference/type-system/#datatable-queries) lists what the checker understands
of the bracket syntax.

Unlike data.table, a DataFrame follows R's copy-on-modify rule, so `df[, y := value]` leaves a copy
you took earlier unchanged. When nothing else refers to the DataFrame, the update happens in place
and costs only the new column.

## Speed

db-benchmark's ten group-by and five join questions, three `:=` update loops and a keyed rolling
join, at a million rows on 4 cores, take 0.6 s on DataFrames and 5.0 s on data.table using every
thread, best of two runs each, with the same checksums, and DataFrames are faster on every
question. data.table's own functions (`fread`, `setkey`, `rbindlist`,
`melt` and the rest) run data.table's C code, at the speed they have under GNU R. The
[design notes](/contributing/design/r-runtime/#7-the-table-type) describe the benchmark.
