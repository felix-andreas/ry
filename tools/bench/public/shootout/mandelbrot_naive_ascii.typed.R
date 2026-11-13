# Each point is iterated as a pair of F64 values, because typed ry has no complex numbers.
execute <- function(n: I64) -> List<Str> {
  lim <- 2f64
  iter <- 50
  hex <- Str.to_utf8("0123456789abcdef")
  fn <- Num.to_f64(n)
  output <- ["P4\n", f"{n} {n} \n"]
  for (y in List.range(0, n - 1)) {
    bits <- 0
    x <- 0
    while (x < n) {
      cr <- 2 * Num.to_f64(x) / fn - 1.5
      ci <- 2 * Num.to_f64(y) / fn - 1
      zr <- 0f64
      zi <- 0f64
      i <- 0
      while (i < iter && Num.sqrt(zr * zr + zi * zi) <= lim) {
        next_zr <- zr * zr - zi * zi + cr
        zi <- 2 * zr * zi + ci
        zr <- next_zr
        i <- i + 1
      }
      bits <- 2 * bits + (if (Num.sqrt(zr * zr + zi * zi) <= lim) 1 else 0)
      x <- x + 1
      if (x %% 8 == 0) {
        output <- List.append(output, Str.concat(Str.from_utf8_lossy([hex[[bits %/% 16 + 1]], hex[[bits %% 16 + 1]]]), " \n"))
        bits <- 0
      }
    }
    xmod <- x %% 8
    if (xmod != 0) {
      byte <- bits * Num.pow_int(2, 8 - xmod)
      output <- List.append(output, Str.concat(Str.from_utf8_lossy([hex[[byte %/% 16 + 1]], hex[[byte %% 16 + 1]]]), " \n"))
    }
  }
  output
}

# ---------------------------------------------------------------------------
# Driver: the same as mandelbrot_naive_ascii.R's, except that the typed program returns its
# output and the driver prints it inside the clock.
local({
  started <- proc.time()[[3]]
  output <- capture.output(cat(List.to_r(execute(300)), sep = ""))
  seconds <- proc.time()[[3]] - started
  value <- NULL
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("mandelbrot_naive_ascii %s %.4f\n", checksum, seconds))
})
