# blocked: taxed
rate: F64 <- 0.2
taxed <- function(amount: F64) -> F64 amount * (1 + rate)
cat(taxed(100), "\n")
