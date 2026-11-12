execute <- function(n: I64) -> List<F64> {
  u <- List.repeat(1, n)
  v <- times_ata(u, n)
  times_ata(v, n)
}

eval_a <- function(i: I64, j: I64) -> F64
  1 / Num.to_f64(((i + j) * (i + j + 1)) %/% 2 + i + 1)

times_a <- function(u: List<F64>, n: I64) -> List<F64>
  List.map(List.range(0, n - 1), \(i) List.sum(List.map(List.range(0, n - 1), \(j) u[[j + 1]] * eval_a(i, j))))

times_at <- function(u: List<F64>, n: I64) -> List<F64>
  List.map(List.range(0, n - 1), \(i) List.sum(List.map(List.range(0, n - 1), \(j) u[[j + 1]] * eval_a(j, i))))

times_ata <- function(u: List<F64>, n: I64) -> List<F64>
  times_at(times_a(u, n), n)

# ---------------------------------------------------------------------------
# Driver: the same as spectralnorm.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output({
    options(digits = 10)
    result <- execute(800)
  })
  seconds <- proc.time()[[3]] - started
  value <- List.to_r(result)
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("spectralnorm %s %.4f\n", checksum, seconds))
})
