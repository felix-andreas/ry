# Inside a dispatched method, NextMethod continues .Generic whatever generic argument it gets.
f <- function(x, ...) UseMethod("f"); f.a <- function(x, ...) NextMethod(x); f.default <- function(x, ...) "def"
print(f(structure(numeric(0), class="a")))
print(f(structure(1:3, class="a")))
g <- function(x, ...) UseMethod("g"); g.a <- function(x, ...) NextMethod("g", x, extra = 1); g.default <- function(x, ...) list(...)
print(g(structure(1, class="a")))
h.a <- function(x) NextMethod("h"); h <- function(x) UseMethod("h"); h.b <- function(x) "b"
print(h(structure(1, class=c("a","b"))))
k <- function(x) UseMethod("k"); k.a <- function(x) NextMethod("zzz"); k.default <- function(x) "kdef"
print(tryCatch(k(structure(1, class="a")), error=conditionMessage))
