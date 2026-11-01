# A value that only an earlier statement of a block produced is garbage once the next
# statement starts, so a finalizer runs at the gc() inside the same block.
e <- new.env()
reg.finalizer(e, function(x) cat("finalized\n"))
{
  e <- NULL
  invisible(gc())
  cat("after gc\n")
}
f <- function() {
  a <- new.env()
  reg.finalizer(a, function(x) cat("finalized in f\n"))
  a <- NULL
  invisible(gc())
  cat("end of f\n")
}
f()
