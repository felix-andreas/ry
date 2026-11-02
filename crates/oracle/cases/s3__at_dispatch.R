# Since R 4.3, `@` and `@<-` dispatch S3 methods on objects that are not S4, with the
# slot name passed as a string. S7 reads and writes properties this way.
`@.foo` <- function(object, name) { print(sys.call()); paste("got", name) }
`@<-.foo` <- function(object, name, value) {
  cat("set", name, "\n")
  if (name == "outer") { object@inner <- value; return(object) }
  attr(object, name) <- value
  object
}
x <- structure(list(), class = "foo")
print(x@abc)
x@def <- 3
print(attr(x, "def"))
x@outer <- 2
print(attr(x, "inner"))
f <- function(o) { o@ghi <- 4; attr(o, "ghi") }
print(f(x))
