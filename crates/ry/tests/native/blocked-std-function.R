# blocked: dropped_first
dropped_first <- function(first: F64) -> F64 {
  rest <- List.drop_at([first, 2, 3], 1)
  rest[[1]]
}
cat(dropped_first(1), "\n")
