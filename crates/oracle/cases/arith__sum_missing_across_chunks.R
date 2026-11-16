x <- rep(1.5, 5000)
y <- x; y[700] <- NA; y[3000] <- NaN
print(sum(y))
y <- x; y[700] <- NaN; y[3000] <- NA
print(sum(y))
y <- x; y[100] <- NA; y[200] <- NaN
print(sum(y))
y <- x; y[600] <- Inf; y[2000] <- -Inf
print(sum(y))
y[4000] <- NA
print(sum(y))
y <- x; y[5000] <- NaN
print(sum(y))
print(sum(x))
print(sum(c(1e308, 1e308, -1e308)))
print(sum(rep(c(0.1, 0.2, 0.3), 2000)), digits = 17)
print(sum(y, na.rm = TRUE))
print(colSums(matrix(c(1L, NA, 3L, 4L, 5L, 6L), 2)))
print(rowSums(matrix(c(1L, NA, 3L, 4L, 5L, 6L), 2)))
print(colMeans(matrix(1:12, 3)))
print(rowMeans(matrix(c(TRUE, FALSE, NA, TRUE), 2), na.rm = TRUE))
print(sum(c(Inf, -Inf, NA)))
print(sum(c(NA, Inf, -Inf)))
print(sum(c(NaN, NA)))
print(sum(c(NA, NaN)))
y <- rep(1, 1000); y[510] <- Inf; y[515] <- -Inf; y[520] <- NA
print(sum(y))
y[515] <- 1; y[512] <- NA; y[513] <- -Inf
print(sum(y))
