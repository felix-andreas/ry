# rbinom with a probability and a size per draw: small means (inversion), large means (BTPE),
# probabilities above one half, and invalid parameters, which give NA with a warning.
set.seed(11)
p <- c(runif(20), 0, 1, 0.5)
print(rbinom(23, 1, p))
print(rbinom(23, 7, p))
print(rbinom(23, 200, p))
print(rbinom(6, c(1, 10, 100, 1000), c(0.2, 0.9)))
print(rbinom(5, c(3, 2.5, -1, NA, 4), c(0.3, 0.3, 0.3, 0.3, 1.2)))
print(rbinom(4, 2^31, c(0.1, 0.5)))
print(.Random.seed[1:4])
