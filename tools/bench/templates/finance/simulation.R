#@ weight 1
#@ param PATHS int 5 40
# Interest-rate scenario generation, with an arrays-of-paths layout.
#: fn([paths]: integer, [steps]: integer, [r0]: double, [kappa]: double, [theta]: double, [sigma]: double, [dt]: double, [seed]: integer) -> matrix
vasicek_paths_@ID@ <- function(paths = @PATHS@L, steps = 24L, r0 = 0.03, kappa = 0.5, theta = 0.04, sigma = 0.01, dt = 1 / 12, seed = 9L) {
  set.seed(seed)
  #: matrix
  rates <- matrix(r0, nrow = steps + 1L, ncol = paths)
  for (t in seq_len(steps)) {
    shock <- stats::rnorm(paths)
    #: double[]
    current <- rates[t, ]
    rates[t + 1L, ] <- current + kappa * (theta - current) * dt + sigma * sqrt(dt) * shock
  }
  rates
}

#: fn(...: matrix) -> Any
stack_scenarios_@ID@ <- function(...) {
  abind::abind(..., along = 3)
}

#: fn(rates: matrix, [dt]: double) -> matrix
discount_factors_@ID@ <- function(rates, dt = 1 / 12) {
  #: fn(path: double[]) -> double[]
  accumulate <- function(path) cumsum(path) * dt
  #: @trust matrix
  exp(-apply(rates, 2, accumulate))
}

#: fn() -> logical
esg_available_@ID@ <- function() {
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
