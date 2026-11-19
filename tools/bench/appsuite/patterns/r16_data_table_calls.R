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

# Pattern: the calls a test user's suite makes on a 20-row table and a 10-row lookup table, in the
# order the suite makes them. `merge()` runs last because it calls `[.data.table` with arguments
# the earlier joins and groupings did not use, so its row also shows whether those calls left the
# process slower (they did when glibc's malloc returned package buffers to the kernel).
library(data.table)
setDTthreads(1)

dt <- data.table(key1 = sprintf("P%02d", 1:20), col1 = rep(c("a", "b"), 10), col2 = as.numeric(1:20), col3 = as.Date("2026-01-01") + 0:19)
other <- data.table(key1 = sprintf("P%02d", seq(1, 20, 2)), col4 = letters[1:10])

bench("data.table calls: dt[col2 > 5]", function(i) dt[col2 > 5])
bench("data.table calls: dt[key1 %chin% keys]", function(i) dt[key1 %chin% c("P01", "P07")])
bench("data.table calls: dt[, .(key1, col2)]", function(i) dt[, .(key1, col2)])
bench("data.table calls: dt[, .(s = sum(col2)), by = col1]", function(i) dt[, .(s = sum(col2)), by = col1])
bench("data.table calls: copy(dt)[, w := col2 / 2]", function(i) copy(dt)[, w := col2 / 2])
bench("data.table calls: other[dt, on = 'key1']", function(i) other[dt, on = "key1"])
bench("data.table calls: rbindlist() of 2 tables", function(i) rbindlist(list(dt, dt)))
bench("data.table calls: setorder(copy(dt), -col2)", function(i) setorder(copy(dt), -col2))
bench("data.table calls: data.table() with 3 columns", function(i) data.table(a = 1:5, b = letters[1:5], c = 5:1))
bench("data.table calls: as.data.table(data.frame())", function(i) as.data.table(data.frame(a = 1:5, b = letters[1:5])))
bench("data.table calls: nrow() and names()", function(i) { nrow(dt); names(dt) })
bench("data.table calls: merge(all.x = TRUE) after joins", function(i) merge(dt, other, by = "key1", all.x = TRUE))
