# max.col for each ties method, with missing and infinite entries, ties within the relative
# tolerance, and more rows than one scan block.
m <- rbind(c(1, 3, 3), c(NA, 1, 2), c(2, NaN, 1), c(-Inf, -Inf, -Inf), c(Inf, 1, Inf),
           c(5, 5 + 1e-7, 4), c(0, 0, 0), c(-1, -2, -1))
for (method in c("first", "last")) print(max.col(m, method))
set.seed(42); print(max.col(m)); print(max.col(m)); print(.Random.seed[2:4])
print(max.col(matrix(numeric(0), 0, 3)))
print(max.col(matrix(1:6, 2)))
set.seed(1)
big <- matrix(sample(1:4, 600 * 7, replace = TRUE), 600)
big[c(3, 300, 599), 2] <- NA
for (method in c("first", "last")) print(sum(max.col(big, method) * seq_len(600), na.rm = TRUE))
set.seed(7); r <- max.col(big); print(sum(r * seq_len(600), na.rm = TRUE)); print(which(is.na(r)))
print(runif(1))
