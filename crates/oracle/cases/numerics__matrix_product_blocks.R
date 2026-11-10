# Matrix products with every remainder of four result columns, and non-finite entries.
set.seed(42)
for (cols in 1:9) {
  a <- matrix(rnorm(6 * 5), 6)
  b <- matrix(rnorm(5 * cols), 5)
  p <- a %*% b
  cat(cols, dim(p), sprintf("%.17g", sum(p * seq_along(p))), "\n")
}
a <- matrix(c(1, NA, 3, Inf, 5, 6), 3)
b <- matrix(c(1, 2, 3, 4, -Inf, 0, 7, 8, 9, 10), 2)
a %*% b
crossprod(matrix(1:6, 3), matrix(1:15, 3))
