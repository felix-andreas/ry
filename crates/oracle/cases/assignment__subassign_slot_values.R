# `x[i] <- value`, `x[[i]] <- value`, `x$name <- value` and replacement calls such as
# `names(x) <- value` on a function's local variables. A value another variable shares must be
# copied, a value the variable alone holds may change in place, and an argument's value never
# changes in the caller. Also growth from `NULL`, a list that stores itself, a matrix, a factor
# that dispatches, a replacement closure, a warning whose calling handler looks at the frame
# mid-assignment, and the calls errors report. Each runs a few times so compiled code runs it
# too.
aliased <- function() {
  x <- c(1, 2, 3)
  y <- x
  x[1] <- 0
  list(x = x, y = y)
}

aliased_list <- function() {
  x <- list(a = 1, b = "two")
  y <- x
  x$a <- 10
  x[["b"]] <- "three"
  x$c <- TRUE
  list(x = x, y = y)
}

argument <- function(v) {
  v[2] <- 99L
  v[[3]] <- 100L
  v
}

grow <- function(n) {
  out <- NULL
  for (i in seq_len(n)) out[i] <- i * 2
  names <- character()
  for (i in seq_len(n)) names[[i]] <- letters[i]
  record <- list()
  for (i in seq_len(n)) record[[letters[i]]] <- i
  list(out, names, record)
}

self_store <- function() {
  x <- list(1, 2)
  x[[2]] <- x
  x
}

matrix_cell <- function() {
  m <- matrix(1:6, 2)
  keep <- m
  m[3] <- 0L
  list(m, keep)
}

factor_level <- function() {
  f <- factor(c("a", "b", "a"))
  f[2] <- "a"
  f
}

type_change <- function() {
  x <- 1:3
  y <- x
  x[2] <- "b"
  x[[4]] <- 2.5
  list(x, y)
}

watched <- function() {
  x <- 1:4
  withCallingHandlers(
    x[1:2] <- 1:3,
    warning = function(w) {
      cat("warning:", conditionMessage(w), "\n")
      frame <- environment(sys.function())
      cat("frame has x:", exists("x", envir = frame, inherits = FALSE), "\n")
      invokeRestart("muffleWarning")
    }
  )
  x[3] <- 30L
  x
}

failing <- function(which) {
  x <- 1:3
  if (which == 1) x[[5:6]] <- 1L
  if (which == 2) x[[0]] <- 1L
  x
}

replaced <- function(v) {
  x <- c(a = 1, b = 2)
  y <- x
  names(x) <- c("p", "q")
  attr(x, "note") <- "kept"
  dim(v) <- c(2L, 2L)
  z <- 1:2
  class(z) <- "tagged"
  levels(z) <- c("lo", "hi")
  w <- z
  names(w) <- c("m", "n")
  list(x = x, y = y, v = v, z = unclass(z), w = unclass(w))
}

`second<-` <- function(x, value) {
  x[2] <- value
  x
}

by_closure <- function() {
  x <- 1:3
  y <- x
  second(x) <- 20L
  names(x) <- c("a", "b", "c")
  list(x, y)
}

self_named <- function() {
  x <- c("a", "b")
  names(x) <- x
  x
}

x_global <- c(5, 6)
global_alias <- function() {
  x <- x_global
  x[1] <- -1
  c(x, x_global)
}

for (round in 1:3) {
  cat("round", round, "\n")
  str(aliased())
  str(aliased_list())
  original <- 1:4
  print(argument(original))
  print(original)
  str(grow(3))
  str(self_store())
  print(matrix_cell())
  print(factor_level())
  print(type_change())
  print(watched())
  print(tryCatch(failing(1), error = function(e) conditionCall(e)))
  print(tryCatch(failing(2), error = function(e) conditionMessage(e)))
  print(global_alias())
  arg <- 1:4
  str(replaced(arg))
  print(arg)
  str(by_closure())
  print(self_named())
}
