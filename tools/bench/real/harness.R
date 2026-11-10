# Shared by every workload. `measure()` runs the workload once cold (GNU R byte-compiles, ry tiers
# up), then `RY_BENCH_REPEAT` more times warm (default 5), and prints one line:
#   <name> <checksum> <median warm seconds> <best warm seconds> <cold seconds>
# A workload returns a value that `checksum()` folds into one string, so both engines must agree on
# every result, not only on the time.
#
# Comparing two builds of ry on the testthat workload reports two phases, each the median of five
# fresh processes with the JIT on and off, plus the peak RSS of the process:
# - test: field 3 of `R_LIBS=$LIB RY_LIBR=$BUILD/libR.so LC_ALL=C.UTF-8 RY_JIT=on|off
#   RY_BENCH_REPEAT=5 $BUILD/ry run --runtime ry testthat_full.R`, where testthat_full.R is this
#   file followed by testthat.R (checksum 27:22821). Start-up and loading testthat are not in it.
# - load: the wall time of a process that runs only
#   `suppressPackageStartupMessages(library(testthat))`, so start-up and the package load.
# $LIB is a library that ry itself installed testthat and its dependencies into, not GNU R's
# site library, and both builds use the same one.
checksum <- function(value) {
  text <- capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
}

measure <- function(name, run) {
  repeats <- as.integer(Sys.getenv("RY_BENCH_REPEAT", "5"))
  started <- proc.time()[[3]]
  value <- run()
  cold <- proc.time()[[3]] - started
  expected <- checksum(value)
  times <- numeric(repeats)
  for (k in seq_len(repeats)) {
    started <- proc.time()[[3]]
    again <- run()
    times[[k]] <- proc.time()[[3]] - started
    if (!identical(checksum(again), expected)) stop("the workload is not deterministic")
  }
  cat(sprintf("%s %s %.4f %.4f %.4f\n", name, expected, stats::median(times), min(times), cold))
}
