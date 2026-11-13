# The plane is two flat column-major lists of F64, the real and the imaginary parts, because
# typed ry has no complex numbers, and the 3-d output array is a list of its 20 planes.
execute <- function(n: I64) -> List<Str> {
  fn <- Num.to_f64(n)
  output <- ["P4\n", f"{n} {n} \n"]

  # R's `C[, 0] <- ...` assigns nothing, so the first row's values never land, and the last
  # column keeps the zero it starts with.
  cr <- List.repeat(0f64, n * n)
  ci <- List.repeat(0f64, n * n)
  for (y in List.range(1, n - 1)) {
    for (x in List.range(0, n - 1)) {
      cr[[(y - 1) * n + x + 1]] <- 2 * Num.to_f64(x) / fn - 1.5
      ci[[(y - 1) * n + x + 1]] <- 2 * Num.to_f64(y) / fn - 1
    }
  }

  m <- n
  zr <- List.repeat(0f64, m * m)
  zi <- List.repeat(0f64, m * m)
  planes <- List.repeat(List.repeat(0f64, m * m), 20)
  for (k in List.range(1, 20)) {
    next_zr <- zr * zr - zi * zi + cr
    zi <- zr * zi * 2 + ci
    zr <- next_zr
    planes[[k]] <- List.map2(zr, zi, \(r, im) Num.exp(0 - Num.sqrt(r * r + im * im)))
  }
  output
}

# ---------------------------------------------------------------------------
# Driver: the same as mandelbrot_noout_naive.R's, except that the typed program returns its
# output and the driver prints it inside the clock.
local({
  started <- proc.time()[[3]]
  output <- capture.output(cat(List.to_r(execute(1000)), sep = ""))
  seconds <- proc.time()[[3]] - started
  value <- NULL
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("mandelbrot_noout_naive %s %.4f\n", checksum, seconds))
})
