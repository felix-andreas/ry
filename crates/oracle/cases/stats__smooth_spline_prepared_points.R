# smooth.spline prepares each point's knot interval and B-spline values once and reuses them
# for every trial smoothing parameter, which must leave the fit, the criterion and the
# leverages bit for bit equal to GNU R's under every criterion and control.
show <- function(f) {
  cat(sprintf("%.17g", c(f$df, f$lambda, f$spar, f$cv.crit, f$pen.crit, sum(f$y), sum(f$lev),
                         sum(f$fit$coef))), "\n")
}
set.seed(6)
x <- sort(runif(300, 0, 10))
y <- sin(x) + rnorm(300, sd = 0.3)
w <- runif(300, 0.5, 2)
show(smooth.spline(x, y))
show(smooth.spline(x, y, cv = TRUE))
show(smooth.spline(x, y, df = 5))
show(smooth.spline(x, y, w = w, spar = 0.4))
show(smooth.spline(x, y, lambda = 1e-4))
show(smooth.spline(x, y, all.knots = TRUE))
show(smooth.spline(c(1:8, 8), c(3, 1, 4, 1, 5, 9, 2, 6, 6)))
f <- smooth.spline(x, y)
cat(sprintf("%.17g", predict(f, c(-1, 0, x[1], 5, x[300], 11))$y), "\n")
cat(sprintf("%.17g", sum(predict(f, seq(0, 10, by = 0.37), deriv = 2)$y)), "\n")
cat(sprintf("%.17g", predict(f, seq(-1, 11, by = 0.5), deriv = 1)$y), "\n")
x2 <- runif(1000)
y2 <- 2 * x2 + rnorm(1000)
show(smooth.spline(x2, y2))
show(smooth.spline(x2, y2, df = 5))
