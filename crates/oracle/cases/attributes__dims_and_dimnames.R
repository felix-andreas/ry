m <- matrix(1:6, nrow = 2, dimnames = list(c("r1", "r2"), c("A", "B", "C")))
m
t(m)
m["r2", "B"]
m[, "C"]
m[, "C", drop = FALSE]
dim(m)
dimnames(m)
apply(m, 1, sum)
a <- array(1:24, dim = c(2, 3, 4))
a[2, 3, 4]
dim(a[, , 1])
m %*% t(m)
diag(2)
outer(1:3, 1:3)
rownames(m) <- NULL
m
