# Element assignments whose subscript is any expression, such as the growth idiom
# `x[length(x) + 1L] <- v`, inside compiled closures. The value is evaluated first, then the
# variable is read, then each subscript is evaluated exactly once, and a replacement method sees
# the subscript's code.

grow_vector <- function(n) {
  x <- numeric(0)
  for (k in seq_len(n)) x[length(x) + 1L] <- k
  x
}
print(grow_vector(7))

grow_list <- function(n) {
  x <- list()
  for (k in seq_len(n)) x[[length(x) + 1L]] <- k * 2
  str(x)
}
grow_list(4)

grow_integer_into_double <- function(n) {
  x <- numeric(0)
  for (k in seq_len(n)) x[[k]] <- k
  x[[n + 1L]] <- NA_integer_
  x[[n + 2L]] <- TRUE
  x
}
print(grow_integer_into_double(3))
str(grow_integer_into_double(2))

grow_logical_into_integer <- function() {
  x <- integer(0)
  x[[1L]] <- TRUE
  x[2L] <- NA
  x[[length(x) + 1L]] <- 5L
  x
}
str(grow_logical_into_integer())

# A vector the variable shares, or reads through an argument, is copied before it grows.
shared <- function() {
  x <- c(1, 2)
  kept <- x
  for (k in 1:3) x[length(x) + 1L] <- k
  list(x = x, kept = kept)
}
str(shared())
through_argument <- function(x) {
  for (k in 1:2) x[[length(x) + 1L]] <- k
  x
}
original <- c(5, 6)
print(through_argument(original))
print(original)

order <- function() {
  x <- c(1, 2, 3)
  x[{cat("subscript\n"); 2L}] <- {cat("value\n"); 9}
  x
}
print(order())

counted <- function() {
  calls <- 0L
  position <- function() {
    calls <<- calls + 1L
    calls
  }
  x <- c(0, 0, 0)
  for (k in 1:3) x[position()] <- k
  m <- matrix(0, 2, 2)
  m[position() - 2L, position() - 4L] <- 1
  y <- list(a = 1)
  attr(y, paste0("t", position())) <- "set"
  list(x = x, m = m, y = y, calls = calls)
}
str(counted())

unbound <- function() {
  missing_variable[{cat("never printed\n"); 1L}] <- 1
}
print(tryCatch(unbound(), error = function(e) conditionMessage(e)))

symbol_subscript <- function() {
  x <- list(a = 1)
  x[[as.name("a")]] <- 2
  x
}
print(tryCatch(symbol_subscript(), error = function(e) conditionMessage(e)))

`[<-.tracked` <- function(x, i, value) {
  cat("subscript code:", deparse(substitute(i)), "value:", i, "\n")
  x <- unclass(x)
  x[i] <- value
  structure(x, class = "tracked")
}
dispatch <- function() {
  x <- structure(c(1, 2, 3), class = "tracked")
  n <- 1L
  for (k in 1:2) x[n + k] <- k * 10
  unclass(x)
}
print(dispatch())

remove_element <- function() {
  x <- list(1, 2, 3)
  x[[length(x) - 1L]] <- NULL
  x[seq_len(1L)] <- list(NULL)
  str(x)
}
remove_element()

out_of_range <- function() {
  x <- c(1, 2)
  x[length(x) + 3L] <- 5
  x[[-length(x)]] <- 1
}
print(tryCatch(out_of_range(), error = function(e) conditionMessage(e)))
