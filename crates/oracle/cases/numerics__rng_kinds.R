# The buggy Kinderman-Ramage normal generator is what RNGversion() picks before 1.7.0.
suppressWarnings(RNGversion("1.6.2"))
RNGkind()
set.seed(1); rnorm(5)
RNGkind(normal.kind = "Kinderman-Ramage")
set.seed(1); rnorm(5)
RNGkind("default", "default", "default")
set.seed(1); rnorm(3)
