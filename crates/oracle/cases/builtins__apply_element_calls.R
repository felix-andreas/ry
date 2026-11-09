# `lapply` and `vapply` bind `i` to one integer updated in place and read `X[[i]]`
# directly for a vector with no class; these pin where that must match GNU R.
str(lapply(1:3, function(x) get("i", parent.frame())))
str(vapply(1:3, function(x) get("i", parent.frame()), 0L))
e <- NULL; invisible(lapply(1:3, function(x) e <<- parent.frame())); print(e$i)
keep <- list(); invisible(lapply(1:3, function(x) keep[[x]] <<- get("i", parent.frame()))); str(keep)
rebind <- function(x) { assign("i", 10L, envir = parent.frame()); x }
str(lapply(1:3, rebind))
str(lapply(as.list(quote(f(a, ))), function(x) if (missing(x)) "miss" else deparse(x)))
str(lapply(structure(list(1, 2), class = "foo"), identity))
`[[.foo` <- function(x, i) "dispatched"
str(lapply(structure(list(1, 2), class = "foo"), identity))
str(vapply(c(a = 1.5, b = 2), function(v) v * 2, 0))
m <- matrix(1:4, 2); str(lapply(m, identity))
str(lapply(c(TRUE, NA), identity)); str(lapply(c("p", NA), identity))
str(lapply(1:3, function(x, k) x * k, k = 2))
str(vapply(list(1:2, 3:4), function(v) v, c(x = 0L, y = 0L)))
str(vapply(list(c(p = 1L, q = 2L)), function(v) v, c(x = 0L, y = 0L)))
str(vapply(list(c(p = 1L, q = 2L)), function(v) v, c(0L, 0L)))
str(vapply(list(1:2), function(v) v, c(x = 0L, y = 0L), USE.NAMES = FALSE))
