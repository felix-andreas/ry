# Sums, means and moments accumulate in long double and must match GNU R to the last bit,
# including the rows and columns that hold NA, NaN or infinities.
hex <- function(x) cat(sprintf("%a", x), "\n")
set.seed(11)
x <- runif(103) * 1e3 + 1e6
y <- rnorm(103)
hex(sum(x)); hex(mean(x)); hex(var(x)); hex(sd(x)); hex(cov(x, y)); hex(cor(x, y))
hex(mean(1:7)); hex(mean(c(.Machine$integer.max, 1L, 2L)))
hex(sum(c(1, NaN, NA))); hex(sum(c(1, NA, NaN))); hex(sum(c(Inf, -Inf))); hex(sum(c(1, Inf)))
hex(sum(c(.Machine$double.xmax, .Machine$double.xmax, -.Machine$double.xmax)))
hex(mean(c(1:9, -1e300, 1e300)))
m <- matrix(x[1:100], 20, 5)
m[3, 2] <- NA; m[5, 4] <- NaN; m[7, 1] <- Inf; m[9, 5] <- -Inf
for (na.rm in c(FALSE, TRUE)) {
  hex(rowSums(m, na.rm = na.rm)); hex(rowMeans(m, na.rm = na.rm))
  hex(colSums(m, na.rm = na.rm)); hex(colMeans(m, na.rm = na.rm))
}
hex(rowMeans(matrix(numeric(), 3, 0)))
im <- matrix(c(1:11, NA), 4); hex(rowSums(im)); hex(colMeans(im, na.rm = TRUE))
mm <- matrix(c(x[1:60], y[1:60]), 30)
hex(var(mm)); hex(cor(mm))
z <- x[1:30]; w <- y[1:30]; z[4] <- NA; w[9] <- NA
hex(var(z, na.rm = TRUE)); hex(cov(z, w, use = "complete.obs")); hex(cor(z, w, use = "complete.obs"))
hex(cov(z, w, use = "pairwise")); hex(cor(z, w, use = "pairwise")); hex(cov(z, w))
hex(cor(cbind(z, w, x[31:60]), use = "pairwise"))
