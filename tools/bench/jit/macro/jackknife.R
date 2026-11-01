# From tools/bench/templates/biostatistics/bootstrap.R: a jackknife through vapply and a closure.
jackknife <- function(x) {
  estimates <- vapply(seq_along(x), function(i) mean(x[-i]), numeric(1))
  n <- length(x)
  c(estimate = mean(estimates), se = sqrt((n - 1) / n * sum((estimates - mean(estimates))^2)))
}
x <- sin(seq_len(3000L)) * 10 + 5
measure("jackknife", function() sum(jackknife(x)))
