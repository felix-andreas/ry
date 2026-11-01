#@ weight 3
#@ param TESTS int 5 60
#@ param ALPHA float 0.01 0.1 3
# Multiple-testing correction and power calculations.
#: fn([tests]: integer, [true_effects]: integer, [seed]: integer) -> double[]
simulate_pvalues_@ID@ <- function(tests = @TESTS@L, true_effects = 2L, seed = 21L) {
  set.seed(seed)
  effects <- c(rep(3, true_effects), rep(0, tests - true_effects))
  z <- stats::rnorm(tests, mean = effects)
  2 * stats::pnorm(-abs(z))
}

#: fn(p: double[], [method]: character[]) -> double[]
adjust_@ID@ <- function(p, method = c("bonferroni", "holm", "BH")) {
  method <- match.arg(method)
  stats::p.adjust(p, method = method)
}

#: fn(p: double[], [alpha]: double) -> integer[]
discoveries_@ID@ <- function(p, alpha = @ALPHA@) {
  vapply(c("bonferroni", "holm", "BH"), function(m) sum(adjust_@ID@(p, m) < alpha), integer(1))
}

#: fn(delta: double, [sd]: double, [power]: double) -> double
required_sample_@ID@ <- function(delta, sd = 1, power = 0.8) {
  ceiling(stats::power.t.test(delta = delta, sd = sd, power = power, sig.level = @ALPHA@)$n)
}

#: fn(x: double[], y: double[], [reps]: integer, [seed]: integer) -> double
permutation_test_@ID@ <- function(x, y, reps = 200L, seed = 1L) {
  set.seed(seed)
  observed <- mean(x) - mean(y)
  pooled <- c(x, y)
  #: double[]
  permuted <- replicate(reps, {
    index <- sample(length(pooled), length(x))
    mean(pooled[index]) - mean(pooled[-index])
  })
  (1 + sum(abs(permuted) >= abs(observed))) / (reps + 1)
}
#@ test
test_that("inference helpers @ID@ are ordered", {
  p <- simulate_pvalues_@ID@()
  expect_length(p, @TESTS@)
  found <- discoveries_@ID@(p)
  expect_lte(found[["bonferroni"]], found[["holm"]])
  expect_lte(found[["holm"]], found[["BH"]])
  expect_gt(required_sample_@ID@(0.5), required_sample_@ID@(1))
  expect_lte(permutation_test_@ID@(c(5, 6, 7, 8), c(1, 2, 3, 4)), 0.2)
})
