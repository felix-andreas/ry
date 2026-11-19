# Kendall's tau sums exact concordance signs: ties, infinities (Inf - Inf has sign 0), rows
# dropped for NA, and a long column whose pair count passes 2^31.
set.seed(3)
x <- round(rnorm(500), 1); y <- round(x + rnorm(500), 1)
print(cor(x, y, method = "kendall"), digits = 17)
print(cov(x, y, method = "kendall"))
print(cor(c(1, Inf, Inf, -Inf, 2), c(3, 1, 1, 2, -Inf), method = "kendall"), digits = 17)
m <- cbind(a = c(1, NA, 3, 4, 2, 2), b = c(2, 1, NA, 3, 3, 1), c = c(6:1))
print(cor(m, method = "kendall", use = "complete.obs"), digits = 17)
print(cor(m, method = "kendall", use = "pairwise.complete.obs"), digits = 17)
u <- rep_len(c(1, 2, 3, 2, 1), 70000); v <- rep_len(1:7, 70000)
print(cov(u, v, method = "kendall"))
print(cor.test(x, y, method = "kendall", exact = FALSE)$p.value, digits = 15)
