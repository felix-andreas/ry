# Zero-length arguments, NULL included, add a column only when no argument has a nonzero
# extent, and an all-zero extent gives dimnames of two NULLs, as GNU R's bind.c does.
m <- matrix(0, 0, 2); w <- NULL
str(cbind(m, NULL)); str(cbind(m, w)); str(cbind(m, numeric())); str(cbind(m, x = character()))
str(rbind(matrix(0, 2, 0), NULL)); str(rbind(matrix(0, 2, 0), w))
str(cbind(NULL, NULL)); str(cbind(numeric(), NULL)); str(cbind(numeric(), w)); str(cbind(a = numeric(), b = integer()))
str(cbind(matrix(0, 1, 2), w)); str(cbind(m, 1:2))
str(cbind(m, w, deparse.level = 0)); str(cbind(m, w, deparse.level = 2))
str(rbind(numeric(), w)); str(cbind(data.frame(), NULL))
m2 <- matrix(1:4, 2, dimnames = list(c("r1", "r2"), c("a", "b")))
x <- c(u = 5L, v = 6L)
print(cbind(m2, x)); print(rbind(m2, x)); print(cbind(x, 7:8, deparse.level = 2))
print(cbind(1:3, c("a", "b", "c"))); print(rbind(list(1, "a"), 2:3)); print(cbind(NULL, 1:2, NULL))
print(cbind(m2, NULL, z = 9L)); print(rbind(NULL, x, NULL, y = 1))
