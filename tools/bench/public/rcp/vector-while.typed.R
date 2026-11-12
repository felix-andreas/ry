execute <- function(n: I64) -> I64 {
  x <- List.repeat(0, n)
  i <- 1
  while (i < n) {
    x[[i]] <- i
    i <- i + 1
  }
  List.sum(x)
}

# ---------------------------------------------------------------------------
# Driver: the same as vector-while.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output(result <- Num.to_r(execute(20000000)))
  seconds <- proc.time()[[3]] - started
  value <- result
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("vector-while %s %.4f\n", checksum, seconds))
})
