# S3 method lookup order: the calling environment, then the defining environment's top, then the
# generic's registered methods, then the search path. One method name is looked up from several
# top environments in turn, and methods change between calls.
show <- function(x) UseMethod("show")
show.default <- function(x) "default"
obj <- structure(1, class = "widget")

# Define, call, mask in the global environment, call, remove, call, register, call.
ns <- new.env()
ns$.packageName <- "fakepkg"
ns$.__S3MethodsTable__. <- new.env()
environment(show) <- ns
print(show(obj))
show.widget <- function(x) "global"
print(show(obj))
registerS3method("show", "widget", function(x) "registered", envir = ns)
print(show(obj))
rm(show.widget)
print(show(obj))
show.widget <- function(x) "global again"
print(show(obj))
rm(show.widget)
print(show(obj))

# The calling environment comes first: a method in a caller's frame, at two depths.
outer <- function(x) {
  show.widget <- function(x) "outer frame"
  inner <- function(x) show(x)
  c(inner(x), show(x))
}
print(outer(obj))
print(show(obj))

# The same method name from a top environment that is not the global one: code evaluated in
# `ns` finds the method bound there before the registered one, and global code does not.
ns$show.widget <- function(x) "ns frame"
in_ns <- function(x) show(x)
environment(in_ns) <- ns
for (i in 1:2) print(c(in_ns(obj), show(obj)))
rm("show.widget", envir = ns)
print(c(in_ns(obj), show(obj)))

# A method bound beside a generic in a function's frame is not found, because lookup reaches the
# defining environment at its top, not at the frame that holds the generic.
make <- function() {
  gen <- function(x) UseMethod("gen")
  gen.widget <- function(x) "beside the generic"
  gen
}
gen <- make()
print(tryCatch(gen(obj), error = function(e) "no method"))
gen.widget <- function(x) "global gen"
print(gen(obj))
rm(gen.widget)

# Inheritance: a method for the second class, then one for the first appears and goes.
two <- structure(1, class = c("leaf", "widget"))
registerS3method("show", "widget", function(x) "registered widget", envir = ns)
print(show(two))
show.leaf <- function(x) paste("leaf >", NextMethod())
print(show(two))
show.widget <- function(x) "global widget"
print(show(two))
rm(show.leaf, show.widget)
print(show(two))

# A binding of the method name that is not a function is passed over, and turning it into a
# function is seen at once.
show.widget <- 3
print(show(obj))
show.widget <- function(x) "now a function"
print(show(obj))
rm(show.widget)

# Base generics and internal dispatch from base code: a method defined after the first call.
stamp <- structure(list(), class = "stamp")
print(length(stamp))
length.stamp <- function(x) 99L
print(length(stamp))
format.stamp <- function(x, ...) "formatted stamp"
print(format(stamp))
rm(length.stamp, format.stamp)
print(length(stamp))

# A `$` method on a classed list, defined after the first access, then masked and removed.
rec <- structure(list(a = 1), class = "rec")
print(rec$a)
`$.rec` <- function(x, name) paste("field", name)
print(rec$a)
print(rec$b)
`$<-.rec` <- function(x, name, value) {
  y <- unclass(x)
  y[[name]] <- value * 10
  structure(y, class = "rec")
}
rec$a <- 2
print(unclass(rec)$a)
rm(`$.rec`, `$<-.rec`)
print(rec$a)

# conditionMessage methods, which base's message path looks up by name.
cond <- structure(class = c("custom", "message", "condition"),
  list(message = "plain\n", call = NULL))
withCallingHandlers(message(cond), message = function(m) {
  cat("handler saw:", conditionMessage(m))
  invokeRestart("muffleMessage")
})
conditionMessage.custom <- function(c) "custom message\n"
withCallingHandlers(message(cond), message = function(m) {
  cat("handler saw:", conditionMessage(m))
  invokeRestart("muffleMessage")
})
rm(conditionMessage.custom)
print(conditionMessage(cond))
