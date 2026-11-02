#@ weight 2
#@ param READINGS int 300 900
#@ param LIMIT float 2.5 3.5 2
# Sensor readings summarised into a mean, a spread and counts of outliers and gaps. The typed
# program models a reading as a tag and the summary as a record; the untyped one uses lists with
# a `kind` field.
#: @alias Reading@ID@ {
#:   | :Value(x: double, weight: double)
#:   | :Gap(seconds: double)
#: }

#: fn([count]: integer, [seed]: integer) -> list[Reading@ID@]
readings_@ID@ <- function(count = @READINGS@L, seed = 5L) {
  set.seed(seed)
  #: double[]
  values <- stats::rnorm(count)
  #: double[]
  weights <- stats::runif(count, 0.5, 1.5)
  #: logical[]
  gaps <- stats::runif(count) < 0.08
  #: list[Reading@ID@]
  out <- vector("list", count)
  for (i in seq_len(count)) {
    #:= out[[i]] <- if (gaps[[i]]) :Gap(seconds = 60 * weights[[i]]) else :Value(x = values[[i]], weight = weights[[i]])
    out[[i]] <- if (gaps[[i]]) list(kind = "gap", seconds = 60 * weights[[i]]) else list(kind = "value", x = values[[i]], weight = weights[[i]])
  }
  out
}

#: fn(readings: list[Reading@ID@], [limit]: double) -> Record{mean: double, spread: double, outliers: integer, gap: double}
summarise_readings_@ID@ <- function(readings, limit = @LIMIT@) {
  total <- 0
  squares <- 0
  weight <- 0
  outliers <- 0L
  gap <- 0
  for (reading in readings) {
    #:= match (reading) {
    #:=   :Value(x = x, weight = w) if (abs(x) > limit) => outliers <- outliers + 1L
    #:=   :Value(x = x, weight = w) => { total <- total + w * x; squares <- squares + w * x * x; weight <- weight + w }
    #:=   :Gap(seconds = s) => gap <- gap + s
    #:= }
    if (reading$kind == "value" && abs(reading$x) > limit) {
      outliers <- outliers + 1L
    } else if (reading$kind == "value") {
      { total <- total + reading$weight * reading$x; squares <- squares + reading$weight * reading$x * reading$x; weight <- weight + reading$weight }
    } else gap <- gap + reading$seconds
  }
  mean <- total / weight
  #:= (mean = mean, spread = sqrt(squares / weight - mean * mean), outliers = outliers, gap = gap)
  list(mean = mean, spread = sqrt(squares / weight - mean * mean), outliers = outliers, gap = gap)
}

#: fn(batches: integer, [count]: integer) -> double
readings_spread_@ID@ <- function(batches, count = @READINGS@L) {
  total <- 0
  for (seed in seq_len(batches)) {
    total <- total + summarise_readings_@ID@(readings_@ID@(count, seed))$spread
  }
  total
}

#@ test
test_that("readings @ID@ summarise values, outliers and gaps", {
  #:= readings <- list(:Value(x = 1, weight = 1), :Value(x = 3, weight = 1), :Gap(seconds = 30), :Value(x = 10, weight = 2))
  readings <- list(list(kind = "value", x = 1, weight = 1), list(kind = "value", x = 3, weight = 1), list(kind = "gap", seconds = 30), list(kind = "value", x = 10, weight = 2))
  summary <- summarise_readings_@ID@(readings, limit = 5)
  expect_equal(summary$mean, 2)
  expect_equal(summary$spread, 1)
  expect_equal(summary$outliers, 1L)
  expect_equal(summary$gap, 30)
  expect_true(is.finite(readings_spread_@ID@(2L)))
})
