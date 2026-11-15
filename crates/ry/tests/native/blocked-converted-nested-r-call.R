# blocked: announce, table
# A converted R call in the arguments of another R call.
announce <- function(amount: F64) -> F64 {
  cat("value", Str.from_r(format(amount, nsmall = 2)), "\n")
  amount
}
table <- function(xs: List<F64>) -> F64 {
  made <- DataFrame(a = List.from_r(c(1, 2)), b = List.to_r(xs))
  List.sum(List.from_r(made$a))
}
ahead <- function(amount: F64) -> F64 {
  text <- Str.from_r(format(amount, nsmall = 2))
  cat("value", text, "\n")
  amount
}
cat(announce(1.5), table([1.5, 2.5]), ahead(1.5), "\n")
