# x[i] <- v and x[[i]] <- v inside compiled closures, where a vector the variable alone holds
# takes a value of its own type in place, and everything else takes the general assignment.

doubles <- function(n) {
  x <- c(1, 2, 3)
  for (k in seq_len(n)) x[[(k %% 3L) + 1L]] <- k * 1.5
  for (k in seq_len(n)) x[(k %% 3L) + 1L] <- -k
  x
}
print(doubles(5))
print(doubles(7))

kinds <- function() {
  i <- 2L
  ints <- 1:3 + 0L
  lgl <- c(TRUE, FALSE, NA)
  lst <- list(1, "a", NULL)
  named <- c(a = 1, b = 2, c = 3)
  for (k in 1:2) {
    ints[[i]] <- 10L
    ints[i + 1L] <- 20L
    lgl[[i]] <- TRUE
    lgl[3] <- FALSE
    lst[[i]] <- list("nested")
    lst[[1L]] <- c(x = 5)
    named[[2.7]] <- 99
    named[1L] <- 0
  }
  list(ints, lgl, lst, named)
}
print(kinds())
print(kinds())

# Values that do not fit the vector's type widen it, and odd subscripts and values take the
# general assignment.
widen <- function() {
  ints <- 1:3 + 0L
  ints[[2L]] <- 2.5
  lgl <- c(TRUE, FALSE)
  lgl[[1L]] <- 1L
  dbl <- c(1, 2)
  dbl[[1L]] <- "a"
  grow <- c(1, 2)
  grow[[3L]] <- 3
  grow[5L] <- 5
  lst <- list(1, 2, 3)
  lst[[2L]] <- NULL
  lst2 <- list(1, 2)
  lst2[2L] <- list(NULL)
  lst3 <- list(1, 2)
  lst3[1L] <- list(5)
  attrs <- c(1, 2)
  attrs[[1L]] <- c(a = 9)
  vec <- c(1, 2, 3)
  vec[c(1L, 3L)] <- 0
  real <- c(1, 2, 3)
  real[[c(a = 2L)]] <- 7
  list(ints, lgl, dbl, grow, lst, lst2, lst3, attrs, vec, real)
}
print(widen())
print(widen())

errors <- function() {
  x <- c(1, 2)
  c(
    tryCatch({ x[[NA_integer_]] <- 1; "no error" }, error = conditionMessage),
    tryCatch({ x[[0L]] <- 1; "no error" }, error = conditionMessage),
    tryCatch({ x[[-1L]] <- 1; "no error" }, error = conditionMessage),
    tryCatch({ x[[c(1L, 2L)]] <- 1; "no error" }, error = conditionMessage),
    tryCatch({ x[[1L]] <- numeric(0); "no error" }, error = conditionMessage)
  )
}
print(errors())
print(errors())

# Copy semantics: a value another variable, a caller or a list also holds is copied first.
shared <- function(arg) {
  x <- c(1, 2, 3)
  y <- x
  l <- list(x)
  for (k in 1:2) {
    x[[k]] <- 0
    arg[[k]] <- 0
  }
  list(x, y, l, arg)
}
outer <- c(5, 6, 7)
print(shared(outer))
print(shared(outer))
print(outer)

self_store <- function() {
  l <- list(1, 2)
  for (k in 1:2) l[[2L]] <- l
  str(l)
  x <- 5
  for (k in 1:2) x[[1L]] <- x
  x
}
print(self_store())
print(self_store())

# A classed vector or a matrix keeps its general assignment and its attributes.
`[[<-.tagged` <- function(x, i, value) {
  x <- unclass(x)
  x[[i]] <- value * 100
  structure(x, class = "tagged")
}
classed <- function() {
  t <- structure(c(1, 2), class = "tagged")
  m <- matrix(1:4, 2)
  for (k in 1:2) {
    t[[k]] <- k
    m[[k + 2L]] <- 0L
  }
  list(unclass(t), m)
}
print(classed())
print(classed())

# A function that leaves the store's closure through a promise still sees its own copy.
lazy <- function(x, f) {
  f
  x[[1L]] <- 100
  x
}
v <- c(1, 2)
print(lazy(v, v[[1L]] <- 50))
print(v)

# x$name <- v and a list's x[i] <- v store in place when the name exists or the value is one
# element; a new name appends, NULL deletes, and a list holding itself copies first.
lists <- function() {
  x <- list(a = 1, b = "two", c = NULL, a = 4)
  y <- list(1, 2, 3)
  for (k in 1:3) {
    x$b <- k
    x$a <- list(k)
    x[["c"]] <- c(z = k)
    y[2L] <- list(k * 2)
    y[3L] <- "s"
    y[1L] <- list(NULL)
  }
  x$d <- "new"
  x$c <- NULL
  y[2L] <- list(y)
  str(x)
  str(y)
  z <- list(p = 1)
  for (k in 1:2) z$p <- z
  str(z)
  w <- list(1)
  w[1L] <- list(c(q = 1))
  w[1L] <- 1:2
  w
}
print(lists())
print(lists())

# A non-ASCII name and a list with names that are not all set.
names_edge <- function() {
  x <- list("é" = 1, 2)
  names(x)[2] <- NA
  for (k in 1:2) {
    x$"é" <- k
    x[["é"]] <- k + 10
  }
  x$`NA` <- "na"
  x
}
print(names_edge())
print(names_edge())
