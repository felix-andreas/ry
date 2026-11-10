f <- function() { e <- quote(g(x)); e[[2]] <- 1; e }
h <- function(x) missing(x) || identical(x, quote(expr = ))
k <- function() { quote <- function(x) "mine"; quote(z) }
m <- function(v) { w <- quote(v); list(w, quote(expr = v + 1)) }
for (i in 1:4) cat(deparse(f()), h(), h(1), k(), deparse(m(2)), "\n")
print(body(f))
p <- function(y) base::identical(y, 1)
r <- function() stats:::Pillai
s <- function(x) { x <- 2; base::c(x, utils::head(1:3, 1)) }
for (i in 1:4) cat(p(1), p(2), is.function(r()), s(1), "\n")
invisible(NULL)
