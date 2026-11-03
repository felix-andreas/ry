# do.call() puts the argument values themselves into the call, and sys.call() and sys.calls()
# return that call without copying the values, so their cost does not grow with the arguments.
big <- lapply(1:200, function(i) runif(2000))
f <- function(x, ...) sys.call()
cl <- do.call(f, list(big))
identical(cl[[2]], big)
length(cl)
g <- function(x) sys.calls()
calls <- do.call(g, list(big))
identical(calls[[length(calls)]][[2]], big)

# Rprofmem logs every vector allocation above the threshold; returning the call allocates none.
# GNU R also logs the pages it takes for small objects, which ry has no counterpart for.
log <- tempfile()
h <- function(x) {
  Rprofmem(log, threshold = 10000)
  for (i in 1:5) {
    sys.call()
    sys.calls()
    match.call()
  }
  Rprofmem(NULL)
}
do.call(h, list(big))
sum(!startsWith(readLines(log), "new page:"))

# The returned call is a copy as far as R code can tell: changing it, or a value or call inside
# it, leaves the frame's call alone, so a later sys.call() sees the original.
k <- function(x, y) {
  first <- sys.call()
  first[[1]] <- quote(changed)
  first[[2]][[1]][1] <- -1
  first[[3]][[1]] <- quote(other)
  second <- sys.call()
  list(first[[1]], first[[2]][[1]][1], first[[3]], second[[1]], second[[2]][[1]][1], second[[3]])
}
small <- list(c(1, 2), c(3, 4))
do.call("k", list(small, quote(sum(1, 2))))
small[[1]][1]
k2 <- function(x) {
  first <- sys.call()
  attr(first, "note") <- "set"
  second <- sys.call()
  c(is.null(attr(first, "note")), is.null(attr(second, "note")))
}
do.call("k2", list(1))
deparse(do.call("f", list(c(a = 1, b = 2), y = "text")))
