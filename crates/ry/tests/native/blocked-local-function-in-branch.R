# blocked: pick
pick <- function(x: F64, up: Bool) -> F64 {
  step <- function(v: F64) -> F64 v - 1
  if (up) {
    step <- function(v: F64) -> F64 v + 1
  }
  step(x)
}
cat(pick(1, TRUE), "\n")
