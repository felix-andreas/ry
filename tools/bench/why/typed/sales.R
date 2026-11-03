# A table pipeline: filter a million sales rows, add a revenue column, and total it per region.
pipeline <- function(sales: DataFrame{region: character[], units: integer[], price: double[]}) -> double {
  totals <- sales |>
    DataFrame.filter(col("units") > 3L) |>
    DataFrame.with_columns(revenue = col("units") * col("price")) |>
    DataFrame.group_by("region") |>
    GroupBy.agg(total = Expr.sum(col("revenue"))) |>
    DataFrame.sort("region")
  sum(totals$total * seq_along(totals$total))
}
i <- seq_len(1000000L)
sales <- DataFrame(region = c("north", "south", "east", "west")[i %% 4L + 1L], units = i %% 17L, price = 1 + i %% 100L)
measure("sales", function() pipeline(sales))
