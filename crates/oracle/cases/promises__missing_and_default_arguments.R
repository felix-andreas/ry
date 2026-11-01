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
# Assigning a missing formal, by `<-`, a for loop or assign(), makes it no longer missing.
f <- function(x) { x <- 1; missing(x) }
g <- function(x) { for (x in 1:2) NULL; missing(x) }
h <- function(x) { assign("x", 3); missing(x) }
k <- function(x = 0) { x <- 1; missing(x) }
c(f(), g(), h(), k())
