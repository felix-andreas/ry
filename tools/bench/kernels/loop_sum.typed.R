# A numeric loop: a scalar double accumulator in a counted loop.
loop_sum <- function(n: I64) -> F64 {
  s <- 0
  for (i in List.range(1, n)) s <- s + Num.to_f64(i) * 0.5
  s
}
measure("loop_sum", function() loop_sum(10000000))
