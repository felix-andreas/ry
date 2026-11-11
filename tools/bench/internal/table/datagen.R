# Writes the table benchmark's input data as CSV, so data.table and ry read identical bytes.
#
# The shapes follow the published description of the db-benchmark suite (the group-by and join
# questions H2O started and DuckDB Labs maintains): `groupby.csv` has N rows with three string
# ids, three integer ids and three values; `join_x.csv` is joined against a small (N/1e6), a
# medium (N/1e3) and a big (N) right-hand table, with about 90% of the right-hand keys matching.
# `rolljoin_*.csv` are a keyed price history and a set of trades for the rolling-join question.
#
# Usage: Rscript datagen.R <out-dir> <rows> [groupby | joins]
#   1e7 rows is the 0.5 GB size, 1e8 the 5 GB size; `groupby` writes only `groupby.csv`, and
#   `joins` only the `join_*.csv` files.

args <- commandArgs(trailingOnly = TRUE)
out <- args[[1]]
n <- as.numeric(args[[2]])
part <- if (length(args) > 2L) args[[3]] else "all"
k <- 100
library(data.table, lib.loc = Sys.getenv("RY_BENCH_RLIB", "~/R/library"))
setDTthreads(0L)
dir.create(out, showWarnings = FALSE, recursive = TRUE)
set.seed(108)

ids <- function(prefix_width, count) sprintf(paste0("id%0", prefix_width, "d"), seq_len(count))

# Writes `table` in slices of ten million rows, adding each text column `id<k>` (the key column
# `key` written as "id<key>") slice by slice: at 1e8 rows the text columns of a whole table would
# take more memory than the integer keys they are made from.
write_sliced <- function(table, path, text, order) {
  for (start in seq(1, nrow(table), by = 1e7)) {
    slice <- table[start:min(start + 1e7 - 1, nrow(table))]
    for (name in names(text)) set(slice, j = name, value = sprintf("id%d", slice[[text[[name]]]]))
    setcolorder(slice, order)
    fwrite(slice, path, append = start > 1)
  }
}

if (part != "joins") {
  groupby <- data.table(
    id1 = sample(ids(3, k), n, TRUE),
    id2 = sample(ids(3, k), n, TRUE),
    id3 = sample(ids(10, n / k), n, TRUE),
    id4 = sample(k, n, TRUE),
    id5 = sample(k, n, TRUE),
    id6 = sample(n / k, n, TRUE),
    v1 = sample(5L, n, TRUE),
    v2 = sample(15L, n, TRUE),
    v3 = round(runif(n, max = 100), 6)
  )
  fwrite(groupby, file.path(out, "groupby.csv"))
  rm(groupby)
}
if (part == "groupby") quit(save = "no")

# A right-hand table of `rows` rows over a key space of `space` values: 90% of its keys are drawn
# from the left-hand key space, so they match, and 10% from just above it, so they do not.
right_keys <- function(rows, space) {
  hit <- sample(space, rows * 0.9, replace = rows * 0.9 > space)
  miss <- space + sample(space, rows - length(hit), replace = TRUE)
  as.integer(sample(c(hit, miss)))
}
n_small <- max(n / 1e6, 10)
n_medium <- n / 1e3
x <- data.table(
  id1 = sample(n_small, n, TRUE),
  id2 = sample(n_medium, n, TRUE),
  id3 = sample(n, n, TRUE)
)
x[, v1 := round(runif(.N, max = 100), 6)]
write_sliced(x, file.path(out, "join_x.csv"), c(id4 = "id1", id5 = "id2", id6 = "id3"),
             c("id1", "id2", "id3", "id4", "id5", "id6", "v1"))
rm(x)
small <- data.table(id1 = right_keys(n_small, n_small))
small[, v2 := round(runif(.N, max = 100), 6)]
write_sliced(small, file.path(out, "join_small.csv"), c(id4 = "id1"), c("id1", "id4", "v2"))
medium <- data.table(id1 = sample(n_small, n_medium, TRUE), id2 = right_keys(n_medium, n_medium))
medium[, v2 := round(runif(.N, max = 100), 6)]
write_sliced(medium, file.path(out, "join_medium.csv"), c(id4 = "id1", id5 = "id2"),
             c("id1", "id2", "id4", "id5", "v2"))
big <- data.table(id1 = sample(n_small, n, TRUE), id2 = sample(n_medium, n, TRUE), id3 = right_keys(n, n))
big[, v2 := round(runif(.N, max = 100), 6)]
write_sliced(big, file.path(out, "join_big.csv"), c(id4 = "id1", id5 = "id2", id6 = "id3"),
             c("id1", "id2", "id3", "id4", "id5", "id6", "v2"))
rm(small, medium, big)
if (part == "joins") quit(save = "no")

# Prices: 1,000 tickers, each with an increasing integer timestamp series. Trades: N/10 rows at
# random times, so a rolling join finds the last price at or before each trade.
tickers <- 1000L
prices <- data.table(ticker = sample(tickers, n, TRUE), time = sample(1e9, n))
setorder(prices, ticker, time)
prices[, price := round(runif(.N, 10, 500), 4)]
fwrite(prices, file.path(out, "rolljoin_prices.csv"))
trades <- data.table(ticker = sample(tickers, n / 10, TRUE), time = sample(1e9, n / 10), qty = sample(1000L, n / 10, TRUE))
fwrite(trades, file.path(out, "rolljoin_trades.csv"))
cat("wrote", out, "\n")
