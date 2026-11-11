# Index draws take their sixteen-bit chunks as integers: permutations, draws with replacement,
# the duplicate-redrawing sampler for a small sample of a large population (with a hash set of
# drawn values, and with a bitset once the sample is at least 1/128 of it), populations past
# the integer range (doubles), bit counts at multiples of 16, and weighted draws without
# replacement, for every generator and both sample kinds.
bits <- function(x) {
  r <- as.numeric(writeBin(as.double(x), raw()))
  sprintf("%.0f", sum(r * ((seq_along(r) %% 251) + 1)))
}
kinds <- c("Wichmann-Hill", "Marsaglia-Multicarry", "Super-Duper", "Mersenne-Twister",
           "L'Ecuyer-CMRG")
for (kind in kinds) for (sampler in c("Rejection", "Rounding")) {
  suppressWarnings(RNGkind(kind, "Inversion", sampler))
  set.seed(31)
  cat(kind, sampler, "\n")
  print(bits(sample(70000)))
  print(bits(sample(65536, 2000, replace = TRUE)))
  print(bits(sample(65537, 2000, replace = TRUE)))
  print(bits(sample.int(1e9, 3000)))
  print(bits(sample.int(2e7, 2e5)))
  print(bits(sample.int(20000001, 2e6)))
  print(sample.int(2^40, 3))
  print(sample.int(2^40 + 1, 3))
  print(sample.int(4e15, 3))
  print(bits(sample(3e9, 50, replace = TRUE)))
  print(bits(sample(200, 50, prob = rep(c(1, 2), 100))))
  print(bits(.Random.seed))
}
RNGkind("default", "default", "default")
