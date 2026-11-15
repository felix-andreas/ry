# blocked: pick
pick <- function(x: F64, up: Bool) -> F64 {
  if (up) {
    step <- function(v: F64) -> F64 v + 1
    return(step(x))
  }
  x - 1
}
cat(pick(1, TRUE), "\n")
