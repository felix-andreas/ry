# A logical subscript assigns where it is TRUE, recycled over the vector; a single value skips
# NA flags, and a longer one fails on them.
x <- as.numeric(1:10)
x[c(TRUE, FALSE)] <- 0
print(x)
x <- as.numeric(1:10)
x[c(TRUE, NA, FALSE)] <- -1
print(x)
x <- 1:10
x[x > 4] <- c(100L, 200L)
print(x)
x <- 1:10
x[c(TRUE, FALSE, TRUE)] <- c(7L, 8L)
print(x)
x <- as.numeric(1:6)
print(tryCatch({ x[c(TRUE, NA)] <- c(1, 2); x }, error = conditionMessage))
x <- c(TRUE, FALSE, NA, TRUE)
x[c(FALSE, TRUE)] <- NA
print(x)
x <- as.numeric(1:4)
x[c(TRUE, FALSE, TRUE, FALSE, TRUE)] <- 9
print(x)
x <- c(a = 1, b = 2, c = 3)
x[logical()] <- 5
print(x)
m <- matrix(as.numeric(1:6), 2)
m[m > 3] <- 0
print(m)
x <- as.numeric(1:5)
y <- x
x[c(FALSE, TRUE)] <- 1:2
print(list(x, y))
z <- c(1, 2, 3)
z[z > 1] <- z[1:2]
print(z)
