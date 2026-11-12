# blocked: reversed_first
reversed_first <- function(first: F64) -> F64 {
  flipped <- List.reverse([first, 2, 3])
  flipped[[1]]
}
cat(reversed_first(1), "\n")
