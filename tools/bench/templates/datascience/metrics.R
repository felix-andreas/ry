#@ weight 3
#@ param ROWS int 20 200
# Evaluation metrics for regression and classification.
#: fn(actual: double[], predicted: double[]) -> double
rmse_@ID@ <- function(actual, predicted) sqrt(mean((actual - predicted)^2))

#: fn(actual: double[], predicted: double[]) -> double
mae_@ID@ <- function(actual, predicted) mean(abs(actual - predicted))

#: fn(actual: double[], predicted: double[]) -> integer[]
confusion_@ID@ <- function(actual, predicted) {
  table(actual = factor(actual, levels = c(0, 1)), predicted = factor(predicted, levels = c(0, 1)))
}

#: fn(actual: double[], predicted: double[]) -> double
f1_@ID@ <- function(actual, predicted) {
  m <- confusion_@ID@(actual, predicted)
  #: integer
  true_positives <- m[2, 2]
  #: integer[]
  predicted_positives <- m[, 2]
  #: integer[]
  actual_positives <- m[2, ]
  precision <- true_positives / max(sum(predicted_positives), 1)
  recall <- true_positives / max(sum(actual_positives), 1)
  if (precision + recall == 0) 0 else 2 * precision * recall / (precision + recall)
}

#: fn(probability: double[], actual: double[], [bins]: integer) -> data.frame
calibration_@ID@ <- function(probability, actual, bins = 5L) {
  bucket <- cut(probability, seq(0, 1, length.out = bins + 1L), include.lowest = TRUE)
  data.frame(
    bucket = levels(bucket),
    predicted = as.numeric(tapply(probability, bucket, mean)),
    observed = as.numeric(tapply(actual, bucket, mean))
  )
}
#@ test
test_that("metrics @ID@ are consistent", {
  set.seed(1)
  actual <- rbinom(@ROWS@, 1, 0.5)
  expect_equal(rmse_@ID@(actual, actual), 0)
  expect_equal(f1_@ID@(actual, actual), 1)
  expect_gte(rmse_@ID@(actual, 1 - actual), mae_@ID@(actual, 1 - actual))
  expect_equal(sum(confusion_@ID@(actual, actual)), @ROWS@)
  expect_equal(nrow(calibration_@ID@(runif(@ROWS@), actual)), 5L)
})
