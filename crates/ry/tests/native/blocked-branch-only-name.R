# blocked: itself, disagreeing
# A loop that reads a name in the statement that first sets it, and a read after branches that
# set a name to values of different types.
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
acc <- 10
print(itself(3))
print(disagreeing(3))
