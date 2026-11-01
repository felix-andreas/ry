# Whole-vector arithmetic: the builtins' kernels, called from a loop.
vector_arith <- function(x, times) {
  total <- 0
  for (k in seq_len(times)) {
    y <- x * 2 + x / 3 - 1
    total <- total + sum(y)
  }
  total
}
x <- seq(0, 1, length.out = 1000000L)
measure("vector_arith", function() vector_arith(x, 20L))
