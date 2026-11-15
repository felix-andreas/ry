# blocked: first_or_zero
first_or_zero <- function(first: F64) -> F64 {
  match ([first, 5]) {
    [] => 0
    [head, ..] => head
  }
}
cat(first_or_zero(4), "\n")
