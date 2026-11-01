# Element-wise reads and writes of a double vector inside a loop.
scale_shift <- function(x) {
  y <- numeric(length(x))
  for (i in seq_along(x)) y[i] <- x[i] * 2 + 1
  y
}
x <- seq(0, 1, length.out = 1000000L)
measure("vector_loop", function() sum(scale_shift(x)))
