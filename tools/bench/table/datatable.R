# Runs the table benchmark's questions under data.table and prints one tab-separated line per run:
# `question  run  seconds  rows  cols  checksum`. Loading is not timed, as in db-benchmark, and
# every question runs twice so the second run shows warm-cache behavior.
#
# Usage: Rscript datatable.R <data-dir>

args <- commandArgs(trailingOnly = TRUE)
dir <- args[[1]]
suppressMessages(library(data.table, lib.loc = Sys.getenv("RY_BENCH_RLIB", "~/R/library")))
setDTthreads(0L)
cat(sprintf("# data.table %s, %d threads\n", packageVersion("data.table"), getDTthreads()))

read <- function(name) fread(file.path(dir, name), stringsAsFactors = TRUE, showProgress = FALSE)

checksum <- function(answer) {
  if (!is.data.frame(answer)) return(sum(as.numeric(answer)))
  numeric <- Filter(is.numeric, answer)
  sum(vapply(numeric, function(column) sum(as.numeric(column), na.rm = TRUE), numeric(1)))
}

report <- function(question, run, seconds, answer) {
  rows <- if (is.data.frame(answer)) nrow(answer) else length(answer)
  cols <- if (is.data.frame(answer)) ncol(answer) else 1L
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
time_question("groupby q1", x[, .(v1 = sum(v1)), by = id1])
time_question("groupby q2", x[, .(v1 = sum(v1)), by = .(id1, id2)])
time_question("groupby q3", x[, .(v1 = sum(v1), v3 = mean(v3)), by = id3])
time_question("groupby q4", x[, lapply(.SD, mean), by = id4, .SDcols = 7:9])
time_question("groupby q5", x[, lapply(.SD, sum), by = id6, .SDcols = 7:9])
time_question("groupby q6", x[, .(median_v3 = median(v3), sd_v3 = sd(v3)), by = .(id4, id5)])
time_question("groupby q7", x[, .(range_v1_v2 = max(v1) - min(v2)), by = id3])
time_question("groupby q8", x[order(-v3), .(largest2_v3 = head(v3, 2L)), by = id6])
time_question("groupby q9", x[, .(r2 = cor(v1, v2)^2), by = .(id2, id4)])
time_question("groupby q10", x[, .(v3 = sum(v3), count = .N), by = id1:id6])

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
time_question("join q1", x[small, on = "id1", nomatch = NULL])
time_question("join q2", x[medium, on = "id2", nomatch = NULL])
time_question("join q3", medium[x, on = "id2"])
time_question("join q4", x[medium, on = "id5", nomatch = NULL])
time_question("join q5", x[big, on = "id3", nomatch = NULL])
rm(x, small, medium, big)
invisible(gc())

prices <- fread(file.path(dir, "rolljoin_prices.csv"), showProgress = FALSE)
trades <- fread(file.path(dir, "rolljoin_trades.csv"), showProgress = FALSE)
setkey(prices, ticker, time)
time_question("rolljoin keyed", prices[trades, on = .(ticker, time), roll = TRUE])
