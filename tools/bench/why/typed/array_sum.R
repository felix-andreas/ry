# Element loops over a million doubles: a sum of `x[[i]]`.
# The typed program stores the same values in Arrays, which hold no NA and no attributes.
array_sum <- function(x: Array<double>, y: Array<double>, times: integer) -> double {
  total <- 0
  n <- List.len(x)
  for (k in seq_len(times)) {
    for (i in seq_len(n)) total <- total + x[[i]]
  }
  total
}
x <- as.Array(seq(0, 1, length.out = 1000000L))
y <- as.Array(rev(seq(0, 1, length.out = 1000000L)))
measure("array_sum", function() array_sum(x, y, 5L))
