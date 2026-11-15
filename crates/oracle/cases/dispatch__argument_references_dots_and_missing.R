# S3 dispatch with reference arguments, `...` through lapply/vapply, missing() by name.
gen <- function(x, ...) UseMethod("gen")
gen.default <- function(x, scale = 1, ...) x * scale
gen.foo <- function(x, scale = 2, ...) { y <- NextMethod(); y + length(list(...)) }
gen.keep <- function(x, ...) function() x
gen.sub <- function(x, ...) substitute(x)
gen.miss <- function(x, a, ...) c(missing(a), nargs())
caller <- function(v, s) gen(v, s)
print(caller(3, 4))
print(caller(structure(3, class = "foo"), 4))
k <- function(v) { f <- gen(structure(v, class = "keep")); v <- NULL; f }
print(unclass(k(7)()))
print((function(z) gen(structure(z, class = "sub")))(1))
print((function(z, w) gen(structure(z, class = "miss"), w))(1))
print((function(z, w) gen(structure(z, class = "miss")))(1))
apply_dots <- function(xs, ...) lapply(xs, function(x, a, b = 10) x + a + b, ...)
print(unlist(apply_dots(1:3, 5)))
print(unlist((function(n) apply_dots(1:3, n, b = n * 2))(4)))
vd <- function(xs, ...) vapply(xs, function(x, p) x^p, numeric(1), ...)
print((function(p) vd(c(2, 3), p))(3))
sd2 <- function(xs, ...) sapply(xs, paste, ...)
print((function(s) sd2(c("a", "b"), s, sep = "-"))("z"))
keepers <- function(xs, ...) lapply(xs, function(x, e) function() e + x, ...)
fs <- (function(q) { r <- keepers(1:2, q); q <- 100; r })(5)
print(c(fs[[1]](), fs[[2]]()))
m1 <- function(a, b) { g <- function() missing(b); c(missing(a), g()) }
print(m1(1)); print(m1(1, 2))
m2 <- function(m, i, j) m[i, j]
mm <- matrix(1:6, 2)
print(m2(mm, 1)); print(m2(mm, , 2)); print(m2(mm, 2, 3))
m3 <- function(x, ...) { h <- function() missing(..1); c(missing(x), h(), ...length()) }
print(m3()); print(m3(1, 2))
m4 <- function(a, b = a) { force(b); eval(quote(missing(b))) }
print(m4(1)); print(m4(1, 2))
m5 <- function(a) m6(a); m6 <- function(b) eval(quote(missing(b)))
print(m5()); print(m5(1))
dd <- function(...) do.call("sum", list(...)); print((function(a, b) dd(a, b, 3))(1, 2))
pp <- function(x, ...) UseMethod("pp"); pp.default <- function(x, ...) list(...)
print(length((function(a) pp(1, a, a + 1))(2)))
tt <- function(x, ...) UseMethod("tt")
tt.default <- function(x, n = stop("needed"), ...) if (x > 0) x else n
print((function(a) tt(a))(2))
print(tryCatch((function(a) tt(a))(-1), error = function(e) conditionMessage(e)))
print(format(structure(1.5, class = "bar"), nsmall = 2))
print.bar <- function(x, ...) cat("<bar", unclass(x), ">\n")
print(structure(2, class = "bar"))
toString(structure(list(1, 2), class = "baz"))
