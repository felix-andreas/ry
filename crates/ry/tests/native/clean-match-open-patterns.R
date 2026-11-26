# Record patterns ending in `..`, at the top and nested, on tags R passes in, and `I64` patterns
# against doubles.
alias Reading = :Value(amount: F64, unit: Str) | :Missing
alias Slot = :Full(reading: Reading) | :Empty
grade <- function(reading: Reading) -> F64 {
  match (reading) {
    :Value(amount = a, ..) => a
    :Missing => 0
  }
}
describe <- function(slot: Slot) -> Str {
  match (slot) {
    :Full(reading = :Value(amount = _, ..)) => "value"
    :Full(reading = :Missing) => "missing"
    :Empty => "empty"
  }
}
pair <- function(p: Tuple{Reading, I64}) -> I64 {
  match (p) {
    (:Value(..), n) => n
    _ => 0i64
  }
}
big <- function(x: F64) -> I64 {
  match (x) {
    9007199254740993i64 => 1i64
    9007199254740992i64 => 2i64
    9223372036854775807i64 => 3i64
    _ => 0i64
  }
}
main <- function(amount: F64) -> Str {
  d <- describe(:Full(reading = :Value(amount = amount, unit = "kg")))
  n <- pair((:Value(amount = amount, unit = "g"), 5i64)) + big(9007199254740992) +
    big(9223372036854775807)
  Str.concat(d, if (n == 7i64) " ok" else " bad")
}
cat(grade(:Value(amount = 2, unit = "kg")), grade(:Missing), main(2), "\n")
