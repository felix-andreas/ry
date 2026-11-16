# Matrix covariances must match GNU R to the last bit: means rounded to double, deviations and
# products in long double, sums in row order. Zero-mean columns show a rounding difference first.
set.seed(2)
m <- matrix(rnorm(700), 100)
m[, 2] <- m[, 2] + 1e3
shifted <- matrix(rnorm(500, 50, 3), 100)
gaps <- m
gaps[3, 5] <- NA
gaps[7, 1] <- NA
bits <- function(x) sprintf("%a", x)
bits(var(m))
bits(cov(m))
bits(cor(m))
bits(cov(m, shifted))
bits(cor(m, shifted))
bits(cov(gaps))
bits(cov(gaps, use = "complete.obs"))
bits(cor(gaps, use = "complete.obs"))
bits(cov(gaps, shifted, use = "na.or.complete"))
bits(cov(m[1:2, ]))
cov(m[1, , drop = FALSE])
bits(cov(m[, 1:3], method = "kendall"))
