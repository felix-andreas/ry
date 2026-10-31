a <- matrix(1:4, 2); b <- matrix(5:8, 2)
a %*% b; t(a); a * b; dim(a); nrow(b); ncol(b)
solve(matrix(c(2, 1, 1, 3), 2))
diag(3); diag(c(1, 2)); det(matrix(c(1, 2, 3, 4), 2))
cbind(1:2, 3:4); rbind(a = 1:3, b = 4:6)
crossprod(a)
rowSums(a); colMeans(b); rownames(a) <- c("r1", "r2"); a
upper.tri(matrix(1:9, 3))
arr <- array(1:8, c(2, 2, 2)); dim(arr); arr[, , 2]
aperm(arr)[2, 1, 1]
outer(1:2, 1:3, "+")
identical(matrix(1:4, 2, byrow = TRUE), t(matrix(1:4, 2)))
