f <- function(x, ...) substitute(x)
f(a + b * c)
g <- function(x, y) match.call()
g(1 + 2, y = "z")
h <- function(...) sapply(substitute(list(...))[-1], deparse)
h(alpha, beta + 1, "s")
quote(f(x, y = 2))[[3]]
