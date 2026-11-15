# `[` and `[[` called from a function whose index is one of its formals, on classed and plain
# subjects. The method lookup comes before the index is evaluated, a missing index (even one
# missing in a caller) passes on as an empty argument, and a method gets a promise of the
# formal's name in the calling frame.
settings <- structure(list(alpha = 1, beta = "b"), class = "settings")
key <- function(s) vapply(names(s), function(field) {
  value <- s[[field]]
  paste(field, format(value))
}, "")
for (i in 1:40) r <- key(settings)
print(r)

# No method: errors and their calls match the primitive's.
item <- function(x, i) x[[i]]
show_error <- function(expr) tryCatch(expr, error = function(e) {
  cat("error:", conditionMessage(e), "\n")
  print(conditionCall(e))
})
show_error(item(settings, "gamma"))
show_error(item(settings, 5))
show_error(item(settings))
show_error(item(1:3))
show_error(item(1:3, 7))
print(item(settings, 2))

# A missing index, also when the caller's own argument is missing, selects everything.
pick <- function(x, i) x[i]
pass <- function(x, j) pick(x, j)
pass_item <- function(x, j) item(x, j)
print(pick(1:3))
print(pass(1:3))
print(names(pass(settings)))
show_error(pass_item(settings))
show_error(pass_item(1:3))
defaulted <- function(x, i = 2) x[i]
print(defaulted(1:3))
print(defaulted(settings))

# A class that gains a `[[` method after the first call dispatches to it from then on.
counter <- structure(list(a = 1), class = "counter")
for (i in 1:3) print(item(counter, "a"))
`[[.counter` <- function(x, i) {
  cat("method sees", deparse(substitute(i)), "\n")
  paste("counted", NextMethod())
}
for (i in 1:3) print(item(counter, "a"))
rm(`[[.counter`)
print(item(counter, "a"))

# The method forces the index late, and the index's side effect runs once, then.
lazy <- structure(list(a = 1, b = 2), class = "lazy")
`[[.lazy` <- function(x, i) {
  cat("before\n")
  value <- unclass(x)[[i]]
  cat("after\n")
  value
}
noisy <- function() { cat("index forced\n"); "b" }
twice <- function(x, i) c(x[[i]], x[[i]])
print(twice(lazy, noisy()))
never <- structure(list(), class = "never")
`[[.never` <- function(x, i) "ignored the index"
print(item(never, stop("not forced")))

# `[` from a slot frame on a classed subject with a method, and NextMethod from it.
`[.lazy` <- function(x, i) {
  cat("[.lazy sees", deparse(substitute(i)), missing(i), "\n")
  structure(NextMethod(), class = "lazy")
}
print(unclass(pick(lazy, 2)))
print(tryCatch(pass(lazy), error = function(e) conditionMessage(e)))

# A global `[[.default` catches every classed subject without its own method.
`[[.default` <- function(x, i) paste("default for", class(x)[1])
print(item(settings, "alpha"))
print(item(1:3, 2))
rm(`[[.default`)
print(item(settings, "alpha"))

# Internal dispatch uses the class attribute only: a matrix without one does not reach
# `[[.matrix`, a classed matrix with one class does reach its own method.
`[[.matrix` <- function(x, i) "matrix method"
m <- matrix(1:4, 2)
print(item(m, 3))
print(item(structure(m, class = "matrix"), 3))
rm(`[[.matrix`)

# A local function named like the method is found from the calling frame.
local_method <- function(x, i) {
  `[[.settings` <- function(x, i) paste("local", i)
  x[[i]]
}
print(local_method(settings, "alpha"))
print(item(settings, "alpha"))
