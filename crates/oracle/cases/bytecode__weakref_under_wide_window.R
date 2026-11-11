# oracle: requires rlang
# A weak reference whose key a deeper call dropped loses its key at a gc() inside a later
# function with a large register window.
make_ref <- function(log) {
  key <- new.env()
  reg.finalizer(key, function(e) log$count <- log$count + 1L)
  rlang::new_weakref(key, value = "payload")
}
deeper <- function(log) make_ref(log)
wide <- function(ref) {
  invisible(gc())
  a <- 1; b <- a * 2; c <- b - a; d <- list(a, b, c); e <- paste(a, b, c)
  f <- nchar(e); g <- c(a, b, c, f); h <- rev(g); i <- sum(h); j <- toupper(e)
  list(key = is.null(rlang::wref_key(ref)), value = rlang::wref_value(ref), n = length(list(d, g, h, i, j)))
}
log <- new.env()
log$count <- 0L
ref <- deeper(log)
wide(ref)
log$count
