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

# Pattern: every function starts with a check of its arguments. The checker takes the values
# through `...` and compares each one with an expected class. Small helpers test for missing
# values with ifelse() and isTRUE() on vectors of length 1.
library(methods)

is_blank <- function(x) {
  if (is.null(x) || length(x) == 0) return(TRUE)
  ifelse(is.na(x), TRUE, ifelse(is.character(x), x == "", FALSE))
}

check_args <- function(..., types, same_length = FALSE) {
  args <- list(...)
  if (length(args) != length(types)) stop("one type per argument is needed")
  for (i in seq_along(args)) {
    x <- args[[i]]
    type <- types[i]
    ok <- if (isVirtualClass(type) || isS4(x)) is(x, type) else inherits(x, type) || is(x, type)
    if (!isTRUE(ok)) stop("argument ", i, " must be of class ", type)
  }
  if (same_length) {
    n <- vapply(args, length, integer(1))
    if (length(unique(n[n != 1])) > 1) stop("the arguments must have the same length")
  }
  invisible(TRUE)
}

f <- function(name, key = NA_character_, day = as.Date(NA), flag = FALSE) {
  check_args(name, key, day, types = c("character", "character", "Date"), same_length = TRUE)
  if (!is_blank(key)) name <- paste(name, key, sep = "|")
  if (isTRUE(flag)) toupper(name) else name
}

day <- as.Date("2024-01-31")
bench("checks: function with an argument check", function(i) f("a", "b", day))
bench("checks: check_args(...) alone", function(i) check_args("a", "b", day, types = c("character", "character", "Date")))
bench("checks: list(...) of three values", function(i) (function(...) list(...))("a", "b", day))
bench("checks: ifelse() on a scalar", function(i) ifelse(i > 5, "x", "y"))
bench("checks: nested ifelse() with is.na()", function(i) is_blank("a"))
bench("checks: isTRUE()", function(i) isTRUE(i > 5))
bench("checks: inherits(x, 'Date')", function(i) inherits(day, "Date"))
bench("checks: is(x, 'character')", function(i) is("a", "character"))
bench("checks: missing() and default arguments", function(i) (function(a, b = 2, d = NULL) if (missing(d)) a + b else a)(i))
bench("checks: match.arg()", function(i) (function(kind = c("one", "two", "three")) match.arg(kind))("two"))
bench("checks: stopifnot() with two conditions", function(i) stopifnot(is.numeric(i), length(i) == 1))
bench("checks: identical() of two short vectors", function(i) identical(c("a", "b"), c("a", "b")))
