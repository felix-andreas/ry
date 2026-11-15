# blocked: clamp_all
clamp_all <- function(low: F64, high: F64) -> F64 {
  clamp <- function(v: F64) -> F64 {
    if (v > 1) return(1)
    v
  }
  List.sum(List.map([low, high], clamp))
}
cat(clamp_all(0.5, 3), "\n")
