# A call that forwards `...` twice holds each argument reference twice in its
# dots list, and lifting the callee's frame detaches every one of them once.
f <- function(...) g(..., ...)
g <- function(...) function() list(...)
h <- function(...) k(..., ...)
k <- function(...) {
  environment()
  c(...)
}
run <- function(x, y) {
  a <- x + 1
  print(f(x, y)()[[4]])
  print(f(x + 1, a)()[[3]])
  h(x, y)
}
for (i in 1:4) print(run(i, i * 2))
