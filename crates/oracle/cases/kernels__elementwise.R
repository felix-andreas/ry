# Every element-wise kernel on NA, NaN, recycling, attributes and integer overflow, called
# directly and from compiled code.
quiet <- function(expr) withCallingHandlers(expr, warning = function(w) {
  cat("warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning")
})
d <- c(NA, NaN, 1.5, -2, 0, Inf)
e <- c(NaN, NA, -0.5, 3, 0, -Inf)
n <- c(NA, 2147483647L, -2147483647L, 7L, 0L, -3L)
o <- c(1L, 1L, -1L, NA, 0L, 2L)
b <- c(TRUE, NA, FALSE, TRUE, NA, FALSE)
ops <- list(`+` = `+`, `-` = `-`, `*` = `*`, `/` = `/`, `^` = `^`, `%%` = `%%`, `%/%` = `%/%`,
  `==` = `==`, `!=` = `!=`, `<` = `<`, `>` = `>`, `<=` = `<=`, `>=` = `>=`)
pairs <- list(dd = list(d, e), ed = list(e, d), nn = list(n, o), dn = list(d, n), nb = list(n, b),
  bb = list(b, rev(b)), ds = list(d, 2), sd = list(NA_real_, e), ns = list(n, 2L),
  short = list(d, c(1, NA)))
for (op in names(ops)) for (pair in names(pairs)) {
  cat(op, pair, ": ")
  print(quiet(ops[[op]](pairs[[pair]][[1]], pairs[[pair]][[2]])))
}
compiled <- function(a, b) list(a + b, a - b, a * b, a / b, a > b, a == b)
for (k in 1:3) result <- quiet(compiled(d, e))
print(result)
for (k in 1:3) result <- quiet(compiled(n, o))
print(result)

# Unary minus, logic and negation.
print(-d); print(-n); print(quiet(-c(-2147483647L, NA)))
print(b & rev(b)); print(b | rev(b)); print(!b); print(!n); print(!d)
print(xor(b, rev(b)))

# Attributes: names, dim and dimnames come from the operands the way GNU R takes them.
m <- matrix(c(1, NA, NaN, 4), 2, dimnames = list(c("a", "b"), c("x", "y")))
print(m + 1); print(1:2 * m); print(m > 2); print(-m); print(!m)
named <- c(p = 1L, q = NA, r = 3L)
print(named + 1L); print(c(1, 2, 3) + named); print(named * 2147483647L)
print(quiet(c(1, 2, 3) + c(1, 2)))
print(structure(1:4, class = "myclass") + 1L)

# Math functions on NA, NaN, Inf and integers.
for (f in c("abs", "sign", "sqrt", "floor", "ceiling", "trunc", "exp", "expm1", "log1p",
  "log2", "log10", "cos", "sin", "tan", "cospi", "sinpi", "tanpi", "acos", "asin", "atan",
  "cosh", "sinh", "tanh", "acosh", "asinh", "atanh", "gamma", "lgamma", "digamma", "trigamma")) {
  cat(f, ": ")
  print(quiet(get(f)(c(d, -1, 0.5, 2))))
  print(typeof(quiet(get(f)(n))))
}

# Reductions.
for (v in list(d, e, n, o, b, c(1.5, 2.5), 1:10, c(2147483647L, 1L), numeric(), logical())) {
  print(quiet(sum(v))); print(quiet(mean(v))); print(any(v > 0)); print(all(v > 0))
  print(sum(v, na.rm = TRUE)); print(mean(v, na.rm = TRUE))
}
print(sprintf("%.17g", sum(c(0.1, 0.2, 0.3, 1e16, -1e16))))
print(sprintf("%.17g", mean(c(0.1, 0.2, 0.3))))

# `%%` and `%/%` on vectors keep integers integer, and a double `%%` that loses accuracy warns
# even when its result reuses an operand.
mod <- function(a, b) list(a %% b, a %/% b, (a * 1) %% b, (a + 0L) %/% b)
for (i in 1:3) str(mod(c(-7L, 7L, NA, 0L, 5L, -5L), c(3L, -3L, 2L)))
for (i in 1:3) str(mod(c(5.5, -5.5, NaN, Inf, 1, 0), c(2, -2, 0)))
loses <- function(x) withCallingHandlers((x * 1) %% 3,
  warning = function(w) { cat("warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") })
for (i in 1:3) print(loses(c(1e300, 7)))

# `x <- f(x, ...)` may write the result into x's own storage; nothing else that sees x's old value
# may change, and a shared scalar such as TRUE never does.
x <- c(1, 2); print(list(x, x <- x + 1))
x <- c(1, 2); print(c(x, {x <- x * 2; 0}))
y <- c(1L, 2L); print(list(y, y <- y + 1L))
z <- c(1, 2); f <- function(a) { z <<- z + 1; a }; print(f(z))
g <- function() { x <- c(1, 2); list(x, x <- x + 1) }; print(g())
h <- function() { x <- c(1, 2); for (i in 1:3) x <- x + 1; x }; print(h())
k <- function() { x <- c(1, 2); p <- function() x; x <- x * 3; c(p(), x) }; print(k())
e <- c(0.5, 1); w <- e; e <- exp(e); print(w); print(e)
l <- c(TRUE, NA); m <- l; l <- l == TRUE; print(m)
u <- c(1, 2); v <- u; u <- u + 1; print(v); print(u)
a <- c(1, -2, NA); b <- a; a <- -a; print(a); print(b)
a <- c(1L, -2L, NA); a <- -a; print(a); a <- abs(a); print(a)
l <- c(TRUE, FALSE, NA); m <- l; l <- !l; print(l); print(m)
l <- c(TRUE, FALSE, NA); l <- l & c(TRUE, TRUE, FALSE); print(l); l <- l | NA; print(l)
t1 <- 1 > 0; t1 <- !t1; print(t1); print(TRUE); print(2 > 1)
t2 <- 1 > 0; t2 <- t2 & FALSE; print(TRUE); t3 <- 1 > 0; t3 <- t3 == FALSE; print(TRUE)
q <- c(0.5, 4); q <- sqrt(q); print(q); q <- log2(q); print(q)
n <- c(-1, 4); n <- withCallingHandlers(sqrt(n), warning = function(w) { cat("w:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") }); print(n)
s <- c(a = 1, b = 2); s <- s + 1; print(s); s <- exp(s); print(s)
i <- c(2147483647L, 1L); i <- withCallingHandlers(i + 1L, warning = function(w) { cat("w:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") }); print(i)
p <- c(1, 2, 3, 4); p <- p > 2; print(p); p <- p == c(TRUE, FALSE); print(p)
r <- c(1L, 2L); r <- r / 2L; print(r); r <- r %% 0.3; print(r)
fn <- function() { v <- c(1, 2, 3); w <- v; v <- -v; v <- exp(v); v <- v > 0.1; v <- !v; list(v, w) }; print(fn()); print(fn())
