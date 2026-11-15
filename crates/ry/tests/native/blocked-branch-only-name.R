# blocked: last_seen
last_seen <- function(n: I64) -> F64 {
  for (i in List.range(1, n)) last <- Num.to_f64(i)
  last
}
cat(last_seen(3), "\n")
