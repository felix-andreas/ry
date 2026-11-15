# blocked: shown
# `List.to_r` outside the arguments of an R call.
shown <- function(values: List<F64>) -> F64 {
  raw <- List.to_r(values)
  print(raw)
  List.sum(values)
}
main <- function(scale: F64) -> F64 shown([scale, 2, 3])
cat(main(1), "\n")
