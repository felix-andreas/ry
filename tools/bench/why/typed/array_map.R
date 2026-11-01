# Element loops over a million doubles: a map writing `out[[i]] <- x[[i]] * 2 + 1` into a preallocated vector.
# The typed program stores the same values in Arrays, which hold no NA and no attributes.
array_map <- function(x: Array<double>, y: Array<double>, times: integer) -> double {
  total <- 0
  n <- List.len(x)
  out <- as.Array(numeric(n))
  for (k in seq_len(times)) {
    for (i in seq_len(n)) out[[i]] <- x[[i]] * 2 + 1
    total <- total + List.sum(out)
  }
  total
}
x <- as.Array(seq(0, 1, length.out = 1000000L))
y <- as.Array(rev(seq(0, 1, length.out = 1000000L)))
measure("array_map", function() array_map(x, y, 5L))
