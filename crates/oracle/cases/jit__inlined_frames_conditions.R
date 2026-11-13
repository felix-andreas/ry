# Compiled code runs a closure call without a frame when nothing can tell, and a condition the
# callee signals is where something can: the condition names the call, a calling handler runs
# under the callee's frame, and `sys.call` in the callee reads it. Each group warms a caller
# until compiled code runs it, then signals from a call that had been running without a frame.
# The callees run on their own first, so that a caller compiled early already knows them.

rate <- 2
callee <- function(x, k) {
  if (x > 90L) stop("too big: ", x)
  y <- x * k * rate
  y + 1
}
for (i in 1:30) callee(i, 2)
caller <- function(n) {
  s <- 0
  i <- 0L
  while (i < n) {
    i <- i + 1L
    s <- s + callee(i, 2)
  }
  s
}
for (round in 1:5) print(caller(80L))
print(tryCatch(caller(100L), error = function(e) deparse(conditionCall(e))))
print(tryCatch(caller(100L), error = function(e) conditionMessage(e)))
print(tryCatch(
  withCallingHandlers(caller(100L), error = function(e) print(sys.call(-2L))),
  error = function(e) "caught"
))
print(caller(80L))

warner <- function(x, d) {
  if (x > 90L) warning("big ", x)
  if (x > 95L) return(sys.call())
  x / d
}
for (i in 1:30) warner(i, 2)
warning_caller <- function(n) {
  s <- 0
  i <- 0L
  while (i < n) {
    i <- i + 1L
    r <- warner(i, 2)
    if (is.call(r)) return(r)
    s <- s + r
  }
  s
}
for (round in 1:5) print(warning_caller(80L))
seen <- character()
print(withCallingHandlers(
  warning_caller(93L),
  warning = function(w) {
    seen <<- c(seen, paste(conditionMessage(w), deparse(conditionCall(w)), sys.nframe()))
    invokeRestart("muffleWarning")
  }
))
print(seen)
print(suppressWarnings(warning_caller(100L)))
print(tryCatch(warning_caller(93L), warning = function(w) deparse(conditionCall(w))))
print(warning_caller(80L))

base <- 10
inner <- function(a) {
  if (a > 90L) stop("inner fails at ", a)
  a + base
}
middle <- function(x, k) {
  if (x < 0L) stop("negative")
  t <- inner(x)
  t * k
}
for (i in 1:30) middle(i, 2)
nested_caller <- function(n) {
  s <- 0
  i <- 0L
  while (i < n) {
    i <- i + 1L
    s <- s + middle(i, 2)
  }
  s
}
for (round in 1:5) print(nested_caller(80L))
print(tryCatch(nested_caller(100L), error = function(e) deparse(conditionCall(e))))
print(tryCatch(
  withCallingHandlers(nested_caller(100L), error = function(e) print(tail(sys.calls(), 5L)[1:3])),
  error = function(e) conditionMessage(e)
))
print(nested_caller(80L))
