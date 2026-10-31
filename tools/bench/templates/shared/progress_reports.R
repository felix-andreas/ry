#@ weight 1
#@ param STEPS int 2 12
# Long-running loops report progress through an injectable bar.
run_steps_@ID@ <- function(steps = @STEPS@L, bar = progress::progress_bar$new(total = steps, format = ":current/:total", clear = FALSE, force = FALSE, width = 40)) {
  results <- numeric(steps)
  for (step in seq_len(steps)) {
    results[[step]] <- step^2
    bar$tick()
  }
  results
}

describe_run_@ID@ <- function(results) {
  cli::format_inline("{length(results)} step{?s}, total {sum(results)}")
}
#@ test
test_that("progress run @ID@ finishes", {
  results <- run_steps_@ID@(bar = list(tick = function() invisible()))
  expect_equal(results, seq_len(@STEPS@)^2)
  expect_match(describe_run_@ID@(results), "@STEPS@ step")
})
