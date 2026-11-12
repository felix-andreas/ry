# `.Class` in an S3 method: the class vector itself when the first class has the method, NULL for
# the default method, and otherwise the remaining classes with the whole vector as `previous`.
show_class <- function(x) UseMethod("show_class")
show_class.default <- function(x) print(.Class)
show_class.b <- function(x) {
  print(.Class)
  NextMethod()
}
show_class.numeric <- function(x) print(.Class)

show_class(structure(1, class = "a"))
show_class(structure(1, class = c("a", "b")))
show_class(structure(1, class = c("b", "c")))
show_class(1)
show_class(1L)
show_class(matrix(1))
show_class("text")

# Internal generics and group generics dispatch through the same lookup.
length.wide <- function(x) {
  print(.Class)
  99L
}
length(structure(list(), class = c("tall", "wide")))
Ops.unit <- function(e1, e2) {
  print(.Class)
  print(.Generic)
  unclass(e1) + unclass(e2)
}
structure(1, class = c("metre", "unit")) + 1

# A method that changes `.Class` changes only its own copy.
show_class.d <- function(x) {
  .Class <- c(.Class, "extra")
  attr(.Class, "previous") <- NULL
  print(.Class)
  NextMethod()
}
obj <- structure(1, class = c("c", "d"))
show_class(obj)
class(obj)
