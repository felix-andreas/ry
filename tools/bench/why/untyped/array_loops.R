# Element loops: a dot product read item by item in a counted loop over a million doubles. The
# typed program stores the same values in Arrays, which hold no NA and no attributes.
array_loops <- function(x, y, times) {
  total <- 0
  n <- length(x)
  for (k in seq_len(times)) {
    for (i in seq_len(n)) total <- total + x[[i]] * y[[i]]
  }
  total
}
x <- seq(0, 1, length.out = 1000000L)
y <- rev(x)
measure("array_loops", function() array_loops(x, y, 5L))
