# blocked: as_floats
as_floats <- function(n: I64) -> F64 List.sum(List.map(List.range(1, n), Num.to_f64))
cat(as_floats(4), "\n")
