# A simulation kernel from the finance code base: Vasicek interest-rate paths, every path stepped
# at once as a list of rates, summing the rates of every step. The shocks come from R's generator.
vasicek <- function(paths: I64 = 200, steps: I64 = 2000, r0: F64 = 0.03, kappa: F64 = 0.5, theta: F64 = 0.04, sigma: F64 = 0.01, dt: F64 = 1f64 / 12f64, seed: I64 = 9) -> F64 {
  set.seed(Num.to_r(seed))
  rates <- List.repeat(r0, paths)
  total <- List.sum(rates)
  for (t in List.range(1, steps)) {
    shock <- List.from_r(stats::rnorm(Num.to_r(paths)))
    rates <- rates + kappa * (theta - rates) * dt + sigma * Num.sqrt(dt) * shock
    total <- total + List.sum(rates)
  }
  total
}
measure("vasicek", function() vasicek())
