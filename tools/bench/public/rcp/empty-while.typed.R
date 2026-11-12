execute <- function(n: F64) -> F64 {
  x <- 1
  while (x < n) {
    x <- x + 1
  }
  x
}

# ---------------------------------------------------------------------------
# Driver: the same as empty-while.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output(result <- execute(20000000))
  seconds <- proc.time()[[3]] - started
  value <- result
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("empty-while %s %.4f\n", checksum, seconds))
})
