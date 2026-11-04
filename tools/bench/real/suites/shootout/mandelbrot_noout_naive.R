# ------------------------------------------------------------------
# The Computer Language Shootout
# http://shootout.alioth.debian.org/
#
# Contributed by Leo Osvald
# ------------------------------------------------------------------

execute <- function(n=500) {
    lim <- 2
    iter <- 50

    mandelbrot_noout_naive <- function(args) {
        n = if (length(args)) as.integer(args[[1]]) else 200L
        n_mod8 = n %% 8L
        pads <- if (n_mod8) rep.int(0, 8L - n_mod8) else integer(0)
        p <- rep(as.integer(rep.int(2, 8) ^ (7:0)), length.out=n)

        cat("P4\n")
        cat(n, n, "\n")
        C <- matrix(0, n, n)
        for (y in 0:(n-1)) {
            C[, y] <- 2 * 0:(n-1) / n - 1.5 + 1i * (2 * y / n - 1)
        }

        m <- n
        Z <- 0                   # initialize Z to zero
        X <- array(0, c(m,m,20)) # initialize output 3D array
        for (k in 1:20) {        # loop with 20 iterations
            Z <- Z^2+C             # the central difference equation
              X[,,k] <- exp(-abs(Z)) # capture results
        }
    }

    mandelbrot_noout_naive(n)
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
local({
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(1000))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("mandelbrot_noout_naive %s %.4f\n", checksum, seconds))
})
