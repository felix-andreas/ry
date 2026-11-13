# The R program lays out the outer product beside a block of zeros, reshapes it so that each
# column shifts down by one row, and sums the rows. The port builds the same column-major list
# and sums its columns into the row totals with whole-list additions.
execute <- function(n: I64) -> F64 {
  convolve_v <- function(x: List<F64>, y: List<F64>) -> List<F64> {
    nx <- List.len(x)
    ny <- List.len(y)
    xy <- List.join_map(y, \(yj) List.concat(x * yj, List.repeat(0, nx)))
    nxy <- nx + ny - 1
    xy <- List.take_first(xy, nxy * ny)
    List.walk(List.range(0, ny - 1), List.repeat(0, nxy), \(sums, column) sums + List.sublist(xy, column * nxy + 1, nxy))
  }

  a <- List.map(List.range(1, n), Num.to_f64)
  b <- List.map(List.range(1, n), Num.to_f64)
  checksum <- 0
  for (i in List.range(1, 10)) {
    checksum <- checksum + convolve_v(a, b)[[n]]
  }
  checksum
}

# ---------------------------------------------------------------------------
# Driver: the same as convolution_v.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output(result <- execute(1500))
  seconds <- proc.time()[[3]] - started
  value <- result
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("convolution_v %s %.4f\n", checksum, seconds))
})
