#@ weight 2
#@ param LEVEL float 0.02 0.06 3
#@ param SLOPE float -0.03 -0.005 3
# Nelson-Siegel yield curves, fitted and evaluated.
curve_maturities_@ID@: double[] <- c(0.25, 0.5, 1, 2, 3, 5, 7, 10, 20, 30)

curve_rates_@ID@ <- function(level: double = @LEVEL@, slope: double = @SLOPE@, curvature: double = 0.01, lambda: double = 0.6) -> double[] {
  m <- curve_maturities_@ID@
  loading <- (1 - exp(-lambda * m)) / (lambda * m)
  level + slope * loading + curvature * (loading - exp(-lambda * m))
}

fit_curve_@ID@ <- function(rates: double[] = curve_rates_@ID@(), maturities: double[] = curve_maturities_@ID@) -> matrix {
  rate_series <- zoo::zoo(matrix(rates, nrow = 1, dimnames = list(NULL, paste0("m", maturities))), order.by = as.Date("2024-01-02"))
  YieldCurve::Nelson.Siegel(rate = as.matrix(rate_series), maturity = maturities)
}

forward_rate_@ID@ <- function(rates: double[], maturities: double[], from: double, to: double) -> double[] {
  spot: fn(v: double[]) -> double[] <- stats::approxfun(maturities, rates, rule = 2)
  ((1 + spot(to))^to / (1 + spot(from))^from)^(1 / (to - from)) - 1
}
#@ test
test_that("curve @ID@ fits its own data", {
  rates <- curve_rates_@ID@()
  expect_length(rates, length(curve_maturities_@ID@))
  fit <- fit_curve_@ID@(rates)
  expect_equal(ncol(fit), 4L)
  expect_true(is.finite(forward_rate_@ID@(rates, curve_maturities_@ID@, 1, 2)))
})
