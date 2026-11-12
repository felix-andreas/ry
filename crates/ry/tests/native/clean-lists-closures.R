# Lists, local functions that capture, and function literals passed to the standard library.
square <- function(x: F64) -> F64 x * x
scaled_sum <- function(n: I64, factor: F64) -> F64 {
  scale <- function(x: F64) -> F64 x * factor
  values <- List.map(List.range(1, n), function(i: I64) -> F64 scale(Num.to_f64(i)))
  squares <- List.map(values, square)
  kept <- List.count_if(squares, function(value) value > 10)
  running <- List.walk(squares, 0, function(total, value) total + value)
  sorted <- List.sort_desc(values)
  doubled <- values * 2 + 1
  grown <- List.with_capacity(n)
  for (value in sorted) grown <- List.append(grown, value)
  grown[[1]] <- grown[[1]] + List.sum(doubled)
  running + Num.to_f64(kept) + grown[[1]] + Num.to_f64(List.len(List.concat(values, sorted)))
}
cat(scaled_sum(6, 1.5), "\n")
