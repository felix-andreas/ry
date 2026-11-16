# An argument passed on through `...` is shared with the caller, so a callee that modifies
# it changes a copy and `..1` or `list(...)` still read the original.
gg <- function(v) { v[1] <- 99; v }
f1 <- function(...) { a <- gg(...); ..1 }
print(f1(c(1, 2)))
f2 <- function(...) { a <- gg(...); list(...)[[1]] }
print(f2(c(1, 2)))
f3 <- function(x) { a <- gg(x); x }
print(f3(c(1, 2)))
f4 <- function(...) { a <- gg(..1); ..1 }
print(f4(c(1, 2)))
f5 <- function(...) { environment(); a <- gg(...); ..1 }
print(f5(c(1, 2)))
g2 <- function(v) { v[[1]] <- 99; v }
f6 <- function(...) { a <- g2(...); ..1 }
print(f6(list(1, 2)))
g3 <- function(v) { names(v) <- c("a", "b"); v }
f7 <- function(...) { a <- g3(...); ..1 }
print(f7(c(1, 2)))
g4 <- function(v, w) { v[1] <- 99; w[1] <- 98; c(v, w) }
f8 <- function(...) { a <- g4(...); c(..1, ..2) }
print(f8(c(1, 2), c(3, 4)))
f9 <- function(...) { a <- gg(...); b <- gg(...); ..1 }
print(f9(c(1, 2)))
h1 <- function(...) f1(...)
print(h1(c(1, 2)))
a <- function(x) UseMethod("a")
a.k <- function(x) { x[1] <- 99; NextMethod() }
a.default <- function(x) unclass(x)
print(a(structure(c(1, 2), class = "k")))
b <- function(x) { b1(x); x }
b1 <- function(y) { y[1] <- 99; y }
print(b(c(1, 2)))
d <- function(...) { r <- lapply(list(...), function(v) { v[1] <- 99; v }); ..1 }
print(d(c(1, 2)))
e <- function(...) { r <- (function(v) { v[[1]] <- 0; v })(...); list(...)[[1]] }
print(e(list(5, 6)))
m <- function(x) { x[1] <- 99; sys.function() ; x }
print(m(c(1, 2)))
s3 <- function(x, ...) UseMethod("s3")
s3.default <- function(x, ...) { x[2] <- 0; c(x, ...) }
w <- function(...) { r <- s3(...); list(r, ..1) }
print(w(c(1, 2), 3))
dd <- function(...) { g <- function(v) { attr(v, "k") <- 1; v }; r <- g(...); attributes(..1) }
print(dd(c(1, 2)))
mm <- function(...) { g <- function(v) { v[2] <- 7L; v }; r <- g(...); r2 <- g(...); c(r, ..1) }
print(mm(1:3))
gd <- function(v) { v$a <- 99; v }
fd <- function(...) { r <- gd(...); ..1 }
print(fd(list(a = 1)))
gl <- function(v) { levels(v) <- c("x", "y"); v }
fl <- function(...) { r <- gl(...); ..1 }
print(fl(factor(c("a", "b"))))
gm <- function(v) { dim(v) <- c(2L, 2L); v }
fm <- function(...) { r <- gm(...); ..1 }
print(fm(1:4))
gs <- function(v) { v[2] <- 0L; v }
fs <- function(...) { r <- lapply(1:2, function(i) gs(...)); ..1 }
print(fs(1:3))
gw <- function(v) { v[["a"]][1] <- 0; v }
fw <- function(...) { r <- gw(...); ..1 }
print(fw(list(a = c(1, 2))))
gg <- function(v) { v[1] <- 99; v }
fn <- function(...) { r <- gg(..1); ..1 }
print(fn(c(1, 2)))
fr <- function(...) { r <- do.call(gg, list(...)); ..1 }
print(fr(c(1, 2)))
fa <- function(...) { args <- list(...); r <- gg(args[[1]]); args[[1]] }
print(fa(c(1, 2)))
