# `exp` and `log` over long double vectors, where ry runs four lanes at a time, and fused
# chains that write their result into the buffer the assigned variable already holds.
bits <- function(v) paste(sprintf("%a", v), collapse = " ")
edges <- c(NA, NaN, Inf, -Inf, 0, -0, 1e-310, -1e-310, 5e-324, .Machine$double.xmin,
           .Machine$double.xmax, 709.78, 709.79, -708.39, -745.13, -745.14, 2^-54, -2^-54,
           2^-55, 511.9, 512, -512, 1, 1 - 2^-53, 1 + 2^-52, 0.9375, 0.93749, 1.0649, 1.065,
           -1, -0.5, 2, 10, 1e300)
v <- rep(edges, 3)
cat(bits(exp(v)), "\n")
cat(bits(suppressWarnings(log(v))), "\n")
w <- seq(-30, 30, length.out = 997)
cat(bits(exp(w)), "\n")
cat(bits(log(abs(w) + 1e-3)), "\n")
cat(bits(log(seq(0.9, 1.1, length.out = 401))), "\n")

# The warning comes once per call, and `log` keeps the names and dimensions.
r <- withCallingHandlers(log(c(a = -1, b = 2, c = 3, d = 4, e = 5)),
  warning = function(w) { cat("warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") })
print(r)
m <- matrix(seq(0.5, 50, length.out = 100), 10)
print(dim(log(m))); print(dimnames(exp(provideDimnames(m)))[[1]][1:3])
print(log(c(NA_real_, rep(2, 99)))[1:3])
print(tryCatch({ options(warn = 2); log(rep(-1, 100)) }, error = function(e) conditionMessage(e)))
options(warn = 0)

# A chain assigned to a variable it also reads: each pass reads the old value and writes the
# new one into the same buffer.
f <- function(x) { for (i in 1:4) x <- exp(-x^2 / 2) * 0.5 + x; x }
cat(bits(f(seq(-2, 2, length.out = 200))), "\n")
g <- function(x) { y <- x; for (i in 1:3) y <- y * 2 - x; y }
cat(bits(g(seq(1, 100, length.out = 150))), "\n")

# The old value stays visible to everything that captured it before the assignment.
h <- function(x) {
  y <- x + 0
  keep <- y
  for (i in 1:3) y <- exp(y) * 0.5
  c(sum(keep), sum(y))
}
print(h(seq(0, 1, length.out = 300)))
k <- function(x) {
  y <- numeric(300)
  both <- function(a, b) c(sum(a), sum(b))
  both(y, { y <- exp(x) * 2 })
}
print(k(seq(0, 1, length.out = 300)))
p <- function(x) {
  y <- x * 1
  e <- environment()
  for (i in 1:3) y <- abs(y - 0.5) * 2
  c(sum(get("y", e)), sum(x))
}
print(p(seq(0, 1, length.out = 300)))

# A locked binding refuses the assignment after the chain has run.
q <- function(x) {
  y <- x * 1
  lockBinding("y", environment())
  tryCatch({ y <- exp(x) * 2; "assigned" }, error = function(e) conditionMessage(e))
}
for (i in 1:3) print(q(seq(0, 1, length.out = 300)))

# Integer chains can overflow halfway, so they never write into the old buffer.
z <- function(n) { y <- seq_len(300); for (i in 1:2) y <- y * n + 1L; y[c(1, 300)] }
for (i in 1:3) print(suppressWarnings(z(10000L)))

# `x^2` squares with `x * x`, which quiets NA's payload, and division by a power of two
# gives the same bits as multiplying by its reciprocal.
u <- rep(c(NA, NaN, -0, 3, 1e-310, 1e308), 20)
for (r in list(u^2, u^3, u^0.5, u / 2, u / 1024, u / 3, -u / 2, 2^u))
  print(writeBin(r[1:6], raw(), endian = "big"))
