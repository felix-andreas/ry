# blocked: widened
widened <- function(n: I64) -> F64 {
  value <- n
  value <- Num.to_f64(value)
  value
}
cat(widened(2), "\n")
