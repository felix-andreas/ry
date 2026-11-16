# Recall passes on the promises its own call received, unforced, so the recalled function decides
# again whether to evaluate each one and substitute() sees the code Recall was called with.
cnt <- 0
r <- function(n, a) if (n == 0) 1 else Recall(n - 1, {cnt <<- cnt + 1; a})
r(3, 1); print(cnt)
v <- "a"
r2 <- function(n, a) if (n == 0) list(deparse(substitute(a)), a) else Recall(n - 1, a)
str(r2(1, v))
f <- function() {
  out <- character()
  for (k in 1:3) {
    hits <- 0
    r3 <- function(n, a) if (n == 0) deparse(substitute(a)) else Recall(n - 1, a)
    out <- c(out, r3(2, {hits <- hits + 1; k}), hits)
  }
  out
}
print(f()); print(f()); print(f())
fact <- function(n) if (n <= 1) 1 else n * Recall(n - 1)
print(sapply(0:6, fact))
fib <- function(n) if (n < 2L) n else Recall(n - 1L) + Recall(n - 2L)
print(vapply(0:10, fib, integer(1)))
dflt <- function(n, a = n * 10) if (n == 0) a else Recall(n - 1)
print(dflt(3))
dots <- function(n, ...) if (n == 0) c(...length(), ...) else Recall(n - 1, ...)
print(dots(2, 4, 5))
miss <- function(n, a) if (n == 0) missing(a) else Recall(n - 1, a)
print(miss(2))

# The name of `$`, `$<-`, `@` and `@<-` reaches an S3 method as a string, as the promise of
# that string.
`$.cls` <- function(x, name) list(deparse(substitute(name)), name)
`$<-.cls` <- function(x, name, value) { attr(x, "s") <- list(deparse(substitute(name)), name, deparse(substitute(value))); x }
`@.cls` <- function(object, name) list(deparse(substitute(name)), name)
`@<-.cls` <- function(object, name, value) { attr(object, "s") <- list(deparse(substitute(name)), name); object }
obj <- structure(list(), class = "cls")
str(obj$abc)
str(obj$"abc")
obj$abc <- v
str(attr(obj, "s"))
str(obj@abc)
obj@abc <- 1
str(attr(obj, "s"))
g <- function() { o <- obj; o$def <- v; list(o$abc, attr(o, "s")) }
str(g()); str(g()); str(g())
