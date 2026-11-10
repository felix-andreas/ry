# Batched draws must produce exactly GNU R's stream for every generator and normal method:
# whole batches, recycled parameters, parameters that skip the draw, and the generator state
# left behind for the next call.
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(x), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
show17 <- function(x) cat(sprintf("%.17g", x), "\n")
kinds <- c("Wichmann-Hill", "Marsaglia-Multicarry", "Super-Duper", "Mersenne-Twister",
           "L'Ecuyer-CMRG")
normals <- c("Inversion", "Box-Muller", "Kinderman-Ramage")
for (kind in kinds) for (normal in normals) {
  suppressWarnings(RNGkind(kind, normal, "Rejection"))
  set.seed(2024)
  cat(kind, normal, "\n")
  print(bits(rnorm(1000)))
  print(bits(rnorm(777, 5, 0.5)))
  print(bits(runif(1000)))
  print(bits(runif(333, -2, 7)))
  print(bits(rexp(500, 3)))
  show17(rnorm(3))
  print(sample(20))
  print(sample(1e6, 5))
  print(sample(5, 10, replace = TRUE))
  print(sample(4, 8, replace = TRUE, prob = c(0.1, 0.2, 0.3, 0.4)))
  print(bits(.Random.seed))
}
RNGkind("Mersenne-Twister", "Inversion", "Rounding")
set.seed(7)
print(sample(10))
print(bits(sample(1e5, 1000, replace = TRUE)))
RNGkind("default", "default", "default")
# Batches that straddle the twister's 624-word blocks, one draw at a time against in bulk.
set.seed(99)
a <- c(rnorm(1), rnorm(311), rnorm(624), rnorm(1250), runif(1), runif(623), runif(625))
set.seed(99)
b <- c(rnorm(2186), runif(1249))
identical(a, b)
print(bits(a))
# Draws stay in order across a call boundary: the rest of the batch continues the stream.
set.seed(5)
x <- rnorm(1e5)
print(bits(x))
show17(c(runif(2), rnorm(2)))
# Recycled parameters, and parameters for which no deviate is drawn.
set.seed(11)
show17(rnorm(9, c(0, 10, 100), c(1, 0.1)))
show17(suppressWarnings(rnorm(8, c(0, NA, Inf, -Inf, 1, 1, 1, NaN), c(1, 1, 1, 1, 0, -1, Inf, 1))))
show17(suppressWarnings(runif(6, c(0, 1, 2, NA, 5, 0), c(1, 1, Inf, 1, 4, 1))))
show17(rnorm(4, 2, 0))
show17(runif(4, 3, 3))
show17(rnorm(3))
rnorm(c(1, 2, 3))
rnorm(2L)
runif(TRUE)
rnorm(0)
rnorm(3, numeric(0))
rnorm(NA)
rnorm(-1)
runif(2, "a")
str(rnorm(2, mean = c(a = 1, b = 2)))
# A copy of .Random.seed keeps its value while the generator moves on.
set.seed(3)
saved <- .Random.seed
kept <- list(.Random.seed)
f <- function(seed) { force(seed); runif(2); seed }
inside <- f(.Random.seed)
runif(3)
identical(saved, kept[[1]])
identical(saved, inside)
identical(saved, .Random.seed)
.Random.seed <- saved
show17(runif(3))
set.seed(3); show17(runif(3))
