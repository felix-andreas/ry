# Base functions with a native fast-path prologue, called from closures that run in slot frames
# and called with their own formals missing, defaulted, passed on or given through `...`. The
# prologue reads the formals from the slots and returns through the frame without making its
# environment. Each runs a few times so compiled code runs it too.
f <- function(x) setdiff(x, 1)
g <- function(a, b = a * 2) intersect(a, b)
h <- function(...) stopifnot(...)
s <- function(v) structure(v, class = "tag", note = "n")
k <- function(x, ...) structure(x, ...)
p <- function() { y <- 3; stopifnot(y == 3); format(pi * y) }
q <- function(n) Sys.getenv(n, unset = "none")
w <- function(x) { if (missing(x)) "missing" else format(x, nsmall = 2) }
for (round in 1:3) {
  print(tryCatch(f(), error = function(e) conditionMessage(e)))
  print(f(c(1, 2, 3)))
  print(g(1:5))
  print(h(TRUE, c(TRUE, TRUE)))
  print(tryCatch(h(TRUE, FALSE), error = function(e) conditionMessage(e)))
  print(unclass(s(1:3)))
  str(k(1:2, names = c("a", "b")))
  str(k(1:2))
  print(p())
  print(q("RY_DOES_NOT_EXIST"))
  print(Sys.setenv(RY_PROLOGUE = "x")); print(Sys.getenv("RY_PROLOGUE"))
  print(w()); print(w(1))
}
