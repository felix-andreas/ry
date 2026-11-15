# validObject() with validity methods: the native slot check runs the R code after the slot
# loop, which calls the validity methods from validObject's frame.
caught <- function(expr) tryCatch(expr, error = function(e)
  list(message = conditionMessage(e), class = class(e), call = deparse(conditionCall(e))))

setClass("Record", slots = c(a = "character", b = "character", e = "integer"))
setValidity("Record", function(object) {
  if (length(object@a) != length(object@b)) "a and b differ in length" else TRUE
})
r <- new("Record", a = "x", b = "y", e = 1L)
r@a
str(caught(new("Record", a = c("x", "y"), b = "y")))
str(caught(validObject(r)))
r@b <- c("p", "q")
str(caught(validObject(r)))
validObject(r, test = TRUE)
r@b <- "q"
validObject(r, test = TRUE)
validObject(r, complete = TRUE)

# Several messages, a character vector from the validity function, and a non-TRUE logical.
setClass("Point", slots = c(x = "numeric", y = "numeric"))
setValidity("Point", function(object) {
  messages <- character()
  if (length(object@x) != 1) messages <- c(messages, "x must have length 1")
  if (length(object@y) != 1) messages <- c(messages, "y must have length 1")
  if (length(messages)) messages else TRUE
})
new("Point", x = 1, y = 2)
str(caught(new("Point", x = c(1, 2), y = c(3, 4))))
setClass("Flag", slots = c(v = "logical"), validity = function(object) object@v)
new("Flag", v = TRUE)
str(caught(new("Flag", v = FALSE)))
str(caught(new("Flag", v = NA)))

# Inherited validity: the superclass method runs first, on as(object, superclass).
setClass("Base", slots = c(id = "integer"),
  validity = function(object) {
    cat("Base validity on", class(object), "\n")
    if (length(object@id) && any(object@id < 0L)) "negative id" else TRUE
  })
setClass("Derived", contains = "Base", slots = c(label = "character"),
  validity = function(object) {
    cat("Derived validity on", class(object), "\n")
    if (length(object@label) > 1) "one label only" else TRUE
  })
d <- new("Derived", id = 1L, label = "a")
str(caught(new("Derived", id = -1L, label = "a")))
str(caught(new("Derived", id = 1L, label = c("a", "b"))))
setClass("Plain", contains = "Base", slots = c(z = "numeric"))
new("Plain", id = 2L, z = 1)
str(caught(new("Plain", id = -2L, z = 1)))

# What the validity function sees of its call and its caller.
setClass("Probe", slots = c(n = "numeric"), validity = function(object) {
  print(sys.call())
  print(class(sys.function()))
  frame <- parent.frame()
  print(sort(ls(frame)))
  print(get("Class", frame))
  print(get("i", frame))
  print(get("namei", frame))
  print(get("ok", frame))
  print(identical(get("sloti", frame), object@n))
  calls <- vapply(sys.calls(), function(call) deparse(call)[[1]], "")
  calls <- tail(calls, 4)
  print(calls[-2])
  print(nchar(calls[[2]]))
  TRUE
})
invisible(new("Probe", n = 3))

# Slot values the slot loop has to judge: a wrong class, a subclass, a union member and a
# virtual class.
setClass("Holder", slots = c(item = "Base", tag = "character"),
  validity = function(object) if (length(object@tag) != 1) "one tag" else TRUE)
h <- new("Holder", item = new("Base", id = 1L), tag = "t")
h2 <- new("Holder", item = d, tag = "t")
class(h2@item)
h2@item <- new("Base", id = 3L)
validObject(h2)
bad <- h
slot(bad, "item", check = FALSE) <- new("Point", x = 1, y = 1)
str(caught(validObject(bad)))
setClassUnion("NumberOrText", c("numeric", "character"))
setClass("Cell", slots = c(value = "NumberOrText"),
  validity = function(object) if (length(object@value) == 0) "empty cell" else TRUE)
