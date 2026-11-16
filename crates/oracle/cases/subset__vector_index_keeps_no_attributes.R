# x[i] with one subscript on a vector whose attributes the selection drops: a matrix, a vector
# with other attributes, and a long vector that structure() wraps, against those that keep one.
m <- matrix(1:12 + 0.5, 3, dimnames = list(letters[1:3], LETTERS[1:4]))
str(m[c(2, 5, 13, NA)]); str(m[c(2L, 5L)]); str(m[-1]); str(m[m > 6]); str(m[c(TRUE, FALSE)])
str(m[0]); str(m[c(1.9, 2.2)])
im <- matrix(1:6, 2); str(im[c(6, 1)]); str(im[7L])
cm <- matrix(letters[1:6], 2); str(cm[c(2, 7)])
lm <- matrix(list(1, "a", TRUE, NULL), 2); str(lm[c(1, 4)])
x <- structure(c(3.5, 1, 2), foo = "bar", class = "baz"); str(unclass(x)[2:3]); str(.subset(x, 2))
long <- seq(0.5, 300.5); w <- structure(long, foo = 1); long[1] <- 0.25
str(w[c(1, 300, 302)]); str(attributes(w[1:3])); str(w[w > 299])
wi <- structure(c(1:200, NA), units = "s"); str(wi[c(201, 1, 400)])
d <- as.Date("2020-01-01") + 0:199; str(d[c(1, 200)]); str(.subset(d, 3))
f <- factor(c("u", "v", "u")); str(.subset(f, 2:3))
a <- array(1:3, 3, list(c("p", "q", "r"))); str(a[2:3]); str(a[2])
n <- c(a = 1, b = 2); attr(n, "foo") <- 1; str(n[2])
ts1 <- ts(1:10); str(ts1[2:3])
e <- structure(expression(a, b+1), levels = "q"); str(e[2]); h <- structure(1:3, levels = c("a","b","c"), class = "factor", names = c("p","q","r")); str(.subset(h, 2)); str(.subset(factor(c("u","v")), -1))
invisible(NULL)
