# expect: ok
# A top-level literal binding takes its type from a later top-level use, as it does in a function.
rate <- 2.0
half <- function(x: F64) -> F64 x / 2
print(half(rate))
#> [1] 1
