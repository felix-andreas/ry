# An object a deeper call dropped must be finalized by a gc() that runs inside a later function
# whose register window covers the registers the deeper call used, before that function has
# written them.
finalized <- 0L
drop_tracked <- function(n) {
  for (i in seq_len(n)) {
    e <- new.env()
    reg.finalizer(e, function(e) finalized <<- finalized + 1L)
  }
  invisible(NULL)
}
deeper <- function(n) drop_tracked(n)
wide <- function(k) {
  before <- finalized
  invisible(gc())
  after <- finalized
  a <- k + 1; b <- a * 2; c <- b - a; d <- list(a, b, c); e <- paste(a, b, c)
  f <- nchar(e); g <- c(a, b, c, f); h <- rev(g); i <- sum(h); j <- toupper(e)
  m <- list(d, g, h, i, j); o <- length(m) + f; p <- o * i; q <- p - o; r <- c(p, q)
  c(after - before, length(m), r)
}
deeper(3)
wide(1)
deeper(4)
wide(2)
finalized
