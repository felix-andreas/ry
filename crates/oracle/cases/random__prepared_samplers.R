# rpois and rbinom prepare their setup once per parameter set, which must not change a draw:
# every branch of both algorithms, parameters that recycle and change between elements, the
# generators that call rpois with a fresh mean every draw, and the integer type of the counts.
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(x), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
set.seed(31)
for (mu in c(0, 1e-9, 0.5, 1, 2.5, 3, 7.3, 9, 9.999, 10, 10.5, 37, 1000, 1e6, 2^31)) {
  x <- rpois(3000, mu)
  cat(mu, typeof(x), bits(x), "\n")
}
print(rpois(5, 4))
print(suppressWarnings(rpois(6, c(NA, -1, Inf, NaN, 0, 3))))
print(bits(rpois(2000, c(0.3, 4, 15, 120))))
print(bits(rpois(2000, rep(c(2, 2, 2, 30, 30), length.out = 2000))))
print(bits(rpois(2000, seq(0.1, 50, length.out = 2000))))
for (size in c(0, 1, 5, 40, 60, 100, 1000, 123456)) for (prob in c(0, 0.01, 0.2, 0.5, 0.73, 0.99, 1)) {
  x <- rbinom(500, size, prob)
  cat(size, prob, typeof(x), bits(x), "\n")
}
print(rbinom(5, 2^31, 0.5))
print(suppressWarnings(rbinom(6, c(NA, 1.5, -1, Inf, 10, 10), c(0.5, 0.5, 0.5, 0.5, NaN, 2))))
print(bits(rbinom(3000, c(10, 200, 5000), c(0.1, 0.6))))
print(bits(rbinom(3000, 1:3000, 0.4)))
print(bits(rgeom(3000, 0.05)))
print(bits(rnbinom(3000, 3.5, 0.2)))
print(bits(rnbinom(3000, mu = 25, size = 2)))
print(bits(rchisq(2000, 3, ncp = 4)))
print(bits(rgamma(2000, c(0.4, 2, 30))))
print(bits(rt(1000, 5)))
print(bits(rbeta(1000, 0.5, 3)))
print(bits(rhyper(1000, 30, 20, 10)))
print(bits(rweibull(1000, 2, 3)))
print(bits(rlogis(1000)))
print(bits(rcauchy(1000)))
print(bits(rlnorm(1000)))
print(bits(rsignrank(500, 10)))
print(bits(rwilcox(500, 4, 6)))
print(bits(.Random.seed))
