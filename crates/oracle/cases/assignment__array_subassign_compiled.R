# `x[i, j, ] <- value` and `x[, j] <- value` inside a compiled loop replace through `[<-`: empty
# subscripts select a whole extent, a missing formal still selects everything, a shared value
# is copied first, names and dimnames survive, and an object dispatches to its method.
fill <- function(n) {
  a <- array(0, dim = c(n, n, 2))
  for (i in 1:n) for (j in 1:n) a[i, j, ] <- c(i, j) * 10
  a
}
print(fill(3))
columns <- function() {
  m <- matrix(0L, 2, 3, dimnames = list(c("a", "b"), c("x", "y", "z")))
  for (j in 1:3) m[, j] <- j * 1:2
  for (i in 1:2) m[i, ] <- m[i, ] + 100L
  m
}
print(columns())
everything <- function(m, i) { for (k in 1:2) m[i, 1] <- k; m }
print(everything(matrix(1:4, 2)))
shared <- function() {
  m <- matrix(1:4, 2); kept <- m
  for (j in 1:2) m[, j] <- 0L
  list(m, kept)
}
str(shared())
deep <- function() {
  l <- list(a = matrix(1:4, 2))
  for (j in 1:2) l[["a"]][, j] <- -j
  l
}
str(deep())
`[<-.tagged` <- function(x, ..., value) { cat("method\n"); y <- unclass(x); y[...] <- value; structure(y, class = "tagged") }
dispatch <- function() {
  t <- structure(matrix(1:4, 2), class = "tagged")
  for (j in 1:2) t[, j] <- 0L
  unclass(t)
}
print(dispatch())
grow_types <- function() {
  m <- matrix(1:4, 2)
  for (j in 1:2) m[, j] <- c(0.5, 1.5)
  for (i in 1:2) m[i, ] <- c("p", "q")
  m
}
print(grow_types())
errors <- function() {
  m <- matrix(1:4, 2)
  for (j in 1:1) tryCatch(m[, 3] <- 1L, error = function(e) cat("Error:", conditionMessage(e), "\n"))
  m
}
print(errors())
slabs <- function() {
  a <- array(1:24, c(2, 3, 4))
  for (k in 1:2) a[k, , 2] <- c(-1L, -2L, -3L)
  for (k in 1:2) a[, k, ] <- 0L
  a[2, 3, 4] <- 99L
  l <- array(TRUE, c(2, 2, 2)); l[, 1, ] <- c(FALSE, NA)
  r <- array(0, c(3, 2, 2)); r[2, , ] <- 1:4 / 2
  z <- array(0, c(2, 0, 2)); z[1, , 1] <- 5
  list(a, l, r, z)
}
str(slabs())
tryCatch({ a <- array(0, c(2, 2, 2)); a[3, , 1] <- 1 }, error = function(e) cat("Error:", conditionMessage(e), "\n"))
tryCatch({ a <- array(0, c(2, 2, 2)); a[1, , ] <- 1:3 }, error = function(e) cat("Error:", conditionMessage(e), "\n"))
