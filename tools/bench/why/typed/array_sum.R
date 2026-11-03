# Element loops over a million doubles: a sum of `x[[i]]`.
# The typed program holds the values in a List<F64>, which has no NA and no attributes.
array_sum <- function(x: List<F64>, times: I64) -> F64 {
  total: F64 <- 0
  for (k in List.range(1, times)) {
    for (item in x) total <- total + item
  }
  total
}
x <- List.from_r(seq(0, 1, length.out = 1000000L))
measure("array_sum", function() array_sum(x, 5))
