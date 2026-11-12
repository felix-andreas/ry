# The typed twin of sales.R: the same pipeline on ry's built-in table. The typed function calls the
# table's verbs natively, reads the totals back as a `List<F64>`, and weights them in ry's own
# arithmetic.
pipeline <- function(sales: DataFrame{region: character[], units: integer[], price: double[]}) -> F64 {
  totals <- sales |>
    DataFrame.filter(Expr.col("units") > 3L) |>
    DataFrame.with_columns(revenue = Expr.col("units") * Expr.col("price")) |>
    DataFrame.group_by("region") |>
    GroupBy.agg(total = Expr.sum(Expr.col("revenue"))) |>
    DataFrame.sort("region")
  revenue <- List.from_r(totals$total)
  weighted <- 0
  for (k in List.range(1, List.len(revenue))) {
    weighted <- weighted + revenue[[k]] * Num.to_f64(k)
  }
  weighted
}
i <- seq_len(1000000L)
sales <- DataFrame(region = c("north", "south", "east", "west")[i %% 4L + 1L], units = i %% 17L, price = 1 + i %% 100L)
measure("sales", function() pipeline(sales))
