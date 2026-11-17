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

# Pattern: calls are wrapped in tryCatch() and withCallingHandlers(), and most of them raise no
# condition. Some raise a warning that a handler muffles, or an error that tryCatch() catches.
f <- function(i) i + 1
warn <- function(i) { warning("w"); i }
fail <- function(i) stop("e")

bench("conditions: tryCatch(f(i), error = h)", function(i) tryCatch(f(i), error = function(e) NULL))
bench("conditions: tryCatch(f(i), finally = g)", function(i) tryCatch(f(i), finally = NULL))
bench("conditions: tryCatch() with error and warning", function(i) tryCatch(f(i), error = function(e) NULL, warning = function(w) NULL))
bench("conditions: withCallingHandlers(f(i), warning = h)", function(i) withCallingHandlers(f(i), warning = function(w) NULL))
bench("conditions: tryCatch around withCallingHandlers", function(i) tryCatch(withCallingHandlers(f(i), warning = function(w) NULL), error = function(e) NULL))
bench("conditions: an error caught by tryCatch()", function(i) tryCatch(fail(i), error = function(e) NULL))
bench("conditions: a warning muffled by a handler", function(i) withCallingHandlers(warn(i), warning = function(w) invokeRestart("muffleWarning")))
bench("conditions: suppressWarnings(f(i))", function(i) suppressWarnings(f(i)))
bench("conditions: on.exit() in a function", function(i) (function() { on.exit(NULL); i })())
bench("conditions: try(f(i), silent = TRUE)", function(i) try(f(i), silent = TRUE))
bench("conditions: sys.call() and match.call()", function(i) (function(a, b = 2) { sys.call(); match.call() })(i))
bench("conditions: sys.calls() 10 frames deep", function(i) { g <- function(d) if (d == 0) sys.calls() else g(d - 1); g(10) })
