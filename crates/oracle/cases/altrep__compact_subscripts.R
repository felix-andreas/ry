# A compact sequence as a subscript selects the same elements as its expansion.
x <- c(10L, 20L, 30L, 40L, 50L)
x[2:4]
x[4:2]
x[3:7]
x[0:2]
x[-1:-2]
d <- c(1.5, 2.5, 3.5)
d[as.double(1:2)]
d[as.double(2:1) + 0.5]
s <- c("a", "b", "c", "d")
s[2:3]
l <- list(1, "two", 3)
str(l[2:3])
y <- x
y[2:3] <- c(0L, -1L)
y
y[1:2] <- 99L
y
y[4:7] <- 7L
y
z <- c(1, 2, 3)
z[3:2] <- c(30, 20)
z
k <- 3L
w <- 1:10
w[1:k] <- rev.default(w[1:k])
w
f <- function(m) {
  perm <- 1:12
  for (i in 1:m) {
    k <- (i %% 12L) + 1L
    r <- 1:k
    perm[r] <- rev.default(perm[r])
  }
  perm
}
f(5000L)
