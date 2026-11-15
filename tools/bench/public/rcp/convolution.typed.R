execute <- function(n: I64) -> F64 {
  convolve <- function(a: List<F64>, b: List<F64>) -> List<F64> {
    na <- List.len(a)
    nb <- List.len(b)
    ab <- List.repeat(0, na + nb)
    for (i in List.range(1, na))
      for (j in List.range(1, nb))
        ab[[i + j]] <- ab[[i + j]] + a[[i]] * b[[j]]
    ab
  }

  a <- List.map(List.range(1, n), function(i: I64) -> F64 Num.to_f64(i))
  b <- List.map(List.range(1, n), function(i: I64) -> F64 Num.to_f64(i))
  checksum <- 0
  for (i in List.range(1, 10)) {
    checksum <- checksum + convolve(a, b)[[n]]
  }
  checksum
}

# ---------------------------------------------------------------------------
# Driver: the same as convolution.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output(result <- execute(1000))
  seconds <- proc.time()[[3]] - started
  value <- result
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("convolution %s %.4f\n", checksum, seconds))
})
