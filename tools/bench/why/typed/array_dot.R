# Element loops over a million doubles: a dot product of `x[[i]] * y[[i]]`.
# The typed program holds the values in a List<F64>, which has no NA and no attributes.
array_dot <- function(x: List<F64>, y: List<F64>, times: I64) -> F64 {
  total: F64 <- 0
  n <- List.len(x)
  for (k in List.range(1, times)) {
    for (i in List.range(1, n)) total <- total + x[[i]] * y[[i]]
  }
  total
}
x <- List.from_r(seq(0, 1, length.out = 1000000L))
y <- List.reverse(x)
measure("array_dot", function() array_dot(x, y, 5))
