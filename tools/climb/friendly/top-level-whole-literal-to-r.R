# expect: type-mismatch
# In a typed file a top-level whole-number literal is an I64, which R code cannot take.
n <- 4
total <- function(x: I64) -> I64 x + 1
print(total(n))
print(sqrt(n))
