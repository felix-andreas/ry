# blocked: grade
alias Reading = :Value(amount: F64) | :Missing
grade <- function(reading: Reading) -> Str {
  match (reading) {
    :Value(amount = a) if (a > 10) => "high"
    :Value(amount = _) => "low"
    :Missing => "none"
  }
}
graded <- function(amount: F64) -> Str grade(:Value(amount = amount))
cat(graded(12), "\n")
