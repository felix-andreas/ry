# Rmath routines rewritten clean room: the gamma quantile, the noncentral chi-squared density,
# the geometric quantile, and the densities built on the deviance and Stirling error terms.
p <- c(1e-300, 1e-10, 0.001, 0.1, 0.5, 0.9, 0.999, 1 - 1e-10)
for (a in c(0.5, 1, 2.5, 10, 100, 1e4)) {
  print(signif(qgamma(p, a), 12))
  print(signif(qgamma(p, a, scale = 3, lower.tail = FALSE), 12))
  print(signif(qgamma(log(p), a, log.p = TRUE), 12))
}
print(qgamma(c(0, 1, NaN, -1, 2), 2))
print(qgamma(0.5, c(0, Inf, -1)))
print(qgamma(1e-300, 0.5))
print(signif(qchisq(c(0.01, 0.5, 0.95), c(1, 3, 30)), 12))
print(qgeom(c(0, 1e-10, 0.1, 0.5, 0.9, 0.999, 1), 0.2))
print(qgeom(pgeom(0:40, 0.2), 0.2))
print(qgeom(pgeom(0:40, 0.37, lower.tail = FALSE), 0.37, lower.tail = FALSE))
print(qgeom(c(-5, -0.1, -1e-10), 0.01, log.p = TRUE))
print(qgeom(0.5, c(0, 1, 1.5, NaN)))
x <- c(0.5, 1, 3, 10)
for (df in c(1, 2.5, 10)) for (ncp in c(0.5, 5, 20)) {
  print(signif(dchisq(x, df, ncp = ncp), 10))
  print(signif(dchisq(x, df, ncp = ncp, log = TRUE), 10))
}
print(dchisq(c(-1, 0, Inf, NaN), 3, ncp = 2))
print(dchisq(0, 1, ncp = 2))
print(signif(dbinom(0:10, 10, 0.3), 14))
print(signif(dbinom(c(0, 500, 1000, 4999), 1e4, 0.5, log = TRUE), 14))
print(signif(dpois(c(0, 3, 15, 16, 50, 1000), c(0.5, 2.5, 15.5, 16, 80.2, 1000)), 13))
print(signif(dt(c(-3, 0, 0.5, 7), c(1, 15.5, 35.5, 501)), 14))
print(signif(dgamma(c(0.5, 7, 100, 1e4), c(0.5, 16, 81, 1e4)), 13))
print(signif(dnbinom(c(0, 3, 12, 100), size = c(0.5, 15.5, 40, 1000), prob = 0.3), 13))
