#@ weight 3
#@ param ASSETS int 3 8
#@ param TARGET float 0.001 0.004 4
# Mean-variance portfolio construction with a quadratic programme.
covariance_@ID@ <- function(assets: integer = @ASSETS@L, seed: integer = 3L) -> matrix {
  set.seed(seed)
  loadings: matrix <- matrix(stats::rnorm(assets * assets), assets)
  ridge: matrix <- diag(0.05, assets)
  crossprod(loadings) / assets + ridge
}

min_variance_@ID@ <- function(sigma: matrix, expected: double[] | NULL = NULL, target: double | NULL = NULL) -> double[] {
  n: integer <- nrow(sigma)
  constraints: matrix <- matrix(1, n, 1)
  bounds <- 1
  if (!is.null(expected) && !is.null(target)) {
    constraints: matrix <- cbind(constraints, expected)
    bounds <- c(bounds, target)
  }
  constraints: matrix <- cbind(constraints, diag(n))
  bounds <- c(bounds, rep(0, n))
  solution <- quadprog::solve.QP(
    Dmat = 2 * sigma, dvec = rep(0, n), Amat = constraints, bvec = bounds,
    meq = if (ncol(constraints) > n + 1L) 2L else 1L
  )
  weights <- solution$solution
  names(weights) <- colnames(sigma)
  weights
}

portfolio_risk_@ID@ <- function(weights: double[], sigma: matrix) -> double {
  sqrt(drop(t(weights) %*% sigma %*% weights))
}

risk_parity_@ID@ <- function(sigma: matrix) -> double[] {
  n: integer <- nrow(sigma)
  objective <- function(w: matrix) -> double {
    contribution <- w * (sigma %*% w)
    sum((contribution - mean(contribution))^2)
  }
  equality <- function(w: matrix) -> list{ceq: double, c: NULL} list(ceq = sum(w) - 1, c = NULL)
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
