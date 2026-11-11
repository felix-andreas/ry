#@ weight 2
#@ param READINGS int 300 900
#@ param LIMIT float 2.5 3.5 2
# Sensor readings summarised into a mean, a spread and counts of outliers and gaps. The typed
# program models a reading as a tag and the summary as a record; the untyped one uses lists with
# a `kind` field.
alias Reading@ID@ = :Value(x: double, weight: double) | :Gap(seconds: double)

readings_@ID@ <- function(count: integer = @READINGS@L, seed: integer = 5L) -> list[Reading@ID@] {
  set.seed(seed)
  values: double[] <- stats::rnorm(count)
  weights: double[] <- stats::runif(count, 0.5, 1.5)
  gaps: logical[] <- stats::runif(count) < 0.08
  out: list[Reading@ID@] <- vector("list", count)
  for (i in seq_len(count)) {
#@ typed
    out[[i]] <- if (gaps[[i]]) :Gap(seconds = 60 * weights[[i]]) else :Value(x = values[[i]], weight = weights[[i]])
#@ untyped
    out[[i]] <- if (gaps[[i]]) list(kind = "gap", seconds = 60 * weights[[i]]) else list(kind = "value", x = values[[i]], weight = weights[[i]])
#@ end
  }
  out
}

summarise_readings_@ID@ <- function(readings: list[Reading@ID@], limit: double = @LIMIT@) -> Record{mean: double, spread: double, outliers: integer, gap: double} {
  total <- 0
  squares <- 0
  weight <- 0
  outliers <- 0L
  gap <- 0
  for (reading in readings) {
#@ typed
    match (reading) {
      :Value(x = x, weight = w) if (abs(x) > limit) => outliers <- outliers + 1L
      :Value(x = x, weight = w) => { total <- total + w * x; squares <- squares + w * x * x; weight <- weight + w }
      :Gap(seconds = s) => gap <- gap + s
    }
#@ untyped
    if (reading$kind == "value" && abs(reading$x) > limit) {
      outliers <- outliers + 1L
    } else if (reading$kind == "value") {
      { total <- total + reading$weight * reading$x; squares <- squares + reading$weight * reading$x * reading$x; weight <- weight + reading$weight }
    } else gap <- gap + reading$seconds
#@ end
  }
  mean <- total / weight
#@ typed
  (mean = mean, spread = sqrt(squares / weight - mean * mean), outliers = outliers, gap = gap)
#@ untyped
  list(mean = mean, spread = sqrt(squares / weight - mean * mean), outliers = outliers, gap = gap)
#@ end
}

readings_spread_@ID@ <- function(batches: integer, count: integer = @READINGS@L) -> double {
  total <- 0
  for (seed in seq_len(batches)) {
    total <- total + summarise_readings_@ID@(readings_@ID@(count, seed))$spread
  }
  total
}

#@ test
test_that("readings @ID@ summarise values, outliers and gaps", {
#@ typed
  readings <- list(:Value(x = 1, weight = 1), :Value(x = 3, weight = 1), :Gap(seconds = 30), :Value(x = 10, weight = 2))
#@ untyped
  readings <- list(list(kind = "value", x = 1, weight = 1), list(kind = "value", x = 3, weight = 1), list(kind = "gap", seconds = 30), list(kind = "value", x = 10, weight = 2))
#@ end
  summary <- summarise_readings_@ID@(readings, limit = 5)
  expect_equal(summary$mean, 2)
  expect_equal(summary$spread, 1)
  expect_equal(summary$outliers, 1L)
  expect_equal(summary$gap, 30)
  expect_true(is.finite(readings_spread_@ID@(2L)))
})
