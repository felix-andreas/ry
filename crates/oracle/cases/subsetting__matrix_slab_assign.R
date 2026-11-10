# Assigning whole columns, rows and array slabs with `[<-`: contiguous columns, recycled values,
# integer and logical matrices, three-dimensional slabs, a shared matrix, and shapes that leave
# the fast path (type changes, out-of-range positions, dimnames).
m <- matrix(0, 4, 3)
m[, 2] <- c(1, 2, 3, 4)
m[, 3] <- c(7, 8)
m[2, ] <- c(-1, -2, -3)
print(m)
n <- m
n[, 1] <- 9
print(m[, 1])
print(n[, 1])
im <- matrix(1:6, 2)
im[, 2] <- 10:11
im[1, ] <- 0L
print(im)
lm <- matrix(TRUE, 2, 2)
lm[, 1] <- c(FALSE, NA)
print(lm)
a <- array(0, c(2, 3, 2))
a[, , 2] <- 1:6 + 0
a[1, , 1] <- c(5, 6, 7)
a[, 2, ] <- c(-1, -2, -3, -4)
print(a)
cm <- matrix(0, 2, 2)
cm[, 1] <- 1:2
print(cm)
cm[, 2] <- c("a", "b")
print(cm)
dm <- matrix(0, 2, 2, dimnames = list(c("a", "b"), c("x", "y")))
dm[, "y"] <- c(3, 4)
dm["a", ] <- 1
print(dm)
print(tryCatch({ dm[, 3] <- 1; dm }, error = conditionMessage))
big <- matrix(0, 30000, 10)
v <- seq_len(30000) / 7
fill <- function(m, v) { for (k in 1:10) m[, k] <- v * k; m }
filled <- fill(big, v)
print(colSums(filled))
print(sum(big))
