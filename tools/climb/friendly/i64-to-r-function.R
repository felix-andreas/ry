# expect: type-mismatch
# An I64 passed to an R function is a boundary error before the code runs.
n <- 4i64
print(sqrt(n))
