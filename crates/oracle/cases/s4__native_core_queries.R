setClass("Point", representation(x = "numeric", y = "numeric", label = "ANY"))
setClass("Point3", contains = "Point", representation(z = "numeric"))
setClassUnion("NumOrChar", c("numeric", "character"))
setClass("Holder", representation(value = "NumOrChar", point = "Point", n = "integer"))
setClass("Checked", representation(n = "numeric"),
  validity = function(object) if (length(object@n) > 1) "n must be a scalar" else TRUE)
setClass("CheckedChild", contains = "Checked", representation(m = "numeric"),
  validity = function(object) {
    calls <- sys.calls()
    if (any(object@m < 0)) paste("negative m, called from", deparse(calls[[length(calls) - 1L]])) else TRUE
  })
setClass("Abstract", representation("VIRTUAL"))
setClass("Tagged", representation(tag = "character"), prototype(tag = "none"))
setMethod("initialize", "Tagged", function(.Object, ...) {
  .Object <- callNextMethod(.Object, ...)
  .Object@tag <- toupper(.Object@tag)
  .Object
})

p <- new("Point", x = 1, y = 2, label = structure(list(), class = "someS3"))
p@x; p@y; class(p@label)
q <- new("Point3", x = 1, y = 2, z = 3)
q@z
new("Point")@label
h <- new("Holder", value = "a", point = q, n = 2L)
class(h@point)
h2 <- new("Holder", value = 1, n = 3L)
h2@value
tryCatch(new("Holder", n = 1.5), error = function(e) conditionMessage(e))
tryCatch(new("Holder", value = TRUE), error = function(e) conditionMessage(e))
tryCatch(new("Point", x = "a"), error = function(e) conditionMessage(e))
tryCatch(new("Point", nope = 1), error = function(e) conditionMessage(e))
tryCatch(new("Point", x = 1, x = 2), error = function(e) conditionMessage(e))
tryCatch(new("Abstract"), error = function(e) conditionMessage(e))
tryCatch(new("NoSuchClass"), error = function(e) conditionMessage(e))
tryCatch(new("Point", x = stop("boom")), error = function(e) deparse(conditionCall(e)))
tryCatch(new("Checked", n = 1:2), error = function(e) deparse(conditionCall(e)))
tryCatch(new("Checked", n = 1:2), error = function(e) conditionMessage(e))
tryCatch(new("CheckedChild", n = 1, m = -1), error = function(e) conditionMessage(e))
new("CheckedChild", n = 1, m = 2)@m
new("Tagged", tag = "abc")@tag
new("Tagged")@tag
new(getClass("Point"), x = 5)@x
xs <- 1:3
new("Point", x = xs, y = 2)@x
f <- function(cls, ...) new(cls, ...)
f("Point", y = 4)@y
tryCatch(new("Point", q), error = function(e) conditionMessage(e))
new("Point3", p, z = 9)@z

c3 <- new("Checked", n = 1)
c3@n <- c(1, 2)
validObject(c3, test = TRUE)
tryCatch(validObject(c3), error = function(e) conditionMessage(e))
validObject(q)
validObject(h, complete = TRUE)
withVisible(validObject(p))$visible
bad <- p
attr(bad, "y") <- NULL
validObject(bad, test = TRUE)

is(p, "Point"); is(q, "Point"); is(p, "Point3"); is(q, "ANY")
is(1, "numeric"); is(1L, "numeric"); is("a", "NumOrChar"); is(TRUE, "NumOrChar")
is(1L, "NumOrChar"); is(matrix(1), "matrix"); is(matrix(1), "array")
is(data.frame(), "data.frame"); is(data.frame(), "list"); is(data.frame(), "oldClass")
is(structure(1, class = "foo"), "foo"); is(structure(1, class = c("foo", "bar")), "bar")
is(factor("a"), "factor"); is(factor("a"), "integer")
is(q, getClass("Point")); is(p, getClass("Point3"))
is(p); is(1)
tryCatch(is(p, c("a", "b")), error = function(e) conditionMessage(e))
is(getClass("Point"), "classRepresentation")
is(sum, "function"); is(NULL, "NULL"); is(list(), "vector")

extends("Point3", "Point"); extends("Point", "Point3"); extends("numeric", "NumOrChar")
extends("integer", "NumOrChar"); extends("Point", "ANY"); extends(c("Point3", "x"), "Point")
extends(getClass("Point3"), "Point"); extends("Point3", "Point", fullInfo = TRUE)@distance
extends("integer", "numeric"); extends("integer", "numeric", maybe = FALSE)
extends("Point3"); extends("NoSuchClass", "Point")
extends("factor", "integer"); extends("data.frame", "oldClass")

table <- environment(getGeneric("initialize"))$.AllMTable
sort(grep("^(Point|Holder|Checked|Tagged)", names(table), value = TRUE))
sort(grep("Point|NumOrChar", names(environment(getGeneric("coerce"))$.MTable), value = TRUE))
