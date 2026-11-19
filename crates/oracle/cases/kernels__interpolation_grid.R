# approx() and spline evaluation find a point's knot interval through a grid of buckets over
# the knots when there are enough of both, and by bisection otherwise; both must give GNU R's
# interval for points on a knot, between tied knots, at both ends, outside the knots and NaN.
set.seed(5)
for (n in c(2, 15, 16, 17, 100, 5000)) {
  x <- sort(unique(round(runif(n, -10, 10), 2)))
  y <- rnorm(length(x))
  xo <- c(x, round(runif(3000, -12, 12), 3), NA, NaN, Inf, -Inf, x[1], x[length(x)])
  a <- approx(x, y, xout = xo, rule = 2)
  b <- approx(x, y, xout = xo, method = "constant", f = 0.3, rule = 1:2)
  cat(n, sprintf("%.17g", sum(a$y * seq_along(xo), na.rm = TRUE)),
      sprintf("%.17g", sum(b$y * seq_along(xo), na.rm = TRUE)), sum(is.na(a$y)), sum(is.na(b$y)), "\n")
  for (method in c("fmm", "natural", "periodic", "hyman")) {
    yy <- if (method == "periodic") c(y[-length(y)], y[1]) else if (method == "hyman") cumsum(abs(y)) else y
    if (length(x) < 2) next
    f <- splinefun(x, yy, method = method)
    cat(method, sprintf("%.17g", sum(f(xo) * seq_along(xo), na.rm = TRUE)), "\n")
  }
}
x <- c(rep(0, 10), 1:20, rep(20, 5), rep(25, 3))
y <- seq_along(x) * 1.5
v <- c(0, 0.5, 20, 20.5, 25, 3, 7.25, seq(-1, 26, by = 0.25))
print(.Call(stats:::C_Approx, x, y, v, 1L, NA_real_, NA_real_, 0, TRUE))
print(.Call(stats:::C_Approx, x, y, v, 2L, -1, -2, 0.4, FALSE))
print(approx(x, y, xout = c(0, 0.5, 21, 20.5, 3), ties = "ordered")$y)
