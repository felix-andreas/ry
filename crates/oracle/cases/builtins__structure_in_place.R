# `structure()` with plain attributes and a class sets them in place; these pin the
# attribute order, copying and replacement that must match GNU R.
x <- 1:4
y <- structure(x, foo = "a", class = "bar")
str(attributes(x)); str(attributes(y))
m <- matrix(1:4, 2)
str(attributes(structure(m, foo = 1)))
z <- 1:4; attr(z, "foo") <- 1; dim(z) <- c(2L, 2L)
str(attributes(structure(z, bar = 2)))
dput(structure(z, bar = 2))
n <- c(a = 1, b = 2); attr(n, "k") <- "v"
dput(structure(n, k = NULL, j = 3))
dput(structure(n, k = "w", k = "u"))
dput(structure(list(p = 1), class = c("one", "two"), extra = TRUE))
df <- data.frame(u = 1:3)
str(attributes(structure(df, note = "hi")))
f <- function() structure(list(), class = "cond", msg = "m")
a <- f(); b <- f(); attr(a, "msg") <- "changed"; str(attr(b, "msg"))
e <- new.env(); invisible(structure(e, tag = 1)); str(attr(e, "tag"))
s4 <- setClass("P", representation(v = "numeric"))
p <- s4(v = 1); q <- structure(p, extra = 2); print(isS4(q)); print(attr(q, "extra"))
