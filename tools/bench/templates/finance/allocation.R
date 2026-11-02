#@ weight 2
#@ param ITEMS int 3 7
#@ param BUDGET int 20 60
# Discrete allocation: lot selection as a mixed-integer programme, plus subset search.
select_lots_@ID@ <- function(values: double[], costs: double[], budget: double = @BUDGET@L) -> integer[] {
  n <- length(values)
  model <- ompr::MIPModel() |>
    ompr::add_variable(x[i], i = 1:n, type = "binary") |>
    ompr::set_objective(ompr::sum_over(values[i] * x[i], i = 1:n), "max") |>
    ompr::add_constraint(ompr::sum_over(costs[i] * x[i], i = 1:n) <= budget)
  loadNamespace("ROI.plugin.highs")
  result <- ompr::solve_model(model, ompr.roi::with_ROI(solver = "highs"))
  picked: double[] <- ompr::get_solution(result, x[i])$value
  as.integer(round(picked))
}

lp_allocation_@ID@ <- function(returns: double[], cap: double = 0.5) -> double[] {
  n <- length(returns)
  constraints: matrix <- rbind(rep(1, n), diag(n))
  solution <- Rglpk::Rglpk_solve_LP(
    obj = returns, mat = constraints,
    dir = c("==", rep("<=", n)), rhs = c(1, rep(cap, n)), max = TRUE
  )
  solution$solution
}

trade_subsets_@ID@ <- function(notionals: double[], target: double) -> list[integer[]] {
  FLSSS::FLSSS(len = length(notionals), v = notionals, target = target, ME = 0, solutionNeed = 1)
}

lot_splits_@ID@ <- function(units: integer) -> partition {
  partitions::parts(units)
}
#@ test
test_that("allocation @ID@ respects its budget", {
  values <- seq(10, by = 3, length.out = @ITEMS@)
  costs <- seq(5, by = 2, length.out = @ITEMS@)
  picked <- select_lots_@ID@(values, costs)
  expect_lte(sum(costs * picked), @BUDGET@)
  weights <- lp_allocation_@ID@(seq(0.01, by = 0.01, length.out = @ITEMS@), cap = 1)
  expect_equal(sum(weights), 1)
  expect_equal(ncol(lot_splits_@ID@(5L)), 7L)
})
