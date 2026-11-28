# blocked: in_branch, in_loop, in_operand
# A name bound before a branch or loop that takes a value of another type inside it: code after
# the branch or loop could read either value.
in_branch <- function(n: I64) -> I64 {
  x <- n
  if (n > 1) {
    x <- Num.to_f64(n)
  }
  n
}
in_loop <- function(xs: List<I64>) -> I64 {
  last <- 0
  for (x in xs) {
    last <- Num.to_str(x)
  }
  List.len(xs)
}
in_operand <- function(n: I64) -> I64 {
  x <- n
  if (n > 1 && { x <- "big"; TRUE }) n else 0
}
xs <- [1, 2, 3]
cat(Num.to_r(in_branch(3)), Num.to_r(in_loop(xs)), Num.to_r(in_operand(3)), "\n")
