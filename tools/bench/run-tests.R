#!/usr/bin/env Rscript
# Runs the testthat suite of a generated package under GNU R, with the corpus library.
#   Rscript tools/bench/run-tests.R PACKAGE_DIR [REPORTER]
args <- commandArgs(trailingOnly = TRUE)
if (length(args) < 1) stop("usage: run-tests.R PACKAGE_DIR [REPORTER]")
script <- sub("^--file=", "", grep("^--file=", commandArgs(FALSE), value = TRUE))
repo_root <- normalizePath(file.path(dirname(script), "..", ".."))
corpus_dir <- Sys.getenv("RY_CORPUS_DIR", file.path(repo_root, ".corpus"))
.libPaths(c(file.path(corpus_dir, "library"), .libPaths()))
reporter <- if (length(args) >= 2) args[[2]] else "summary"

# The generated tests are deterministic, but a worker pool left over from an earlier test
# would make a failure order-dependent.
Sys.setenv(NOT_CRAN = "true", TESTTHAT_CPUS = "1")
results <- testthat::test_local(args[[1]], reporter = reporter, stop_on_failure = FALSE)
frame <- as.data.frame(results)
cat(sprintf(
  "\n%s: %d tests, %d failed, %d errors, %d skipped, %d warnings\n",
  basename(normalizePath(args[[1]])), nrow(frame), sum(frame$failed), sum(frame$error), sum(frame$skipped), sum(frame$warning)
))
quit(status = if (sum(frame$failed) + sum(frame$error) > 0) 1 else 0)
