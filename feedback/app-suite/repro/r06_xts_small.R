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

# Pattern: many operations on short time series of the xts package (250 rows, 1 to 3 columns).
library(xts)

days <- as.Date("2024-01-01") + 0:249
x <- xts(matrix(as.numeric(1:500), ncol = 2, dimnames = list(NULL, c("a", "b"))), order.by = days)
y <- xts(matrix(as.numeric(1:250), ncol = 1, dimnames = list(NULL, "d")), order.by = days)
day <- days[100]

bench("xts: xts() from a matrix and dates", function(i) xts(matrix(as.numeric(1:500), ncol = 2), order.by = days))
bench("xts: x[day] with a Date", function(i) x[day])
bench("xts: x[day, 'a']", function(i) x[day, "a"])
bench("xts: x['2024-03'] with a text range", function(i) x["2024-03"])
bench("xts: x[10:20, ]", function(i) x[10:20, ])
bench("xts: coredata(x)", function(i) coredata(x))
bench("xts: index(x)", function(i) index(x))
bench("xts: as.numeric(x[day, 'a'])", function(i) as.numeric(x[day, "a"]))
bench("xts: merge(x, y)", function(i) merge(x, y))
bench("xts: rbind() of two halves", function(i) rbind(x[1:125, ], x[126:250, ]))
bench("xts: lag(y) and diff(y)", function(i) diff(y) + lag(y))
bench("xts: x * 2 + x", function(i) x * 2 + x)
bench("xts: first(x) and last(x)", function(i) rbind(first(x), last(x)))
bench("xts: window by dates index(x) <= day", function(i) x[index(x) <= day, ])
bench("xts: zoo::na.locf(y)", function(i) zoo::na.locf(y))
