# A builtin that may dispatch, called from a compiled closure's frame on an object, looks its
# methods up from the closure's environment, and the frame is handed to a method only when one
# exists. Each function runs many times so its body compiles and runs on slots.

# Objects with no method on each path: `$`, an internal generic, `[<-`, `[[<-`, `$<-` and a
# replacement function.
plain <- function(n) {
  x <- structure(list(a = n, b = "z"), class = "no_methods")
  first <- x$a
  size <- length(x)
  x[["a"]] <- first + 1
  x["b"] <- "y"
  x$c <- TRUE
  names(x)[3] <- "d"
  attr(x, "tag") <- "t"
  list(first, size, unclass(x))
}
for (i in 1:30) r <- plain(i)
str(r)

# Methods that look at their caller's frame: parent.frame(), sys.function() and environment()
# must see the real calling frame and its locals.
`$.probe` <- function(x, name) {
  caller <- parent.frame()
  list(name = name, local = get("local_value", envir = caller), fn = is.function(sys.function()))
}
length.probe <- function(x) {
  caller <- parent.frame()
  caller$local_value * 10L
}
`[<-.probe` <- function(x, i, value) {
  assign("seen", i, envir = parent.frame())
  unclass_x <- unclass(x)
  unclass_x[[i]] <- value
  structure(unclass_x, class = "probe")
}
`names<-.probe` <- function(x, value) {
  attr(x, "renamed") <- value
  x
}
probing <- function(n) {
  local_value <- n
  seen <- NULL
  x <- structure(list(1, 2), class = "probe")
  field <- x$anything
  size <- length(x)
  x[2] <- "two"
  names(x) <- c("p", "q")
  list(field, size, seen, attr(x, "renamed"), environmentName(environment()) == "")
}
for (i in 1:30) r <- probing(i)
str(r)

# NextMethod from such a method reaches the default with the same arguments.
length.wrapped <- function(x) {
  inner <- NextMethod()
  inner + 100L
}
`$.wrapped` <- function(x, name) {
  paste("wrapped", NextMethod())
}
wrapping <- function(n) {
  x <- structure(list(v = as.character(n)), class = c("wrapped", "list"))
  c(length(x), x$v)
}
for (i in 1:30) r <- wrapping(i)
print(r)

# Group generics: Ops and Math methods, and a classed object without one.
Ops.money <- function(e1, e2) {
  structure(unclass(e1) + unclass(e2), class = "money", seen = .Generic)
}
grouped <- function(n) {
  m <- structure(n, class = "money")
  bare <- structure(n, class = "nothing_here")
  list(attr(m + m, "seen"), unclass(bare * 2), floor(bare + 0.5))
}
for (i in 1:30) r <- grouped(i)
str(r)

# A method bound in the calling frame itself, which only the frame's environment can find.
local_method <- function(n) {
  length.local_only <- function(x) 42L
  x <- structure(list(n), class = "local_only")
  length(x)
}
for (i in 1:30) r <- local_method(i)
print(r)

# S4 and Reference Class objects through `$`.
setClass("Holder", representation(value = "numeric"))
setMethod("$", "Holder", function(x, name) x@value * 2)
Account <- setRefClass("Account", fields = list(balance = "numeric"))
s4_fields <- function(n) {
  holder <- new("Holder", value = n)
  account <- Account$new(balance = n)
  account$balance <- account$balance + 1
  c(holder$anything, account$balance)
}
for (i in 1:30) r <- s4_fields(i)
print(r)
