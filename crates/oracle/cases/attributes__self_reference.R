# Setting an object's attribute to the object itself attaches a copy, so the result has no cycle
# and serializes.
f <- function() {
  x <- c("a", "b")
  names(x) <- x
  x
}
y <- f()
attributes(names(y))
length(serialize(y, NULL)) > 0
g <- function() {
  x <- 1:3
  attr(x, "self") <- x
  x
}
z <- g()
attributes(attr(z, "self"))
identical(unserialize(serialize(z, NULL)), z)
