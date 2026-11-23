# rhyper prepares its setup once per parameter set, rpois with a large mean draws through the
# normal approximation's constants alone, and rgeom calls rpois with a fresh mean every draw;
# each must consume the generator exactly as GNU R does.
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(x), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
set.seed(7)
print(rhyper(20, 20, 30, 15))
print(rhyper(20, 2000, 3000, 1500))
print(rhyper(10, 5, 5, 10))
print(rhyper(10, 1e10, 1e10, 1))
print(rhyper(5, 3e9, 2, 2))
print(rhyper(12, c(20, 2000, 7), c(30, 3000, 1), c(15, 1500, 3, 8)))
print(suppressWarnings(rhyper(4, c(NA, 5), 3, 2)))
print(rgeom(20, c(0.01, 0.5, 0.9, 1)))
print(rpois(20, c(0.5, 12, 150, 1e6)))
print(rgeom(5, 1e-9))
for (parameters in list(c(20, 30, 15), c(2000, 3000, 1500), c(1, 1, 1), c(50, 5, 40), c(1e6, 1e6, 3e5))) {
  cat(parameters, bits(rhyper(4000, parameters[1], parameters[2], parameters[3])), "\n")
}
cat(bits(rgeom(5000, 0.001)), bits(rpois(5000, 50)), bits(rpois(5000, runif(5000, 0, 100))), "\n")
cat(.Random.seed[1:3], "\n")
