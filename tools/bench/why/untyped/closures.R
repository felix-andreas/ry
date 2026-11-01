# Closures and lapply: small closures called in a loop, then lapply and vapply over a sequence.
closures <- function(n) {
  add <- function(a, b) a + b
  scale <- 3
  times_scale <- function(a) a * scale
  s <- 0
  for (i in 1:n) s <- add(s, times_scale(i))
  squares <- lapply(seq_len(n), function(x) x * (x + 0.5))
  halves <- vapply(seq_len(n), function(x) x / 2, numeric(1))
  squared <- unlist(squares)
  s + sum(squared) + sum(halves)
}
measure("closures", function() closures(300000L))
