#@ weight 1
#@ param PATHS int 5 40
# Interest-rate scenario generation, with an arrays-of-paths layout.
vasicek_paths_@ID@ <- function(paths: integer = @PATHS@L, steps: integer = 24L, r0: double = 0.03, kappa: double = 0.5, theta: double = 0.04, sigma: double = 0.01, dt: double = 1 / 12, seed: integer = 9L) -> matrix {
  set.seed(seed)
  rates: matrix <- matrix(r0, nrow = steps + 1L, ncol = paths)
  for (t in seq_len(steps)) {
    shock <- stats::rnorm(paths)
    current: double[] <- rates[t, ]
    rates[t + 1L, ] <- current + kappa * (theta - current) * dt + sigma * sqrt(dt) * shock
  }
  rates
}

stack_scenarios_@ID@ <- function(...: matrix) -> Any {
  abind::abind(..., along = 3)
}

discount_factors_@ID@ <- function(rates: matrix, dt: double = 1 / 12) -> matrix {
  accumulate <- function(path: double[]) -> double[] cumsum(path) * dt
  factors: @trust matrix <- exp(-apply(rates, 2, accumulate))
  factors
}

esg_available_@ID@ <- function() -> logical {
  requireNamespace("esgtoolkit", quietly = TRUE)
}
#@ test
test_that("scenario paths @ID@ have the requested shape", {
  rates <- vasicek_paths_@ID@()
  expect_equal(dim(rates), c(25L, @PATHS@))
  stacked <- stack_scenarios_@ID@(rates, rates)
  expect_equal(dim(stacked), c(25L, @PATHS@, 2L))
  factors <- discount_factors_@ID@(rates)
  expect_true(all(factors > 0 & factors <= 1.01))
  expect_type(esg_available_@ID@(), "logical")
})
