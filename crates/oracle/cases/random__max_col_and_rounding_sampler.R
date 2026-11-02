# max.col breaks ties at random from the session's generator, and the rounding sampler of
# R before 3.6.0 survives a switch to it and back.
set.seed(5)
m <- matrix(c(1, 1, 1, 1, 2, 2, NA, 0, 3), 3)
max.col(m)
max.col(m, "first")
max.col(m, "last")
max.col(matrix(c(1, 1, 1, 1), 2))
suppressWarnings(RNGversion("3.5.0"))
set.seed(1)
.Random.seed[1]
sample(10)
RNGkind(sample.kind = "Rejection")
set.seed(2)
rmultinom(2, 10, c(0.2, 0.3, 0.5))
