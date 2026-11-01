#@ weight 3
#@ param ASSETS int 3 8
#@ param TARGET float 0.001 0.004 4
# Mean-variance portfolio construction with a quadratic programme.
#: fn([assets]: integer, [seed]: integer) -> matrix
covariance_@ID@ <- function(assets = @ASSETS@L, seed = 3L) {
  set.seed(seed)
  #: matrix
  loadings <- matrix(stats::rnorm(assets * assets), assets)
  #: matrix
  ridge <- diag(0.05, assets)
  crossprod(loadings) / assets + ridge
}

#: fn(sigma: matrix, [expected]: double[] | NULL, [target]: double | NULL) -> double[]
min_variance_@ID@ <- function(sigma, expected = NULL, target = NULL) {
  #: integer
  n <- nrow(sigma)
  #: matrix
  constraints <- matrix(1, n, 1)
  bounds <- 1
  if (!is.null(expected) && !is.null(target)) {
    #: matrix
    constraints <- cbind(constraints, expected)
    bounds <- c(bounds, target)
  }
  #: matrix
  constraints <- cbind(constraints, diag(n))
  bounds <- c(bounds, rep(0, n))
  solution <- quadprog::solve.QP(
    Dmat = 2 * sigma, dvec = rep(0, n), Amat = constraints, bvec = bounds,
    meq = if (ncol(constraints) > n + 1L) 2L else 1L
  )
  weights <- solution$solution
  names(weights) <- colnames(sigma)
  weights
}

#: fn(weights: double[], sigma: matrix) -> double
portfolio_risk_@ID@ <- function(weights, sigma) {
  sqrt(drop(t(weights) %*% sigma %*% weights))
}

#: fn(sigma: matrix) -> double[]
risk_parity_@ID@ <- function(sigma) {
  #: integer
  n <- nrow(sigma)
  #: fn(w: matrix) -> double
  objective <- function(w) {
    contribution <- w * (sigma %*% w)
    sum((contribution - mean(contribution))^2)
  }
  #: fn(w: matrix) -> list{ceq: double, c: NULL}
  equality <- function(w) list(ceq = sum(w) - 1, c = NULL)
  result <- NlcOptim::solnl(
    X = rep(1 / n, n), objfun = objective,
    confun = equality,
    lb = rep(0, n), ub = rep(1, n)
  )
  drop(result$par)
}
#@ test
test_that("portfolio @ID@ weights are feasible", {
  sigma <- covariance_@ID@()
  expected <- seq(0.0005, 0.0015, length.out = @ASSETS@)
  weights <- min_variance_@ID@(sigma)
  expect_equal(sum(weights), 1, tolerance = 1e-8)
  expect_true(all(weights >= -1e-8))
  constrained <- min_variance_@ID@(sigma, expected, sum(expected) / @ASSETS@)
  expect_gte(portfolio_risk_@ID@(constrained, sigma), portfolio_risk_@ID@(weights, sigma) - 1e-8)
  parity <- risk_parity_@ID@(sigma)
  expect_equal(sum(parity), 1, tolerance = 1e-4)
})
