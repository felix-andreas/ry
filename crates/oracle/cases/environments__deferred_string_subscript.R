# `as.character` of a number is a deferred string vector, which `[[` on an environment reads as
# a name like any other character vector.
e <- new.env(parent = emptyenv())
k <- as.character(1.5)
print(e[[k]])
e[[k]] <- "set"
print(e[[k]])
print(e[[as.character(2L)]])
memo <- new.env(parent = emptyenv())
fib <- function(n) {
  key <- as.character(n)
  if (!is.null(memo[[key]])) return(memo[[key]])
  value <- if (n < 2) n else fib(n - 1) + fib(n - 2)
  memo[[key]] <- value
  value
}
print(vapply(seq_len(30), fib, numeric(1))[[30]])
