# blocked: reset
# `assign` writes the frame it is called from, which native code does not keep, so `reset` runs
# in the interpreter, and `main`, which calls it, does too.
reset <- function(a: F64 = 1.0, b: F64 = a) -> F64 {
  assign("a", 5)
  b
}
main <- function(x: F64) -> F64 reset(x) + reset(x, 2.0)
print(main(1.0))
