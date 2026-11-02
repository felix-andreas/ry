#@ weight 4
#@ param SUBJECTS int 30 200
#@ param RATE float 0.02 0.2 3
#@ param CENSOR float 0.1 0.5 2
# Kaplan-Meier estimation and a log-rank test, written without a survival package.
#: fn([subjects]: integer, [rate]: double, [censor]: double, [seed]: integer) -> data.frame
simulate_survival_@ID@ <- function(subjects = @SUBJECTS@L, rate = @RATE@, censor = @CENSOR@, seed = 8L) {
  set.seed(seed)
  event_time <- stats::rexp(subjects, rate)
  censor_time <- stats::rexp(subjects, rate * censor)
  data.frame(
    id = seq_len(subjects),
    arm = rep(c("control", "treated"), length.out = subjects),
    time = pmin(event_time, censor_time),
    event = as.integer(event_time <= censor_time)
  )
}

#: fn(time: double[], event: integer[]) -> data.frame
kaplan_meier_@ID@ <- function(time, event) {
  ordered <- order(time)
  time <- time[ordered]
  event <- event[ordered]
  at_risk <- length(time) - seq_along(time) + 1L
  factor_i <- ifelse(event == 1L, 1 - 1 / at_risk, 1)
  data.frame(time = time, at_risk = at_risk, survival = cumprod(factor_i))
}

#: fn(curve: data.frame) -> double
median_survival_@ID@ <- function(curve) {
  #: @if-unknown double[]
  time <- curve$time
  #: @if-unknown double[]
  survival <- curve$survival
  below <- time[survival <= 0.5]
  if (length(below) == 0L) NA_real_ else below[[1]]
}

#: fn(data: data.frame) -> double[]
log_rank_@ID@ <- function(data) {
  #: @if-unknown double[]
  time <- data$time
  #: @if-unknown integer[]
  event <- data$event
  #: @if-unknown character[]
  arm <- data$arm
  times <- sort(unique(time[event == 1L]))
  observed <- 0
  expected <- 0
  variance <- 0
  for (t in times) {
    risk <- time >= t
    n <- sum(risk)
    n1 <- sum(risk & arm == "treated")
    d <- sum(time == t & event == 1L)
    d1 <- sum(time == t & event == 1L & arm == "treated")
    observed <- observed + d1
    expected <- expected + d * n1 / n
    if (n > 1) variance <- variance + d * (n1 / n) * (1 - n1 / n) * (n - d) / (n - 1)
  }
  statistic <- (observed - expected)^2 / variance
  c(statistic = statistic, p_value = stats::pchisq(statistic, df = 1, lower.tail = FALSE))
}
#@ test
test_that("survival analysis @ID@ is monotone and bounded", {
  data <- simulate_survival_@ID@()
  #: @if-unknown double[]
  time <- data$time
  #: @if-unknown integer[]
  event <- data$event
  curve <- kaplan_meier_@ID@(time, event)
  expect_equal(nrow(curve), @SUBJECTS@)
  #: @if-unknown double[]
  survival <- curve$survival
  expect_true(all(diff(survival) <= 1e-12))
  expect_true(all(survival >= 0 & survival <= 1))
  expect_true(is.na(median_survival_@ID@(curve)) || median_survival_@ID@(curve) > 0)
  test <- log_rank_@ID@(data)
  expect_gte(test[["p_value"]], 0)
  expect_lte(test[["p_value"]], 1)
})
