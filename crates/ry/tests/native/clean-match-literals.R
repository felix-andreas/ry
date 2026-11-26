# `match` on numbers, strings and logicals, with alternatives, `as` and a guard.
alias Shape = :Circle(r: F64) | :Square(side: F64) | :Point
classify <- function(n: I64) -> Str {
  match (n) {
    0i64 | 1i64 => "small"
    big if (big > 100i64) => "big"
    _ => "medium"
  }
}
named <- function(name: Str) -> I64 {
  match (name) {
    "one" => 1i64
    "two" => 2i64
    _ => 0i64
  }
}
sign <- function(flag: Bool) -> F64 {
  match (flag) {
    TRUE => 1
    FALSE => -1
  }
}
extent <- function(shape: Shape) -> F64 {
  match (shape) {
    :Circle(r = x) | :Square(side = x) => x
    :Point as whole => 0
  }
}
ends <- function(values: List<F64>) -> F64 {
  match (values) {
    [] => 0
    [only] => only
    [first, .., last] => first + last
  }
}
main <- function(n: I64) -> F64 {
  total <- (if (classify(n) == "big") 10 else 0) + Num.to_f64(named("two"))
  total + sign(TRUE) + extent(:Square(side = 2)) + ends([1, 2, 3])
}
cat(main(200i64), "\n")
