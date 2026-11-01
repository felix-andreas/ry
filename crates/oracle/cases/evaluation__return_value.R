# returnValue() inside on.exit code, even in functions it calls, is the exiting function's value,
# and its default while an error unwinds.
g <- function() { r1 <- "marker"; print(identical(returnValue(r1), r1)); print(returnValue("d")) }
f <- function() { on.exit(g()); 42 }
f()
h <- function() { on.exit(print(returnValue("none"))); stop("x") }
try(h(), silent = TRUE)
k <- function() { on.exit(local(print(returnValue("d")))); "v" }
k()
m <- function() { on.exit({ q <- function() { on.exit(NULL); 1 }; q(); print(returnValue("d")) }); "outer" }
m()
