# Higher-order calls: lapply and vapply with an anonymous function.
higher_order <- function(n) {
  squares <- lapply(seq_len(n), function(x) x * (x + 0.5))
  halves <- vapply(seq_len(n), function(x) x / 2, numeric(1))
  sum(unlist(squares)) + sum(halves)
}
measure("lapply", function() higher_order(300000L))
