# A function compiled before its first call keeps its locals in slots, and a name it keeps
# there shadows the base function of that name for its nested closures and for S3 dispatch.
show <- function(tag, value) cat(tag, ":", format(value), "\n")

f1 <- compiler::cmpfun(function() {
  paste <- function(...) "local"
  g <- function() paste("a")
  g()
})
show("local function, nested call", f1())
show("second call", f1())

f2 <- compiler::cmpfun(function() {
  nchar <- function(...) "local"
  sapply("abc", function(s) nchar(s))
})
show("through sapply", f2())

f3 <- compiler::cmpfun(function() {
  basename <- function(...) "local"
  h <- function() { k <- function() basename("a/b"); k() }
  h()
})
show("two closures deep", f3())

f4 <- compiler::cmpfun(function(dirname = function(...) "local") {
  g <- function() dirname("a/b")
  g()
})
show("a formal", f4())
show("a formal, supplied", f4(function(...) "supplied"))

f5 <- compiler::cmpfun(function() {
  for (tolower in list(function(...) "local")) {
    g <- function() tolower("A")
    result <- g()
  }
  result
})
show("a loop variable", f5())

f6 <- compiler::cmpfun(function() {
  g <- function() toupper("a")
  before <- g()
  toupper <- function(...) "local"
  c(before, g())
})
show("bound after the closure exists", f6())

f7 <- compiler::cmpfun(function() {
  format.shadow_a <- function(x, ...) "local method"
  format(structure(1, class = "shadow_a"))
})
show("a base generic", f7())

f8 <- compiler::cmpfun(function() {
  print.shadow_b <- function(x, ...) cat("local print method\n")
  print(structure(1, class = "shadow_b"))
  invisible(NULL)
})
f8()

generic <- function(x, ...) UseMethod("generic")
generic.default <- function(x, ...) "default"
f9 <- compiler::cmpfun(function() {
  generic.shadow_c <- function(x, ...) "local method"
  generic(structure(1, class = "shadow_c"))
})
show("a user generic", f9())

f10 <- compiler::cmpfun(function() {
  length.shadow_d <- function(x) 99L
  length(structure(list(), class = "shadow_d"))
})
show("an internal generic", f10())

# The same without the compiler package: the third call runs compiled, and only it binds the name.
f11 <- function(flag) {
  if (flag) trimws <- function(...) "local"
  g <- function() trimws(" a ")
  g()
}
show("unbound", f11(FALSE))
show("unbound", f11(FALSE))
show("bound on the third call", f11(TRUE))
show("unbound again", f11(FALSE))

f12 <- function(flag) {
  if (flag) rev.shadow_e <- function(x) "local method"
  rev(structure(1:3, class = "shadow_e"))
}
show("no method", f12(FALSE))
show("no method", f12(FALSE))
show("a method on the third call", f12(TRUE))
