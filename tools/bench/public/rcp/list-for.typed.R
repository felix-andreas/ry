execute <- function(n: I64) -> NULL {
  x <- []
  for (i in List.range(1, n)) {
    x <- List.append(x, i)
  }
  NULL
}

# ---------------------------------------------------------------------------
# Driver: the same as list-for.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output(result <- execute(1500000))
  seconds <- proc.time()[[3]] - started
  value <- result
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("list-for %s %.4f\n", checksum, seconds))
})
