# Numeric model code as finance and statistics packages write it: vectorized arithmetic inside
# moderate loops, matrix products, apply over rows, quantiles and sorting, with small helper
# functions called per step. Tight scalar loops are deliberately a minor part.
simulate_paths <- function(paths, steps, mu = 0.03, sigma = 0.2, dt = 1 / 252, seed = 1) {
  set.seed(seed)
  levels <- matrix(0, paths, steps + 1)
  levels[, 1] <- 100
  for (t in seq_len(steps)) {
    shock <- rnorm(paths)
    levels[, t + 1] <- levels[, t] * exp((mu - sigma^2 / 2) * dt + sigma * sqrt(dt) * shock)
  }
  levels
}

value_at_risk <- function(pnl, level = 0.99, method = c("historical", "normal")) {
  method <- match.arg(method)
  if (method == "historical") -unname(quantile(pnl, 1 - level, type = 7))
  else -(mean(pnl) + qnorm(1 - level) * sd(pnl))
}

drawdown <- function(path) {
  peaks <- cummax(path)
  max((peaks - path) / peaks)
}

covariance_risk <- function(returns, weights) {
  sigma <- cov(returns)
  sqrt(drop(t(weights) %*% sigma %*% weights))
}

simulation <- function(n) {
  paths <- simulate_paths(n, 252)
  final <- paths[, ncol(paths)]
  pnl <- final - 100
  var_hist <- value_at_risk(pnl)
  var_norm <- value_at_risk(pnl, method = "normal")
  drawdowns <- apply(paths[seq_len(min(n, 500L)), ], 1, drawdown)
  returns <- diff(log(t(paths[1:8, ])))
  risk <- covariance_risk(returns, rep(1 / 8, 8))
  ranks <- order(final, decreasing = TRUE)
  buckets <- table(cut(final, breaks = quantile(final, seq(0, 1, 0.1)), include.lowest = TRUE))
  stepwise <- numeric(200)
  level <- 100
  for (i in seq_along(stepwise)) {
    level <- level * (1 + 0.0001 * sin(i))
    stepwise[[i]] <- level
  }
  list(var = round(c(var_hist, var_norm), 8), drawdown = round(mean(drawdowns), 10), risk = round(risk, 10),
       top = ranks[1:5], buckets = as.vector(buckets), stepwise = round(sum(stepwise), 8))
}

measure("simulation", function() simulation(30000L))
