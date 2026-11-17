# dist() for every method on finite rows (more rows than one block of four, and a remainder),
# with NA, NaN and Inf coordinates, and with no columns. Results print in hexadecimal so that a
# change in the order of summation shows.
set.seed(3)
x <- matrix(rnorm(7 * 5), 7)
methods <- c("euclidean", "maximum", "manhattan", "canberra", "binary", "minkowski")
for (method in methods) print(sprintf("%a", dist(x, method, p = 1.5)))
y <- x
y[2, 3] <- NA
y[4, 1] <- Inf
y[5, 5] <- NaN
for (method in methods) print(sprintf("%a", dist(y, method, p = 1.5)))
print(dist(matrix(numeric(), 3, 0)))
print(dist(x[1, , drop = FALSE]))
print(dist(x[1:5, ], diag = TRUE, upper = TRUE))
invisible(NULL)
