# Element loops over a million doubles: a filter counting the items above one half.
# The typed program stores the same values in Arrays, which hold no NA and no attributes.
array_filter <- function(x: Array<double>, y: Array<double>, times: integer) -> double {
  total <- 0
  n <- List.len(x)
  for (k in seq_len(times)) {
    for (i in seq_len(n)) if (x[[i]] > 0.5) total <- total + 1
  }
  total
}
x <- as.Array(seq(0, 1, length.out = 1000000L))
y <- as.Array(rev(seq(0, 1, length.out = 1000000L)))
measure("array_filter", function() array_filter(x, y, 5L))
