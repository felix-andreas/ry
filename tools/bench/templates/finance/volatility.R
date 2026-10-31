#@ weight 2
#@ param OMEGA float 0.00001 0.00005 6
#@ param ALPHA float 0.05 0.15 3
#@ param BETA float 0.7 0.85 3
# GARCH volatility models: specification and simulation.
garch_spec_@ID@ <- function(omega = @OMEGA@, alpha = @ALPHA@, beta = @BETA@) {
  rugarch::ugarchspec(
    variance.model = list(model = "sGARCH", garchOrder = c(1, 1)),
    mean.model = list(armaOrder = c(0, 0), include.mean = FALSE),
    fixed.pars = list(omega = omega, alpha1 = alpha, beta1 = beta)
  )
}

simulate_garch_@ID@ <- function(n = 250L, seed = 11L) {
  path <- rugarch::ugarchpath(garch_spec_@ID@(), n.sim = n, m.sim = 1, rseed = seed)
  as.numeric(rugarch::sigma(path))
}

persistence_@ID@ <- function(alpha = @ALPHA@, beta = @BETA@) alpha + beta

multivariate_spec_@ID@ <- function(assets = 2L) {
  single <- rugarch::ugarchspec(mean.model = list(armaOrder = c(0, 0)))
  rmgarch::dccspec(rugarch::multispec(replicate(assets, single)), dccOrder = c(1, 1), distribution = "mvnorm")
}

calibrate_decay_@ID@ <- function(target) {
  objective <- function(par) (par[[1]] - target)^2
  RcppDE::DEoptim(objective, lower = 0, upper = 1, control = RcppDE::DEoptim.control(itermax = 25, trace = FALSE, NP = 20))$optim$bestmem[[1]]
}
#@ test
test_that("volatility @ID@ model is stationary and simulable", {
  expect_lt(persistence_@ID@(), 1)
  sigma <- simulate_garch_@ID@(n = 100L)
  expect_length(sigma, 100L)
  expect_true(all(sigma > 0))
  expect_s4_class(multivariate_spec_@ID@(), "DCCspec")
  expect_equal(calibrate_decay_@ID@(0.3), 0.3, tolerance = 0.05)
})
