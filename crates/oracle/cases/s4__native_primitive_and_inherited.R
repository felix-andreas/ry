# setMethod on a primitive, the first time and again, and inherited single-argument dispatch.
setClass("Box", slots = c(items = "list"))
setMethod("length", "Box", function(x) length(x@items))
b <- new("Box", items = list(1, 2, 3))
length(b)
isGeneric("length")
setClass("Bag", slots = c(n = "numeric"))
setMethod("length", "Bag", function(x) as.integer(x@n))
length(new("Bag", n = 7))
length(b)
length(1:4)
showMethods("length", classes = c("Box", "Bag"))

setMethod("[", "Box", function(x, i, ...) new("Box", items = x@items[i]))
length(b[2:3])

r <- tryCatch(setMethod("length", "numeric", function(x) 0L), error = function(e) conditionMessage(e))
cat(r, "\n")
length(c(1, 2))

setGeneric("area", function(shape) standardGeneric("area"))
setClass("Shape", representation("VIRTUAL"))
setClass("Rect", contains = "Shape", slots = c(w = "numeric", h = "numeric"))
setClass("Square", contains = "Rect")
setMethod("area", "Rect", function(shape) shape@w * shape@h)
setMethod("area", "ANY", function(shape) NA)
s <- new("Square", w = 2, h = 2)
area(s)
area(s)
m <- getMethod("area", "Rect")
sel <- selectMethod("area", "Square")
identical(body(sel), body(m))
is(sel@target)
as.character(sel@target)
sel@defined
area("x")

setClass("Base", slots = c(v = "numeric"))
setClass("Derived", contains = "Base")
setMethod("initialize", "Base", function(.Object, ...) {
  .Object <- callNextMethod(.Object, ...)
  if (length(.Object@v) == 0L) .Object@v <- 42
  .Object
})
new("Derived")@v
new("Derived", v = 1)@v
existsMethod("initialize", "Derived")
hasMethod("initialize", "Derived")
