# The replacement-length warning names the current call: the assignment at its own level, the
# closure that calls `[<-` directly, and `NextMethod(...)` for a method that defers to `[<-`.
h <- function(expr) withCallingHandlers(expr, warning = function(w) { print(conditionCall(w)); invokeRestart("muffleWarning") })
x <- as.Date(0:2); h(x[2] <- as.Date(c(0, 3)))
x <- structure(c(1, 2, 3), class = "foo")
`[<-.foo` <- function(x, i, value) { NextMethod("[<-") }
h(x[2] <- c(0.1, 3))
`[<-.foo` <- function(x, i, value) { NextMethod() }
h(x[2] <- c(0.1, 3))
`[<-.foo` <- function(x, i, value) { y <- NextMethod(.Generic); y }
h(x[2] <- c(0.1, 3))
f <- function(x) { x[2] <- c(0.1, 3); x }
h(f(x))
x <- as.POSIXct(c(0, 1), origin = "1970-01-01", tz = "UTC"); h(x[1] <- x[c(1, 2, 1)])
d <- as.difftime(c(1, 2), units = "secs"); h(d[1] <- c(1, 2))
tryCatch(x[1] <- x[c(1, 2, 1)], warning = function(w) print(w))
f <- function(x) `[<-`(x, 2, value = c(1, 2)); h(f(1:3))
f <- function(x) { y <- `[<-`(x, 2, value = c(1, 2)); y }; h(f(1:3))
f <- function(x) { x[2] <- c(1, 2); x }; h(f(1:3))
f <- function(x) { x[[2]] <- 1; x[2:3] <- c(1, 2, 3); x }; h(f(list(1, 2, 3)))
h(local({ x <- 1:3; x[2] <- c(1, 2) }))
g <- function(x) { x[cbind(1, 1:2)] <- 1:3; x }; h(g(matrix(1:4, 2)))
