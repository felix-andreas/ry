# blocked: counted
counted <- function(n: I64) -> F64 {
  total <- 0
  for (i in 1:3) total <- total + Num.to_f64(n)
  total
}
cat(counted(2), "\n")
