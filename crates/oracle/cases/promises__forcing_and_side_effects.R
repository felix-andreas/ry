f <- function(x) { cat("before\n"); x; cat("after\n"); x }
f({ cat("evaluating\n"); 1 })
g <- function(x, y = x * 2) { x <- 10; y }
g(1)
lazy <- function(a, b) if (missing(b)) "no b" else b
lazy(1)
tryCatch(lazy(1, stop("forced")), error = function(e) conditionMessage(e))
h <- function(x) { force(x); function() x }
fs <- list(); for (i in 1:3) fs[[i]] <- h(i)
fs[[1]](); fs[[3]]()
delayedAssign("lz", { cat("now\n"); 42 })
cat("defined\n"); lz; lz
