# oracle: requires PerformanceAnalytics, xts, zoo
# The charts finance code draws: PerformanceAnalytics on return series, and xts and zoo
# time series plots.
suppressPackageStartupMessages({
  library(PerformanceAnalytics)
  library(xts)
  library(zoo)
})
data(edhec, package = "PerformanceAnalytics")
data(managers, package = "PerformanceAnalytics")
returns <- managers[, 1:4]

figure("pa_performance_summary", charts.PerformanceSummary(edhec[, 1:3], main = "Strategies"))
figure("pa_cumulative_returns", chart.CumReturns(returns, legend.loc = "topleft", wealth.index = TRUE))
figure("pa_drawdown", chart.Drawdown(edhec[, 1:2], legend.loc = "bottomleft"))
figure("pa_time_series", chart.TimeSeries(returns[, 1:2], date.format = "%Y", legend.loc = "top"))
figure("pa_bar", chart.Bar(managers[, 1], main = "Monthly returns"))
figure("pa_histogram", chart.Histogram(edhec[, 1], methods = c("add.density", "add.normal")))
figure("pa_boxplot", chart.Boxplot(edhec[, 1:6]))
figure("pa_risk_return", chart.RiskReturnScatter(edhec, Rf = 0.003 / 12))
figure("pa_rolling_performance", chart.RollingPerformance(managers[, 1:2], width = 12,
                                                          legend.loc = "topleft"))
figure("pa_correlation", chart.Correlation(edhec[, 1:4], histogram = TRUE))
figure("pa_relative_performance", chart.RelativePerformance(managers[, 1:3], managers[, 8],
                                                            legend.loc = "topleft"))
figure("pa_qqplot", chart.QQPlot(managers[, 2], distribution = "norm", envelope = 0.95))
figure("xts_plot", {
  series <- xts(cumsum(rnorm(250)), order.by = as.Date("2023-01-02") + 0:249)
  print(plot(series, main = "xts series", col = "darkblue"))
})
figure("xts_multi_panel", {
  prices <- xts(apply(matrix(rnorm(600, 0, 0.01), ncol = 3), 2, function(r) 100 * cumprod(1 + r)),
                order.by = as.Date("2022-06-01") + 0:199)
  colnames(prices) <- c("A", "B", "C")
  print(plot(prices, multi.panel = TRUE, yaxis.same = FALSE, main = "Prices"))
})
figure("zoo_plot", {
  series <- zoo(cbind(a = cumsum(rnorm(100)), b = cumsum(rnorm(100))), as.Date("2020-01-01") + 0:99)
  plot(series, plot.type = "single", col = c("red", "blue"), lwd = 2)
})
