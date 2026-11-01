---
title: Moving from data.table
description: The DataFrame equivalent of each common data.table idiom
---

Your data.table code keeps running unchanged on ry, so moving it is optional. What a move buys you
is the checker: a [DataFrame](/guides/dataframes) query is typed column by column, so a misspelled
column or a sum of a text column is reported before the query runs. You can move one query at a
time, because `as.DataFrame(dt)` converts a `data.table` and `as.data.table(df)` converts back,
keeping every column's type.

Each section below shows a data.table idiom and then its DataFrame equivalent, with the output of
`ry run`. The examples share one table:

```r
sales <- DataFrame(
  region = ["eu", "us", "eu", "asia"],
  units = [3L, 5L, 2L, 7L],
  price = [1.5, 2, 3, 0.5]
)
sales
```

```text ry run runtime=ry exact
   region units price
   <char> <int> <num>
1:     eu     3   1.5
2:     us     5   2.0
3:     eu     2   3.0
4:   asia     7   0.5
```

## Reading a file

| data.table | DataFrame |
| --- | --- |
| `fread("sales.csv")` | `DataFrame.read_csv("sales.csv")` |
| `fread("sales.csv", stringsAsFactors = TRUE)` | `DataFrame.read_csv("sales.csv", strings_as_factors = TRUE)` |

When the path is a string literal, the checker reads the file and types every column.

## Filtering rows: `dt[i]`

```r
dt[units > 2L & region != "asia"]
```

`DataFrame.filter` keeps the rows where every condition holds:

```r
sales |> DataFrame.filter(col("units") > 2L, col("region") != "asia")
```

```text ry run runtime=ry exact continue
   region units price
   <char> <int> <num>
1:     eu     3   1.5
2:     us     5   2.0
```

## Selecting and computing columns: `dt[, j]`

```r
dt[, .(region, units)]
dt[, .(revenue = units * price)]
```

`DataFrame.select` takes column names as strings and computed columns as named expressions:

```r
sales |> DataFrame.select("region", "units")
sales |> DataFrame.select(revenue = col("units") * col("price"))
```

```text ry run runtime=ry exact continue
   region units
   <char> <int>
1:     eu     3
2:     us     5
3:     eu     2
4:   asia     7
   revenue
     <num>
1:     4.5
2:    10.0
3:     6.0
4:     3.5
```

## Grouping: `dt[, j, by]`

```r
dt[, .(units = sum(units)), by = region]
```

`DataFrame.group_by` names the groups, and `GroupBy.agg` gives each aggregation:

```r
sales |>
  DataFrame.group_by("region") |>
  GroupBy.agg(units = col("units") |> Expr.sum())
```

```text ry run runtime=ry exact continue
   region units
   <char> <int>
1:     eu     5
2:     us     5
3:   asia     7
```

## Adding a column: `:=`

```r
dt[, revenue := units * price]
```

`DataFrame.with_columns` returns a new DataFrame with the column added or replaced, so assign the
result. A DataFrame follows R's copy-on-modify rule instead of updating in place, so a copy you took
earlier keeps its old columns:

```r
sales <- sales |> DataFrame.with_columns(revenue = col("units") * col("price"))
sales
```

```text ry run runtime=ry exact continue
   region units price revenue
   <char> <int> <num>   <num>
1:     eu     3   1.5     4.5
2:     us     5   2.0    10.0
3:     eu     2   3.0     6.0
4:   asia     7   0.5     3.5
```

To change only some rows, as `dt[units > 2L, flag := TRUE]` does, compute the column for every row
with `Expr.if_else(test, yes, no)`.

## Joins: keys and `on =`

```r
regions[sales, on = "region"]
# or, with a key
setkey(regions, region)
regions[sales]
```

`X[Y]` keeps every row of `Y`, which is a left join of `Y` with `X`. `DataFrame.join` names both
sides and the kind of join, and needs no key:

```r
regions <- DataFrame(region = ["eu", "us"], manager = ["Ana", "Bo"])
DataFrame.join(sales, regions, on = "region", how = "left")
```

```text ry run runtime=ry exact continue
   region units price revenue manager
   <char> <int> <num>   <num>  <char>
1:     eu     3   1.5     4.5     Ana
2:     us     5   2.0    10.0      Bo
3:     eu     2   3.0     6.0     Ana
4:   asia     7   0.5     3.5    <NA>
```

The left table's columns come first. Use `left_on` and `right_on` when the key names differ, and
`DataFrame.join_asof` for a rolling join (`roll = TRUE`).

## Ordering: `setorder`

```r
setorder(dt, region, -units)
```

`DataFrame.sort` returns the sorted table, with one `descending` flag per column:

```r
sales |> DataFrame.sort("region", "units", descending = c(FALSE, TRUE))
```

```text ry run runtime=ry exact continue
   region units price revenue
   <char> <int> <num>   <num>
1:   asia     7   0.5     3.5
2:     eu     3   1.5     4.5
3:     eu     2   3.0     6.0
4:     us     5   2.0    10.0
```

## Finding the data.table code

Set the `data-table` lint to `"warn"` in `ry.toml` to have `ry check` and the editor mark each call
that creates a data.table (`data.table()`, `fread()`, `as.data.table()` and `setDT()`) with its
DataFrame equivalent:

```toml
[lint]
data-table = "warn"
```
