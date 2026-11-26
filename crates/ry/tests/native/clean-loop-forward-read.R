# A loop body that reads a name a later statement of it sets: the read is guarded so that it
# runs only once the name is set, in a `for` loop, a nested loop and a `while` loop, in an inner
# loop when only the outer body sets the name, for a name set from another such name, and for an
# empty list of records.
gaps <- function(xs: List<I64>) -> I64 {
  widest <- 0
  for (x in xs) {
    if (x > 1) {
      gap <- x - last
      if (gap > widest) widest <- gap
    }
    last <- x
  }
  widest
}
nested <- function(n: I64) -> I64 {
  total <- 0
  for (i in List.range(1, n)) {
    for (j in List.range(1, n)) {
      if (j > 1) total <- total + j * seen
      seen <- j
    }
  }
  total
}
counting <- function(n: I64) -> I64 {
  k <- 0
  while (k < n) {
    if (k > 0) k <- k + step else k <- k + 1
    step <- 2
  }
  k
}
enclosing <- function(n: I64) -> I64 {
  total: I64 <- 0
  for (i in List.range(1, n)) {
    for (j in List.range(1, n)) {
      if (i > 1) total <- total + previous
    }
    previous <- i
  }
  total
}
chained <- function(n: I64) -> I64 {
  total: I64 <- 0
  for (i in List.range(1, n)) {
    if (i > 2) total <- total + a + b
    if (i > 1) b <- a
    a <- i
  }
  total
}
alias Stamp = Record{at: I64}
batches <- function(n: I64) -> I64 {
  total: I64 <- 0
  for (i in List.range(1, n)) {
    if (i > 1) total <- total + List.len(batch)
    batch: List<Stamp> <- []
    if (i > 1) batch <- List.append(batch, (at = i,))
  }
  total
}
print(gaps([1, 4, 5, 9, 10]))
print(nested(3))
print(counting(7))
print(enclosing(3))
print(chained(5))
print(batches(3))
