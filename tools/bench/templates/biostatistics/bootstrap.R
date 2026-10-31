#@ weight 2
#@ param REPS int 50 300
#@ param SAMPLE int 20 120
# Bootstrap confidence intervals through foreach.
bootstrap_means_@ID@ <- function(x, reps = @REPS@L, seed = 41L) {
  set.seed(seed)
  foreach::foreach(i = seq_len(reps), .combine = c) %do% mean(sample(x, replace = TRUE))
}

percentile_interval_@ID@ <- function(draws, level = 0.95) {
  stats::quantile(draws, c((1 - level) / 2, 1 - (1 - level) / 2), names = FALSE)
}

jackknife_@ID@ <- function(x) {
  estimates <- vapply(seq_along(x), function(i) mean(x[-i]), numeric(1))
  n <- length(x)
  c(estimate = mean(estimates), se = sqrt((n - 1) / n * sum((estimates - mean(estimates))^2)))
}

effect_size_@ID@ <- function(treated, control) {
  pooled <- sqrt(((length(treated) - 1) * stats::var(treated) + (length(control) - 1) * stats::var(control)) / (length(treated) + length(control) - 2))
  (mean(treated) - mean(control)) / pooled
}
#@ test
test_that("bootstrap @ID@ brackets the sample mean", {
  set.seed(1)
  x <- rnorm(@SAMPLE@, mean = 5)
  draws <- bootstrap_means_@ID@(x)
  expect_length(draws, @REPS@)
  interval <- percentile_interval_@ID@(draws)
  expect_lt(interval[[1]], mean(x) + 1e-8)
  expect_gt(interval[[2]], mean(x) - 1e-8)
  expect_equal(jackknife_@ID@(x)[["estimate"]], mean(x), tolerance = 1e-8)
  expect_gt(effect_size_@ID@(x + 1, x), 0)
})
