# d/p/q over every distribution, with both tails and log scales, at points that reach the
# extreme tails and the switches between series, continued-fraction and asymptotic code.
# Each line is a checksum of the exact bit pattern of every double, so a last-bit change fails.
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(x), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
P <- c(1e-300, 1e-100, 1e-20, 1e-10, 1e-5, .001, .01, .1, .25, .5, .75, .9, .99, .999, 1 - 1e-10)
LP <- log(P)
xs <- list(
  norm = c(-40, -10, -5, -3, -1.5, -.5, 0, .3, 1, 2, 4, 8, 38, 40),
  t = c(-1e3, -50, -10, -3, -1, -.1, 0, .3, 1, 2.5, 6, 20, 1e3, 1e6),
  chisq = c(0, 1e-5, .01, .5, 1, 2, 5, 10, 30, 100, 400, 1500, 1e4),
  f = c(0, 1e-4, .01, .3, 1, 2, 5, 20, 100, 1e3, 1e5),
  gamma = c(0, 1e-8, .01, .5, 1, 2, 5, 10, 30, 100, 400, 1500, 1e4),
  beta = c(0, 1e-8, .001, .05, .2, .5, .8, .99, 1 - 1e-9, 1),
  binom = c(-1, 0, 1, 2, 3, 5, 10, 25, 50, 100, 400, 1000, 5000),
  pois = c(-1, 0, 1, 2, 3, 5, 10, 25, 50, 100, 400, 1000, 5000),
  nbinom = c(-1, 0, 1, 2, 3, 5, 10, 25, 50, 100, 400, 1000, 5000),
  exp = c(-1, 0, 1e-9, .01, .5, 1, 2, 5, 30, 300, 800),
  weibull = c(-1, 0, 1e-9, .01, .5, 1, 2, 5, 30, 300, 800),
  lnorm = c(-1, 0, 1e-9, .01, .5, 1, 2, 5, 30, 300, 1e8),
  unif = c(-5, -1, 0, .2, .5, 1, 1.5, 3, 7, 11),
  hyper = c(-1, 0, 1, 2, 3, 5, 10, 25, 50),
  logis = c(-800, -50, -10, -3, -1, 0, .5, 2, 10, 50, 800),
  cauchy = c(-1e10, -1e3, -10, -1, -.1, 0, .3, 1, 5, 1e3, 1e10),
  signrank = c(-1, 0, 1, 2, 5, 10, 20, 50, 100, 300, 500, 1000, 5000),
  wilcox = c(-1, 0, 1, 2, 5, 10, 20, 50, 100, 300, 500, 1000, 5000)
)
params <- list(
  norm = list(c(0, 1), c(1, 2.5), c(-3, 1e-3), c(0, 1e5)),
  t = list(.5, 1, 2.5, 5, 30, 100, 1e5, Inf),
  chisq = list(.1, 1, 3, 10, 100, 1000),
  f = list(c(1, 1), c(3, 7), c(10, 2.5), c(50, 100), c(.5, 5), c(1e3, 1e4)),
  gamma = list(c(.01, 1), c(.5, 1), c(1, 2), c(2.5, 1), c(10, .5), c(100, 1), c(1e4, 1)),
  beta = list(c(.01, .5), c(.5, .5), c(1, 1), c(2, 3), c(10, 2.5), c(100, 200), c(.2, 5e3), c(1e3, 1e3)),
  binom = list(c(10, .3), c(100, .01), c(100, .5), c(1000, .1), c(5000, .5), c(50, .999), c(1, .5)),
  pois = list(.1, 1, 3.7, 10, 50, 400, 5000),
  nbinom = list(c(1, .5), c(3, .2), c(10, .5), c(2.5, .01), c(100, .9), c(1000, .5)),
  exp = list(1, .01, 5),
  weibull = list(c(.5, 1), c(1, 1), c(2, 3), c(10, .5)),
  lnorm = list(c(0, 1), c(1, .25), c(-2, 3)),
  unif = list(c(0, 1), c(-1, 2), c(3, 3.5)),
  hyper = list(c(10, 7, 8), c(30, 20, 40), c(5, 5, 5), c(100, 200, 50)),
  logis = list(c(0, 1), c(2, .5), c(-1, 20)),
  cauchy = list(c(0, 1), c(1, .01), c(-5, 100)),
  signrank = list(5, 10, 25, 60, 150),
  wilcox = list(c(4, 6), c(10, 12), c(30, 25), c(5, 50), c(50, 50))
)
for (dn in names(xs)) {
  d <- get(paste0("d", dn)); p <- get(paste0("p", dn)); q <- get(paste0("q", dn))
  for (ps in params[[dn]]) {
    tag <- paste0(dn, "(", paste(ps, collapse = ","), ")")
    dv <- pv <- qv <- numeric()
    for (lg in c(FALSE, TRUE))
      dv <- c(dv, suppressWarnings(do.call(d, c(list(xs[[dn]]), as.list(ps), list(log = lg)))))
    for (lt in c(TRUE, FALSE)) for (lg in c(FALSE, TRUE)) {
      pv <- c(pv, suppressWarnings(do.call(p, c(list(xs[[dn]]), as.list(ps), list(lower.tail = lt, log.p = lg)))))
      qv <- c(qv, suppressWarnings(do.call(q, c(list(if (lg) LP else P), as.list(ps), list(lower.tail = lt, log.p = lg)))))
    }
    cat(sprintf("%-26s d %-12s p %-12s q %s\n", tag, bits(dv), bits(pv), bits(qv)))
  }
}
