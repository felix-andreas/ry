# blocked: cycled
alias Light = :Red | :Amber | :Green
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
cat(cycled(3), "\n")
