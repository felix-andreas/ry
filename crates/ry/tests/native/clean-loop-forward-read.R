# A loop body that reads a name a later statement of it sets: the read is guarded so that it
# runs only once the name is set, in a `for` loop, a nested loop and a `while` loop.
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
print(gaps([1, 4, 5, 9, 10]))
print(nested(3))
print(counting(7))
