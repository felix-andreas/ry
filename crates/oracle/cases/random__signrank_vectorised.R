# Signed rank densities, probabilities and quantiles over many points of one n and of several,
# which share a count table within a call.
k <- 0:60
print(signif(dsignrank(k, 10), 12)); print(signif(dsignrank(k, c(10, 11)), 12))
print(signif(psignrank(k, 10), 12)); print(signif(psignrank(k, 12, lower.tail = FALSE, log.p = TRUE), 12))
print(qsignrank(c(0, 0.1, 0.5, 0.9, 1), 15)); print(qsignrank(seq(0.05, 0.95, 0.15), c(5, 20)))
print(dsignrank(c(2.5, -1, NA, 1000), 10)); print(dsignrank(3, c(0, NA, 3)))
invisible(NULL)
