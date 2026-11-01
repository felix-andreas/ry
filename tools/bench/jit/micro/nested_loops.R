# Nested double loops with a matrix-free inner product: an O(n^2) pairwise distance sum.
pairwise <- function(x) {
  n <- length(x)
  total <- 0
  for (i in 1:n) {
    xi <- x[i]
    for (j in 1:n) {
      d <- xi - x[j]
      total <- total + d * d
    }
  }
  total
}
x <- seq(0, 1, length.out = 2000L)
measure("nested_loops", function() pairwise(x))
