# loess: interpolated and direct surfaces, degrees 0 to 2, one to three predictors, robust
# fitting, exact and approximate statistics, parametric and drop.square terms, and predict with
# standard errors. Every printed number is a checksum of the exact bit pattern of the doubles.
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(x), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
h <- function(...) cat(bits(c(...)), "\n")
report <- function(f, nd = NULL) {
  h(f$fitted)
  h(f$enp, f$one.delta, f$two.delta, f$trace.hat, f$s)
  if (!is.null(nd)) {
    p <- predict(f, nd, se = TRUE)
    h(p$fit, p$se.fit, p$residual.scale, p$df)
    h(predict(f, nd))
  }
}

set.seed(1)
y <- rnorm(200); x <- seq_len(200)
cat("one predictor, interpolate\n"); report(loess(y ~ x))
cat("one predictor, direct\n"); report(loess(y ~ x, control = loess.control(surface = "direct")))
cat("symmetric, degree 1\n"); report(loess(y ~ x, family = "symmetric", degree = 1, span = 0.3))
for (degree in 0:2) {
  cat("direct degree", degree, "\n")
  report(loess(y ~ x, degree = degree, control = loess.control(surface = "direct")))
}

x2 <- sort(round(runif(60) * 10, 1))
y2 <- sin(x2) + rnorm(60) / 3
for (degree in 0:2) {
  cat("irregular x, degree", degree, "\n")
  report(loess(y2 ~ x2, degree = degree, span = 0.5, control = loess.control(surface = "direct")))
}

set.seed(7)
n <- 120
x1 <- runif(n); x2 <- runif(n); x3 <- runif(n)
y <- sin(3 * x1) + x2^2 + rnorm(n) / 5
dat <- data.frame(y = y, x1 = x1, x2 = x2, x3 = x3)
nd <- data.frame(x1 = c(.2, .5, .7, .35), x2 = c(.3, .6, .1, .9), x3 = c(.5, .5, .2, .8))
cat("two predictors\n"); report(loess(y ~ x1 + x2, dat), nd)
cat("degree 1\n"); report(loess(y ~ x1 + x2, dat, degree = 1), nd)
cat("degree 0\n"); report(loess(y ~ x1 + x2, dat, degree = 0, span = 0.6), nd)
cat("three predictors\n"); report(loess(y ~ x1 + x2 + x3, dat, span = 0.9), nd)
cat("symmetric\n"); report(loess(y ~ x1 + x2, dat, family = "symmetric"), nd)
cat("exact statistics\n"); report(loess(y ~ x1 + x2, dat, statistics = "exact"), nd)
cat("approximate statistics\n")
report(loess(y ~ x1 + x2, dat, statistics = "approximate", control = loess.control(trace.hat = "approximate")), nd)
cat("direct\n"); report(loess(y ~ x1 + x2, dat, control = loess.control(surface = "direct")), nd)
cat("direct symmetric\n")
report(loess(y ~ x1 + x2, dat, family = "symmetric", control = loess.control(surface = "direct")), nd)
cat("drop.square\n"); report(loess(y ~ x1 + x2, dat, drop.square = "x2"), nd)
cat("parametric\n"); report(loess(y ~ x1 + x2, dat, parametric = "x2"), nd)
cat("parametric and drop.square\n"); report(loess(y ~ x1 + x2, dat, parametric = "x2", drop.square = "x2"), nd)
cat("no normalize\n"); report(loess(y ~ x1 + x2, dat, normalize = FALSE), nd)
cat("span above 1\n"); report(loess(y ~ x1 + x2, dat, span = 1.5), nd)
cat("fine cells\n"); report(loess(y ~ x1 + x2, dat, control = loess.control(cell = 0.05)), nd)
cat("weights\n"); report(loess(y ~ x1 + x2, dat, weights = runif(n) + .5), nd)
nd1 <- data.frame(x1 = c(.1, .5, .9))
cat("one predictor, data frame\n"); report(loess(y ~ x1, dat), nd1)
cat("symmetric exact\n"); report(loess(y ~ x1, dat, family = "symmetric", statistics = "exact"), nd1)
cat("loess.smooth and lowess\n"); h(loess.smooth(x1, y)$y, lowess(x1, y)$y)
cat("loess.smooth robust\n"); h(loess.smooth(x1, y, degree = 1, family = "symmetric", evaluation = 20)$y)
xt <- rep(1:10, 12); yt <- sin(xt) + rnorm(120) / 4
cat("ties\n"); report(loess(yt ~ xt), data.frame(xt = c(2.5, 5, 7.7)))
cat("small span\n")
print(tryCatch(loess(y ~ x1, dat, span = 0.05), warning = function(w) conditionMessage(w)))
cat("singular\n")
xs <- c(rep(1, 30), 2:11); ys <- rnorm(40)
r <- withCallingHandlers(loess(ys ~ xs, span = 0.5), warning = function(w) {
  cat("warning:", conditionMessage(w), "\n")
  invokeRestart("muffleWarning")
})
h(fitted(r))
