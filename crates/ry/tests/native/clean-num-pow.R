# `Num.pow` with a fractional and a whole exponent, and `Num.exp`.
grown <- function(principal: F64, rate: F64, years: F64) -> F64 principal * Num.pow(1 + rate, years)
discounted <- function(amount: F64, rate: F64, years: F64) -> F64 amount * Num.exp(-rate * years)
cat(Num.to_r(grown(100, 0.05, 2.5)), Num.to_r(discounted(100, 0.05, 2)), "\n")
