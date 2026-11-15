execute <- function(n: I64) -> F64 {
  convolve_slow <- function(x: List<I64>, y: List<I64>) -> List<F64> {
    nx <- List.len(x)
    ny <- List.len(y)
    z <- List.repeat(0, nx + ny - 1)
    for (i in List.range(1, nx)) {
      xi <- x[[i]]
      for (j in List.range(1, ny)) {
        ij <- i + j - 1
        z[[ij]] <- z[[ij]] + Num.to_f64(xi * y[[j]])
      }
    }
    z
  }

  a <- List.range(1, n)
  b <- List.range(1, n)
  checksum <- 0
  for (i in List.range(1, 10)) {
    checksum <- checksum + convolve_slow(a, b)[[n]]
  }
  checksum
}

# ---------------------------------------------------------------------------
# Driver: the same as convolution_slow.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output(result <- execute(1000))
  seconds <- proc.time()[[3]] - started
  value <- result
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("convolution_slow %s %.4f\n", checksum, seconds))
})
