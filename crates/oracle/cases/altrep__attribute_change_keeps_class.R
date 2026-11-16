# An attribute change keeps an ALTREP object's class, which serialize records.
record <- function(x) {
  lines <- strsplit(rawToChar(serialize(x, NULL, ascii = TRUE)), "\n")[[1]]
  lines[7]
}
x <- .Internal(wrap_meta(c(1, 2), 0L, 0L))
attr(x, "foo") <- 1
record(x)
class(x) <- "bar"
record(x)
oldClass(x) <- "baz"
record(x)
unclass(x)
y <- x
class(y) <- "qux"
c(class(x), class(y))
y[1] <- 10
x
y
