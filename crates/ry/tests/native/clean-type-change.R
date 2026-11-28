# A name that takes a second value the typed core stores differently gets a new variable where
# no later code can read the old one: outside any branch or loop, or inside the branch or loop
# body that bound the name.
widened <- function(n: I64) -> F64 {
  value <- n
  value <- Num.to_f64(value)
  value
}
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
loop_local <- function(xs: List<I64>) -> Str {
  out <- ""
  for (x in xs) {
    v <- x * 2
    v <- Num.to_str(v)
    out <- Str.concat(out, v)
  }
  out
}
branch_local <- function(n: I64) -> Str {
  if (n > 1) {
    label <- n * 10
    label <- Num.to_str(label)
    label
  } else {
    "small"
  }
}
same_type <- function(n: I64) -> I64 {
  n <- n + 1
  n
}
xs <- [1, 2, 3]
cat(widened(2), Num.to_r(from_record(2)), Num.to_r(from_null(2)), Num.to_r(nested(xs)), "\n")
cat(parameter(3), loop_variable(xs), loop_local(xs), Num.to_r(same_type(2)), "\n")
cat(branch_local(3), branch_local(1), "\n")
