execute <- function(n: F64) -> List<F64> {
  x: F64 <- 0
  i: F64 <- 0
  while (i < n) {
    x <- i
    i <- i + 1
  }
  [x, i]
}

# ---------------------------------------------------------------------------
# Driver: the same as scalar-while.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output(result <- execute(25000000))
  seconds <- proc.time()[[3]] - started
  value <- List.to_r(result)
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("scalar-while %s %.4f\n", checksum, seconds))
})
