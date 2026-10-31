cat("before\n")
f <- function(x) stop("bad input: ", x)
f(3)
cat("after\n")
