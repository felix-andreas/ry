# blocked: apply_twice, main
apply_twice <- function(step: fn(F64) -> F64, x: F64) -> F64 step(step(x))
bump <- function(x: F64) -> F64 x + 1
main <- function(x: F64) -> F64 apply_twice(bump, x)
cat(main(1), "\n")
