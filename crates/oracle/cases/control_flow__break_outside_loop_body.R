# A body with `break` or `next` outside any loop runs until it reaches it, then fails, however
# often it is called and whether or not the branch is taken.
f <- function(x) if (x) break else x + 1
for (k in 1:4) print(f(FALSE))
print(tryCatch(f(TRUE), error = function(e) "break failed"))
g <- function(x) { y <- x * 2; if (y > 10) next; y }
for (k in 1:4) print(g(k))
print(tryCatch(g(6), error = function(e) "next failed"))
h <- g
environment(h) <- new.env()
print(h(3))
