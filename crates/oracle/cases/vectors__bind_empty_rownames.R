# Bound names survive when a matrix argument had names on that axis, even all empty ones.
m <- matrix(as.character(1:20), 10, dimnames = list(rep("", 10), c("x", "y")))
r <- rbind(head(m, 5), "---", tail(m, 5))
str(dimnames(r))
rownames(r) <- rep("", 11)
print(dim(r))
str(dimnames(cbind(matrix(1:4, 2, dimnames = list(NULL, c("", ""))), 9)))
