x <- 1:6
attr(x, "note") <- "hello"
attributes(x)
dim(x) <- c(2, 3)
x
attributes(x)
y <- structure(1:3, class = "foo", extra = TRUE)
unclass(y)
attributes(y)
names(y) <- c("a", "b", "c")
attributes(y)
oldClass(y)
inherits(y, "foo")
z <- c(a = 1, b = 2)
attributes(z[1])
attributes(z + 1)
mostattributes(z) <- list(names = c("p", "q"), colour = "red")
attributes(z)

# A call's argument names are not attributes; a pairlist's tags are.
print(attributes(quote(foo(bar = 1))))
print(attributes(pairlist(a = 1)))
# `.set_row_names()` returns a double, which is stored as integer compact row names.
d <- structure(list(), class = "data.frame", row.names = c(NA, -3))
print(dim(d))
print(typeof(attr(d, "row.names")))
