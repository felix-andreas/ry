# `x[]` and `x[[]]` carry one empty subscript, so a replacement method's `nargs()` counts it.
length(quote(x[]))
length(quote(x[[]]))
quote(x[])
`[<-.counted` <- function(x, i, j, value) {
    cat("nargs:", nargs(), "\n")
    x
}
x <- structure(list(1), class = "counted")
x[] <- 1
x[1] <- 1
x[1, ] <- 1
v <- 1:3
v[] <- 0L
v
m <- matrix(1:4, 2)
m[] <- 9L
m
df <- data.frame(a = 1, b = "x")
df[] <- lapply(df, toupper)
df
