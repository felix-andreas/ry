# Runs one R file the way `Rscript` does (every top-level expression is evaluated in the
# global environment and visible values are printed) and writes what the script's own
# output cannot show to a JSON report: messages, warnings, the error that stopped the
# run, and the last value. Any engine that accepts `<script> <report>` and writes the same
# report can stand in for GNU R.
#
# Known differences from Rscript, which the cases avoid: the file is parsed as a whole, so a
# syntax error hides the output of the lines before it; the driver's own frames show up in
# sys.nframe() and sys.calls(); commandArgs(TRUE) holds the two paths; and quit() ends the
# run without a report.
local({
  argv <- commandArgs(trailingOnly = TRUE)
  script <- argv[1L]
  report <- argv[2L]

  messages <- character()
  warnings <- character()
  errors <- character()
  last <- NULL

  # The evaluation frame below is an artifact of this driver: a condition signalled at top
  # level has no call under Rscript.
  top_level <- quote(eval(expression, globalenv()))

  describe <- function(condition) {
    call <- conditionCall(condition)
    if (identical(call, top_level)) call <- NULL
    text <- conditionMessage(condition)
    if (is.null(call)) text else paste0(text, " (in ", deparse(call, nlines = 1L), ")")
  }

  json_string <- function(text) {
    text <- gsub("\\", "\\\\", text, fixed = TRUE)
    text <- gsub("\"", "\\\"", text, fixed = TRUE)
    text <- gsub("\n", "\\n", text, fixed = TRUE)
    text <- gsub("\r", "\\r", text, fixed = TRUE)
    text <- gsub("\t", "\\t", text, fixed = TRUE)
    for (code in setdiff(1:31, c(9L, 10L, 13L))) {
      text <- gsub(rawToChar(as.raw(code)), sprintf("\\u%04x", code), text, fixed = TRUE, useBytes = TRUE)
    }
    paste0("\"", text, "\"")
  }
  json_array <- function(items) paste0("[", paste(vapply(items, json_string, ""), collapse = ","), "]")

  write_report <- function() {
    value <- "null"
    if (!is.null(last)) {
      deparsed <- paste(
        deparse(last$value, control = c("keepNA", "keepInteger", "niceNames", "showAttributes")),
        collapse = "\n"
      )
      value <- sprintf("{\"kind\":%s,\"deparsed\":%s}", json_string(typeof(last$value)), json_string(deparsed))
    }
    body <- sprintf(
      "{\"messages\":%s,\"warnings\":%s,\"errors\":%s,\"value\":%s}\n",
      json_array(messages), json_array(warnings), json_array(errors), value
    )
    writeLines(body, report, useBytes = TRUE)
  }

  expressions <- tryCatch(
    parse(script, keep.source = FALSE, encoding = "UTF-8"),
    error = function(condition) {
      errors <<- c(errors, paste0("parse error: ", conditionMessage(condition)))
      NULL
    }
  )

  for (expression in expressions) {
    failed <- FALSE
    withCallingHandlers(
      tryCatch(
        {
          result <- withVisible(eval(expression, globalenv()))
          last <- list(value = result$value)
          if (result$visible) print(result$value)
        },
        error = function(condition) {
          errors <<- c(errors, describe(condition))
          last <<- NULL
          failed <<- TRUE
        }
      ),
      warning = function(condition) {
        warnings <<- c(warnings, describe(condition))
        if (isTRUE(getOption("warn") >= 2)) return(invisible())
        invokeRestart("muffleWarning")
      },
      message = function(condition) {
        messages <<- c(messages, sub("\n$", "", conditionMessage(condition)))
        invokeRestart("muffleMessage")
      }
    )
    if (failed) break
  }
  write_report()
})
