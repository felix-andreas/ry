# Prepended to every kernel and application program by bench.py. `measure()` runs the program once
# cold (GNU R byte-compiles, ry tiers up), then `RY_BENCH_REPEAT` more times warm (default 5), and
# prints one line:
#   <name> <checksum> <median warm seconds> <best warm seconds> <cold seconds>
# The checksum folds the result into one string, so every engine must agree on the value, not only
# on the time. A numeric scalar is printed to 15 significant digits whether it is an integer or a
# double, because a typed program returns `F64` where its plain R twin may return an integer.
# Anything else is hashed from its `str()` output.
checksum <- function(value) {
  if (is.numeric(value) && length(value) == 1L && is.null(attributes(value))) {
    return(sprintf("%.15g", as.double(value)))
  }
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
    if (!identical(checksum(again), expected)) stop("the program is not deterministic")
  }
  cat(sprintf("%s %s %.4f %.4f %.4f\n", name, expected, stats::median(times), min(times), cold))
}
