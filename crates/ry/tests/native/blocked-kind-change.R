# blocked: from_record, from_null, nested, parameter, loop_variable
# A name that takes a second value the typed core stores differently.
from_record <- function(n: I64) -> I64 {
  point <- (x = n, y = n)
  point <- n
  point
}
from_null <- function(n: I64) -> I64 {
  held <- NULL
  held <- n
  held
}
nested <- function(xs: List<I64>) -> I64 {
  ys <- xs
  ys <- [xs, xs]
  List.len(ys)
}
parameter <- function(n: I64) -> F64 {
  n <- Num.to_f64(n) / 2
  n
}
loop_variable <- function(xs: List<I64>) -> F64 {
  total <- 0.5
  for (x in xs) {
    x <- Num.to_f64(x) / 2
    total <- total + x
  }
  total
}
same_type <- function(n: I64) -> I64 {
  n <- n + 1
  n
}
xs <- [1, 2, 3]
cat(Num.to_r(from_record(2)), Num.to_r(from_null(2)), Num.to_r(nested(xs)), "\n")
cat(parameter(3), loop_variable(xs), Num.to_r(same_type(2)), "\n")
