# Condition handling: tryCatch around fallible steps, withCallingHandlers that log and muffle
# warnings, custom condition classes, retries, on.exit cleanup, and try(silent = TRUE). Pipelines
# and API wrappers run every unit of work inside handlers like these.
fetch_error <- function(message, attempt) {
  structure(class = c("fetch_error", "error", "condition"),
            list(message = message, call = sys.call(-1), attempt = attempt))
}

fetch <- function(key, attempt) {
  if ((key + attempt) %% 5L == 0L) stop(fetch_error(paste("timeout for", key), attempt))
  if (key %% 7L == 0L) warning("stale value for ", key)
  key * 2
}

with_retry <- function(f, ..., attempts = 3L) {
  for (attempt in seq_len(attempts)) {
    result <- tryCatch(f(..., attempt = attempt), fetch_error = function(e) e)
    if (!inherits(result, "fetch_error")) return(result)
  }
  stop(result)
}

with_logging <- function(expr, log) {
  withCallingHandlers(expr,
    warning = function(w) {
      log$warnings <- log$warnings + 1L
      invokeRestart("muffleWarning")
    },
    message = function(m) {
      log$messages <- log$messages + 1L
      invokeRestart("muffleMessage")
    }
  )
}

process <- function(key, log) {
  state <- new.env()
  state$open <- TRUE
  on.exit(state$open <- FALSE, add = TRUE)
  value <- with_logging({
    if (key %% 11L == 0L) message("checkpoint ", key)
    with_retry(fetch, key)
  }, log)
  stopifnot(state$open)
  value
}

conditions <- function(n) {
  log <- new.env()
  log$warnings <- 0L
  log$messages <- 0L
  total <- 0
  failures <- 0L
  classes <- character(0)
  for (key in seq_len(n)) {
    result <- tryCatch(process(key, log),
      fetch_error = function(e) {
        failures <<- failures + 1L
        NA_real_
      },
      error = function(e) stop(e)
    )
    if (!is.na(result)) total <- total + result
    if (key %% 97L == 0L) {
      outcome <- try(suppressWarnings(log(-1)), silent = TRUE)
      caught <- tryCatch(stop("plain"), error = function(e) class(e))
      classes <- c(classes, caught[[1]])
    }
    signalCondition(simpleCondition("tick"))
  }
  list(total = total, failures = failures, warnings = log$warnings, messages = log$messages,
       classes = table(classes))
}

measure("conditions", function() conditions(10000L))
