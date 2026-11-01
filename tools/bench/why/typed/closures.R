# Closures and lapply: small closures called in a loop, then lapply and vapply over a sequence.
closures <- function(n: integer) -> double {
  add <- function(a: double, b: double) -> double a + b
  scale <- 3
  times_scale <- function(a: double) -> double a * scale
  s <- 0
  for (i in 1:n) s <- add(s, times_scale(i))
  squares <- lapply(seq_len(n), function(x: integer) -> double x * (x + 0.5))
  halves <- vapply(seq_len(n), function(x: integer) -> double x / 2, numeric(1))
  squared: double[] <- unlist(squares)
  s + sum(squared) + sum(halves)
}
measure("closures", function() closures(300000L))
