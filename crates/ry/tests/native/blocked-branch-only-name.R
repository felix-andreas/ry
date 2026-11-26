# blocked: deltas
deltas <- function(n: I64) -> I64 {
  total <- 0
  for (i in List.range(1, n)) {
    if (i > 1) {
      total <- total + i - previous
    }
    previous <- i
  }
  total
}
print(deltas(3))
