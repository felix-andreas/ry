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
# pkgload refuses to load a package whose Imports are not installed, and a few of them
# (polars needs a newer R, Rblpapi needs the Bloomberg SDK) cannot be installed here. The code
# reaches them only through `::` inside injectable or guarded branches, so the tests run on a
# copy whose DESCRIPTION leaves the missing packages out.
package_dir <- file.path(tempfile("bench"), basename(normalizePath(args[[1]])))
dir.create(dirname(package_dir))
file.copy(normalizePath(args[[1]]), dirname(package_dir), recursive = TRUE)
description <- read.dcf(file.path(package_dir, "DESCRIPTION"))
imports <- trimws(strsplit(description[, "Imports"], ",")[[1]])
names <- sub("\\s*\\(.*", "", imports)
available <- names %in% rownames(installed.packages())
if (any(!available)) {
  message("not installed, skipped in Imports: ", paste(names[!available], collapse = ", "))
  description[, "Imports"] <- paste(imports[available], collapse = ",\n    ")
  write.dcf(description, file.path(package_dir, "DESCRIPTION"))
}
results <- testthat::test_local(package_dir, reporter = reporter, stop_on_failure = FALSE)
frame <- as.data.frame(results)
for (entry in results) {
  for (expectation in entry$results) {
    if (inherits(expectation, c("expectation_failure", "expectation_error"))) {
      cat(sprintf("FAILED [%s] %s\n    %s\n", entry$file, entry$test, gsub("\n.*", "", conditionMessage(expectation))))
    }
  }
}
cat(sprintf(
  "\n%s: %d tests, %d failed, %d errors, %d skipped, %d warnings\n",
  basename(normalizePath(args[[1]])), nrow(frame), sum(frame$failed), sum(frame$error), sum(frame$skipped), sum(frame$warning)
))
quit(status = if (sum(frame$failed) + sum(frame$error) > 0) 1 else 0)
