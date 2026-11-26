# blocked: itself, disagreeing, in_local_function, in_literal
# A loop that reads a name in the statement that first sets it, a read after branches that set
# a name to values of different types, and a loop whose local function or function literal reads
# a name a later statement of the loop sets.
itself <- function(n: I64) -> I64 {
  for (i in List.range(1, n)) {
    acc <- acc + i
  }
  n
}
disagreeing <- function(n: I64) -> I64 {
  if (n > 5) {
    x <- 1.5
  } else {
    x <- "small"
  }
  y <- x
  n
}
in_local_function <- function(xs: List<I64>) -> I64 {
  distance <- function(v: I64) -> I64 {
    v - last
  }
  widest <- 0
  for (x in xs) {
    if (x > 1) widest <- widest + distance(x)
    last <- x
  }
  widest
}
in_literal <- function(xs: List<I64>) -> I64 {
  widest <- 0
  for (x in xs) {
    if (x > 1) widest <- widest + List.sum(List.map(xs, \(v) v - last))
    last <- x
  }
  widest
}
acc <- 10
print(itself(3))
print(disagreeing(3))
print(in_local_function([1, 4, 5, 9, 10]))
print(in_literal([1, 4, 5]))
