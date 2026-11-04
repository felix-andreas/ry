# cov and cor with missing values: pairwise deletion, complete cases, and the other methods.
# Every printed number is a checksum of the exact bit pattern of the doubles.
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(x), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
set.seed(11)
M <- matrix(rnorm(300 * 40) * 1e3, 300, 40)
M2 <- matrix(runif(300 * 40) * 10^sample(-3:3, 300 * 40, TRUE), 300, 40)
M3 <- M2; M3[5, 3] <- NA
cat("pairwise cov", bits(cov(M3, use = "pairwise")), "\n")
cat("pairwise cor", bits(cor(M3, use = "pairwise")), "\n")
cat("complete cor", bits(cor(M3, use = "complete.obs")), "\n")
cat("na.or.complete", bits(cov(M3, use = "na.or.complete")), "\n")
M4 <- M2; M4[sample(length(M4), 400)] <- NA
cat("many NA pairwise cov", bits(cov(M4, use = "pairwise")), "\n")
cat("many NA pairwise cor", bits(cor(M4, use = "pairwise")), "\n")
cat("pairwise spearman", bits(cor(M4[, 1:8], use = "pairwise", method = "spearman")), "\n")
cat("pairwise kendall", bits(cor(M4[1:60, 1:5], use = "pairwise", method = "kendall")), "\n")
cat("cov", bits(cov(M2)), "\n")
cat("cor", bits(cor(M)), "\n")
cat("cor spearman", bits(cor(M[, 1:5], method = "spearman")), "\n")
cat("cor kendall", bits(cor(M[1:50, 1:4], method = "kendall")), "\n")
cat("x with y", bits(cov(M2[, 1:3], M2[, 4:8])), "\n")
cat("x with y pairwise", bits(cor(M4[, 1:3], M4[, 4:8], use = "pairwise")), "\n")
x <- M2[, 1]; y <- M[, 2]
cat("vectors", bits(c(cov(x, y), cor(x, y), var(x), sd(y))), "\n")
cat("extremes", bits(c(sd(c(1e154, 1e154)), var(c(1e200, -1e200, 1e200)))), "\n")
cat("constant", bits(suppressWarnings(cor(c(1, 1, 1), 1:3))), "\n")
