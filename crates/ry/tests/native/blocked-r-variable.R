# blocked: discounted
# A global that holds an R value, read inside a native function.
rates <- c(0.01, 0.02, 0.03)
discounted <- function(amount: F64) -> F64 {
  amount * (1 - Num.from_r(rates[[2]]))
}
cat(discounted(100), "\n")
