# `missing(x)` compiles to its own tier-0 op; these pin the cases that must
# keep GNU R's semantics.
f <- function(a, b, c = 1) c(missing(a), missing(b), missing(c))
print(f()); print(f(1)); print(f(b = 2, c = 3))
g <- function(x) f(x)
print(g()); print(g(1))
h <- function(x) { missing <- function(y) "shadow"; missing(x) }
print(h())
k <- function(x) { v <- missing(x); x <- 1; c(v, missing(x)) }
print(k())
d <- function(...) missing(..1)
print(d()); print(d(1))
print(withVisible((function(z) missing(z))())$visible)
loop <- function(a, b) { s <- 0; for (i in 1:10) if (missing(b)) s <- s + 1; s }
print(loop(1)); print(loop(1, 2))
