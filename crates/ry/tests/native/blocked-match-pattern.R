# blocked: describe
alias Reading = :Value(amount: F64) | :Missing
alias Slot = :Full(reading: Reading) | :Empty
describe <- function(slot: Slot) -> Str {
  match (slot) {
    :Full(reading = :Value(amount = _)) => "value"
    :Full(reading = :Missing) => "missing"
    :Empty => "empty"
  }
}
main <- function(amount: F64) -> Str describe(:Full(reading = :Value(amount = amount)))
cat(main(2), "\n")
