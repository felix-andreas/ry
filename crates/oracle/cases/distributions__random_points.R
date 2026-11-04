# 600 pseudo-random parameter points across the continuous and discrete families, with one
# checksum of the exact bit pattern per function, tail and scale.
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(x), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
u <- function(i, k) (sin(i * 12.9898 + k * 78.233) * 43758.5453) %% 1
cols <- list()
for (i in 1:600) {
  a <- exp(10 * u(i, 1) - 6); b <- exp(10 * u(i, 2) - 6)
  x <- u(i, 3); p <- u(i, 4)^(1 + 3 * u(i, 5)); lp <- log(p)
  s <- exp(8 * u(i, 6) - 3)
  for (lt in c(TRUE, FALSE)) for (lg in c(FALSE, TRUE)) {
    pp <- if (lg) lp else p
    v <- suppressWarnings(c(
      pbeta = pbeta(x, a, b, lower.tail = lt, log.p = lg), qbeta = qbeta(pp, a, b, lower.tail = lt, log.p = lg),
      pgamma = pgamma(s * x * 10, a, scale = s, lower.tail = lt, log.p = lg), qgamma = qgamma(pp, a, scale = s, lower.tail = lt, log.p = lg),
      pf = pf(x * 5, a, b, lower.tail = lt, log.p = lg), qf = qf(pp, a, b, lower.tail = lt, log.p = lg),
      pt = pt(x * 6 - 3, a, lower.tail = lt, log.p = lg), qt = qt(pp, a, lower.tail = lt, log.p = lg),
      pchisq = pchisq(x * 50, a, lower.tail = lt, log.p = lg), qchisq = qchisq(pp, a, lower.tail = lt, log.p = lg),
      ppois = ppois(floor(a * 20), s, lower.tail = lt, log.p = lg), qpois = qpois(pp, s * 30, lower.tail = lt, log.p = lg),
      pbinom = pbinom(floor(x * 40), 40, p, lower.tail = lt, log.p = lg), qbinom = qbinom(pp, 40, x, lower.tail = lt, log.p = lg),
      pnbinom = pnbinom(floor(x * 40), a, p, lower.tail = lt, log.p = lg), qnbinom = qnbinom(pp, a, x, lower.tail = lt, log.p = lg),
      dgamma = dgamma(x * 10, a, scale = s, log = lg), dpois = dpois(floor(a * 20), s, lg),
      dbeta = dbeta(x, a, b, log = lg), dt = dt(x * 6 - 3, a, log = lg),
      dnbinom = dnbinom(floor(x * 40), a, p, log = lg), dbinom = dbinom(floor(x * 40), 40, p, log = lg)))
    key <- paste0("lower.tail=", lt, " log=", lg)
    cols[[key]] <- rbind(cols[[key]], v)
  }
}
for (key in names(cols)) {
  cat(key, "\n")
  for (name in colnames(cols[[key]])) cat(sprintf("  %-8s %s\n", name, bits(cols[[key]][, name])))
}
