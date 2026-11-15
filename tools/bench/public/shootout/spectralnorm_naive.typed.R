execute <- function(n: I64) -> F64 {
  u <- List.repeat(1f64, n)
  v <- List.repeat(0f64, n)
  for (itr in List.range(1, 10)) {
    v <- times_at(times_a(u, n), n)
    u <- times_at(times_a(v, n), n)
  }
  Num.sqrt(List.sum(u * v) / List.sum(v * v))
}

eval_a <- function(i: I64, j: I64) -> F64
  1 / Num.to_f64(((i + j) * (i + j + 1)) %/% 2 + i + 1)

times_a <- function(u: List<F64>, n: I64) -> List<F64> {
  ret <- List.repeat(0f64, n)
  for (i in List.range(1, n))
    for (j in List.range(0, n - 1))
      ret[[i]] <- ret[[i]] + u[[j + 1]] * eval_a(i - 1, j)
  ret
}

times_at <- function(u: List<F64>, n: I64) -> List<F64> {
  ret <- List.repeat(0f64, n)
  for (i in List.range(1, n))
    for (j in List.range(0, n - 1))
      ret[[i]] <- ret[[i]] + u[[j + 1]] * eval_a(j, i - 1)
  ret
}

# ---------------------------------------------------------------------------
# Driver: the same as spectralnorm_naive.R's, except that the typed program returns the norm and
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
  cat(sprintf("spectralnorm_naive %s %.4f\n", checksum, seconds))
})
