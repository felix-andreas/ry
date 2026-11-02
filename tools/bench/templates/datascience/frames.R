#@ weight 2
#@ param ROWS int 5 50
# Columnar frames through polars when it is installed, with a base R fallback.
frame_backend_@ID@ <- function() -> character {
  if (requireNamespace("polars", quietly = TRUE)) "polars" else "base"
}

summarise_column_@ID@ <- function(values: double[]) -> double {
  if (identical(frame_backend_@ID@(), "polars")) {
    frame <- polars::as_polars_df(data.frame(x = values))
    summary <- frame$select(polars::pl$col("x")$mean()$alias("mean"))
    mean_value: double <- as.data.frame(summary)$mean
    mean_value
  } else {
    mean(values)
  }
}

sql_filter_@ID@ <- function(table: character, column: character, threshold: double) -> character {
  con <- DBI::ANSI()
  paste0("SELECT * FROM ", DBI::dbQuoteIdentifier(con, table), " WHERE ", DBI::dbQuoteIdentifier(con, column), " > ", threshold)
}
#@ test
test_that("frames @ID@ summarise identically across backends", {
  values <- seq_len(@ROWS@) * 1.5
  expect_equal(summarise_column_@ID@(values), mean(values))
  expect_equal(sql_filter_@ID@("t", "x", 3), "SELECT * FROM \"t\" WHERE \"x\" > 3")
})
