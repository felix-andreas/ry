# blocked: first_of
first_of <- function(first: F64, ...: F64) -> F64 first * 2
main <- function(x: F64) -> F64 first_of(x, 3)
cat(main(2), "\n")
