#@ weight 2
#@ param ATTEMPTS int 2 6
# Retry logic with classed conditions and logged failures.
#: fn(message: character) -> rlang_error
transient_error_@ID@ <- function(message) {
  rlang::error_cnd("transient_error", message = message)
}

#: fn(fail_times: integer) -> fn() -> character
flaky_@ID@ <- function(fail_times) {
  calls <- 0L
  function() {
    calls <<- calls + 1L
    if (calls <= fail_times) rlang::cnd_signal(transient_error_@ID@(paste("failure", calls)))
    paste("ok after", calls)
  }
}

#: <T> fn(fn: fn() -> T, [attempts]: integer) -> list{value: T, failures: character[]}
with_retry_@ID@ <- function(fn, attempts = @ATTEMPTS@L) {
  failures <- character()
  for (attempt in seq_len(attempts)) {
    #: Any
    result <- tryCatch(
      fn(),
      transient_error = function(condition) {
        failures <<- c(failures, conditionMessage(condition))
        NULL
      }
    )
    if (!is.null(result)) return(list(value = result, failures = failures))
  }
  rlang::abort("retries exhausted", class = "retry_error", failures = failures)
}

#: <T> fn(expr: T) -> T | character
logged_@ID@ <- function(expr) {
  tryCatchLog::tryCatchLog(
    expr,
    error = function(e) structure(conditionMessage(e), class = "logged_failure"),
    include.full.call.stack = FALSE
  )
}
#@ test
test_that("retry @ID@ recovers or gives up", {
  #: list{value: character, failures: character[]}
  result <- with_retry_@ID@(flaky_@ID@(1L))
  expect_equal(result$value, "ok after 2")
  expect_equal(result$failures, "failure 1")
  expect_error(with_retry_@ID@(flaky_@ID@(@ATTEMPTS@L)), class = "retry_error")
  suppressMessages(suppressWarnings(
    expect_s3_class(logged_@ID@(stop("broken")), "logged_failure")
  ))
})
