# ppr(): projection pursuit regression with the super smoother and both spline smoothers, every
# optimization level, fixed spans and the bass control, two responses with response weights,
# observation weights, predictions on new data, and the printed summary.
rock1 <- within(rock, { area1 <- area / 10000; peri1 <- peri / 10000 })
full <- function(x) print(x, digits = 15)
show <- function(fit) {
  print(summary(fit))
  full(fit$alpha); full(fit$beta); full(fit$gofn); full(fit$edf)
  full(fit$yb); full(fit$ys); full(fitted(fit))
}
fit <- ppr(log(perm) ~ area1 + peri1 + shape, data = rock1, nterms = 2, max.terms = 5)
show(fit)
full(predict(fit, rock1[c(3, 17, 40), ]))
new <- data.frame(area1 = c(0.5, 0.05, 1.2), peri1 = c(0.2, 0.3, 0.1), shape = c(0.2, 0.3, 0.25))
full(predict(fit, new))
full(fit$smod)
show(ppr(log(perm) ~ area1 + peri1 + shape, data = rock1, nterms = 2, max.terms = 5,
         sm.method = "spline"))
show(ppr(log(perm) ~ area1 + peri1 + shape, data = rock1, nterms = 2, max.terms = 5,
         sm.method = "gcvspline", gcvpen = 2))
show(ppr(log(perm) ~ area1 + peri1 + shape, data = rock1, nterms = 1, max.terms = 3,
         sm.method = "spline", df = 3))
for (level in 0:3) {
  f <- ppr(log(perm) ~ area1 + peri1 + shape, data = rock1, nterms = 2, max.terms = 4,
           optlevel = level)
  full(f$alpha); full(f$gofn); full(fitted(f)[1:6])
}
full(ppr(log(perm) ~ area1 + peri1 + shape, data = rock1, nterms = 2, span = 0.5)$gofn)
full(ppr(log(perm) ~ area1 + peri1 + shape, data = rock1, nterms = 2, bass = 5)$gofn)
weighted <- ppr(log(perm) ~ area1 + peri1 + shape, data = rock1, nterms = 2, max.terms = 3,
                weights = rep(1:3, 16))
show(weighted)
x <- as.matrix(rock1[c("area1", "peri1", "shape")])
y <- cbind(lp = log(rock1$perm), sh = rock1$shape * 10)
two <- ppr(x, y, nterms = 2, max.terms = 4, ww = c(1, 2))
show(two)
full(predict(two, x[1:4, ]))
two_spline <- ppr(x, y, nterms = 2, max.terms = 3, sm.method = "gcvspline")
show(two_spline)
out <- capture.output(traced <- ppr(x, y[, 1], nterms = 1, max.terms = 2, sm.method = "spline",
                                    trace = TRUE))
print(length(out)); print(head(out, 4))
# The smoothing parameter's search reaches a lambda where pow(16, y) and exp2(4 * y) differ
# in the last bit.
set.seed(1)
xs <- matrix(rnorm(24), 12, 2)
ys <- sin(xs[, 1]) + xs[, 2]^2 + rnorm(12, sd = 0.3)
ws <- runif(12) + 0.5
small <- ppr(xs, ys, nterms = 1, max.terms = 3, sm.method = "gcvspline", optlevel = 3,
             weights = ws)
print(sprintf("%a", c(small$alpha, small$gofn[1])))
