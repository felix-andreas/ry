# `x[[i]] <- v` and `x[i] <- v` inside a compiled function, where a local, unshared vector is
# updated in place, next to every shape that must still behave as a full replacement.
f <- function(n) {
  out <- vector("list", n)
  reals <- numeric(n)
  ints <- integer(n)
  flags <- logical(n)
  words <- character(n)
  for (i in seq_len(n)) {
    out[[i]] <- list(kind = "value", x = i)
    reals[i] <- i / 2
    ints[[i]] <- i
    flags[i] <- i %% 2 == 0
    words[[i]] <- letters[i]
  }
  list(out = out, reals = reals, ints = ints, flags = flags, words = words)
}
str(f(3))

shared <- function() {
  x <- c(1, 2, 3)
  y <- x
  x[2] <- 20
  list(x = x, y = y)
}
str(shared())

self_reference <- function() {
  x <- list(1, 2)
  x[[1]] <- x
  x
}
str(self_reference())

deletes <- function() {
  x <- list(1, 2, 3)
  i <- 2
  x[[i]] <- NULL
  x
}
str(deletes())

coerces <- function() {
  x <- c(1L, 2L)
  x[1] <- 2.5
  y <- c(1, 2)
  y[[2]] <- 7L
  z <- c(a = 1, b = 2)
  z[2] <- c(q = 9)
  list(x = x, y = y, z = z)
}
str(coerces())

positions <- function() {
  x <- c(1, 2, 3)
  x[2.7] <- 0
  x[5] <- 5
  y <- c(1, 2)
  i <- NA_integer_
  y[i] <- 9
  list(x = x, y = y)
}
str(positions())

classed <- function() {
  `[[<-.tagged` <- function(x, i, value) {
    cat("method\n")
    unclass(x)
  }
  x <- structure(list(1, 2), class = "tagged")
  x[[1]] <- 3
  x
}
str(classed())

defaults <- function(x = c(1, 2)) {
  before <- missing(x)
  x[1] <- 9
  c(before, missing(x), x)
}
defaults()

locked <- function() {
  x <- c(1, 2)
  lockBinding("x", environment())
  tryCatch({
    x[1] <- 5
    "assigned"
  }, error = function(e) conditionMessage(e))
}
locked()
