# smooth.spline across its smoothing criteria and knot choices, its spar search path, predict
# with derivatives, and lowess. Every printed number is a checksum of the exact bit pattern.
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(x), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
h <- function(f) {
  cat(bits(c(f$df, f$lambda, f$spar, f$cv.crit, f$crit, f$pen.crit, f$iparms[3], f$y, f$lev)), "\n")
}
set.seed(3)
x <- sort(round(runif(80) * 10, 1)); y <- sin(x) + rnorm(80) / 3; w <- runif(80) + .5
h(smooth.spline(x, y))
h(smooth.spline(x, y, cv = TRUE))
h(smooth.spline(x, y, w = w))
h(smooth.spline(x, y, df = 6))
h(smooth.spline(x, y, spar = 0.7))
h(smooth.spline(x, y, lambda = 1e-4))
h(smooth.spline(x, y, all.knots = TRUE))
f <- smooth.spline(x, y, nknots = 12)
h(f)
cat(bits(c(predict(f, c(0, 1.5, 5, 9.9, 12))$y, predict(f, c(1.5, 5), deriv = 1)$y,
           predict(f, 3, deriv = 2)$y)), "\n")

f <- smooth.spline(x, y)
print(f$iparms)
cat(bits(c(f$spar, f$lambda, f$ratio, f$crit, f$df, f$w[1:3], f$yin[1:3])), "\n")
print(length(f$x)); print(f$fit$nk); cat(bits(f$fit$knot), "\n")
g <- smooth.spline(x, y, spar = f$spar)
cat(bits(c(g$spar, g$lambda, g$ratio, g$crit, g$df)), "\n")

cat("spar grid\n")
for (s in c(-0.35410197, 0.35410197, 0.79179607, 0.75502634, 0.59476594, 0.65684705, 0.69, 0.5, 1, 1.2)) {
  f <- smooth.spline(x, y, spar = s)
  cat(s, bits(c(f$crit, f$df, f$lambda, f$cv.crit, f$lev, f$y)), "\n")
}

f <- smooth.spline(x, y, spar = 0.5, keep.stuff = TRUE)
cat("kept internals\n")
cat(bits(f$fit$coef), bits(f$auxM$R), bits(f$auxM$XWX), bits(f$auxM$Sigma), bits(f$auxM$XWy), "\n")

cat("lowess\n")
set.seed(5)
xl <- runif(100); yl <- sin(6 * xl) + rnorm(100) / 4
for (fr in c(2/3, 0.2, 0.9)) for (it in c(0, 3)) for (delta in c(0, 0.01)) {
  l <- lowess(xl, yl, f = fr, iter = it, delta = delta * diff(range(xl)))
  cat(fr, it, delta, bits(c(l$x, l$y)), "\n")
}
l <- lowess(c(1, 2, 2, 3, 3, 3, 4, 5), c(1, 3, 2, 5, 4, 6, 7, 9))
cat(bits(c(l$x, l$y)), "\n")
