# Element loops over a million doubles: a map computing `x[[i]] * 2 + 1` for every item.
# The typed program holds the values in a List<F64> and maps it with list arithmetic.
array_map <- function(x: List<F64>, times: I64) -> F64 {
  total <- 0
  for (k in List.range(1, times)) total <- total + List.sum(x * 2 + 1)
  total
}
x <- List.from_r(seq(0, 1, length.out = 1000000L))
measure("array_map", function() array_map(x, 5))
