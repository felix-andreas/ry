# Runs the table benchmark's questions as R code on ry's DataFrame, under `ry run --runtime ry`,
# and prints one tab-separated line per run:
# `question  run  seconds  rows  cols  checksum`. Loading is not timed, as in db-benchmark, and
# every question runs twice so the second run shows warm-cache behavior. The questions use the
# Polars-style API; the top-two question, the `:=` loops and the rolling join use the data.table
# syntax the DataFrame keeps for compatibility, because the API has no top-k, in-place update or
# rolling join.
#
# Usage: ry run --runtime ry ry.R <data-dir>

args <- commandArgs(trailingOnly = TRUE)
dir <- args[[1]]
cat("# ry DataFrame\n")

read <- function(name, factors = TRUE) DataFrame.read_csv(file.path(dir, name), strings_as_factors = factors)

checksum <- function(answer) {
  if (!is.DataFrame(answer)) return(sum(as.numeric(answer)))
  numeric <- Filter(is.numeric, as.list(answer))
  sum(vapply(numeric, function(column) sum(as.numeric(column), na.rm = TRUE), numeric(1)))
}

report <- function(question, run, seconds, answer) {
  rows <- if (is.DataFrame(answer)) nrow(answer) else length(answer)
  cols <- if (is.DataFrame(answer)) ncol(answer) else 1L
  cat(sprintf("%s\t%d\t%.4f\t%d\t%d\t%.6e\n", question, run, seconds, rows, cols, checksum(answer)))
}

time_question <- function(question, expr) {
  expr <- substitute(expr)
  for (run in 1:2) {
    gc()
    seconds <- system.time(answer <- eval(expr, parent.frame()))[["elapsed"]]
    report(question, run, seconds, answer)
    rm(answer)
  }
}

x <- read("groupby.csv")
sum_of <- function(name) col(name) |> Expr.sum()
mean_of <- function(name) col(name) |> Expr.mean()
by <- function(...) DataFrame.group_by(x, ...)
time_question("groupby q1", by("id1") |> GroupBy.agg(sum_of("v1")))
time_question("groupby q2", by("id1", "id2") |> GroupBy.agg(sum_of("v1")))
time_question("groupby q3", by("id3") |> GroupBy.agg(sum_of("v1"), mean_of("v3")))
time_question("groupby q4", by("id4") |> GroupBy.agg(mean_of("v1"), mean_of("v2"), mean_of("v3")))
time_question("groupby q5", by("id6") |> GroupBy.agg(sum_of("v1"), sum_of("v2"), sum_of("v3")))
time_question("groupby q6", by("id4", "id5") |> GroupBy.agg(
  median_v3 = col("v3") |> Expr.median(),
  sd_v3 = col("v3") |> Expr.std()
))
time_question("groupby q7", by("id3") |> GroupBy.agg(
  range_v1_v2 = (col("v1") |> Expr.max()) - (col("v2") |> Expr.min())
))
time_question("groupby q8", x[order(-v3), .(largest2_v3 = head(v3, 2L)), by = id6])
time_question("groupby q9", by("id2", "id4") |> GroupBy.agg(r2 = Expr.cor(col("v1"), col("v2"))^2))
time_question("groupby q10", by("id1", "id2", "id3", "id4", "id5", "id6") |> GroupBy.agg(
  sum_of("v3"),
  count = Expr.len()
))

# `:=` loops: a whole-column update, a 1% subset update and an added column, each 100 times.
time_question("update full", {
  for (step in 1:100) x[, v3 := v3 * 1.000001]
  x[, .(v3 = sum(v3))]
})
time_question("update subset", {
  for (step in 1:100) x[id4 == step, v1 := v1 + 1L]
  x[, .(v1 = sum(v1))]
})
time_question("update add", {
  for (step in 1:100) x[, paste0("added", step) := v2 * step]
  x[, .(added100 = sum(added100))]
})
rm(x)
invisible(gc())

x <- read("join_x.csv")
small <- read("join_small.csv")
medium <- read("join_medium.csv")
big <- read("join_big.csv")
time_question("join q1", DataFrame.join(x, small, on = "id1"))
time_question("join q2", DataFrame.join(x, medium, on = "id2"))
time_question("join q3", DataFrame.join(x, medium, on = "id2", how = "left"))
time_question("join q4", DataFrame.join(x, medium, on = "id5"))
time_question("join q5", DataFrame.join(x, big, on = "id3"))
rm(x, small, medium, big)
invisible(gc())

prices <- read("rolljoin_prices.csv", factors = FALSE)
trades <- read("rolljoin_trades.csv", factors = FALSE)
setkey(prices, ticker, time)
time_question("rolljoin keyed", prices[trades, on = .(ticker, time), roll = TRUE])
