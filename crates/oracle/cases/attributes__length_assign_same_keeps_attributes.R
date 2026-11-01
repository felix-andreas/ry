# length<- with an unchanged length returns the object with all attributes, which S4 methods with fewer formals than the generic rely on.
x <- structure(c(a=1,b=2), package="p", class="foo")
length(x) <- 2; str(unclass(x)); print(class(x))
length(x) <- 3; str(x)
y <- structure(1:2, dim=c(1L,2L)); length(y) <- 2L; str(y)
x <- NULL; length(x) <- 2; print(x)
setClass("Foo", representation(x = "numeric"))
setReplaceMethod("[", signature(x = "Foo", i = "numeric", j = "missing", value = "numeric"),
  function(x, i, value) { x@x[i] <- value; x })
f <- new("Foo", x = 1:3 + 0)
f[2] <- 9
print(f@x)
g <- function(x, i, ..., value) { h <- function(x, i, value) value; h(x, i, ..., value) }
print(g(1, 2, value = 3))
setMethod("Summary", "Foo", function(x, ..., na.rm = FALSE) paste(.Generic, do.call(.Generic, list(x@x))))
print(max(f))
