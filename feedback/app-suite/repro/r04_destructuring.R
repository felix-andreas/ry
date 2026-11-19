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

# Pattern: functions return several values in a list, and the caller assigns them to several
# names in one step with the %<-% operator of the zeallot package.
library(zeallot)

f <- function(i) list(i, i + 1)
g <- function(i) list(first = i, second = list(i, "a"), third = NULL)

bench("destructure: c(a, b) %<-% f()", function(i) { c(a, b) %<-% f(i); a + b })
bench("destructure: three names, nested value", function(i) { c(a, b, d) %<-% g(i); a })
bench("destructure: the same with plain assignments", function(i) { r <- f(i); a <- r[[1]]; b <- r[[2]]; a + b })
