f <- function() { on.exit(cat("first\n")); on.exit(cat("second\n"), add = TRUE); cat("body\n"); invisible(1) }
f()
g <- function() { on.exit(cat("cleanup\n")); stop("boom") }
tryCatch(g(), error = function(e) cat("caught:", conditionMessage(e), "\n"))
h <- function() { for (i in 1:10) if (i == 3) return(i * 100); -1 }
h()
k <- function() { lapply(1:3, function(i) if (i == 2) return("inner")); "outer" }
k()
early <- function(x) { if (x > 0) return("positive"); "non-positive" }
early(1); early(-1)
vis <- function() invisible(5)
vis()
print(vis())
w <- withVisible(vis()); w$visible
r <- function() { on.exit(return("from on.exit")); "from body" }
r()
