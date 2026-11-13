# A matrix is a flat column-major List<F64>, because typed ry has no matrices, and `u %*% m`
# is the dot product of u with each column.
execute <- function(n: I64) -> F64 {
  cache <- List.repeat(0f64, n * n)
  for (i in List.range(1, n))
    for (j in List.range(1, n))
      cache[[(j - 1) * n + i]] <- 1 / Num.to_f64(((i + j - 2) * (i + j - 1)) %/% 2 + i)

  # Both products copy the cache element by element, as the R program does through eval_A.
  copy <- \() {
    m <- List.repeat(0f64, n * n)
    for (i in List.range(1, n))
      for (j in List.range(1, n))
        m[[(j - 1) * n + i]] <- cache[[(j - 1) * n + i]]
    m
  }
  ata <- \(u) times(times(u, transpose(copy(), n), n), copy(), n)
  u <- List.repeat(1f64, n)
  v <- List.repeat(0f64, n)
  for (itr in List.range(1, 10)) {
    v <- ata(u)
    u <- ata(v)
  }
  Num.sqrt(List.sum(u * v) / List.sum(v * v))
}

transpose <- function(m: List<F64>, n: I64) -> List<F64>
  List.join_map(List.range(1, n), \(j) List.map(List.range(1, n), \(i) m[[(i - 1) * n + j]]))

times <- function(u: List<F64>, m: List<F64>, n: I64) -> List<F64>
  List.map(List.range(1, n), \(j) List.sum(u * List.sublist(m, (j - 1) * n + 1, n)))

# ---------------------------------------------------------------------------
# Driver: the same as spectralnorm_alt_3.R's, except that the typed program returns the norm and
# the driver prints it inside the clock.
local({
  started <- proc.time()[[3]]
  output <- capture.output({
    options(digits = 10)
    cat(Num.to_r(execute(250)), "\n")
  })
  seconds <- proc.time()[[3]] - started
  value <- NULL
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("spectralnorm_alt_3 %s %.4f\n", checksum, seconds))
})
