execute <- function(n: F64) -> F64 {
  a: F64 <- 0
  b: F64 <- 0
  c: F64 <- 0
  d: F64 <- 0
  e: F64 <- 0
  f: F64 <- 0
  g: F64 <- 0
  h: F64 <- 0
  i: F64 <- 0
  j: F64 <- 0
  k: F64 <- 0
  l: F64 <- 0
  m: F64 <- 0
  o: F64 <- 0
  p: F64 <- 0
  q: F64 <- 0
  r: F64 <- 0
  s: F64 <- 0
  t: F64 <- 0
  u: F64 <- 0
  v: F64 <- 0
  w: F64 <- 0
  x: F64 <- 0
  y: F64 <- 0
  z: F64 <- 0
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
# Driver: the same as bytecodes.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output(result <- execute(500000))
  seconds <- proc.time()[[3]] - started
  value <- result
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("bytecodes %s %.4f\n", checksum, seconds))
})
