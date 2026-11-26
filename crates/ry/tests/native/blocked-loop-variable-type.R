# blocked: loopvar
loopvar <- function(n: I64) -> F64 {
  x <- 1.5
  total <- 0
  for (x in List.range(1, n)) {
    total <- total + x
  }
  Num.to_f64(total)
}
print(loopvar(2))
