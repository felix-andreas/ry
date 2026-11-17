# Assigning through a matrix of indices warns when the rows are not a multiple of the value's
# length and is an error for an empty value, as a vector subscript is.
f <- function(x, i, v) withCallingHandlers({ x[i] <- v; x }, warning = function(w) { message("W: ", conditionMessage(w)); invokeRestart("muffleWarning") })
print(f(matrix(as.raw(1:4), 2), cbind(1, 1), as.raw(c(65, 255))))
print(f(matrix(1:4, 2), cbind(1, 1), 7:8))
print(f(matrix(c(1, 2, 3, 4), 2), cbind(c(1, 2, 1), c(1, 2, 2)), c(7, 8)))
print(f(matrix(letters[1:4], 2), cbind(1, 1:2), c("p", "q", "r")))
print(f(matrix(list(1, 2, 3, 4), 2), cbind(2, 2), list(7, 8)))
print(f(matrix(1:4, 2), cbind(1:2, 1:2), 5:8))
print(f(array(1:8, c(2, 2, 2)), cbind(1, 1, 1:2), 0:2))
print(f(matrix(1:4, 2), matrix(0L, 0, 2), 1:3))
r <- tryCatch(f(matrix(1:4, 2), cbind(1, 1), integer(0)), error = function(e) conditionMessage(e)); print(r)
r <- tryCatch(f(matrix(1:4, 2), cbind(1, 1), NULL), error = function(e) conditionMessage(e)); print(r)
