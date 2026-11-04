# `x$name <- v` inside a compiled function, where a local, unshared list with that name is
# updated in place, next to every shape that must still behave as a full replacement.
counts <- function(n) {
  x <- list(a = 0, b = "keep", a = -1)
  for (i in seq_len(n)) x$a <- x$a + i
  x
}
str(counts(4))

shapes <- function() {
  x <- list(a = 1, b = 2)
  y <- x
  x$a <- 10
  x$c <- 3
  x$b <- NULL
  x$"a" <- "text"
  z <- c(a = 1, b = 2)
  z$a <- 5
  e <- list(alpha = 1)
  e$al <- 2
  unnamed <- list(1, 2)
  unnamed$a <- 3
  na <- list(1)
  names(na) <- NA
  na$`NA` <- 2
  list(x = x, y = y, z = z, e = e, unnamed = unnamed, na = na)
}
str(shapes())

classed <- function() {
  df <- data.frame(a = 1:2)
  df$a <- c(5L, 6L)
  df$b <- c("x", "y")
  df
}
print(classed())
