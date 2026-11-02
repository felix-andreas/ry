# A simulation kernel from the finance code base: Vasicek interest-rate paths, one matrix row per step.
vasicek_paths <- function(paths: integer = 200L, steps: integer = 2000L, r0: double = 0.03, kappa: double = 0.5, theta: double = 0.04, sigma: double = 0.01, dt: double = 1 / 12, seed: integer = 9L) -> matrix {
  set.seed(seed)
  rates <- matrix(r0, nrow = steps + 1L, ncol = paths)
  for (t in seq_len(steps)) {
    shock <- stats::rnorm(paths)
    previous: double[] <- rates[t, ]
    rates[t + 1L, ] <- previous + kappa * (theta - previous) * dt + sigma * sqrt(dt) * shock
  }
  rates
}
measure("vasicek", function() sum(vasicek_paths()))
