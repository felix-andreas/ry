execute <- function(size: I64) -> I64 {
  sum <- 0
  byte_acc <- 0
  bit_num <- 0i64
  fsize <- Num.to_f64(size)

  y <- 0
  while (y < size) {
    ci <- (2.0 * Num.to_f64(y) / fsize) - 1.0
    x <- 0

    while (x < size) {
      zr <- 0.0
      zrzr <- 0.0
      zi <- 0.0
      zizi <- 0.0
      cr <- (2.0 * Num.to_f64(x) / fsize) - 1.5

      z <- 0
      not_done <- Bool.true
      escape <- 0i64
      while (not_done && (z < 50)) {
        zr <- zrzr - zizi + cr
        zi <- 2.0 * zr * zi + ci

        # preserve recalculation
        zrzr <- zr * zr
        zizi <- zi * zi

        if ((zrzr + zizi) > 4.0) {
          not_done <- Bool.false
          escape <- 1i64
        }
        z <- z + 1
      }

      byte_acc <- Num.shift_left_by(byte_acc, 1) + escape

      bit_num <- bit_num + 1

      # Code is very similar for these cases, but using separate blocks
      # ensures we skip the shifting when it's unnecessary, which is most cases.
      if (bit_num == 8) {
        sum <- Num.bitwise_xor(sum, byte_acc)
        byte_acc <- 0i64
        bit_num <- 0i64
      } else if (x == (size - 1)) {
        byte_acc <- Num.shift_left_by(byte_acc, 8 - bit_num)
        sum <- Num.bitwise_xor(sum, byte_acc)
        byte_acc <- 0i64
        bit_num <- 0i64
      }
      x <- x + 1
    }
    y <- y + 1
  }
  sum
}

# ---------------------------------------------------------------------------
# Driver: the same as mandelbrot.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output(result <- Num.to_r(execute(450)))
  seconds <- proc.time()[[3]] - started
  value <- result
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("mandelbrot %s %.4f\n", checksum, seconds))
})
