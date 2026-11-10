# Element loops over a million doubles: a filter counting the items above one half.
# The typed program holds the values in a List<F64>, which has no NA and no attributes.
array_filter <- function(x: List<F64>, times: I64) -> F64 {
  total: F64 <- 0
  for (k in List.range(1, times)) {
    for (item in x) if (item > 0.5) total <- total + 1
  }
  total
}
x <- List.from_r(seq(0, 1, length.out = 1000000L))
measure("array_filter", function() array_filter(x, 5))
