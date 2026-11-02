# `m[, j]` copies one column; the result keeps the row names and drops to a plain vector.
m <- matrix(1:6, nrow = 3)
m[, 2]
m[, 2.9]
x <- matrix(c(1.5, 2.5, 3.5, 4.5), nrow = 2, dimnames = list(c("a", "b"), c("p", "q")))
x[, 2]
x[, "q"]
x[, 2, drop = FALSE]
flags <- matrix(c(TRUE, FALSE, NA, TRUE), nrow = 2)
flags[, 1]
one_row <- matrix(1:3, nrow = 1, dimnames = list("r", c("u", "v", "w")))
one_row[, 2]
tryCatch(m[, 3], error = function(e) conditionMessage(e))
m[, NA_integer_]
m[, 0]
apply(matrix(1:6, nrow = 2), 1, function(row) sum(row * 2))
