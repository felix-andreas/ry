f <- function(x, y) if (missing(y)) "no y" else y
f(1)
f(1, 2)
g <- function(x = stop("needed")) missing(x)
g()
h <- function(...) nargs()
h(1, 2, 3)
delayedAssign("lazy", {cat("evaluated\n"); 42})
cat("before\n")
lazy
lazy
