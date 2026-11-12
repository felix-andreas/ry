# oracle: requires R6
# R6 objects are environments with a class vector, so `self$x` and `obj$method()` go through `$`
# on a classed object, and a `$` method defined for one of its classes takes over.
Counter <- R6::R6Class("Counter",
  public = list(
    n = 0,
    add = function(by = 1) {
      self$n <- self$n + by
      invisible(self)
    },
    get = function() self$n
  )
)
counter <- Counter$new()
for (i in 1:5) counter$add(i)
print(counter$get())
print(counter$n)
print(class(counter))

# A `$` method for the R6 class, defined after objects exist, then removed.
`$.Counter` <- function(x, name) {
  if (identical(name, "n")) "intercepted" else get(name, envir = x)
}
print(counter$n)
print(counter$get())
rm(`$.Counter`)
print(counter$n)

# An inheriting class reaches the parent's methods through `super$`.
Loud <- R6::R6Class("Loud", inherit = Counter,
  public = list(add = function(by = 1) {
    super$add(by * 2)
    invisible(self)
  })
)
loud <- Loud$new()
for (i in 1:3) loud$add(i)
print(loud$get())
print(class(loud))

# A plain classed list with a `$` method in a function's frame and in the global environment.
box <- structure(list(v = 1), class = "box")
peek <- function(b) {
  `$.box` <- function(x, name) "frame method"
  b$v
}
print(c(peek(box), box$v))
`$.box` <- function(x, name) "global method"
print(c(peek(box), box$v))
rm(`$.box`)
print(box$v)
