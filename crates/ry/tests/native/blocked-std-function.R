# blocked: interspersed
interspersed <- function(first: F64) -> F64 {
  spaced <- List.intersperse([first, 2, 3], 0)
  spaced[[1]]
}
cat(interspersed(1), "\n")
