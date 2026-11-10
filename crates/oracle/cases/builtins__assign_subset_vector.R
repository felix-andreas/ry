# x[i] <- value on plain vectors inside a closure, whose compiled body writes in place:
# recycling, logical, real, zero, NA and out-of-range indices, coercion, and shared values.

show <- function(expr) {
  result <- tryCatch(
    withCallingHandlers(expr, warning = function(w) {
      cat("warning:", conditionMessage(w), "\n")
      invokeRestart("muffleWarning")
    }),
    error = function(e) paste("error:", conditionMessage(e))
  )
  print(result)
}

run <- function(index, value, subject) {
  x <- subject
  x[index] <- value
  x
}

for (round in 1:3) {
  cat("round", round, "\n")
  show(run(c(1L, 3L), 9L, 1:4))
  show(run(c(1L, 3L), c(8L, 9L), 1:4))
  show(run(1:4, c(8L, 9L), 1:4))
  show(run(1:3, c(8L, 9L), 1:4))
  show(run(c(TRUE, FALSE, TRUE, FALSE), 0L, 1:4))
  show(run(c(TRUE, NA, TRUE, FALSE), 0L, 1:4))
  show(run(c(TRUE, FALSE), 0L, 1:4))
  show(run(2.9, 7L, 1:4))
  show(run(c(1.5, 4.2), c(7, 8), c(1, 2, 3, 4)))
  show(run(0L, 5L, 1:4))
  show(run(c(0L, 2L), 5L, 1:4))
  show(run(NA_integer_, 5L, 1:4))
  show(run(c(NA_integer_, 2L), 5L, 1:4))
  show(run(6L, 5L, 1:4))
  show(run(-1L, 5L, 1:4))
  show(run(c(-1L, -2L), 5L, 1:4))
  show(run(2L, 2.5, 1:4))
  show(run(2L, "a", 1:4))
  show(run(2L, TRUE, c(1, 2, 3)))
  show(run(2L, 1L, c(TRUE, FALSE)))
  show(run(2L, NA, c("a", "b")))
  show(run(c(1L, 2L), 3+1i, c(1i, 2i)))
  show(run(1L, as.raw(9), as.raw(1:3)))
  show(run(1L, integer(), 1:4))
  show(run(1L, NULL, 1:4))
  show(run(integer(), 5L, 1:4))
  show(run(1:2, c(a = 7L, b = 8L), c(x = 1L, y = 2L, z = 3L)))
  show(run(2L, 7L, structure(1:3, dim = 3L)))
  show(run(2L, 7L, factor(c("a", "b", "c"))))
  show(run(2L, list(7), 1:3))
  show(run("b", 7L, c(a = 1L, b = 2L)))
}

shared <- function() {
  x <- 1:4
  y <- x
  x[c(1L, 2L)] <- 0L
  list(x, y)
}
for (round in 1:3) print(shared())

repeated <- function() {
  x <- numeric(5)
  for (i in 1:5) x[c(i, 1L)] <- c(i, -i)
  x
}
for (round in 1:3) print(repeated())

dims <- function(value) {
  x <- 1:6
  attr(x, "dim") <- value
  x
}
for (round in 1:3) {
  show(dims(c(2L, 3L)))
  show(dims(NA))
  show(dims(c(2L, NA)))
  show(dims(c(-2L, -3L)))
  show(dims(c(2147483647L, 2147483647L, 2147483647L)))
  show(dims(c(2L, 2L)))
}

for (round in 1:3) {
  show(dims(integer()))
  show(dims(c(2.5, 3)))
}
