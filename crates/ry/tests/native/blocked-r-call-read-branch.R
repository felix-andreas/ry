# blocked: assigned
# An R call compiles natively only where the program drops its value. A branch whose value is
# assigned reads the value of R's `cat`.
assigned <- function(n: I64) -> F64 {
  x <- if (n > 2) cat("a\n") else NULL
  1
}

assigned(1L)
