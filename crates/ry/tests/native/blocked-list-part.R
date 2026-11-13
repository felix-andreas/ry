# blocked: second
# A part of a list taken with single brackets.
second <- function(xs: List<I64>) -> I64 {
  part <- xs[2]
  List.len(part)
}
item <- function(xs: List<I64>) -> I64 xs[[2]]
xs <- [1, 2, 3]
cat(Num.to_r(second(xs)), Num.to_r(item(xs)), "\n")
