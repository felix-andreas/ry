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

# Pattern: lapply(), sapply(), vapply(), mapply() and do.call() over short lists (5 to 20
# elements) with a small anonymous function.
xs <- as.list(1:10)
ys <- as.list(11:20)
named <- setNames(as.list(1:10), paste0("k", 1:10))
add <- function(a, b = 1, ...) a + b

bench("functionals: lapply() over 10 values", function(i) lapply(xs, function(x) x + i))
bench("functionals: sapply() over 10 values", function(i) sapply(xs, function(x) x + i))
bench("functionals: vapply() over 10 values", function(i) vapply(xs, function(x) x + i, numeric(1)))
bench("functionals: mapply() over two lists", function(i) mapply(function(a, b) a + b, xs, ys))
bench("functionals: Map() over two lists", function(i) Map(function(a, b) a + b, xs, ys))
bench("functionals: do.call() with a named list", function(i) do.call(add, list(a = i, b = 2)))
bench("functionals: do.call('c', list)", function(i) do.call("c", xs))
bench("functionals: Reduce(`+`, 10 values)", function(i) Reduce(`+`, xs))
bench("functionals: Filter() over 10 values", function(i) Filter(function(x) x > 5, xs))
bench("functionals: unlist() of 10 values", function(i) unlist(xs))
bench("functionals: unlist() of a named list", function(i) unlist(named))
bench("functionals: setNames() and names()", function(i) names(setNames(xs, paste0("k", 1:10))))
bench("functionals: structure() with a class", function(i) structure(list(a = i), class = "rec"))
bench("functionals: call forwarding ...", function(i) (function(...) add(...))(i, 2))
bench("functionals: call with 5 named arguments", function(i) (function(a, b = 1, d = 2, e = 3, g = 4) a + b + d + e + g)(a = i, d = 1, g = 2))
bench("functionals: seq_along() loop that fills a list", function(i) { out <- vector("list", 10); for (j in seq_along(xs)) out[[j]] <- xs[[j]] + i; out })
bench("functionals: c() that grows a vector 10 times", function(i) { out <- c(); for (j in 1:10) out <- c(out, j); out })
bench("functionals: rev(), sort() and order() of 10 values", function(i) order(rev(sort(c(3, 1, 2, 9, 8, 7, 4, 5, 6, i)))))
bench("functionals: sum(), mean(), max() of 10 values", function(i) sum(1:10) + mean(1:10) + max(1:10))
bench("functionals: rep(), seq() and length()", function(i) length(rep(seq(1, 10, by = 1), 2)))
