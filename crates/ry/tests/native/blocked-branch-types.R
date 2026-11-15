# blocked: branches, half
# Two branches that give a name different types, and the value of an `if` without `else`.
branches <- function(n: I64) -> I64 {
  if (n > 5) {
    x <- 1.5
  } else {
    x <- "small"
  }
  y <- x
  n
}
half <- function(n: I64) -> I64 {
  x <- if (n > 5) n
  n
}
agreeing <- function(n: I64) -> I64 {
  if (n > 5) {
    x <- n
  } else {
    x <- n + 1
  }
  n + x
}
statement <- function(n: I64) -> I64 {
  total <- n
  if (n > 5) total <- total + 1
  total
}
cat(Num.to_r(branches(2)), Num.to_r(half(2)), Num.to_r(agreeing(2)), Num.to_r(statement(7)), "\n")
