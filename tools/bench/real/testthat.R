# A unit-test suite in the shape package CI runs: many small test_that() blocks, each calling the
# code under test a few times inside testthat's calling handlers, with expect_equal, expect_identical,
# expect_true, expect_error, expect_warning and expect_named. The code under test is small
# validation and formatting helpers built from the predicates such code leans on (is.null, length,
# [[, missing, is.na, identical, is.character).
suppressPackageStartupMessages(library(testthat))

normalize_id <- function(id, width = 6L) {
  if (is.null(id) || length(id) == 0L) return(NA_character_)
  if (is.character(id)) id <- as.integer(sub("^ID", "", id))
  if (is.na(id)) stop("invalid id")
  formatC(id, width = width, flag = "0")
}

check_record <- function(record, required = c("id", "name", "amount")) {
  missing_fields <- required[!required %in% names(record)]
  if (length(missing_fields)) stop("missing fields: ", paste(missing_fields, collapse = ", "))
  if (!is.numeric(record[["amount"]])) stop("amount must be numeric")
  if (record[["amount"]] < 0) warning("negative amount for ", record[["name"]])
  record[["id"]] <- normalize_id(record[["id"]])
  record
}

summarize_amounts <- function(records, by) {
  groups <- vapply(records, function(r) r[[by]], character(1))
  totals <- tapply(vapply(records, function(r) r[["amount"]], numeric(1)), groups, sum)
  totals[order(names(totals))]
}

scale_or <- function(x, factor) {
  if (missing(factor)) factor <- 1
  if (!is.logical(x) && !is.function(x)) x * factor else x
}

make_records <- function(n, seed) {
  lapply(seq_len(n), function(i) list(
    id = if (i %% 2L) i + seed else paste0("ID", i + seed),
    name = c("ann", "bob", "cy")[[i %% 3L + 1L]],
    amount = ((i * 7L + seed) %% 50L) - 3
  ))
}

testthat_suite <- function(blocks) {
  # A package's DESCRIPTION sets the edition; without one testthat searches for it on every block.
  local_edition(3L)
  reporter <- ListReporter$new()
  # testthat 3.3 indexes its running tests by file, and outside test_file() nothing names one.
  reporter$start_file("testthat.R")
  with_reporter(reporter, {
    for (block in seq_len(blocks)) {
      records <- make_records(12L, block)
      test_that(paste("ids", block), {
        expect_equal(normalize_id(block), formatC(block, width = 6L, flag = "0"))
        expect_identical(normalize_id(paste0("ID", block)), normalize_id(block))
        expect_true(is.na(normalize_id(NULL)))
        expect_error(normalize_id("IDx"), "invalid id")
      })
      test_that(paste("records", block), {
        checked <- suppressWarnings(lapply(records, check_record))
        expect_length(checked, 12L)
        expect_named(checked[[1L]], c("id", "name", "amount"))
        expect_true(all(vapply(checked, function(r) nchar(r[["id"]]) == 6L, logical(1))))
        expect_error(check_record(list(id = 1L)), "missing fields")
        expect_warning(check_record(list(id = 1L, name = "z", amount = -1)), "negative")
      })
      test_that(paste("summaries", block), {
        totals <- summarize_amounts(records, "name")
        expect_equal(sum(totals), sum(vapply(records, function(r) r[["amount"]], numeric(1))))
        expect_identical(names(totals), sort(unique(names(totals))))
        expect_equal(scale_or(2, 3), 6)
        expect_equal(scale_or(2), 2)
        expect_false(scale_or(FALSE, 2))
      })
    }
  })
  results <- as.data.frame(reporter$get_results())
  c(tests = nrow(results), failed = sum(results$failed), skipped = sum(results$skipped),
    expectations = sum(results$nb))
}

measure("testthat", function() testthat_suite(12L))
