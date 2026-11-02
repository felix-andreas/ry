#@ weight 3
#@ param DAYS int 60 300
#@ param VOL float 0.005 0.03 4
#@ param ASSETS int 2 6
# Return series on xts objects and their performance statistics.
#: fn([days]: integer, [assets]: integer, [vol]: double, [seed]: integer) -> xts
simulate_prices_@ID@ <- function(days = @DAYS@L, assets = @ASSETS@L, vol = @VOL@, seed = 7L) {
  set.seed(seed)
  #: Date
  dates <- seq(as.Date("2023-01-02"), by = "day", length.out = days)
  #: matrix
  steps <- matrix(stats::rnorm(days * assets, mean = 0.0003, sd = vol), nrow = days)
  #: matrix
  prices <- 100 * apply(1 + steps, 2, cumprod)
  colnames(prices) <- paste0("A", seq_len(assets))
  xts::xts(prices, order.by = dates)
}

#: fn(prices: xts) -> xts
compute_returns_@ID@ <- function(prices) {
  returns <- PerformanceAnalytics::Return.calculate(prices, method = "discrete")
  returns[-1, ]
}

#: fn(returns: xts) -> list{annualised: data.frame, drawdown: matrix | double, sharpe: matrix}
performance_table_@ID@ <- function(returns) {
  list(
    annualised = PerformanceAnalytics::table.AnnualizedReturns(returns, scale = 252),
    drawdown = PerformanceAnalytics::maxDrawdown(returns),
    sharpe = PerformanceAnalytics::SharpeRatio.annualized(returns, scale = 252)
  )
}

#: fn(returns: xts, [window]: integer) -> xts
rolling_volatility_@ID@ <- function(returns, window = 20L) {
  zoo::rollapply(returns, width = window, FUN = stats::sd, by.column = TRUE, align = "right")
}
#@ test
test_that("returns @ID@ are computed from prices", {
  prices <- simulate_prices_@ID@()
  returns <- compute_returns_@ID@(prices)
  expect_equal(nrow(returns), @DAYS@ - 1L)
  expect_equal(ncol(returns), @ASSETS@)
  stats_list <- performance_table_@ID@(returns)
  expect_true(all(as.numeric(stats_list$drawdown) >= 0))
  expect_equal(nrow(stats_list$annualised), 3L)
  rolling <- rolling_volatility_@ID@(returns, window = 20L)
  expect_equal(nrow(rolling), nrow(returns))
  expect_true(all(is.na(rolling[1:19, ])))
  expect_false(anyNA(rolling[20:nrow(rolling), ]))
  expect_true(xts::is.xts(prices))
})
