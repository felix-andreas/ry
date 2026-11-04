# expect: ok
# A literal binding takes its type from the first call that uses it.
half <- function(x: F64) -> F64 x / 2
rate <- 2.0
print(half(rate))
#> [1] 1
