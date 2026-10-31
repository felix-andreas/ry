setClass("Vec", representation(x = "numeric", y = "numeric"),
  prototype(x = 0, y = 0),
  validity = function(object) {
    if (length(object@x) != length(object@y)) "x and y lengths differ" else TRUE
  })
v <- new("Vec", x = c(1, 2, 3), y = c(4, 5, 6))
v@x
new("Vec")@y
isVirtualClass("Vec")
isS4(v)
is(v, "Vec")
v
tryCatch(new("Vec", x = 1, y = c(1, 2)), error = function(e) conditionMessage(e))
v@y <- c(7, 8, 9)
v@y
slot(v, "x") <- c(0, 0, 0)
v@x
slotNames("Vec")
tryCatch(v@z, error = function(e) conditionMessage(e))
tryCatch(v@x <- "a", error = function(e) conditionMessage(e))
tryCatch(new("Vec", z = 1), error = function(e) conditionMessage(e))
tryCatch(new("Nope"), error = function(e) conditionMessage(e))
w <- v
w@x <- c(9, 9, 9)
v@x
identical(new("Vec"), new("Vec"))
setClass("Shape", representation("VIRTUAL"))
isVirtualClass("Shape")
tryCatch(new("Shape"), error = function(e) conditionMessage(e))
setClass("Circle", contains = "Shape", representation(r = "numeric"))
setValidity("Circle", function(object) if (object@r < 0) "negative radius" else TRUE)
tryCatch(new("Circle", r = -1), error = function(e) conditionMessage(e))
cc <- new("Circle", r = 1)
cc@r <- -5
tryCatch(validObject(cc), error = function(e) conditionMessage(e))
is(cc, "Shape")
extends("Circle", "Shape")
setClass("WithProto", representation(n = "numeric", s = "character"),
  prototype(n = 42, s = "default"))
new("WithProto")
setClass("Celsius", contains = "numeric")
temperature <- new("Celsius", c(20, 25.5))
temperature
temperature@.Data
temperature + 1
mean(temperature)
is(temperature, "numeric")
attributes(new("WithProto"))
setClass("Base", representation(id = "integer"))
setClass("Derived", contains = "Base", representation(label = "character"))
setMethod("initialize", "Base", function(.Object, ...) {
  .Object <- callNextMethod(.Object, ...)
  if (length(.Object@id) == 0) .Object@id <- 99L
  .Object
})
d <- new("Derived", label = "hi")
d@id
d@label
initialize(d, id = 2L)@id
is(d)
