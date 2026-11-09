# attr(), names(), class(), oldClass(), dim(), inherits() and their setters on the edges a
# native fast path must keep: partial matching, row.names, implicit classes, NULL, S4 bits.

x <- structure(1:3, foo = "a", foobar = 2, names = c("p", "q", "r"))
attr(x, "foo")
attr(x, "foob")
attr(x, "fo")
attr(x, "fo", exact = TRUE)
attr(x, "names")
attr(x, "nam")
attr(x, "dim")
attr(NULL, "a")
attr(x, which = "foobar")
attr(which = "foo", x = x)
tryCatch(attr(x, c("a", "b")), error = conditionMessage)
tryCatch(attr(x, 1), error = conditionMessage)
tryCatch(attr(x, NA_character_), error = conditionMessage)
attr(data.frame(a = 1:3), "row.names")
attr(data.frame(a = 1:3), "row.n")
attr(quote(f(a = 1)), "names")
attr(pairlist(a = 1, 2), "names")
attr(pairlist(1, 2), "names")
f <- factor(c("u", "v"))
attr(f, "levels")
attr(f, "class")

y <- 1:4
attr(y, "dim") <- c(2L, 2L)
y
attr(y, "dim") <- NULL
y
attr(y, "names") <- letters[1:2]
y
attr(y, "names") <- NULL
attr(y, "class") <- "myclass"
inherits(y, "myclass")
attr(y, "class") <- NULL
inherits(y, "myclass")
tryCatch(attr(y, "dim") <- 3L, error = conditionMessage)
tryCatch(attr(y, "class") <- 1, error = conditionMessage)
z <- NULL
attr(z, "a") <- NULL
z
tryCatch(attr(z, "a") <- 1, error = conditionMessage)
w <- 1:2
attr(w, "names") <- 1:2
w
attr(w, "row.names") <- 1:2
attributes(w)

names(1:3)
names(c(a = 1, 2))
names(list(a = 1, b = 2))
names(NULL)
names(quote(f(x, y = 2)))
names(pairlist(a = 1))
e <- new.env()
assign("k", 1, envir = e)
names(e)
v <- 1:3
names(v) <- c("a", "b")
v
names(v) <- NULL
v
names(v) <- factor(c("x", "y", "z"))
v
tryCatch(names(e) <- "a", error = conditionMessage)

class(1)
class(1L)
class("a")
class(NULL)
class(sum)
class(function() 1)
class(matrix(1:4, 2))
class(array(1:8, c(2, 2, 2)))
class(array(1:2, 2))
class(quote(x))
class(quote(x + 1))
class(quote(if (a) b))
class(quote(for (i in 1) 1))
class(quote((x)))
class(quote({x}))
class(data.frame())
class(structure(1, class = c("a", "b")))
oldClass(1)
oldClass(structure(1, class = "k"))
oldClass(matrix(1))
inherits(1, "numeric")
inherits(1L, "integer")
inherits(matrix(1), "matrix")
inherits(matrix(1), "array")
inherits(array(1, c(1, 1, 1)), "matrix")
inherits(structure(1, class = c("a", "b")), c("x", "b"), which = TRUE)
inherits(structure(1, class = c("a", "b")), c("b", "a"), TRUE)
inherits(structure(1, class = c("a", "b")), character())
inherits(NULL, "NULL")
inherits(sum, "function")
inherits(data.frame(), "data.frame")
inherits(structure(1, class = "été"), "été")
tryCatch(inherits(1, 2), error = conditionMessage)

dim(1:3)
dim(matrix(1:6, 2))
dim(array(1:24, 2:4))
dim(data.frame(a = 1:3, b = 4:6))
dim(NULL)
m <- 1:6
dim(m) <- c(2, 3)
storage.mode(dim(m))
m
dim(m) <- NULL
m
tryCatch(dim(m) <- c(4, 4), error = conditionMessage)
tryCatch(dim(m) <- c(-1, -6), error = conditionMessage)
nm <- c(a = 1, b = 2)
dim(nm) <- 2L
attributes(nm)
