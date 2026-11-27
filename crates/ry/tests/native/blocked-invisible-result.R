# blocked: mixed, early, mixed_local
# A function whose paths disagree about whether R shows its value: one gives it visibly, another
# invisibly, as after an assignment.
mixed <- function(x: F64) -> F64 if (x > 0) x else { y <- 0 }
early <- function(x: F64) -> F64 {
  if (x > 0) return(x)
  y <- 0
}
mixed_local <- function(x: F64) -> F64 {
  keep <- function(y: F64) -> F64 if (y > 0) y else { z <- y }
  keep(x)
}
mixed(1)
mixed(-1)
early(2)
early(-2)
mixed_local(1)
mixed_local(-1)
