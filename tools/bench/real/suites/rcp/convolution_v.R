execute <- function(n=2000) {
  convolveV <- function(x, y) {
    nx <- length(x)
    ny <- length(y)
    xy <- rbind(outer(x,y), matrix(0, nx, ny))
    nxy <- nx + ny - 1
    length(xy) <- nxy * ny
    dim(xy) <- c(nxy, ny)
    rowSums(xy)
  }

  a <- 1:n
  b <- 1:n
  checksum <- 0
  for (i in 1:10) {
    checksum <- checksum + convolveV(a,b)[[n]]
  }
  # cat("Convolution ", n, " " , checksum, ": ")
  checksum
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(1500))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("convolution_v %s %.4f\n", checksum, seconds))
})
