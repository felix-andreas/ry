# blocked: announce
# An R call in the arguments of another R call.
announce <- function(amount: F64) -> F64 {
  cat(format(amount, nsmall = 2), "\n")
  amount
}
cat(announce(1.5), "\n")
