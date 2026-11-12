execute <- function(n: I64) -> List<F64> {
  x <- List.range(1, n)
  f <- function(x: I64, y: F64) -> F64 Num.to_f64(x) + 1.5 * y
  a <- Num.to_f64(n) / 2
  List.map(x, function(x: I64) -> F64 f(x, a))
}

# ---------------------------------------------------------------------------
# Driver: the same as lapply-dots.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output(result <- execute(1000000))
  seconds <- proc.time()[[3]] - started
  value <- as.list(List.to_r(result))
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("lapply-dots %s %.4f\n", checksum, seconds))
})
