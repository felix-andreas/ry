f <- function(x, y) {
  cat("entering\n")
  x
}
f({ cat("forcing x\n"); 1 }, stop("never forced"))
g <- function(a, b = a * 2) { a <- 10; b }
g(1)
h <- function(x) { force(x); function() x }
h(5)()
