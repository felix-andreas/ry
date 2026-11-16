# S3 replacement methods defined as locals of the function that assigns. A frame whose locals sit
# in slots must still count those names as bound, or the method lookup goes straight to base.
f <- function() {
  `[<-.k` <- function(x, i, value) { cat("[<-.k\n"); unclass(x) }
  `$<-.k` <- function(x, name, value) { cat("$<-.k\n"); x }
  `[[<-.k` <- function(x, i, value) { cat("[[<-.k\n"); x }
  `names<-.k` <- function(x, value) { cat("names<-.k\n"); x }
  x <- structure(list(a = 1), class = "k"); x[1] <- 2
  print(x)
  x <- structure(list(a = 1), class = "k"); x$a <- 3
  x <- structure(list(a = 1), class = "k"); x[[1]] <- 4
  x <- structure(list(a = 1), class = "k"); names(x) <- "q"
  y <- structure(list(a = 1), class = "k"); y[1] <- 2
  invisible()
}
for (i in 1:3) f()
# A local method reached from a closure the frame encloses.
g <- function() {
  `[<-.j` <- function(x, i, value) { cat("[<-.j\n"); x }
  h <- function(x) { x[1] <- 0; x }
  for (i in 1:2) h(structure(1:3, class = "j"))
  `length<-.j` <- function(x, value) { cat("length<-.j\n"); x }
  z <- structure(1:3, class = "j"); length(z) <- 1
  invisible()
}
for (i in 1:3) g()
