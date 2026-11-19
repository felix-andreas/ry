# A factor subscript selects by its codes, not its labels, whatever its attributes.
x <- c(a = 10, b = 20, c = 30, d = 40)
f <- factor(c("c", "a", "c", NA, "b"), levels = c("b", "c", "a"))
print(x[f])
print(unname(x)[f])
l <- list(1, "two", 3L, TRUE)
print(l[f])
g <- factor(c("x", "y"), levels = c("x", "y", "z", "w"))
print(letters[g])
names(g) <- c("first", "second")
print(letters[g])
m <- matrix(1:12, 3)
print(m[factor(c("q", "p"), levels = c("p", "q"))])
h <- factor(c("u", "v", "u"))
attr(h, "extra") <- "kept"
print(c(TRUE, FALSE)[h])
y <- x
y[factor(c("z", "y"), levels = c("y", "z"))] <- 0
print(y)
print(x[factor(character())])
