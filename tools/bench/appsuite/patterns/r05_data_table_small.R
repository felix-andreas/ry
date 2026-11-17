# bench(label, f) times f(i). It doubles the number of calls until one batch takes 0.3 s of CPU
# time, then prints the label and the nanoseconds per call of the last batch. With the environment
# variable UNDER_HANDLER=1, the calls run inside withCallingHandlers(), as the code of a test does.
under_handler <- nzchar(Sys.getenv("UNDER_HANDLER"))
run <- function(n, f) for (i in seq_len(n)) f(i)
bench <- function(label, f) {
  n <- 64
  repeat {
    start <- sum(proc.time()[1:2])
    if (under_handler) withCallingHandlers(run(n, f), warning = function(w) NULL) else run(n, f)
    seconds <- sum(proc.time()[1:2]) - start
    if (seconds >= 0.3) break
    n <- n * 2
  }
  cat(sprintf("%s\t%.0f\n", label, 1e9 * seconds / n))
}

# Pattern: many operations on small data.tables (10 to 100 rows). The time goes into the
# R code of `[.data.table` and into the calls of the package's C code, not into the data.
library(data.table)
setDTthreads(1)

days <- as.Date("2024-01-01") + 0:9
make <- function(i) data.table(col1 = paste0("k", 1:10), col2 = days, col3 = as.numeric(1:10) + i, col4 = rep(c("x", "y"), 5))
dt <- make(0)
parts <- lapply(1:3, make)
many <- lapply(1:50, make)
df <- as.data.frame(dt)

bench("data.table: data.table() with 4 columns", function(i) make(i))
bench("data.table: rbindlist() of 3 tables", function(i) rbindlist(parts))
bench("data.table: rbind() of 2 tables", function(i) rbind(parts[[1]], parts[[2]]))
bench("data.table: Reduce(rbind) over 50 tables", function(i) Reduce(rbind, many))
bench("data.table: dt[col1 == 'k3']", function(i) dt[col1 == "k3"])
bench("data.table: dt[col1 == 'k3', col3]", function(i) dt[col1 == "k3", col3])
bench("data.table: dt[, sum(col3), by = col4]", function(i) dt[, sum(col3), by = col4])
bench("data.table: dt[, .(col1, col3)]", function(i) dt[, .(col1, col3)])
bench("data.table: copy and := one column", function(i) { x <- copy(dt); x[, col5 := col3 * 2]; x })
bench("data.table: set() one column", function(i) set(dt, j = "col3", value = as.numeric(1:10)))
bench("data.table: dt$col3 and dt[['col3']]", function(i) dt$col3 + dt[["col3"]])
bench("data.table: dt[order(col3)]", function(i) dt[order(-col3)])
bench("data.table: as.data.table(data.frame)", function(i) as.data.table(df))
bench("data.table: merge() of two tables on one key", function(i) merge(parts[[1]], parts[[2]], by = "col1"))
bench("data.table: nrow() and names()", function(i) nrow(dt) + length(names(dt)))
bench("data.table: col1 %chin% keys", function(i) dt$col1 %chin% c("k1", "k2"))
