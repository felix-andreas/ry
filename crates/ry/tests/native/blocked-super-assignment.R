# blocked: count_up
count_up <- function(n: I64) -> I64 {
  total: I64 <- 0
  bump <- function(k: I64) -> I64 {
    total <<- total + k
    k
  }
  for (i in List.range(1, n)) bump(i)
  total
}
cat(Num.to_r(count_up(4)), "\n")
