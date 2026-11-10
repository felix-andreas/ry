# `x[[i, j]]` on a matrix, read in a loop so compiled code takes the element read.
read_all <- function(m) {
    out <- vector(typeof(m), length(m))
    k <- 0L
    for (j in seq_len(ncol(m))) for (i in seq_len(nrow(m))) {
        k <- k + 1L
        out[[k]] <- m[[i, j]]
    }
    out
}
str(read_all(matrix(1:6, 2)))
str(read_all(matrix(c(1.5, 2.5, NA, 4.5), 2)))
str(read_all(matrix(c(TRUE, NA, FALSE, TRUE), 2)))
str(read_all(matrix(c("a", "b", NA, "d"), 2)))
named <- matrix(1:4, 2, dimnames = list(c("r1", "r2"), c("c1", "c2")))
print(named[[2, 2]])
print(named[["r2", "c1"]])
listed <- matrix(list(1, "two", 3L, NULL), 2)
str(listed[[2, 1]])
str(listed[[2, 2]])
m <- matrix(1:6, 2)
print(m[[2.9, 3.1]])
show_error <- function(expr) tryCatch(expr, error = function(e) print(conditionMessage(e)))
show_error(m[[3, 1]])
show_error(m[[1, 4]])
show_error(m[[0, 1]])
show_error(m[[NA, 1]])
show_error(m[[-1, 1]])
show_error(m[[-1L, 1]])
show_error(m[[1, -2L]])
show_error((1:2)[[-1L]])
show_error(m[[1:2, 1]])
a <- array(1:24, c(2, 3, 4))
show_error(a[[2, 3]])
print(a[[2, 3, 4]])
v <- 1:6
show_error(v[[1, 1]])
tagged <- structure(matrix(1:4, 2), class = "tagged")
`[[.tagged` <- function(x, i, j) paste("tagged", i, j)
print(tagged[[1, 2]])
df <- data.frame(a = 1:2, b = c("x", "y"))
print(df[[2, "b"]])
