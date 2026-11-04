execute <- function(n=10000) {
  a <- 0
  b <- 0
  c <- 0
  d <- 0
  e <- 0
  f <- 0
  g <- 0
  h <- 0
  i <- 0
  j <- 0
  k <- 0
  l <- 0
  m <- 0
  o <- 0
  p <- 0
  q <- 0
  r <- 0
  s <- 0
  t <- 0
  u <- 0
  v <- 0
  w <- 0
  x <- 0
  y <- 0
  z <- 0
  while (i < n) {
    a <- i
    b <- i
    c <- i
    d <- i
    e <- i
    f <- i
    g <- i
    h <- i
    j <- i
    k <- i
    l <- i
    m <- i
    o <- i
    p <- i
    q <- i
    r <- i
    s <- i
    t <- i
    u <- i
    v <- i
    w <- i
    x <- i
    y <- i
    z <- i
    i <- i + 1
  }
  a + b + c + d + e + f + g + h + j + k + l + m + o + p + q + r + s + t + u + v + w + x + y + z
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
# The guard skips the driver when another program loads this file for `execute`.
if (!exists(".ry_bench_loading")) local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(500000))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("bytecodes %s %.4f\n", checksum, seconds))
})
