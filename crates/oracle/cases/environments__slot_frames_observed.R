f <- function(x, y = x + 1) { z <- x * 2; g(z) + y }
g <- function(a) a + 1
print(f(1))
h <- function(n) { e <- environment(); e$n + 1 }
print(h(2))
k <- function(a, b) { ls() }
print(k(1, 2))
m <- function(a) { function() a }
print(m(5)())
p <- function(a, b) { missing(b) }
print(p(1))
q <- function(x) substitute(x)
print(q(a + b))
r <- function(x) { on.exit(cat("bye\n")); parent.frame() }
print(identical(r(1), globalenv()))
s <- function(...) { args <- list(...); length(args) }
print(s(1, 2, 3))
t1 <- function(x) { y <- 3; eval(quote(x + y)) }
print(t1(4))
u <- function(x) { v <- function() x <<- x + 1; v(); x }
print(u(1))
w <- function(x) sys.function()
print(w(1))
aa <- function(x) { e <- environment(); assign("zz", 9, e); zz + x }
print(aa(1))
bb <- function(x, y) { mget(c("x", "y")) }
str(bb(1, "a"))
cc <- function(x) UseMethod("cc")
cc.default <- function(x) { NextMethod; "default" }
cc.foo <- function(x) { paste("foo", NextMethod()) }
print(cc(structure(1, class = "foo")))
dd <- function(n) { total <- 0; for (i in seq_len(n)) total <- total + (function(v) v * 2)(i); total }
print(dd(100))
ee <- function(x) { tryCatch(stop("boom"), error = function(e) conditionMessage(e)) }
print(ee(1))
ff <- function(a) { delayedAssign("lazy", a * 2); lazy }
print(ff(21))
gg <- function(x) { e <- environment(); function() e }
print(environmentName(parent.env(gg(1)())))
hh <- function(x, y) { rm(x); exists("x", inherits = FALSE) }
print(hh(1, 2))
ii <- function(x) vapply(1:3, function(i) i + x, numeric(1))
print(ii(10))
jj <- function(x) { l <- as.list(environment()); names(l) }
print(jj(1))
