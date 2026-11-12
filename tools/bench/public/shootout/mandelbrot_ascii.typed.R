# Each point is iterated as a pair of F64 values, because typed ry has no complex numbers. An
# orbit that overflows to NaN is outside the set; R's complex multiplication turns the same
# overflow into an infinity, which is outside the set too.
execute <- function(n: I64) -> List<Str> {
  limit <- 2f64
  iterations <- 50
  hex <- Str.to_utf8("0123456789abcdef")
  fn <- Num.to_f64(n)
  output <- ["P4\n", f"{n} {n} \n"]
  for (y in List.range(0, n - 1)) {
    ci <- 2 * Num.to_f64(y) / fn - 1
    line <- []
    byte: I64 <- 0
    for (x in List.range(0, n - 1)) {
      cr <- 2 * Num.to_f64(x) / fn - 1.5
      zr: F64 <- 0
      zi: F64 <- 0
      for (i in List.range(1, iterations)) {
        next_zr <- zr * zr - zi * zi + cr
        zi <- 2 * zr * zi + ci
        zr <- next_zr
      }
      squared <- zr * zr + zi * zi
      if (!Num.is_nan(squared) && Num.sqrt(squared) <= limit) {
        byte <- byte + Num.pow_int(2, 7 - x %% 8)
      }
      if (x %% 8 == 7 || x == n - 1) {
        line <- List.append(line, hex[[byte %/% 16 + 1]])
        line <- List.append(line, hex[[byte %% 16 + 1]])
        line <- List.append(line, 32)
        byte <- 0i64
      }
    }
    output <- List.append(output, Str.concat(Str.from_utf8_lossy(line), "\n"))
  }
  output
}

# ---------------------------------------------------------------------------
# Driver: the same as mandelbrot_ascii.R's, except that the typed program returns its
# output and the driver prints it inside the clock.
local({
  started <- proc.time()[[3]]
  output <- capture.output(cat(List.to_r(execute(1300)), sep = ""))
  seconds <- proc.time()[[3]] - started
  value <- NULL
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("mandelbrot_ascii %s %.4f\n", checksum, seconds))
})
