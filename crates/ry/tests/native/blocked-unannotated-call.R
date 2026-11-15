# blocked: doubled
# A project function without annotations counts as an R function.
answer <- function() 42
doubled <- function(x: F64) -> F64 {
  base <- answer()
  x * Num.from_r(base)
}
cat(doubled(2), "\n")
