# Call operands of fused element-wise expressions: evaluated once, in R's order, with the
# operator lookups R makes before them, inside compiled closures (each runs 30 times).
counter <- 0
f <- function(v) { counter <<- counter + 1; v * 2 }
g <- function(x, y) {
  a <- f(x) * 2 + y
  b <- sum(x - mean(x) > 0)
  c <- exp(f(x)) / 3 - y
  d <- f(x) * f(y) + x
  list(a, b, c, d)
}
x <- c(1, 2, NA, 4); y <- c(0.5, NaN, 3, 4)
for (i in 1:30) r <- g(x, y)
print(r); print(counter)
# a redefined operator that never evaluates its operand
h <- function(x) { `+` <- function(e1, e2) 42; f(x) * 2 + x }
counter <- 0; for (i in 1:30) r2 <- h(x); print(r2); print(counter)
# the operand redefines the operator after the lookup
k <- function(x) { m <- function(v) { assign("*", function(a, b) "redefined", envir = parent.frame()); v }; z <- m(x) * 2 + 1; z }
for (i in 1:3) print(k(x))
# a call operand that errors
e <- function(x) tryCatch(stop("boom") * 2 + x, error = function(err) conditionMessage(err))
for (i in 1:30) r3 <- e(x); print(r3)
# warnings and integer overflow through a hoisted operand
w <- function(x) as.integer(x) * 2L + .Machine$integer.max
for (i in 1:30) r4 <- suppressWarnings(w(c(1, 2))); print(r4)
s <- function(x) sum(abs(x - median(x)) > 1)
for (i in 1:30) r5 <- s(c(1, 5, 9, 2)); print(r5)
# variables read before a call operand keep the value they had before it ran
gx <- c(1, 2, 3)
bump <- function() { gx <<- gx * 10; 1 }
t1 <- function() sum(gx - bump() > 0) + gx[1]
for (i in 1:3) print(t1())
lazy <- function(p) exp(p) * 2 - mean(c(p, 1))
for (i in 1:30) r6 <- lazy({ cat("forced\n"); c(0, 1) }); print(r6)
t2 <- function(x) x - mean(x) + x * 2
for (i in 1:30) r7 <- t2(c(1L, 5L, NA)); print(r7)
