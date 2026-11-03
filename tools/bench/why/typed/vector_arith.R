# Whole-list arithmetic on a million doubles, repeated in a loop.
vector_arith <- function(x: List<F64>, times: I64) -> F64 {
  total <- 0
  for (k in List.range(1, times)) total <- total + List.sum(x * 2 + x / 3 - 1)
  total
}
x <- List.from_r(seq(0, 1, length.out = 1000000L))
measure("vector_arith", function() vector_arith(x, 20))
