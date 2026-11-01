# Element loops over a million doubles: a dot product of `x[[i]] * y[[i]]`.
# The typed program stores the same values in Arrays, which hold no NA and no attributes.
array_dot <- function(x, y, times) {
  total <- 0
  n <- length(x)
  for (k in seq_len(times)) {
    for (i in seq_len(n)) total <- total + x[[i]] * y[[i]]
  }
  total
}
x <- seq(0, 1, length.out = 1000000L)
y <- rev(x)
measure("array_dot", function() array_dot(x, y, 5L))
