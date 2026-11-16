# colSums and colMeans of double matrices: finite columns through the extended-precision
# kernel, the others through na.rm, and shapes with no rows or a single row.
m <- matrix(c(1e308, 1e308, -1e308, 0.1, 0.2, 0.3, NA, 1, 2, NaN, 1, 2, Inf, -Inf, 1, 1, 1, 1), 3)
print(colSums(m)); print(colSums(m, na.rm = TRUE))
print(colMeans(m)); print(colMeans(m, na.rm = TRUE))
print(colSums(matrix(numeric(0), 0, 3))); print(colMeans(matrix(numeric(0), 0, 3)))
print(colSums(matrix(c(0.1, 0.7, 1e-17), 1))); print(colMeans(matrix(c(0.1, 0.7), 1)))
set.seed(4)
r <- matrix(rnorm(1e4) * 10^runif(1e4, -10, 10), 100)
print(sprintf("%.17g", head(colSums(r), 5))); print(sprintf("%.17g", head(colMeans(r), 5)))
print(identical(colSums(r), apply(r, 2, sum)))
print(colSums(matrix(1:6, 2))); print(colMeans(matrix(c(1L, NA, 3L, 4L), 2), na.rm = TRUE))
invisible(NULL)
