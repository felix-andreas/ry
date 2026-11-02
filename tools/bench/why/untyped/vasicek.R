# A simulation kernel from the finance code base: Vasicek interest-rate paths, one matrix row per step.
vasicek_paths <- function(paths = 200L, steps = 2000L, r0 = 0.03, kappa = 0.5, theta = 0.04, sigma = 0.01, dt = 1 / 12, seed = 9L) {
  set.seed(seed)
  rates <- matrix(r0, nrow = steps + 1L, ncol = paths)
  for (t in seq_len(steps)) {
    shock <- stats::rnorm(paths)
    previous <- rates[t, ]
    rates[t + 1L, ] <- previous + kappa * (theta - previous) * dt + sigma * sqrt(dt) * shock
  }
  rates
}
measure("vasicek", function() sum(vasicek_paths()))
