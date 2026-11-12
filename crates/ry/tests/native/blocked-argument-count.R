# blocked: with_default
scaled <- function(x: F64, factor: F64 = 2) -> F64 x * factor
with_default <- function(x: F64) -> F64 scaled(x)
cat(with_default(3), "\n")
