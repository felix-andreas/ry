# Closures and list maps: small closures called in a loop, then two maps over a range.
closures <- function(n: I64) -> F64 {
  add <- function(a: F64, b: F64) -> F64 a + b
  scale <- 3
  times_scale <- function(a: F64) -> F64 a * scale
  s: F64 <- 0
  for (i in List.range(1, n)) s <- add(s, times_scale(Num.to_f64(i)))
  squares <- List.map(List.range(1, n), function(x: I64) -> F64 Num.to_f64(x) * (Num.to_f64(x) + 0.5))
  halves <- List.map(List.range(1, n), function(x: I64) -> F64 Num.to_f64(x) / 2)
  s + List.sum(squares) + List.sum(halves)
}
measure("closures", function() closures(300000))
