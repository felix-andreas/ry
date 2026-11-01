# Runs a loaded package's testthat suite one `test_that()` block at a time and writes one JSON
# line per block, so two runtimes can be compared test by test. `ry load` has already loaded the
# package, and this file runs after it on either runtime, so the runtimes share everything but
# evaluation.
#
# Environment: RY_BENCH_PACKAGE (the package name), RY_BENCH_TESTS (its tests/testthat
# directory), RY_BENCH_REPORT (the JSON-lines file to write) and RY_BENCH_FILTER (an optional
# regular expression a test file's name must match).
#
# The driver replaces testthat's runner, not its expectations: `test_that` is a local function
# that evaluates the block as the body of a fresh closure (so `withr::local_*` cleanups run when
# the block ends, as under testthat), counts the `expectation` conditions the block signals, and
# resumes after a failed one through testthat's `continue_test` restart. A failure inside
# testthat's own reporter machinery therefore cannot hide every result at once.
local({
  package <- Sys.getenv("RY_BENCH_PACKAGE")
  tests <- Sys.getenv("RY_BENCH_TESTS")
  report <- Sys.getenv("RY_BENCH_REPORT")
  filter <- Sys.getenv("RY_BENCH_FILTER")

  json_string <- function(text) {
    text <- enc2utf8(as.character(text))
    text <- gsub("\\", "\\\\", text, fixed = TRUE)
    text <- gsub("\"", "\\\"", text, fixed = TRUE)
    text <- gsub("\n", "\\n", text, fixed = TRUE)
    text <- gsub("\r", "\\r", text, fixed = TRUE)
    text <- gsub("\t", "\\t", text, fixed = TRUE)
    text <- gsub("[[:cntrl:]]", "?", text)
    paste0("\"", text, "\"")
  }
  json_array <- function(items) paste0("[", paste(vapply(as.character(items), json_string, ""), collapse = ","), "]")
  connection <- file(report, open = "w", encoding = "UTF-8")
  emit <- function(file, test, status, expectations, problems, value, seconds) {
    writeLines(sprintf(
      "{\"file\":%s,\"test\":%s,\"status\":%s,\"expectations\":%d,\"problems\":%s,\"value\":%s,\"seconds\":%.6f}",
      json_string(file), json_string(test), json_string(status), as.integer(expectations),
      json_array(problems), json_string(value), seconds
    ), connection, useBytes = TRUE)
    flush(connection)
  }
  first_line <- function(condition) {
    text <- tryCatch(conditionMessage(condition), error = function(error) "<unprintable condition>")
    sub("\n.*", "", paste(text, collapse = " "))
  }
  # The block's value is usually its last expectation's actual value, which testthat returns
  # invisibly, so comparing it compares what the code computed, not only whether a check held.
  describe_value <- function(value) {
    text <- tryCatch(
      paste(deparse(value, nlines = 20L, control = c("keepNA", "keepInteger", "niceNames", "showAttributes")), collapse = "\n"),
      error = function(error) paste0("<undeparsable ", class(value)[1L], ">")
    )
    if (nchar(text, type = "bytes") > 4000L) text <- paste0(substr(text, 1L, 4000L), "...")
    text
  }

  started <- proc.time()[["elapsed"]]
  status <- tryCatch(
    {
      suppressPackageStartupMessages(library(testthat))
      testthat::local_edition(3L, .env = globalenv())
      NULL
    },
    error = function(error) paste("testthat did not load:", first_line(error))
  )
  if (!is.null(status)) {
    emit("<setup>", "<setup>", "error", 0L, status, "", proc.time()[["elapsed"]] - started)
    close(connection)
    return(invisible())
  }

  namespace <- asNamespace(package)
  test_env <- new.env(parent = namespace)
  current_file <- NULL

  run_block <- function(description, code, file_env) {
    expectations <- 0L
    problems <- character()
    status <- "pass"
    value <- NULL
    block <- function() NULL
    body(block) <- code
    environment(block) <- new.env(parent = file_env)
    began <- proc.time()[["elapsed"]]
    tryCatch(
      withCallingHandlers(
        value <- block(),
        expectation = function(condition) {
          expectations <<- expectations + 1L
          if (inherits(condition, "expectation_skip")) {
            status <<- "skip"
            problems <<- c(problems, paste("skip:", first_line(condition)))
          } else if (!inherits(condition, "expectation_success")) {
            if (status == "pass") status <<- "fail"
            problems <<- c(problems, paste("failure:", first_line(condition)))
          }
          restart <- findRestart("continue_test")
          if (!is.null(restart)) invokeRestart(restart)
        },
        warning = function(condition) {
          problems <<- c(problems, paste("warning:", first_line(condition)))
          invokeRestart("muffleWarning")
        },
        message = function(condition) invokeRestart("muffleMessage")
      ),
      skip = function(condition) {
        status <<- "skip"
        problems <<- c(problems, paste("skip:", first_line(condition)))
      },
      error = function(condition) {
        status <<- "error"
        problems <<- c(problems, paste("error:", first_line(condition)))
      }
    )
    if (status == "pass" && expectations == 0L) status <- "empty"
    emit(current_file, description, status, expectations, problems, describe_value(value), proc.time()[["elapsed"]] - began)
  }

  # Visible to the test files through their enclosure, ahead of testthat's own on the search path.
  test_env$test_that <- function(desc, code) {
    run_block(desc, substitute(code), parent.frame())
    invisible(TRUE)
  }

  files <- sort(list.files(tests, pattern = "^(helper|test).*\\.[rR]$"))
  helpers <- files[startsWith(files, "helper")]
  files <- files[startsWith(files, "test")]
  if (nzchar(filter)) files <- files[grepl(filter, files)]

  for (helper in helpers) {
    current_file <- helper
    outcome <- tryCatch(
      {
        sys.source(file.path(tests, helper), envir = test_env, keep.source = FALSE)
        NULL
      },
      error = function(error) first_line(error)
    )
    if (!is.null(outcome)) emit(helper, "<file>", "error", 0L, paste("error:", outcome), "", 0)
  }

  for (file in files) {
    current_file <- file
    file_env <- new.env(parent = test_env)
    began <- proc.time()[["elapsed"]]
    outcome <- tryCatch(
      {
        expressions <- parse(file.path(tests, file), keep.source = FALSE, encoding = "UTF-8")
        for (expression in expressions) eval(expression, file_env)
        NULL
      },
      error = function(error) first_line(error)
    )
    if (!is.null(outcome)) emit(file, "<file>", "error", 0L, paste("error:", outcome), "", proc.time()[["elapsed"]] - began)
  }
  emit("<total>", "<total>", "pass", 0L, character(), "", proc.time()[["elapsed"]] - started)
  close(connection)
})
