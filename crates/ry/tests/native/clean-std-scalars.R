# The scalar functions of `Num`, `Bool`, `Str`, `List` and `Result` run natively: predicates,
# arithmetic with its errors, rounding, logarithms and trigonometry, `Bool` logic and equality,
# positions in a list, and tests of a result.
predicates <- function(n: I64, x: F64) -> I64 {
  hits <- 0
  if (Num.is_zero(n - n)) hits <- hits + 1
  if (Num.is_positive(n)) hits <- hits + 2
  if (Num.is_negative(x)) hits <- hits + 4
  if (Num.is_even(n)) hits <- hits + 8
  if (Num.is_odd(n + 1)) hits <- hits + 16
  if (Num.is_finite(x)) hits <- hits + 32
  if (Num.is_infinite(x / 0.5)) hits <- hits + 64
  if (Num.is_multiple_of(n * 3, n)) hits <- hits + 128
  if (Num.is_multiple_of(0, 0)) hits <- hits + 256
  if (Num.is_lt(n, 10) && Num.is_gt(x, -10.0)) hits <- hits + 512
  if (Num.is_lte(n, n) && Num.is_gte(x, x)) hits <- hits + 1024
  hits
}
arithmetic <- function(a: I64, b: I64) -> I64 {
  sum <- Num.add(a, b) + Num.sub(a, b) + Num.mul(a, b)
  sum + Num.div_trunc(-a, b) * 100 + Num.rem(-a, b) * 1000 + Num.abs_diff(a, b) + Num.neg(b)
}
fractions <- function(x: F64) -> F64 {
  whole <- Num.to_f64(Num.floor(x) + Num.ceiling(-x) + Num.to_i64(4.0))
  whole + Num.div(x, 4.0) + Num.add(x, 0.5) + Num.sub(x, 0.25) * Num.mul(x, 2.0) +
    Num.abs_diff(x, 10.0) + Num.neg(x)
}
transcendental <- function(x: F64) -> F64 {
  Num.log(x) + Num.sin(x) + Num.cos(x) + Num.tan(x / 4.0) + Num.atan(x) +
    Num.asin(1.0 / x) + Num.acos(-1.0 / x)
}
logic <- function(n: I64, word: Str) -> I64 {
  score <- 0
  if (Bool.and(n > 1, Bool.not(n > 5))) score <- score + 1
  if (Bool.or(n > 5, Bool.is_eq(word, "go"))) score <- score + 2
  if (Bool.is_not_eq(n, 3) && Bool.is_eq(1.5, 1.5)) score <- score + 4
  if (Bool.is_eq(n > 0, Bool.true)) score <- score + 8
  if (Str.is_empty("") && !Str.is_empty(word)) score <- score + 16
  score
}
positions <- function(xs: List<I64>, at: I64) -> I64 {
  first <- Result.with_default(List.first(xs), -1)
  last <- Result.with_default(List.last(xs), -1)
  got <- List.get(xs, at)
  picked <- Result.with_default(got, 0)
  checks <- 0
  if (Result.is_ok(got)) checks <- checks + 1
  if (Result.is_err(List.get(xs, 0))) checks <- checks + 2
  if (Result.is_err(List.first([]))) checks <- checks + 4
  first * 1000 + last * 100 + picked * 10 + checks
}
cat(Num.to_r(predicates(4, -2.5)), Num.to_r(predicates(3, 1.5)), "\n")
cat(Num.to_r(arithmetic(7, 2)), Num.to_r(arithmetic(-9, 4)), "\n")
cat(fractions(2.5), fractions(-1.25), "\n")
cat(transcendental(2.0), "\n")
cat(Num.to_r(logic(3, "go")), Num.to_r(logic(7, "")), "\n")
cat(Num.to_r(positions([4, 5, 6], 2)), Num.to_r(positions([9], 3)), "\n")
