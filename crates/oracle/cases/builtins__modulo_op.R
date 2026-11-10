# `%%` and `%/%` on plain scalars compile to tier-0 arithmetic; these pin the signs,
# zero and NA divisors, double rounding and the cases that must reach the builtin.
f <- function(a, b) list(a %% b, a %/% b)
for (pair in list(c(7L, 3L), c(-7L, 3L), c(7L, -3L), c(-7L, -3L), c(6L, 3L), c(-6L, 3L))) {
  str(f(pair[[1]], pair[[2]]))
}
str(f(5L, 0L)); str(f(NA_integer_, 2L)); str(f(5L, NA_integer_))
str(f(5.5, 2)); str(f(-5.5, 2)); str(f(5.5, -2)); str(f(1, 0)); str(f(-1, 0)); str(f(0, 0))
str(f(NA_real_, 2)); str(f(NaN, 2)); str(f(Inf, 2)); str(f(2, Inf)); str(f(-2, Inf))
str(f(1e17, 7)); str(f(0.3, 0.1)); str(f(7L, 2.5)); str(f(TRUE, 2L))
r <- withCallingHandlers(1e300 %% 3, warning = function(w) { cat("warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") })
print(r)
str(f(structure(7L, class = "money"), 3L))
Ops.money <- function(e1, e2) "dispatched"
str(f(structure(7L, class = "money"), 3L))
str(f(c(a = 7L), 3L)); str(f(1:6, 4L))
loop <- function(n) { s <- 0L; for (i in seq_len(n)) s <- s + i %% 7L + i %/% 5L; s }
print(loop(1000L)); print(loop(1000L)); print(loop(1000L))
# A loop variable that holds an integer and then a double is a mixed number to the JIT.
mixed <- function(n) { k <- 1L; s <- 0; for (i in seq_len(n)) { s <- s + 2.5 %% k + 7 %/% k; k <- k + 0.5 }; s }
for (round in 1:20) value <- mixed(200L)
print(value)
