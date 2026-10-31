#@ weight 3
#@ param LEVEL float 0.9 0.99 2
#@ param SCENARIOS int 50 500
# Historical and parametric value-at-risk, with stress scenarios.
historical_var_@ID@ <- function(returns, level = @LEVEL@) {
  -stats::quantile(returns, probs = 1 - level, names = FALSE)
}

parametric_var_@ID@ <- function(returns, level = @LEVEL@) {
  -(mean(returns) + stats::qnorm(1 - level) * stats::sd(returns))
}

expected_shortfall_@ID@ <- function(returns, level = @LEVEL@) {
  cutoff <- stats::quantile(returns, probs = 1 - level, names = FALSE)
  -mean(returns[returns <= cutoff])
}

stress_@ID@ <- function(weights, shocks) {
  vapply(names(shocks), function(name) sum(weights * shocks[[name]]), numeric(1))
}

scenario_table_@ID@ <- function(n = @SCENARIOS@L, seed = 5L) {
  set.seed(seed)
  returns <- stats::rt(n, df = 5) * 0.01
  data.table::data.table(
    scenario = seq_len(n),
    pnl = returns,
    bucket = cut(returns, breaks = c(-Inf, -0.02, 0, 0.02, Inf), labels = c("tail", "down", "up", "spike"))
  )
}
#@ test
test_that("risk measures @ID@ are ordered", {
  table <- scenario_table_@ID@()
  var_h <- historical_var_@ID@(table$pnl)
  expect_gt(expected_shortfall_@ID@(table$pnl), var_h - 1e-12)
  expect_gt(parametric_var_@ID@(table$pnl), 0)
  shocks <- list(down = c(-0.1, -0.2), up = c(0.1, 0.2))
  expect_equal(stress_@ID@(c(0.5, 0.5), shocks), c(down = -0.15, up = 0.15))
  expect_equal(sum(table[, .N, by = bucket]$N), @SCENARIOS@)
})
