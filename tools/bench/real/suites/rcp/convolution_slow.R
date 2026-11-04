execute <- function(n=1500) {
  convolveSlow <- function(x,y) {
    nx <- length(x)
    ny <- length(y)
    z <- numeric(nx + ny - 1)
    for(i in seq(length = nx)) {
      xi <- x[[i]]
      for(j in seq(length = ny)) {
        ij <- i + j - 1
        z[[ij]] <- z[[ij]] + xi * y[[j]]
      }
    }
    z
  }

  a <- 1:n
  b <- 1:n
  checksum <- 0
  for (i in 1:10) {
    checksum <- checksum + convolveSlow(a,b)[[n]]
  }
  # cat("Convolution ", n, " " , checksum, ": ")
  checksum
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
# The guard skips the driver when another program loads this file for `execute`.
if (!exists(".ry_bench_loading")) local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(1000))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("convolution_slow %s %.4f\n", checksum, seconds))
})
