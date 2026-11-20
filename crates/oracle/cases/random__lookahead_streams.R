# A long batch with scalar parameters draws from the twister a block at a time, with normals
# computed ahead; batches end inside a block and across its end, a scalar draw follows each,
# and every stream and the final seed must match GNU R's. rsignrank and rwilcox reuse their
# state across draws, and dsignrank on one large n builds one count table.
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(x), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
draws <- list(
  pois500 = function(n) rpois(n, 500), pois20 = function(n) rpois(n, 20),
  pois5 = function(n) rpois(n, 5), geom = function(n) rgeom(n, 0.01),
  gamma3 = function(n) rgamma(n, 3), gamma05 = function(n) rgamma(n, 0.5),
  t5 = function(n) rt(n, 5), chisq = function(n) rchisq(n, 4),
  nbinom = function(n) rnbinom(n, 3, 0.2), beta = function(n) rbeta(n, 2, 3),
  exp = function(n) rexp(n), binom = function(n) rbinom(n, 100, 0.3),
  signrank = function(n) rsignrank(n, 30), wilcox = function(n) rwilcox(n, 20, 15)
)
set.seed(21)
for (name in names(draws)) {
  for (n in c(63, 64, 65, 700, 3000)) {
    cat(name, n, bits(draws[[name]](n)), bits(runif(1)), "\n")
  }
}
cat(.Random.seed[1:3], "\n")
set.seed(22)
invisible(runif(620))
cat(bits(rpois(100, 400)), bits(rgamma(100, 2)), bits(rnorm(1)), "\n")
for (kind in c("Kinderman-Ramage", "Box-Muller")) {
  RNGkind(normal.kind = kind)
  set.seed(23)
  cat(kind, bits(rpois(2000, 300)), bits(rgamma(2000, 3)), bits(rt(2000, 4)), "\n")
}
RNGkind(normal.kind = "default")
RNGkind("Wichmann-Hill")
set.seed(24)
cat(bits(rpois(2000, 300)), bits(rsignrank(500, 20)), "\n")
RNGkind("default")
set.seed(25)
print(rsignrank(5, c(0, 1, 2, NA, -1)))
print(suppressWarnings(rwilcox(6, c(0, 3, NA, -1, 2.7, 4), 3)))
print(rsignrank(3, 1e4))
print(signif(dsignrank(c(0, 10, 1000, 2500, 5050), 100), 12))
print(signif(psignrank(c(10, 1000, 2525), 100), 12))
print(qsignrank(c(0.01, 0.5, 0.99), 100))
# A batch that draws nothing leaves a spent block and a request to seed as they were.
set.seed(26)
x <- rpois(3000, 0); x <- rgamma(100, 0)
print(runif(2))
seed <- .Random.seed
seed[2] <- 625L
.Random.seed <- seed
x <- rpois(3000, 0)
print(.Random.seed[2])
print(bits(rpois(100, 400)))
print(.Random.seed[2])
