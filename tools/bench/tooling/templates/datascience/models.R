#@ weight 4
#@ param ROWS int 60 300
#@ param NOISE float 0.1 1.5 2
#@ param FOLDS int 3 6
# Linear and logistic models with k-fold cross-validation.
simulate_regression_@ID@ <- function(rows: integer = @ROWS@L, noise: double = @NOISE@, seed: integer = 4L) -> data.frame {
  set.seed(seed)
  x1 <- stats::rnorm(rows)
  x2 <- stats::rnorm(rows)
  data.frame(x1 = x1, x2 = x2, y = 1 + 2 * x1 - x2 + stats::rnorm(rows, sd = noise))
}

fit_model_@ID@ <- function(data: data.frame) -> Any {
  model <- y ~ x1 + x2
  stats::lm(model, data = data)
}

folds_@ID@ <- function(n: integer, k: integer = @FOLDS@L) -> integer[] sample(rep_len(seq_len(k), n))

cross_validate_@ID@ <- function(data: data.frame, k: integer = @FOLDS@L) -> double[named] {
  assignment <- folds_@ID@(nrow(data), k)
  errors <- vapply(seq_len(k), function(fold: integer) -> double {
    fit <- fit_model_@ID@(data[assignment != fold, ])
    held_out <- data[assignment == fold, ]
    sqrt(mean((stats::predict(fit, held_out) - held_out$y)^2))
  }, numeric(1))
  c(mean = mean(errors), sd = stats::sd(errors))
}

fit_logistic_@ID@ <- function(data: data.frame) -> Any {
  response: double[] <- data$y
  data$label <- as.integer(response > stats::median(response))
  model <- label ~ x1 + x2
  stats::glm(model, data = data, family = stats::binomial())
}

auc_@ID@ <- function(score: double[], label: integer[]) -> double {
  ranks <- rank(score)
  positives <- sum(label == 1L)
  negatives <- sum(label == 0L)
  (sum(ranks[label == 1L]) - positives * (positives + 1) / 2) / (positives * negatives)
}
#@ test
test_that("models @ID@ recover the signal", {
  data <- simulate_regression_@ID@()
  coefficients <- stats::coef(fit_model_@ID@(data))
  expect_equal(unname(coefficients[["x1"]]), 2, tolerance = 0.6)
  scores <- cross_validate_@ID@(data)
  expect_gt(scores[["mean"]], 0)
  logistic <- fit_logistic_@ID@(data)
  response: double[] <- data$y
  label <- as.integer(response > stats::median(response))
  expect_gt(auc_@ID@(stats::fitted(logistic), label), 0.7)
})
