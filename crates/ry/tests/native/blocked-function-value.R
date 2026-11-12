# blocked: adder, add_three
adder <- function(by: F64) -> fn(F64) -> F64 {
  function(x: F64) -> F64 x + by
}
add_three <- function(x: F64) -> F64 {
  step <- adder(3)
  step(x)
}
cat(add_three(1), "\n")
