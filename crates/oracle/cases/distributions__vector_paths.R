# The vectorized d/p/q loops: a long first operand with scalar parameters (the path where
# qnorm's central region runs across SIMD lanes), recycling of operands of different lengths,
# NA, NaN and infinite operands, flags given as logicals, integers, doubles and strings, and
# the attributes the result takes. Exact bit patterns are checksummed.
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(x), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
show17 <- function(x) cat(sprintf("%.17g", x), "\n")
set.seed(42)
p <- c(runif(5000), runif(500, 0, 0.075), runif(500, 0.925, 1), 0, 1, 0.5, 0.075, 0.925,
       0.07499999999999999, 0.925000000000001, 1e-300, 1 - 1e-16, -0.1, 1.1, NA, NaN, Inf, -Inf)
for (lt in c(TRUE, FALSE)) {
  print(bits(suppressWarnings(qnorm(p, lower.tail = lt))))
  print(bits(suppressWarnings(qnorm(p, 3, 0.25, lower.tail = lt))))
  print(bits(suppressWarnings(qnorm(p, -1e5, 1e-3, lower.tail = lt))))
}
print(bits(suppressWarnings(qnorm(log(p), log.p = TRUE))))
print(bits(suppressWarnings(qnorm(log(p), 1, 2, lower.tail = FALSE, log.p = TRUE))))
show17(qnorm(c(0.075, 0.0750000000000001, 0.925, 0.9249999999999999, 0.5, 0.31, 0.69)))
show17(qnorm(c(0.2, 0.8), lower.tail = FALSE))
# Parameters that leave the central region's fast path: zero, negative and infinite scales,
# infinite and NA means.
show17(suppressWarnings(qnorm(c(0.1, 0.5, 0.9), 0, 0)))
show17(suppressWarnings(qnorm(c(0.1, 0.5, 0.9), 0, -1)))
show17(suppressWarnings(qnorm(c(0.1, 0.5, 0.9), 0, Inf)))
show17(suppressWarnings(qnorm(c(0.1, 0.5, 0.9), Inf, 1)))
show17(qnorm(c(0.1, 0.5, 0.9), NA, 1))
show17(qnorm(c(0.1, 0.5, 0.9), NaN, 1))
qnorm(c(0.2, NA, NaN, 0.9))
qnorm(c(-1, 0.5))
x <- seq(-40, 40, length.out = 4001)
print(bits(dnorm(x)))
print(bits(dnorm(x, 1, 3, log = TRUE)))
print(bits(pnorm(x)))
print(bits(pnorm(x, lower.tail = FALSE, log.p = TRUE)))
print(bits(dexp(abs(x), 2)))
print(bits(pgamma(abs(x), 2.5, 3)))
print(bits(dbinom(0:60, 60, 0.3)))
print(bits(ppois(0:60, 12.5)))
# Recycling: every operand cycles with its own length.
show17(dnorm(1:7, c(0, 1, 2), c(1, 2)))
show17(pnorm(c(-1, 0, 1), 0:4, c(1, 2, 3, 4)))
show17(qnorm(c(0.1, 0.9), c(0, 10, 20), 1))
show17(dbinom(0:5, c(5, 10), c(0.2, 0.5, 0.7)))
show17(phyper(1:6, c(5, 10), 7, c(3, 4, 5)))
# The Bessel and polygamma ports differ from GNU R in the last ulp, so these check recycling
# at 12 digits.
print(besselJ(1:5, c(0, 1)), digits = 12)
print(psigamma(c(0.5, 1, 2.5), 0:1), digits = 12)
# NA beats NaN, and a NaN produced from ordinary operands warns.
dnorm(c(1, NA, NaN, Inf, -Inf), c(0, 0, NA, 1, NaN))
pnorm(c(0, 1), c(NA, NaN))
dnorm(1:3, 0, -1)
pbeta(0.5, c(1, NA, -1), 2)
# Flags read as asInteger reads them.
dnorm(1, log = 1L)
dnorm(1, log = 2.7)
dnorm(1, log = "TRUE")
pnorm(1, lower.tail = c(FALSE, TRUE))
pnorm(1, log.p = NA)
qnorm(0.3, lower.tail = 0)
# Attributes come from the first operand as long as the result.
dnorm(c(a = 1, b = 2), 0, 1)
pnorm(matrix(1:4, 2), 0, 1)
qnorm(0.5, c(x = 1, y = 2), 1)
dnorm(structure(1:3, foo = "bar"), c(u = 0, v = 1, w = 2))
dnorm(integer(0), 0, 1)
dnorm(1:3, numeric(0))
dnorm(TRUE, 0L, 1L)
