# Functions passed to typed functions: a project function, a local function that captures a
# variable, a lambda, standard library functions by name, and a tag variable reassigned in a loop.
alias Light = :Red | :Amber | :Green
apply_twice <- function(step: fn(F64) -> F64, x: F64) -> F64 step(step(x))
compose <- function(f: fn(F64) -> F64, g: fn(F64) -> F64, x: F64) -> F64 apply_twice(f, g(x))
fold <- function(xs: List<F64>, f: fn(F64, F64) -> F64, start: F64) -> F64 List.walk(xs, start, f)
bump <- function(x: F64) -> F64 x + 1
scaled <- function(by: F64, x: F64) -> F64 {
  times <- function(v: F64) -> F64 v * by
  half <- function(v: F64) -> F64 times(v) / 2
  compose(half, \(v) v + by, x)
}
next_light <- function(light: Light) -> Light {
  match (light) {
    :Red => :Green
    :Amber => :Red
    :Green => :Amber
  }
}
cycled <- function(n: I64) -> F64 {
  light <- next_light(:Green)
  for (i in List.range(1, n)) light <- next_light(light)
  match (light) {
    :Red => 1f64
    :Amber => 2f64
    :Green => 3f64
  }
}
by_name <- function(n: I64, xs: List<F64>) -> F64 {
  floats <- List.map(List.range(1, n), Num.to_f64)
  List.sum(floats) + List.sum(List.map(xs, Num.sqrt)) + fold(xs, Num.max, 0)
}
main <- function(x: F64) -> F64 {
  apply_twice(bump, x) + scaled(3, x) + apply_twice(\(v) v * 2, x) + cycled(3) +
    by_name(4, [1, 4, 9.5])
}
cat(main(1), "\n")
