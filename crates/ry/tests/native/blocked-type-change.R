# blocked: in_branch, in_loop, in_operand, in_condition, after_local
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
in_condition <- function(n: I64) -> I64 {
  x <- 0.5
  i <- 0
  while ({ cat("x is", x, "\n"); x <- Num.to_str(i); i < n }) {
    i <- i + 1
  }
  i
}
after_local <- function(n: I64) -> I64 {
  x <- n
  g <- function() -> I64 {
    x <- "inner"
    Str.count_utf8_bytes(x)
  }
  k <- g()
  if (n > 1) {
    x <- "big"
  }
  k
}
xs <- [1, 2, 3]
cat(Num.to_r(in_branch(3)), Num.to_r(in_loop(xs)), Num.to_r(in_operand(3)), "\n")
cat(Num.to_r(in_condition(2)), Num.to_r(after_local(3)), "\n")
