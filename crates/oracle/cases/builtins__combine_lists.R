# c() of lists: the elements of each list in order, names as GNU R builds them, and no
# attribute but names on the result.

accumulate <- function(n) {
  x <- list()
  for (k in seq_len(n)) x <- c(x, list(k))
  x
}
str(accumulate(3))

str(c(list(1, "a"), NULL, list(TRUE)))
str(c(NULL, list(1)))
str(c(list(1), list()))
str(c(list(), list()))
str(c(list(), NULL))
str(c(list(a = 1), list(2)))
str(c(list(1), list(b = 2)))
str(c(list(a = 1, b = 2), list(c = NULL)))
str(c(structure(list(1), foo = "dropped"), list(2)))
str(c(list(1), 2, "z"))
str(c(list(list(1, 2)), list(3)))
str(c(list(NULL), list(NULL)))

# The result shares no state with its arguments: changing an element of one leaves the other.
a <- list(c(1, 2))
b <- c(a, list(3))
b[[1]][1] <- 99
str(a)
str(b)

named_loop <- function() {
  x <- list(start = 0)
  for (k in 1:2) x <- c(x, list(k), NULL)
  x
}
str(named_loop())
