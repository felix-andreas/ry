# new() starts each object from the class prototype. Changing a slot of one object, in any of
# the ways R allows, leaves the prototype and every other object as they were.
setClass("Inner", slots = c(v = "numeric"))
setClass("Outer", slots = c(series = "numeric", items = "list", inner = "Inner",
  env = "environment", m = "matrix"),
  prototype = prototype(series = c(1, 2, 3), items = list(a = 1, b = list(c = 2)),
    inner = new("Inner", v = 10), env = new.env(), m = matrix(1:4, 2)))

first <- new("Outer")
second <- new("Outer")
first@series[2] <- 20
first@items[["a"]] <- 100
first@items$b$c <- 200
first@inner@v[1] <- 30
first@m[1, 1] <- 0L
slot(first, "series")[3] <- 33
attr(first, "series")[1] <- 11
first@series
str(first@items)
first@inner@v
first@m
second@series
str(second@items)
second@inner@v
second@m
proto <- getClass("Outer")@prototype
attr(proto, "series")
str(attr(proto, "items"))
attr(proto, "inner")@v
attr(proto, "m")
new("Outer")@series

# An environment slot is a reference: every object shares the prototype's environment.
assign("hits", 1, envir = first@env)
exists("hits", envir = second@env)
identical(first@env, new("Outer")@env)

# Changes made inside a function, through a copy bound to another name, and to a slot value
# fetched first and assigned back.
bump <- function(object) {
  object@series[1] <- -1
  object@items[["a"]] <- -1
  object
}
third <- bump(new("Outer"))
third@series
new("Outer")@series
copy <- third
copy@series[1] <- 99
third@series
values <- second@items
values$a <- 5
second@items <- values
second@items$a
new("Outer")@items$a

# initialize methods that assign slots before and after callNextMethod().
setClass("Seeded", slots = c(series = "numeric", name = "character"),
  prototype = prototype(series = c(5, 6), name = "x"))
setMethod("initialize", "Seeded", function(.Object, ...) {
  .Object@series[1] <- 0
  .Object <- callNextMethod(.Object, ...)
  .Object@series[2] <- .Object@series[2] + 1
  .Object
})
a <- new("Seeded", name = "a")
b <- new("Seeded")
a@series
b@series
a@series[2] <- 50
b@series
attr(getClass("Seeded")@prototype, "series")

# A prototype slot holding an S4 object whose own slot is changed in place.
setClass("Wrap", slots = c(inner = "Inner"), prototype = prototype(inner = new("Inner", v = c(1, 2))))
w1 <- new("Wrap")
w1@inner@v[2] <- 9
w1@inner@v
new("Wrap")@inner@v

# A data part that is a list or a vector, changed through the object itself.
setClass("Bag", contains = "list", prototype = prototype(list(1, list(2))))
bag <- new("Bag")
bag[[1]] <- 10
bag[[2]][[1]] <- 20
unclass(bag)
unclass(new("Bag"))
setClass("Series", contains = "numeric", prototype = prototype(c(1, 2, 3)))
series <- new("Series")
series[2] <- 0
series@.Data
new("Series")@.Data
