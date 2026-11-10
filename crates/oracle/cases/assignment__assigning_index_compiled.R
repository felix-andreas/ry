# An element assignment whose subscript assigns another local variable, as in the growth idiom
# `x[[cap <- if (cap < n) 2L * cap else cap]] <- v`, compiles to the in-place update: the
# subscript runs first, the target keeps its attributes, and a copy shared elsewhere is kept.
grow <- function(n) {
  x <- integer(2)
  attr(x, "tag") <- "kept"
  cap <- 2L
  for (i in 1:n) {
    x[[cap <- if (cap < i) 2L * cap else cap]] <- 0L
    x[[i]] <- i
  }
  list(x = x, cap = cap)
}
str(grow(9))
shared <- function() {
  x <- c(a = 1, b = 2, c = 3); kept <- x; k <- 0L
  for (i in 1:3) x[k <- i] <- i * 10
  list(x = x, kept = kept, k = k)
}
str(shared())
compare <- function(x) {
  for (i in 1:4) x[[j <- if (i > 2) i - 2L else i]] <- -i
  list(x, j)
}
str(compare(1:4))
# A subscript that assigns the target itself still sees the old value through `*tmp*`.
self_assign <- function() {
  x <- 1:3
  x[{x <- c(7L, 8L, 9L); 2L}] <- 0L
  x
}
print(self_assign())
# An `NA` condition in the subscript is the error GNU R reports.
na_condition <- function() {
  x <- 1:3; flag <- NA
  tryCatch({ x[[i <- if (flag) 1L else 2L]] <- 0L; x }, error = function(e) conditionMessage(e))
}
print(na_condition())
