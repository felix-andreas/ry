# A row of points is iterated as two lists of F64, the real and the imaginary parts, because
# typed ry has no complex numbers. An orbit that overflows to NaN is outside the set; R's complex
# multiplication turns the same overflow into an infinity, which is outside the set too.
execute <- function(n: I64) -> List<Str> {
  lim <- 2f64
  iter <- 50
  fn <- Num.to_f64(n)
  n_mod8 <- n %% 8
  pads <- List.repeat(0i64, if (n_mod8 > 0) 8 - n_mod8 else 0i64)
  p <- List.map(List.range(0, n - 1), \(x) Num.pow_int(2i64, 7 - x %% 8))
  hex <- Str.to_utf8("0123456789abcdef")

  output <- ["P4\n", f"{n} {n} \n"]
  for (y in List.range(0, n - 1)) {
    cr <- List.map(List.range(0, n - 1), \(x) 2 * Num.to_f64(x) / fn - 1.5)
    ci <- 2 * Num.to_f64(y) / fn - 1
    zr <- List.repeat(0f64, n)
    zi <- List.repeat(0f64, n)
    i <- 0
    while (i < iter) {
      next_zr <- zr * zr - zi * zi + cr
      zi <- zr * zi * 2 + ci
      zr <- next_zr
      i <- i + 1
    }
    bits <- List.map2(zr, zi, \(r, im) if (Num.sqrt(r * r + im * im) <= lim) 1i64 else 0i64)
    bytes <- List.map(List.chunks_of(List.concat(bits * p, pads), 8), \(chunk) List.sum(chunk))
    line <- List.join_map(bytes, \(byte) [hex[[byte %/% 16 + 1]], hex[[byte %% 16 + 1]], 32i64])
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
