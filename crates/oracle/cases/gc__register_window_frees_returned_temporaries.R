# A returned call's temporaries are garbage at once: a collection made by the next call, before
# that call has used the registers the returned call left values in, frees them and runs their
# finalizers. Each function runs many times so its body compiles.
leave <- function() {
  reg.finalizer(new.env(), function(e) cat("finalized\n"))
  NULL
}
collect <- function() {
  gc()
  cat("collected\n")
  a <- 1
  b <- 2
  d <- 3
  a + b + d
}
for (i in 1:30) {
  leave()
  invisible(collect())
}
cat("end\n")
