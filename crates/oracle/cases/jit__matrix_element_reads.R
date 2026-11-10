# `m[i, j]` and `m[[i, j]]` with scalar subscripts on an ordinary integer, double or logical
# matrix read the element inline once compiled. Everything else must behave as in GNU R: a
# subscript that is out of bounds, `NA`, zero, negative or fractional, a subscript that is an
# integer on one path and a double on another, and an object that is not an ordinary matrix (a
# vector, a 3-d array, a data frame, a classed matrix with its own method). Each function runs
# a few times so compiled code runs it too. A negative subscript is left out: GNU R checks it
# against an uninitialized length, so its error message changes from run to run.
read_cells <- function(m, rows, cols) {
  total <- 0
  for (k in seq_along(rows)) total <- total + m[[rows[[k]], cols[[k]]]] + m[rows[[k]], cols[[k]]]
  total
}

read_one <- function(m, i, j) m[[i, j]]
read_single <- function(m, i, j) m[i, j]

mixed <- function(m, n) {
  out <- numeric(n)
  for (k in seq_len(n)) {
    j <- if (k %% 2L == 0L) 2L else (k %/% 2) %% 3 + 1
    out[[k]] <- m[[1L, j]] + m[2L, j]
  }
  out
}

failure <- function(expr) {
  tryCatch(expr, error = function(e) cat("error:", conditionMessage(e), "| call:", deparse(conditionCall(e)), "\n"))
}

`[[.tagged` <- function(x, i, j) paste("tagged", i, j)

integers <- matrix(1:6, 2)
doubles <- matrix(c(1.5, 2.5, 3.5, 4.5, 5.5, 6.5), 2)
logicals <- matrix(c(TRUE, FALSE, NA, TRUE), 2)
cube <- array(1:8, c(2, 2, 2))
frame <- data.frame(a = 1:2, b = c(10, 20))
tagged <- structure(matrix(1:4, 2), class = "tagged")

for (round in 1:3) {
  cat("round", round, "\n")
  print(read_cells(integers, c(1L, 2L, 1L), c(1L, 3L, 2L)))
  print(read_cells(doubles, c(1, 2, 2), c(3, 1, 2)))
  print(read_cells(logicals, c(1L, 2L), c(2L, 2L)))
  print(c(read_one(integers, 2L, 3L), read_one(doubles, 1.9, 2.7), read_one(logicals, 1L, 2L)))
  print(c(read_single(integers, 2L, 3L), read_single(doubles, NA_real_, 1)))
  print(mixed(doubles, 6L))
  failure(read_one(integers, 3L, 1L))
  failure(read_one(integers, 1L, 4L))
  failure(read_one(integers, NA_integer_, 1L))
  failure(read_one(doubles, 1, NA_real_))
  failure(read_one(integers, 0L, 1L))
  failure(read_one(doubles, 0.5, 1))
  failure(read_single(integers, 3L, 1L))
  failure(read_cells(integers, c(1L, 3L), c(1L, 1L)))
  failure(read_cells(integers, c(1L, NA), c(1L, 1L)))
  failure(read_cells(doubles, c(1, 1), c(2, 0)))
  failure(read_cells(doubles, c(1, 1), c(2, 3.5)))
  failure(read_cells(frame, 1L, 2L))
  print(read_single(integers, NA_integer_, 1L))
  failure(read_one(1:4, 1L, 1L))
  failure(read_one(cube, 1L, 1L))
  print(read_one(frame, 2L, 2L))
  print(read_one(tagged, 1L, 2L))
}
