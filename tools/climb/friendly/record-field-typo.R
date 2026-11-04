# expect: type-mismatch
# A misspelled record field is an error.
p <- (x = 1.5f64, y = 2.0f64)
print(p$z)
