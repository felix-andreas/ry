#@ weight 2
#@ param LEVEL choice info|warn|debug
#@ param ITEMS int 2 12
# Structured logging through a dedicated lgr logger with an in-memory appender.
logger_@ID@ <- function() -> Logger {
  logger <- lgr::get_logger("bench/component@ID@")
  logger$set_propagate(FALSE)
  logger$set_threshold("trace")
  logger$set_appenders(list(memory = lgr::AppenderBuffer$new()))
  logger
}

log_batch_@ID@ <- function(logger: Logger, items: integer[]) -> integer {
  for (item in items) {
    logger$@LEVEL@("processing %s", item, item = item)
  }
  progress_text <- cli::pluralize("{length(items)} item{?s}")
  logger$info("done: %s", progress_text)
  invisible(length(items))
}

log_messages_@ID@ <- function(logger: Logger) -> character[] {
  logger$appenders$memory$data$msg
}
#@ test
test_that("logging @ID@ keeps every record", {
  logger <- logger_@ID@()
  items <- seq_len(@ITEMS@)
  expect_equal(log_batch_@ID@(logger, items), @ITEMS@)
  messages: character[] <- log_messages_@ID@(logger)
  expect_length(messages, @ITEMS@ + 1L)
  expect_match(messages[[1]], "processing 1")
  expect_match(messages[[length(messages)]], "done: ")
})