new("Cell", value = 1)
new("Cell", value = "a")
str(caught(new("Cell", value = character())))
setClass("Shape", representation("VIRTUAL"))
setClass("Circle", contains = "Shape", slots = c(r = "numeric"))
setClass("Drawing", slots = c(shape = "Shape", name = "character"),
  validity = function(object) if (!nzchar(object@name)) "unnamed drawing" else TRUE)
new("Drawing", shape = new("Circle", r = 1), name = "c")
str(caught(new("Drawing", shape = new("Circle", r = 1), name = "")))

# initialize methods that call callNextMethod(), and new() with an unnamed superclass object.
setMethod("initialize", "Derived", function(.Object, ...) {
  .Object <- callNextMethod(.Object, ...)
  if (length(.Object@label) == 0) .Object@label <- "default"
  .Object
})
new("Derived", id = 4L)
str(caught(new("Derived", id = -4L)))
base <- new("Base", id = 5L)
new("Derived", base, label = "from base")
str(caught(new("Derived", new("Base", id = 6L), label = c("x", "y"))))

# What the slot loop leaves as `ok` when the last slot holds a subclass or a union member.
showOk <- function(object) {
  print(class(get("ok", parent.frame())))
  TRUE
}
setClass("Tail", slots = c(n = "numeric", item = "Base"), validity = showOk)
invisible(validObject(new("Tail", n = 1, item = d)))
setClass("UnionTail", slots = c(value = "NumberOrText"), validity = showOk)
invisible(validObject(new("UnionTail", value = 2)))
invisible(validObject(new("UnionTail", value = "b")))

# What a validity method finds in validObject's frame, for the shapes where the slot loop ends
# differently: a data part, no slots at all, an `ANY` slot, an S3 class, an extra attribute.
showFrame <- function(object) {
  frame <- parent.frame()
  print(sort(ls(frame)))
  for (name in c("i", "namei", "idx", "attrNames", "slotNames", "errors"))
    if (exists(name, frame, inherits = FALSE)) { cat(name, ": "); print(get(name, frame)) }
  TRUE
}
setClass("Amounts", contains = "numeric", slots = c(unit = "character"), validity = showFrame)
amounts <- new("Amounts", c(1, 2), unit = "kg")
invisible(validObject(amounts))
names(amounts) <- c("a", "b")
invisible(validObject(amounts))
setClass("Marker", representation("VIRTUAL"))
setClass("Bare", contains = "Marker", validity = showFrame)
invisible(validObject(new("Bare")))
setOldClass(c("stamp", "base"))
setClass("Mixed", slots = c(any = "ANY", old = "stamp"), validity = showFrame)
mixed <- new("Mixed", any = sum, old = structure(list(), class = c("stamp", "base")))
invisible(validObject(mixed))
attr(mixed, "extra") <- 1
invisible(validObject(mixed))

# Shapes the fast path leaves to the R body: a slot that is gone, `complete`, a basic value.
partial <- new("Tail", n = 1, item = d)
attr(partial, "n") <- NULL
str(caught(validObject(partial)))
invisible(validObject(new("Tail", n = 1, item = d), complete = TRUE))
invisible(validObject(new("Tail", n = 1, item = d), TRUE))
print(validObject(1L))

# A superclass reached through a conditional `setIs`, whose validity method runs only when
# the test holds.
setClass("Target", slots = c(v = "numeric"),
  validity = function(object) if (any(object@v < 0)) "bad Target" else TRUE)
setClass("Source", slots = c(w = "numeric"),
  validity = function(object) if (length(object@w) > 2) "long Source" else TRUE)
setIs("Source", "Target", test = function(object) length(object@w) == 1,
  coerce = function(from) new("Target", v = from@w),
  replace = function(from, value) { from@w <- value@v; from })
validObject(new("Source", w = 1))
print(validObject(new("Source", w = c(1, 2))))
source <- new("Source", w = 1)
source@w <- -1
str(caught(validObject(source, test = TRUE)))
source@w <- c(-1, -2, -3)
print(validObject(source, test = TRUE))
