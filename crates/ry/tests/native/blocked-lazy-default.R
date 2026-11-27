# blocked: offset
# R computes `b` where the body first reads it, after `a` changed, so a call that leaves `b` out
# runs in the interpreter.
offset <- function(a: F64, b: F64 = a * 2.0) -> F64 {
  a <- a + 1.0
  a + b
}
shifted <- function(x: F64) -> F64 offset(x)
cat(shifted(1), "\n")
