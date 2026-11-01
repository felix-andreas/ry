#@ weight 2
#@ param ROWS int 5 50
# Columnar frames through polars when it is installed, with a base R fallback.
#: fn() -> character
frame_backend_@ID@ <- function() {
  if (requireNamespace("polars", quietly = TRUE)) "polars" else "base"
}

#: fn(values: double[]) -> double
summarise_column_@ID@ <- function(values) {
  if (identical(frame_backend_@ID@(), "polars")) {
    frame <- polars::as_polars_df(data.frame(x = values))
    #: @if-unknown fn(...: Any) -> polars_data_frame
    select <- frame$select
    #: @if-unknown fn(...: character) -> list{mean: fn() -> list{alias: fn(name: character) -> Any}}
    col <- polars::pl$col
    #: double
    mean_value <- as.data.frame(select(col("x")$mean()$alias("mean")))$mean
    mean_value
  } else {
    mean(values)
  }
}

#: fn(table: character, column: character, threshold: double) -> character
sql_filter_@ID@ <- function(table, column, threshold) {
  con <- DBI::ANSI()
  paste0("SELECT * FROM ", DBI::dbQuoteIdentifier(con, table), " WHERE ", DBI::dbQuoteIdentifier(con, column), " > ", threshold)
}
#@ test
test_that("frames @ID@ summarise identically across backends", {
  values <- seq_len(@ROWS@) * 1.5
  expect_equal(summarise_column_@ID@(values), mean(values))
  expect_equal(sql_filter_@ID@("t", "x", 3), "SELECT * FROM \"t\" WHERE \"x\" > 3")
})
